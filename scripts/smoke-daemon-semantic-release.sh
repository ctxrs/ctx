#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/smoke-daemon-semantic-release.sh --runtime-archive PATH --runtime-platform PLATFORM [--ctx PATH] [--data-root DIR] [--proof-output PATH] [--timeout-seconds N] [--keep-root]
  scripts/smoke-daemon-semantic-release.sh --coreml --runtime-platform macos-arm64|macos-x64 [--ctx PATH] [--data-root DIR] [--proof-output PATH] [--timeout-seconds N] [--keep-root]

Native release smoke for opt-in daemon + semantic search. The smoke creates an
isolated ctx data root, imports a tiny custom-history fixture, enables daemon
and semantic search in that root only, runs the daemon, verifies strict semantic
search can find the fixture, and stops the daemon process it started. The
default mode installs the packaged ONNX Runtime 1.27.0 sidecar under an isolated
CTX_RUNTIME_DIR. --coreml instead exercises the production hash-pinned CoreML
bundle acquisition path using the exact supplied macOS ctx artifact; it cannot
be combined with --runtime-archive. When --data-root is provided, it is the
parent for a fresh unique run root; --keep-root preserves that child for
inspection. --proof-output copies the successful proof to a canonical release
artifact path before the isolated run root is cleaned up.
USAGE
}

ctx_bin="${CTX_BIN:-ctx}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
data_root_parent=""
runtime_archive=""
runtime_platform=""
proof_output=""
runtime_version="1.27.0"
timeout_seconds="${CTX_SEMANTIC_SMOKE_TIMEOUT_SECONDS:-900}"
keep_root=0
coreml_mode=0

while (($# > 0)); do
  case "$1" in
    --ctx)
      shift
      ctx_bin="${1:-}"
      ;;
    --data-root)
      shift
      data_root_parent="${1:-}"
      ;;
    --runtime-archive)
      shift
      runtime_archive="${1:-}"
      ;;
    --runtime-platform)
      shift
      runtime_platform="${1:-}"
      ;;
    --proof-output)
      shift
      proof_output="${1:-}"
      ;;
    --coreml)
      coreml_mode=1
      ;;
    --timeout-seconds)
      shift
      timeout_seconds="${1:-}"
      ;;
    --keep-root)
      keep_root=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
  shift || true
done

if [[ -z "${ctx_bin}" ]]; then
  echo "error: --ctx cannot be empty" >&2
  exit 2
fi
if [[ -z "${runtime_platform}" ]]; then
  echo "error: --runtime-platform is required" >&2
  exit 2
fi
if [[ "${proof_output}" =~ ^[[:space:]]*$ && -n "${proof_output}" ]]; then
  echo "error: --proof-output cannot be whitespace-only" >&2
  exit 2
fi
if [[ ! "${timeout_seconds}" =~ ^[0-9]+$ ]] || ((timeout_seconds < 30)); then
  echo "error: --timeout-seconds must be an integer >= 30" >&2
  exit 2
fi

case "${runtime_platform}" in
  linux-x64|linux-aarch64)
    runtime_dylib_name="libonnxruntime.so"
    ;;
  macos-arm64|macos-x64)
    runtime_dylib_name="libonnxruntime.dylib"
    ;;
  freebsd-x64)
    runtime_dylib_name="libonnxruntime.so"
    ;;
  *)
    echo "error: unsupported --runtime-platform: ${runtime_platform}" >&2
    exit 2
    ;;
esac

expected_runtime_asset=""
runtime_sha_path=""
actual_runtime_sha=""
if [[ "${coreml_mode}" == "1" ]]; then
  case "${runtime_platform}" in
    macos-arm64|macos-x64) ;;
    *)
      echo "error: --coreml requires --runtime-platform macos-arm64 or macos-x64" >&2
      exit 2
      ;;
  esac
  if [[ -n "${runtime_archive}" ]]; then
    echo "error: --coreml cannot be combined with --runtime-archive" >&2
    exit 2
  fi
