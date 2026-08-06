#!/usr/bin/env bash
set -euo pipefail
umask 0002

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
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
case "${HOME}" in
  "${data_root}"|"${data_root}"/*)
    printf 'semantic smoke HOME overlaps the ctx data root\n' >&2
    exit 1
    ;;
esac
case "${CTX_SEMANTIC_CACHE_DIR:-}" in
  "${data_root}"|"${data_root}"/*)
    printf 'semantic smoke model cache overlaps the ctx data root\n' >&2
    exit 1
    ;;
esac

case "${command}" in
  import)
    for _ in {1..100}; do
      [[ -s "${data_root}/fake-daemon-pid" ]] && break
      sleep 0.01
    done
    [[ -s "${data_root}/fake-daemon-pid" ]] || {
      printf 'semantic smoke imported before launching its daemon\n' >&2
      exit 1
    }
    no_daemon=0
    if find "${CTX_SEMANTIC_CACHE_DIR}" -mindepth 1 -print -quit | grep -q .; then
      printf 'daemon-backed import observed model state before publication\n' >&2
      exit 1
    fi
    fixture=""
    while (($# > 0)); do
      case "$1" in
        --path)
          fixture="${2:-}"
          shift 2
          ;;
        --no-daemon)
          no_daemon=1
          shift
          ;;
        *)
          shift
          ;;
      esac
    done
    [[ "${no_daemon}" == "1" ]] || {
      printf 'semantic smoke import could autostart an unowned daemon\n' >&2
      exit 1
    }
    [[ -f "${fixture}" ]]
    case "${fixture}" in
      "${data_root}"|"${data_root}"/*)
        printf 'semantic smoke fixture overlaps the ctx data root\n' >&2
        exit 1
        ;;
    esac
    grep -Eo 'ctx-release-semantic-smoke-[0-9a-f]+' "${fixture}" | head -1 \
      > "${data_root}/fake-marker"
    ;;
  daemon)
    subcommand="${1:-}"
    shift || true
    case "${subcommand}" in
      run)
        [[ -s "${data_root}/config.toml" ]] || {
          printf 'semantic smoke launched its daemon before writing config\n' >&2
          exit 1
        }
        printf '%s\n' "$$" > "${data_root}/fake-daemon-pid"
        mkdir -p "${data_root}/daemon"
        printf '{"schema_version":1,"pid":%s,"transport":"unix","path":"/tmp/fake-ctx-semantic-smoke.sock","token":"0123456789abcdef0123456789abcdef"}\n' "$$" \
          > "${data_root}/daemon/source-refresh-endpoint.json"
        trap 'exit 0' TERM INT
        while [[ ! -f "${data_root}/fake-marker" ]]; do sleep 0.05; done
        mkdir -p "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model"
        printf 'daemon-owned verified model\n' \
          > "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model/complete"
        while :; do sleep 1; done
        ;;
      status)
        pid="$(cat "${data_root}/fake-daemon-pid")"
        if [[ -s "${data_root}/fake-marker" && \
              -s "${CTX_SEMANTIC_CACHE_DIR}/fake-verified-model/complete" ]]; then
          semantic_status=ready
          indexed_chunks=1
        else
          semantic_status=pending
          indexed_chunks=0
        fi
        printf '{"daemon":{"pid":%s,"status":"running","running":true,"jobs":{"semantic_index":{"status":"%s","model_key":"e5-small-v1:mean-pool:l2:query-passage","indexed_chunks":%s}}}}\n' \
          "${pid}" "${semantic_status}" "${indexed_chunks}"
        ;;
      *)
        printf 'unexpected fake daemon command: %s\n' "${subcommand}" >&2
        exit 1
        ;;
    esac
    ;;
  search)
    marker="$(cat "${data_root}/fake-marker")"
    printf '{"retrieval":{"requested_mode":"semantic","effective_mode":"semantic","semantic_status":"ready"},"results":[{"text":"%s"}]}\n' "${marker}"
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
python3 -I - "${run_root}/installed/bin" <<'PY'
import os
import stat
import sys

path = sys.argv[1]
mode = stat.S_IMODE(os.stat(path).st_mode)
if mode & 0o022:
    raise SystemExit(f"semantic smoke executable directory is not owner-safe: {mode:o}")
PY

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

candidate="${tmp}/completed-candidate"
mkdir "${candidate}"
candidate_leaves=(
  ctx
  ctx.build-info.json
  ctx.candidate.json
  ctx.cdx.json
  ctx.cdx.json.sha256
  ctx.dependency-advisory.json
  ctx.sha256
  ctx.size.json
  ctx.third-party-notices.txt
  ctx.third-party-notices.txt.sha256
  ctx.version
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-x64.tar.gz.sha256
  ctx-onnxruntime-linux-x64.tar.zst
  ctx-onnxruntime-linux-x64.tar.zst.asset.json
  ctx-onnxruntime-linux-x64.tar.zst.sha256
)
for leaf in "${candidate_leaves[@]}"; do
  case "${leaf}" in
    ctx)
      cp "${cpu_ctx}" "${candidate}/${leaf}"
      chmod 0755 "${candidate}/${leaf}"
      ;;
    ctx.build-info.json)
      cp "${cpu_ctx}.build-info.json" "${candidate}/${leaf}"
      ;;
    ctx.version)
      printf 'ctx 0.25.0\n' > "${candidate}/${leaf}"
      ;;
    ctx-onnxruntime-linux-x64.tar.gz)
      cp "${runtime_archive}" "${candidate}/${leaf}"
      ;;
    ctx-onnxruntime-linux-x64.tar.gz.sha256)
      cp "${runtime_archive}.sha256" "${candidate}/${leaf}"
      ;;
    *)
      printf 'completed candidate leaf %s\n' "${leaf}" > "${candidate}/${leaf}"
      ;;
  esac
done
candidate_commit=0123456789abcdef0123456789abcdef01234567
bundle_tool="${repo_root}/scripts/release/release_bundle.py"
seal_sha256="$(python3 -I "${bundle_tool}" seal \
  --candidate-dir "${candidate}" \
  --platform linux-x64 \
  --source-commit "${candidate_commit}")"
mkdir -p "${release_root}/tests/fixtures/custom-history-jsonl"
printf '{}\n' > \
  "${release_root}/tests/fixtures/custom-history-jsonl/basic.jsonl"
cat > "${release_root}/scripts/run-native-candidate-smoke.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
ctx_path="$1"
fixture="$2"
expected_version="$3"
output="$4"
[[ -x "${ctx_path}" && -s "${fixture}" && "${expected_version}" == "0.25.0" ]]
[[ "$("${ctx_path}" --version)" == "ctx ${expected_version}" ]]
printf '{"status":"passed"}\n' > "${output}"
EOF
chmod 0755 "${release_root}/scripts/run-native-candidate-smoke.sh"
nested_smoke_root="${tmp}/nested-native-smoke"
mkdir -p "${nested_smoke_root}"
if ! bash -ceu '
    ctx_path="$1"
    runtime="$2"
    version_file="$3"
    smoke_root="$4"
    execution_root="$5"
    cd "${execution_root}"
    expected_version="$(sed -n "s/^ctx //p" "${version_file}")"
    [[ -n "${expected_version}" ]]
    scripts/run-native-candidate-smoke.sh \
      "${ctx_path}" \
      tests/fixtures/custom-history-jsonl/basic.jsonl \
      "${expected_version}" \
      "${smoke_root}/candidate-smoke.json"
    scripts/smoke-daemon-semantic-release.sh \
      --runtime-archive "${runtime}" \
      --runtime-platform linux-x64 \
      --ctx "${ctx_path}" \
      --data-root "${smoke_root}" \
      --require-authoritative \
      --timeout-seconds 30
  ' bash \
    "${candidate}/ctx" \
    "${candidate}/ctx-onnxruntime-linux-x64.tar.gz" \
    "${candidate}/ctx.version" "${nested_smoke_root}" "${release_root}" \
  > "${tmp}/nested-onnx.out" 2> "${tmp}/nested-onnx.err"; then
  cat "${tmp}/nested-onnx.out" >&2
  cat "${tmp}/nested-onnx.err" >&2
  exit 1
fi
python3 -I "${bundle_tool}" verify \
  --candidate-dir "${candidate}" \
  --platform linux-x64 \
  --source-commit "${candidate_commit}" \
  --seal-sha256 "${seal_sha256}"
grep -Fq 'ctx semantic smoke ok:' "${tmp}/nested-onnx.out"
grep -Fq '"status":"passed"' \
  "${nested_smoke_root}/candidate-smoke.json"

printf 'daemon semantic release smoke contract tests passed\n'
