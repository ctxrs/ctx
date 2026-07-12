#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
smoke="${repo_root}/scripts/smoke-daemon-semantic-release.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-semantic-release-smoke-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

fake_ctx="${tmp}/ctx-macos-artifact"
cat > "${fake_ctx}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  printf 'ctx 0.25.0\n'
  exit 0
fi

data_root=""
command=""
while (($# > 0)); do
  case "$1" in
    --data-root)
      data_root="${2:-}"
      shift 2
      ;;
    import|daemon|search)
      command="$1"
      shift
      break
      ;;
    *)
      printf 'unexpected fake ctx prefix argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

[[ -n "${data_root}" && -n "${command}" ]]
[[ "${CTX_INTERNAL_SEMANTIC_BACKEND:-}" == "coreml" ]]
[[ "${CTX_SEMANTIC_COREML_NATIVE_COMPUTE:-}" == "all" ]]
[[ "${CTX_DAEMON_ENABLED:-}" == "1" ]]
[[ "${CTX_SEARCH_SEMANTIC:-}" == "1" ]]
[[ "${CTX_SEMANTIC_CACHE_DIR:-}" == "${data_root}/semantic-cache" ]]

case "${command}" in
  import)
    fixture=""
    while (($# > 0)); do
      if [[ "$1" == "--path" ]]; then
        fixture="${2:-}"
        break
      fi
      shift
    done
    [[ -f "${fixture}" ]]
    grep -Eo 'ctx-release-semantic-smoke-[0-9a-f]+' "${fixture}" | head -1 \
      > "${data_root}/fake-marker"
    ;;
  daemon)
    subcommand="${1:-}"
    shift || true
    case "${subcommand}" in
      run)
        printf '%s\n' "$$" > "${data_root}/fake-daemon-pid"
        trap 'exit 0' TERM INT
        while :; do sleep 1; done
        ;;
      status)
        pid="$(cat "${data_root}/fake-daemon-pid")"
        printf '{"daemon":{"pid":%s,"status":"running","running":true,"jobs":{"semantic_index":{"embedding_runtime":{"backend":"coreml","compute_mode":"all","model_id":"intfloat/multilingual-e5-small","acquisition_source":"download"}}}}}\n' "${pid}"
        ;;
      *)
        printf 'unexpected fake daemon command: %s\n' "${subcommand}" >&2
        exit 1
        ;;
    esac
    ;;
  search)
    marker="$(cat "${data_root}/fake-marker")"
    printf '{"retrieval":{"requested_mode":"semantic","effective_mode":"semantic","semantic_status":"ready","embedding_model":"intfloat/multilingual-e5-small","worker":{"embedding_runtime":{"backend":"coreml","compute_mode":"all","model_id":"intfloat/multilingual-e5-small","acquisition_source":"download"}}},"results":[{"text":"%s"}]}\n' "${marker}"
    ;;
esac
EOF
chmod 755 "${fake_ctx}"
fake_ctx="$(cd -- "$(dirname -- "${fake_ctx}")" && pwd -P)/$(basename -- "${fake_ctx}")"

expect_usage_failure() {
  local name="$1"
  local expected="$2"
  shift 2
  if "${smoke}" "$@" > "${tmp}/${name}.out" 2> "${tmp}/${name}.err"; then
    printf 'expected argument failure: %s\n' "${name}" >&2
    exit 1
  fi
  grep -Fq -- "${expected}" "${tmp}/${name}.err" || {
    printf 'unexpected argument failure for %s\n' "${name}" >&2
    cat "${tmp}/${name}.err" >&2
    exit 1
  }
}

"${smoke}" --help > "${tmp}/help.out" 2>&1
grep -Fq -- '--coreml --runtime-platform macos-arm64|macos-x64' "${tmp}/help.out"

expect_usage_failure coreml_linux \
  '--coreml requires --runtime-platform macos-arm64 or macos-x64' \
  --coreml --runtime-platform linux-x64 --ctx "${fake_ctx}"
expect_usage_failure coreml_archive \
  '--coreml cannot be combined with --runtime-archive' \
  --coreml --runtime-platform macos-arm64 --runtime-archive "${tmp}/unused" \
  --ctx "${fake_ctx}"
expect_usage_failure archive_required \
  '--runtime-archive is required unless --coreml is selected' \
  --runtime-platform macos-arm64 --ctx "${fake_ctx}"

cpu_ctx="${tmp}/ctx-macos-cpu-fallback"
sed 's/"backend":"coreml"/"backend":"cpu"/g' "${fake_ctx}" > "${cpu_ctx}"
chmod 755 "${cpu_ctx}"
started="$(date +%s)"
if "${smoke}" \
  --coreml --runtime-platform macos-arm64 --ctx "${cpu_ctx}" \
  --data-root "${tmp}/cpu-fallback-runs" --timeout-seconds 30 \
  > "${tmp}/cpu-fallback.out" 2> "${tmp}/cpu-fallback.err"; then
  printf 'CoreML smoke accepted a CPU runtime\n' >&2
  exit 1