else
  if [[ -z "${runtime_archive}" ]]; then
    echo "error: --runtime-archive is required unless --coreml is selected" >&2
    exit 2
  fi
  expected_runtime_asset="ctx-onnxruntime-${runtime_platform}.tar.gz"
  if [[ "$(basename "${runtime_archive}")" != "${expected_runtime_asset}" ]]; then
    echo "error: runtime archive for ${runtime_platform} must be named ${expected_runtime_asset}" >&2
    exit 2
  fi
  if [[ ! -f "${runtime_archive}" ]]; then
    echo "error: runtime archive not found: ${runtime_archive}" >&2
    exit 1
  fi
  runtime_archive="$(cd "$(dirname "${runtime_archive}")" && pwd -P)/$(basename "${runtime_archive}")"
  runtime_sha_path="${runtime_archive}.sha256"
  if [[ ! -s "${runtime_sha_path}" ]]; then
    echo "error: runtime archive checksum missing or empty: ${runtime_sha_path}" >&2
    exit 1
  fi
  expected_runtime_sha="$(tr -d '[:space:]' < "${runtime_sha_path}")"
  if [[ ! "${expected_runtime_sha}" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "error: runtime archive checksum is not a SHA-256 digest: ${runtime_sha_path}" >&2
    exit 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    actual_runtime_sha="$(sha256sum "${runtime_archive}" | awk '{ print $1 }')"
  elif command -v shasum >/dev/null 2>&1; then
    actual_runtime_sha="$(shasum -a 256 "${runtime_archive}" | awk '{ print $1 }')"
  else
    echo "error: sha256sum or shasum is required to verify the runtime archive" >&2
    exit 127
  fi
  actual_runtime_sha_lower="$(printf '%s' "${actual_runtime_sha}" | tr 'A-F' 'a-f')"
  expected_runtime_sha_lower="$(printf '%s' "${expected_runtime_sha}" | tr 'A-F' 'a-f')"
  if [[ "${actual_runtime_sha_lower}" != "${expected_runtime_sha_lower}" ]]; then
    echo "error: runtime archive checksum mismatch: expected ${expected_runtime_sha}, got ${actual_runtime_sha}" >&2
    exit 1
  fi
fi

unset LD_LIBRARY_PATH DYLD_LIBRARY_PATH LD_PRELOAD DYLD_INSERT_LIBRARIES \
  DYLD_FORCE_FLAT_NAMESPACE DYLD_FALLBACK_LIBRARY_PATH

run_bounded() {
  local limit_seconds="$1"
  shift
  python3 -I - "${limit_seconds}" "$@" <<'PY'
import subprocess
import sys

limit = int(sys.argv[1])
command = sys.argv[2:]
try:
    result = subprocess.run(command, timeout=limit, check=False)
except subprocess.TimeoutExpired:
    print(f"error: smoke command exceeded {limit} seconds", file=sys.stderr)
    raise SystemExit(124)
raise SystemExit(result.returncode)
PY
}

run_root=""
daemon_pid=""
cleanup() {
  local child_pid
  local daemon_is_running=0
  local attempt

  if [[ -n "${daemon_pid}" ]]; then
    while IFS= read -r child_pid; do
      if [[ "${child_pid}" == "${daemon_pid}" ]]; then
        daemon_is_running=1
        break
      fi
    done < <(jobs -pr)
  fi
  if [[ "${daemon_is_running}" == "1" ]]; then
    kill "${daemon_pid}" >/dev/null 2>&1 || true
    for attempt in {1..50}; do
      if ! kill -0 "${daemon_pid}" >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
    if kill -0 "${daemon_pid}" >/dev/null 2>&1; then
      kill -KILL "${daemon_pid}" >/dev/null 2>&1 || true
    fi
    wait "${daemon_pid}" >/dev/null 2>&1 || true
  fi
  if [[ "${keep_root}" != "1" && -n "${run_root}" ]]; then
    rm -rf -- "${run_root}"
  fi
}
trap cleanup EXIT

if [[ -n "${data_root_parent}" ]]; then
  mkdir -p -- "${data_root_parent}"
  data_root_parent="$(cd -- "${data_root_parent}" && pwd -P)"
  run_root="$(mktemp -d "${data_root_parent%/}/ctx-semantic-smoke.XXXXXX")"
else
  run_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-semantic-smoke.XXXXXX")"
fi
chmod 700 "${run_root}"
run_root="$(cd -- "${run_root}" && pwd -P)"
data_root="${run_root}/data"
mkdir -p -- "${data_root}"
data_root="$(cd -- "${data_root}" && pwd -P)"

fixture_dir="${data_root}/smoke-fixture"
fixture_path="${fixture_dir}/history.jsonl"
smoke_home="${data_root}/home"
smoke_cache="${data_root}/cache"
smoke_config="${data_root}/config-home"
semantic_cache="${data_root}/semantic-cache"
mkdir -p \
  "${fixture_dir}" \
  "${data_root}" \
  "${smoke_home}" \
  "${smoke_cache}" \
  "${smoke_config}" \
  "${semantic_cache}"

printf 'ctx semantic smoke: run_root=%s\n' "${run_root}"
printf 'ctx semantic smoke: data_root=%s\n' "${data_root}"

runtime_root="${data_root}/runtime"
runtime_install_dir="${runtime_root}/onnxruntime/${runtime_version}/${runtime_platform}"
release_artifact_dir="${run_root}/release-artifacts"
install_bin_dir="${run_root}/installed/bin"
release_metadata="${run_root}/release-metadata.env"
mkdir -p "${release_artifact_dir}" "${install_bin_dir}"
IFS=$'\t' read -r \
  host_system host_arch host_native_arch process_translated native_arch_probe \
  < <("${script_dir}/public-cli-host-runtime-evidence.sh")

if [[ "${ctx_bin}" == */* ]]; then
  ctx_source="$(cd "$(dirname "${ctx_bin}")" && pwd -P)/$(basename "${ctx_bin}")"
else
  ctx_source="$(command -v "${ctx_bin}")"
fi
[[ -x "${ctx_source}" ]] || {
  echo "error: ctx binary is not executable: ${ctx_source}" >&2
  exit 1
}
ctx_version="$(run_bounded 30 "${ctx_source}" --version | awk 'NR == 1 { print $2 }')"
[[ -n "${ctx_version}" ]] || {
  echo "error: could not determine ctx version from ${ctx_source}" >&2
  exit 1
}
release_binary="ctx-${runtime_platform}"
cp "${ctx_source}" "${release_artifact_dir}/${release_binary}"
chmod 755 "${release_artifact_dir}/${release_binary}"
if command -v sha256sum >/dev/null 2>&1; then
  binary_sha="$(sha256sum "${release_artifact_dir}/${release_binary}" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  binary_sha="$(shasum -a 256 "${release_artifact_dir}/${release_binary}" | awk '{ print $1 }')"
else
  echo "error: sha256sum or shasum is required to verify the ctx artifact" >&2
  exit 127
fi
runtime_authority="$(
  "${script_dir}/public-cli-runtime-authority.sh" \
    "${runtime_platform}" "${host_system}" "${host_arch}" passed \
    "${host_native_arch}" "${process_translated}"
)"
if [[ "${coreml_mode}" == "1" ]]; then
  ctx_bin="${install_bin_dir}/ctx"
  cp "${release_artifact_dir}/${release_binary}" "${ctx_bin}"
  chmod 755 "${ctx_bin}"
  if ! cmp -s "${ctx_source}" "${ctx_bin}"; then
    echo "error: isolated CoreML smoke artifact differs from the supplied ctx artifact" >&2
    exit 1
  fi
  runtime_dylib=""
else
  cp "${runtime_archive}" "${release_artifact_dir}/${expected_runtime_asset}"
  cp "${runtime_sha_path}" "${release_artifact_dir}/${expected_runtime_asset}.sha256"
  platform_key="${runtime_platform//-/_}"
  cat > "${release_metadata}" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=${ctx_version}
CTX_RELEASE_BASE_URL=https://release-smoke.invalid
CTX_RELEASE_ARTIFACT_${platform_key}=${release_binary}
CTX_RELEASE_SHA256_${platform_key}=${binary_sha}
CTX_RELEASE_ONNXRUNTIME_VERSION=${runtime_version}
CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${platform_key}=${expected_runtime_asset}
CTX_RELEASE_ONNXRUNTIME_SHA256_${platform_key}=${actual_runtime_sha}
EOF

  HOME="${smoke_home}" XDG_CACHE_HOME="${smoke_cache}" XDG_CONFIG_HOME="${smoke_config}" \
    bash "${script_dir}/dev-install-from-metadata.sh" \
      --metadata "${release_metadata}" \
      --artifact-dir "${release_artifact_dir}" \
      --platform "${runtime_platform}" \
      --bin-dir "${install_bin_dir}" \
      --runtime-dir "${runtime_root}" \
      --no-setup --no-skill --no-man --no-modify-path >/dev/null

  ctx_bin="${install_bin_dir}/ctx"
  runtime_dylib="${runtime_install_dir}/lib/${runtime_dylib_name}"
  [[ -x "${ctx_bin}" && -f "${runtime_dylib}" ]] || {
    echo "error: explicit-metadata installer did not create the expected binary/runtime layout" >&2
    exit 1
  }
  grep -F '"manager": "ctx-explicit-metadata-installer"' "${ctx_bin}.install.json" >/dev/null
  grep -F '"metadata_trust": "explicit-unsigned"' "${runtime_install_dir}/ctx-runtime-install.json" >/dev/null
fi
runtime_proof="${data_root}/packaged-runtime-proof.txt"
marker="ctx-release-semantic-smoke-$(python3 -I -c 'import uuid; print(uuid.uuid4().hex)')"
query="synthetic release retrieval cobalt willow transit"
embedding_model="intfloat/multilingual-e5-small"

python3 -I - "${fixture_path}" "${marker}" <<'PY'
import json
import sys

fixture_path, marker = sys.argv[1:]
records = [
    {
        "record_type": "manifest",
        "schema_version": "ctx-history-jsonl-v1",
        "metadata": {"exporter": "ctx-release-smoke"},
    },
    {
        "record_type": "source",
        "source_id": "release-smoke",
        "provider_key": "ctx-smoke",
        "source_format": "release-smoke-jsonl",
        "raw_source_path": fixture_path,
    },
    {
        "record_type": "session",
        "source_id": "release-smoke",
        "session_id": "semantic-daemon-smoke",
        "cwd": "/tmp/ctx-release-smoke",
        "started_at": "2026-07-10T00:00:00Z",
        "agent_type": "primary",
        "role_hint": "developer",
        "is_primary": True,
        "status": "completed",
    },
]
for index, role, text in (
    (0, "user", f"Please remember the {marker} validation task for daemon semantic search."),
    (1, "assistant", f"Recorded {marker} as the release smoke semantic retrieval target."),
):
    records.append(
        {
            "record_type": "event",
            "source_id": "release-smoke",
            "session_id": "semantic-daemon-smoke",
            "event_index": index,
            "event_type": "message",
            "role": role,
            "occurred_at": f"2026-07-10T00:00:0{index + 1}Z",
            "payload": {"text": text},
            "preview": text,
            "native_cursor": f"line:{index + 1}",
        }
    )
with open(fixture_path, "w", encoding="utf-8", newline="\n") as output:
    for record in records:
        output.write(json.dumps(record, separators=(",", ":")) + "\n")
PY

isolated_env=(
  -u CTX_DATA_ROOT
  -u CTX_DAEMON_OFF
  -u CTX_DISABLE_DAEMON
  -u CTX_DAEMON_AUTOSTART_OFF
  -u CTX_DAEMON_AUTOSTART_EXE
  -u CTX_DAEMON_BACKGROUND_CHILD
  -u CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS
  -u CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS
  -u CTX_DISABLE_SEMANTIC_SEARCH
  -u CTX_SEMANTIC_WORKER_OFF
  -u CTX_SEMANTIC_WORKER_MAX_CHUNKS
  -u CTX_SEMANTIC_WORKER_MAX_SECONDS
  -u CTX_SEMANTIC_THREADS
  -u CTX_SEMANTIC_EMBED_BATCH
  -u CTX_INTERNAL_SEMANTIC_BACKEND
  -u CTX_SEMANTIC_COREML_NATIVE_COMPUTE
  -u CTX_ANALYTICS_ENABLED
  -u CTX_ANALYTICS_ENDPOINT
  -u CTX_ANALYTICS_DRY_RUN
  -u CTX_ANALYTICS_DEBUG
  -u CTX_UPGRADE_AUTO
  -u CTX_UPGRADE_CHANNEL
  -u CTX_CHANNEL
  -u CTX_FUNCTIONS_BASE
  -u CTX_UPGRADE_INTERVAL_SECONDS
  -u CTX_UPGRADE_TARGET
  -u CTX_UPGRADE_BACKGROUND_CHILD
  -u CTX_SEMANTIC_CACHE_DIR
  -u FASTEMBED_CACHE_DIR
  -u HF_HOME
  -u HF_HUB_CACHE
  -u HUGGINGFACE_HUB_CACHE
  -u TRANSFORMERS_CACHE
  -u CTX_RUNTIME_DIR
  -u CTX_ONNXRUNTIME_DYLIB
  -u ORT_DYLIB_PATH
  -u CTX_ONNXRUNTIME_DIR
  -u CTX_ONNXRUNTIME_CACHE_DIR
  -u LD_LIBRARY_PATH
  -u DYLD_LIBRARY_PATH
  -u LD_PRELOAD
  -u DYLD_INSERT_LIBRARIES
  -u DYLD_FORCE_FLAT_NAMESPACE
  -u DYLD_FALLBACK_LIBRARY_PATH
)

ctx_env=(
  env "${isolated_env[@]}"
  "HOME=${smoke_home}"
  "XDG_CACHE_HOME=${smoke_cache}"
  "XDG_CONFIG_HOME=${smoke_config}"
  "HF_HOME=${semantic_cache}"
  "HF_HUB_CACHE=${semantic_cache}"
  "FASTEMBED_CACHE_DIR=${semantic_cache}"
  "CTX_SEMANTIC_CACHE_DIR=${semantic_cache}"
  CTX_ANALYTICS_OFF=1
  CTX_DISABLE_ANALYTICS=1
  CTX_UPGRADE_OFF=1
  CTX_DISABLE_AUTO_UPGRADE=1
  CTX_UPGRADE_AUTO=off
  CTX_DAEMON_ENABLED=1
  CTX_SEARCH_SEMANTIC=1
  "CTX_RUNTIME_DIR=${runtime_root}"
)
if [[ "${coreml_mode}" == "1" ]]; then
  ctx_env+=(
    CTX_INTERNAL_SEMANTIC_BACKEND=coreml
    CTX_SEMANTIC_COREML_NATIVE_COMPUTE=all
  )
fi

run_ctx() {
  run_bounded 30 "${ctx_env[@]}" "${ctx_bin}" --data-root "${data_root}" "$@"
}

printf 'ctx semantic smoke: isolated_home=%s\n' "${smoke_home}"
printf 'ctx semantic smoke: semantic_cache=%s\n' "${semantic_cache}"
if [[ "${coreml_mode}" == "1" ]]; then
  printf 'ctx semantic smoke: coreml_artifact=%s\n' "${ctx_bin}"
else
  printf 'ctx semantic smoke: packaged_runtime=%s\n' "${runtime_dylib}"
fi
run_ctx import --no-daemon --format ctx-history-jsonl-v1 --path "${fixture_path}" >/dev/null

cat > "${data_root}/config.toml" <<'EOF'
[analytics]
enabled = false

[upgrade]
auto = "off"

[daemon]
enabled = true

[search]
semantic = true
EOF

daemon_log="${data_root}/daemon-smoke.log"
"${ctx_env[@]}" "${ctx_bin}" --data-root "${data_root}" \
  daemon run --idle-exit-seconds "${timeout_seconds}" --loop-interval-seconds 2 --json \
  > "${daemon_log}" 2>&1 &
daemon_pid="$!"

deadline=$((SECONDS + timeout_seconds))
last_output=""
last_search_error=""
last_status_output=""
last_status_error=""
daemon_status_json="${data_root}/daemon-status.json"
daemon_status_error="${data_root}/daemon-status.err"
search_json="${data_root}/semantic-search.json"
search_error="${data_root}/semantic-search.err"

daemon_status_matches() {
  python3 -I - "${daemon_status_json}" "${daemon_pid}" "${coreml_mode}" "${embedding_model}" <<'PY'
import json
import sys

path, expected_pid_text, coreml_mode, expected_model = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as source:
        payload = json.load(source)
except (OSError, UnicodeError, json.JSONDecodeError):
    raise SystemExit(1)

daemon = payload.get("daemon") if isinstance(payload, dict) else None
if not isinstance(daemon, dict):
    raise SystemExit(1)
pid = daemon.get("pid")
expected_pid = int(expected_pid_text)
if pid is not None and (isinstance(pid, bool) or not isinstance(pid, int) or pid != expected_pid):
    print(f"daemon status PID mismatch: expected {expected_pid}, got {pid!r}", file=sys.stderr)
    raise SystemExit(2)
if daemon.get("status") != "running" or daemon.get("running") is not True or pid != expected_pid:
    raise SystemExit(1)
if coreml_mode != "1":
    raise SystemExit(0)

jobs = daemon.get("jobs")
semantic = jobs.get("semantic_index") if isinstance(jobs, dict) else None
runtime = semantic.get("embedding_runtime") if isinstance(semantic, dict) else None
if not isinstance(runtime, dict):
    raise SystemExit(1)
if runtime.get("backend") != "coreml":
    print(f"CoreML daemon status reported backend {runtime.get('backend')!r}", file=sys.stderr)
    raise SystemExit(2)
if runtime.get("model_id") != expected_model:
    print(f"CoreML daemon status reported model {runtime.get('model_id')!r}", file=sys.stderr)
    raise SystemExit(2)
if runtime.get("compute_mode") != "all":
    print(f"CoreML daemon status reported compute mode {runtime.get('compute_mode')!r}", file=sys.stderr)
    raise SystemExit(2)
if runtime.get("acquisition_source") != "download":
    print(f"CoreML daemon status reported acquisition source {runtime.get('acquisition_source')!r}", file=sys.stderr)
    raise SystemExit(2)
if runtime.get("acquisition_fallback") is not None:
    print("CoreML daemon status reported an acquisition fallback", file=sys.stderr)
    raise SystemExit(2)
PY
}

search_json_matches() {
  python3 -I - "${search_json}" "${marker}" "${embedding_model}" "${coreml_mode}" <<'PY'
import json
import sys

path, marker, expected_model, coreml_mode = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as source:
        payload = json.load(source)
except (OSError, UnicodeError, json.JSONDecodeError):
    raise SystemExit(1)

retrieval = payload.get("retrieval") if isinstance(payload, dict) else None
results = payload.get("results") if isinstance(payload, dict) else None
if not isinstance(retrieval, dict) or retrieval.get("embedding_model") != expected_model:
    raise SystemExit(1)
if retrieval.get("requested_mode") != "semantic" or retrieval.get("effective_mode") != "semantic":
    raise SystemExit(1)
if retrieval.get("semantic_fallback") is not None or retrieval.get("semantic_fallback_code") is not None:
    raise SystemExit(1)
if coreml_mode == "1":
    worker = retrieval.get("worker")
    runtime = worker.get("embedding_runtime") if isinstance(worker, dict) else None
    if not isinstance(runtime, dict):
        print("CoreML search status did not report an embedding runtime", file=sys.stderr)
        raise SystemExit(2)
    if runtime.get("backend") != "coreml":
        print(f"CoreML search status reported backend {runtime.get('backend')!r}", file=sys.stderr)
        raise SystemExit(2)
    if runtime.get("model_id") != expected_model:
        print(f"CoreML search status reported model {runtime.get('model_id')!r}", file=sys.stderr)
        raise SystemExit(2)
    if runtime.get("compute_mode") != "all":
        print(f"CoreML search status reported compute mode {runtime.get('compute_mode')!r}", file=sys.stderr)
        raise SystemExit(2)
    if runtime.get("acquisition_source") != "download":
        print(f"CoreML search status reported acquisition source {runtime.get('acquisition_source')!r}", file=sys.stderr)
        raise SystemExit(2)
    if runtime.get("acquisition_fallback") is not None:
        print("CoreML search status reported an acquisition fallback", file=sys.stderr)
        raise SystemExit(2)
if not isinstance(results, list):
    raise SystemExit(1)

def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for child in value.values():
            yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)

if not any(marker in text for result in results for text in strings(result)):
    raise SystemExit(1)
PY
}

while ((SECONDS < deadline)); do
  if ! kill -0 "${daemon_pid}" >/dev/null 2>&1; then
    echo "ctx semantic smoke: daemon exited before search succeeded" >&2
    cat "${daemon_log}" >&2 || true
    exit 1
  fi

  daemon_status_ready=0
  if run_ctx daemon status --json > "${daemon_status_json}" 2> "${daemon_status_error}"; then
    last_status_output="$(cat "${daemon_status_json}")"
    last_status_error="$(cat "${daemon_status_error}")"
    if daemon_status_matches; then
      daemon_status_ready=1
    else
      status_check=$?
      if [[ "${status_check}" == "2" ]]; then
        cat "${daemon_status_json}" >&2 || true
        exit 1
      fi
    fi
  fi

  if [[ "${daemon_status_ready}" == "1" ]] && \
    run_ctx search "${query}" --backend semantic --refresh off --json \
      > "${search_json}" 2> "${search_error}"; then
    last_output="$(cat "${search_json}")"
    last_search_error="$(cat "${search_error}")"
    if search_json_matches; then
      if run_ctx daemon status --json > "${daemon_status_json}" 2> "${daemon_status_error}" && \
        daemon_status_matches; then
        if [[ "${coreml_mode}" == "1" ]]; then
          runtime_fields="$(python3 -I - "${daemon_status_json}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    payload = json.load(source)
runtime = payload["daemon"]["jobs"]["semantic_index"]["embedding_runtime"]
print(
    "\t".join(
        (
            runtime["compute_mode"],
            runtime["model_id"],
            runtime.get("acquisition_source", "unknown"),
        )
    )
)
PY
)"
          IFS=$'\t' read -r coreml_compute_mode coreml_model coreml_acquisition_source \
            <<< "${runtime_fields}"
          cat > "${runtime_proof}" <<EOF
runtime=coreml
platform=${runtime_platform}
host_system=${host_system}
host_arch=${host_arch}
host_native_arch=${host_native_arch}
process_translated=${process_translated}
native_arch_probe=${native_arch_probe}
runtime_authority=${runtime_authority}
artifact=${ctx_source}
artifact_sha256=${binary_sha}
isolated_artifact=${ctx_bin}
compute_mode=${coreml_compute_mode}
model=${coreml_model}
acquisition_source=${coreml_acquisition_source}
acquisition_fallback=none
CTX_SEMANTIC_CACHE_DIR=${semantic_cache}
daemon_status=running
daemon_pid=${daemon_pid}
marker=${marker}
semantic_search=passed
EOF
        else
          cat > "${runtime_proof}" <<EOF
runtime=onnxruntime
version=${runtime_version}
platform=${runtime_platform}
host_system=${host_system}
host_arch=${host_arch}
host_native_arch=${host_native_arch}
process_translated=${process_translated}
native_arch_probe=${native_arch_probe}
runtime_authority=${runtime_authority}
artifact=${ctx_source}
artifact_sha256=${binary_sha}
archive=${runtime_archive}
runtime_archive_sha256=${actual_runtime_sha}
CTX_RUNTIME_DIR=${runtime_root}
runtime_dylib=${runtime_dylib}
loader_overrides=unset
CTX_SEMANTIC_CACHE_DIR=${semantic_cache}
daemon_status=running
daemon_pid=${daemon_pid}
embedding_model=${embedding_model}
marker=${marker}
semantic_search=passed
EOF
        fi
        if [[ -n "${proof_output}" ]]; then
          mkdir -p -- "$(dirname -- "${proof_output}")"
          install -m 0644 "${runtime_proof}" "${proof_output}"
        fi
        printf 'ctx semantic smoke ok: strict semantic search found %s with %s\n' \
          "${marker}" "${embedding_model}"
        exit 0
      else
        status_check=$?
        if [[ "${status_check}" == "2" ]]; then
          cat "${daemon_status_json}" >&2 || true
          exit 1
        fi
      fi
    else
      search_check=$?
      if [[ "${search_check}" == "2" ]]; then
        cat "${search_json}" >&2 || true
        exit 1
      fi
    fi
  else
    last_output="$(cat "${search_json}" 2>/dev/null || true)"
    last_search_error="$(cat "${search_error}" 2>/dev/null || true)"
  fi

  sleep 5
done

echo "ctx semantic smoke failed: semantic search did not find fixture before timeout" >&2
printf '\nLast search output:\n%s\n' "${last_output}" >&2
printf '\nLast search stderr:\n%s\n' "${last_search_error}" >&2
printf '\nLast daemon status:\n%s\n' "${last_status_output}" >&2
printf '\nLast daemon status stderr:\n%s\n' "${last_status_error}" >&2
printf '\nDaemon log:\n' >&2
cat "${daemon_log}" >&2 || true
exit 1
