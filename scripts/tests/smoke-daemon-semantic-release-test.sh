#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-semantic-release-smoke-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

release_root="${tmp}/release-root"
mkdir -p "${release_root}/contracts" "${release_root}/scripts"
for release_script in \
  check-public-cli-build-info.py \
  dev-install-from-metadata.sh \
  public-cli-host-runtime-evidence.sh \
  public-cli-runtime-authority.sh \
  smoke-daemon-semantic-release.sh; do
  cp -L "${repo_root}/scripts/${release_script}" \
    "${release_root}/scripts/${release_script}"
done
chmod 0755 "${release_root}/scripts/"*
cp -L "${repo_root}/contracts/release-targets-v1.json" \
  "${release_root}/contracts/release-targets-v1.json"
test -f "${release_root}/contracts/release-targets-v1.json"
test ! -L "${release_root}/contracts/release-targets-v1.json"
cat > "${tmp}/ubuntu-22.04-os-release" <<'EOF'
ID=ubuntu
VERSION_ID="22.04"
EOF
mv \
  "${release_root}/scripts/public-cli-host-runtime-evidence.sh" \
  "${release_root}/scripts/public-cli-host-runtime-evidence-real.sh"
cat > "${release_root}/scripts/public-cli-host-runtime-evidence.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
for argument in "\$@"; do
  if [[ "\${argument}" == "--os-baseline-only" ]]; then
    exec "${release_root}/scripts/public-cli-host-runtime-evidence-real.sh" \
      "\$@" --os-release "${tmp}/ubuntu-22.04-os-release"
  fi
done
exec "${release_root}/scripts/public-cli-host-runtime-evidence-real.sh" "\$@"
EOF
chmod 0755 "${release_root}/scripts/public-cli-host-runtime-evidence.sh"
smoke="${release_root}/scripts/smoke-daemon-semantic-release.sh"

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
[[ "${CTX_DAEMON_ENABLED:-}" == "true" ]]
[[ "${CTX_SEARCH_SEMANTIC:-}" == "true" ]]
[[ "${CTX_SEMANTIC_CACHE_DIR:-}" == "${data_root}/semantic-cache" ]]

case "${command}" in
  import)
    if find "${CTX_SEMANTIC_CACHE_DIR}" -mindepth 1 -print -quit | grep -q .; then
      printf 'foreground import observed non-empty semantic cache\n' >&2
      exit 1
    fi
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
        mkdir -p "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model"
        printf 'daemon-owned verified model\n' \
          > "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model/complete"
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
grep -Fq -- '--require-authoritative' "${tmp}/help.out"

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
expect_usage_failure retired_proof_output \
  'Usage:' \
  --coreml --runtime-platform macos-arm64 --proof-output "${tmp}/proof" \
  --ctx "${fake_ctx}"

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
"${smoke}" \
  --coreml \
  --runtime-platform macos-arm64 \
  --ctx "${fake_ctx}" \
  --data-root "${run_parent}" \
  --timeout-seconds 30 \
  --keep-root \
  > "${tmp}/coreml.out" 2> "${tmp}/coreml.err"

run_root="$(find "${run_parent}" -mindepth 1 -maxdepth 1 -type d -name 'ctx-semantic-smoke.*' -print -quit)"
[[ -n "${run_root}" ]]
test ! -e "${run_root}/data/packaged-runtime-proof.txt"
grep -Fq 'ctx semantic smoke ok:' "${tmp}/coreml.out"
[[ ! -e "${run_root}/data/runtime/onnxruntime" ]]

daemon_pid="$(cat "${run_root}/data/fake-daemon-pid")"
if kill -0 "${daemon_pid}" >/dev/null 2>&1; then
  printf 'CoreML smoke left daemon process %s running\n' "${daemon_pid}" >&2
  exit 1
fi

cpu_ctx="${tmp}/ctx-linux-cpu"
sed \
  -e 's/== "coreml"/== "cpu"/' \
  -e '/CTX_SEMANTIC_COREML_NATIVE_COMPUTE/d' \
  -e 's/"backend":"coreml","compute_mode":"all"/"backend":"cpu","preference":"cpu"/g' \
  "${fake_ctx}" > "${cpu_ctx}"
chmod 755 "${cpu_ctx}"
printf 'synthetic lock\n' > "${tmp}/Cargo.lock"
python3 "${repo_root}/scripts/write-public-cli-build-info.py" \
  --output "${cpu_ctx}.build-info.json" \
  --artifact "${cpu_ctx}" \
  --cargo-lock "${tmp}/Cargo.lock" \
  --platform linux-x64 \
  --target x86_64-unknown-linux-gnu \
  --source-commit 0123456789abcdef0123456789abcdef01234567 \
  --source-clean true \
  --rust-version "rustc 1.97.1 (8bab26f4f 2026-07-14)" \
  --expected-builder-base sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 \
  --actual-builder-base sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 \
  --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --builder-recipe-sha256 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd \
  --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --linux-builder-image docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982 \
  --linux-ubuntu-snapshot 20260701T000000Z \
  --linux-glibc-max 2.35 \
  --linux-rust-toolchain 1.97.1 \
  --linux-rust-commit 8bab26f4f68e0e26f0bb7960be334d5b520ea452 \
  --linux-rust-sysroot /opt/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu \
  --static-status passed \
  --local-runtime-status passed \
  --local-runtime-authority authoritative

runtime_payload="${tmp}/runtime-payload"
mkdir -p "${runtime_payload}/lib"
printf 'license\n' > "${runtime_payload}/LICENSE"
printf 'notices\n' > "${runtime_payload}/ThirdPartyNotices.txt"
printf '1.27.0\n' > "${runtime_payload}/VERSION_NUMBER"
printf 'synthetic-commit\n' > "${runtime_payload}/GIT_COMMIT_ID"
printf 'synthetic runtime\n' > "${runtime_payload}/lib/libonnxruntime.so"
runtime_archive="${tmp}/ctx-onnxruntime-linux-x64.tar.gz"
tar --no-recursion -C "${runtime_payload}" -czf "${runtime_archive}" \
  LICENSE ThirdPartyNotices.txt VERSION_NUMBER GIT_COMMIT_ID lib lib/libonnxruntime.so
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${runtime_archive}" | awk '{ print $1 }' > "${runtime_archive}.sha256"
else
  shasum -a 256 "${runtime_archive}" | awk '{ print $1 }' > "${runtime_archive}.sha256"
fi

if ! "${smoke}" \
  --runtime-archive "${runtime_archive}" \
  --runtime-platform linux-x64 \
  --ctx "${cpu_ctx}" \
  --data-root "${tmp}/onnx-runs" \
  --require-authoritative \
  --timeout-seconds 30 \
  > "${tmp}/onnx.out" 2> "${tmp}/onnx.err"; then
  cat "${tmp}/onnx.out" >&2
  cat "${tmp}/onnx.err" >&2
  exit 1
fi
grep -Fq 'ctx semantic smoke ok:' "${tmp}/onnx.out"
if find "${tmp}/onnx-runs" -name packaged-runtime-proof.txt -print -quit | grep -q .; then
  printf 'semantic smoke emitted a retired proof artifact\n' >&2
  exit 1
fi

printf 'daemon semantic release smoke contract tests passed\n'