fi
elapsed="$(( $(date +%s) - started ))"
[[ "${elapsed}" -lt 10 ]] || {
  printf 'CoreML backend mismatch did not fail fast: %ss\n' "${elapsed}" >&2
  exit 1
}
grep -Fq 'CoreML daemon status reported backend' "${tmp}/cpu-fallback.err"

cpu_mode_ctx="${tmp}/ctx-macos-cpu-mode"
sed 's/"compute_mode":"all"/"compute_mode":"cpu_only"/g' "${fake_ctx}" > "${cpu_mode_ctx}"
chmod 755 "${cpu_mode_ctx}"
if "${smoke}" \
  --coreml --runtime-platform macos-arm64 --ctx "${cpu_mode_ctx}" \
  --data-root "${tmp}/cpu-mode-runs" --timeout-seconds 30 \
  > "${tmp}/cpu-mode.out" 2> "${tmp}/cpu-mode.err"; then
  printf 'CoreML smoke accepted CPU-only compute mode\n' >&2
  exit 1
fi
grep -Fq "CoreML daemon status reported compute mode 'cpu_only'" "${tmp}/cpu-mode.err"

cached_ctx="${tmp}/ctx-macos-cached-model"
sed 's/"acquisition_source":"download"/"acquisition_source":"cache"/g' \
  "${fake_ctx}" > "${cached_ctx}"
chmod 755 "${cached_ctx}"
if "${smoke}" \
  --coreml --runtime-platform macos-arm64 --ctx "${cached_ctx}" \
  --data-root "${tmp}/cached-runs" --timeout-seconds 30 \
  > "${tmp}/cached.out" 2> "${tmp}/cached.err"; then
  printf 'CoreML smoke accepted a cached acquisition\n' >&2
  exit 1
fi
grep -Fq "CoreML daemon status reported acquisition source 'cache'" "${tmp}/cached.err"

run_parent="${tmp}/runs"
published_proof="${tmp}/published/coreml-proof.txt"
"${smoke}" \
  --coreml \
  --runtime-platform macos-arm64 \
  --ctx "${fake_ctx}" \
  --data-root "${run_parent}" \
  --proof-output "${published_proof}" \
  --timeout-seconds 30 \
  --keep-root \
  > "${tmp}/coreml.out" 2> "${tmp}/coreml.err"

run_root="$(find "${run_parent}" -mindepth 1 -maxdepth 1 -type d -name 'ctx-semantic-smoke.*' -print -quit)"
[[ -n "${run_root}" ]]
proof="${run_root}/data/packaged-runtime-proof.txt"
[[ -s "${proof}" ]]
cmp -s "${proof}" "${published_proof}"
grep -Fxq 'runtime=coreml' "${proof}"
grep -Fxq 'platform=macos-arm64' "${proof}"
grep -Fxq "host_system=$(uname -s)" "${proof}"
grep -Fxq "host_arch=$(uname -m)" "${proof}"
grep -Fxq "host_native_arch=$(uname -m)" "${proof}"
grep -Fxq 'process_translated=0' "${proof}"
grep -Fxq 'native_arch_probe=uname' "${proof}"
grep -Fxq 'runtime_authority=non_authoritative' "${proof}"
grep -Fxq 'compute_mode=all' "${proof}"
grep -Fxq 'model=intfloat/multilingual-e5-small' "${proof}"
grep -Fxq 'acquisition_source=download' "${proof}"
grep -Fxq 'acquisition_fallback=none' "${proof}"
grep -Fxq 'semantic_search=passed' "${proof}"

if command -v sha256sum >/dev/null 2>&1; then
  expected_sha="$(sha256sum "${fake_ctx}" | awk '{ print $1 }')"
else
  expected_sha="$(shasum -a 256 "${fake_ctx}" | awk '{ print $1 }')"
fi
grep -Fxq "artifact=${fake_ctx}" "${proof}"
grep -Fxq "artifact_sha256=${expected_sha}" "${proof}"
isolated_artifact="$(sed -n 's/^isolated_artifact=//p' "${proof}")"
cmp -s "${fake_ctx}" "${isolated_artifact}"
[[ ! -e "${run_root}/data/runtime/onnxruntime" ]]

daemon_pid="$(cat "${run_root}/data/fake-daemon-pid")"
if kill -0 "${daemon_pid}" >/dev/null 2>&1; then
  printf 'CoreML smoke left daemon process %s running\n' "${daemon_pid}" >&2
  exit 1
fi

printf 'daemon semantic release smoke contract tests passed\n'
