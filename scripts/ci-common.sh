#!/usr/bin/env bash

ctx_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CTX_REPO_ROOT="${CTX_REPO_ROOT:-$(cd "${ctx_script_dir}/.." && pwd)}"

ctx_positive_int() {
  [[ "${1:-}" =~ ^[0-9]+$ ]] && (( "$1" > 0 ))
}

ctx_truthy() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

ctx_is_ci() {
  [[ -n "${CI:-}" || -n "${GITHUB_ACTIONS:-}" || -n "${BUILDKITE:-}" || -n "${BUILDKITE_BUILD_ID:-}" ]]
}

ctx_default_tmpdir() {
  if [[ -n "${TEST_TMPDIR:-}" ]]; then
    printf '%s\n' "${TEST_TMPDIR}/tmp"
  else
    printf '%s\n' "${CTX_REPO_ROOT}/target/tmp"
  fi
}

ctx_init_tool_env_defaults() {
  local root

  root="${CTX_TOOL_ENV_ROOT:-${CTX_REPO_ROOT}/target/tool-env}"
  export HOME="${HOME:-${root}/home}"
  export TMPDIR="${TMPDIR:-$(ctx_default_tmpdir)}"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/cargo-target}"
  if ctx_truthy "${CTX_ISOLATE_TOOL_ENV:-0}"; then
    export CARGO_HOME="${CARGO_HOME:-${root}/cargo-home}"
    export RUSTUP_HOME="${RUSTUP_HOME:-${root}/rustup-home}"
  fi
  mkdir -p "${HOME}" "${TMPDIR}" "${CARGO_TARGET_DIR}"
  if [[ -n "${CARGO_HOME:-}" ]]; then
    mkdir -p "${CARGO_HOME}"
  fi
  if [[ -n "${RUSTUP_HOME:-}" ]]; then
    mkdir -p "${RUSTUP_HOME}"
  fi
}

ctx_init_bazel_test_env() {
  local root isolated_home

  if [[ -n "${CTX_BAZEL_TEST_ENV_ROOT:-}" ]]; then
    root="${CTX_BAZEL_TEST_ENV_ROOT}"
  elif [[ -n "${TEST_TMPDIR:-}" ]]; then
    root="${TEST_TMPDIR}/ctx-env"
  else
    root="${CTX_REPO_ROOT}/target-local/bazel-test-env"
  fi

  isolated_home=0
  if [[ -z "${HOME:-}" || "${HOME}" == "${CTX_REPO_ROOT}/target/tool-env/home" || "${HOME}" == "${CTX_REPO_ROOT}/target/bazel-home" ]]; then
    export HOME="${root}/home"
    isolated_home=1
  fi
  if [[ -z "${TMPDIR:-}" || "${TMPDIR}" == "${CTX_REPO_ROOT}/target/tmp" || "${TMPDIR}" == "${CTX_REPO_ROOT}/target/tool-env/tmp" ]]; then
    export TMPDIR="${root}/tmp"
  fi
  if [[ -z "${CARGO_HOME:-}" ]] && { ctx_truthy "${CTX_ISOLATE_TOOL_ENV:-0}" || (( isolated_home == 1 )); }; then
    export CARGO_HOME="${root}/cargo-home"
  fi
  if [[ -z "${RUSTUP_HOME:-}" ]] && { ctx_truthy "${CTX_ISOLATE_TOOL_ENV:-0}" || (( isolated_home == 1 )); }; then
    export RUSTUP_HOME="${root}/rustup-home"
  fi
  if [[ -n "${TEST_TMPDIR:-}" ]] && ! ctx_truthy "${CTX_SHARED_CARGO_TARGET_DIR:-0}"; then
    export CARGO_TARGET_DIR="${root}/cargo-target"
  else
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/cargo-target}"
  fi
  mkdir -p "${HOME}" "${TMPDIR}" "${CARGO_TARGET_DIR}"
  if [[ -n "${CARGO_HOME:-}" ]]; then
    mkdir -p "${CARGO_HOME}"
  fi
  if [[ -n "${RUSTUP_HOME:-}" ]]; then
    mkdir -p "${RUSTUP_HOME}"
  fi
}

ctx_detect_cpu_count() {
  local cores

  if [[ -r /proc/cpuinfo ]]; then
    cores="$(awk '
      /^physical id[[:space:]]*:/ { physical = $NF }
      /^core id[[:space:]]*:/ {
        if (physical != "") {
          seen[physical ":" $NF] = 1
        }
      }
      END {
        for (core in seen) {
          count++
        }
        if (count > 0) {
          print count
        }
      }
    ' /proc/cpuinfo)"
    if ctx_positive_int "${cores}"; then
      printf '%s\n' "${cores}"
      return 0
    fi
  fi

  cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
  if ctx_positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  cores="$(sysctl -n hw.physicalcpu 2>/dev/null || true)"
  if ctx_positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  cores="$(sysctl -n hw.ncpu 2>/dev/null || true)"
  if ctx_positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  if ctx_positive_int "${NUMBER_OF_PROCESSORS:-}"; then
    printf '%s\n' "${NUMBER_OF_PROCESSORS}"
    return 0
  fi

  printf '2\n'
}

ctx_detect_memory_gb() {
  local kb bytes gb

  if [[ -r /proc/meminfo ]]; then
    kb="$(awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo)"
    if ctx_positive_int "${kb}"; then
      gb=$(( kb / 1048576 ))
      if (( gb < 1 )); then
        gb=1
      fi
      printf '%s\n' "${gb}"
      return 0
    fi
  fi

  bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
  if ctx_positive_int "${bytes}"; then
    gb=$(( bytes / 1073741824 ))
    if (( gb < 1 )); then
      gb=1
    fi
    printf '%s\n' "${gb}"
    return 0
  fi

  printf '4\n'
}

ctx_host_exe_suffix() {
  case "${OS:-$(uname -s 2>/dev/null || printf unknown)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*)
      printf '.exe'
      ;;
    *)
      printf ''
      ;;
  esac
}

ctx_detect_host_triple() {
  local rustc_info

  rustc_info="$(rustc -vV)"
  awk '/^host:/ { print $2; exit }' <<<"${rustc_info}"
}

ctx_require_host_triple() {
  local expected="$1"
  local actual

  if [[ -z "${expected}" ]]; then
    return 0
  fi

  ctx_ensure_rust_build_toolchain
  actual="$(ctx_detect_host_triple)"
  if [[ "${actual}" != "${expected}" ]]; then
    printf 'host triple mismatch: expected %s, got %s\n' "${expected}" "${actual}" >&2
    return 1
  fi
}

ctx_rust_tools_available() {
  command -v cargo >/dev/null 2>&1 || return 1
  command -v rustc >/dev/null 2>&1 || return 1
  cargo fmt --version >/dev/null 2>&1 || return 1
  cargo clippy --version >/dev/null 2>&1 || return 1
}

ctx_rust_build_tools_available() {
  command -v cargo >/dev/null 2>&1 || return 1
  command -v rustc >/dev/null 2>&1 || return 1
}

ctx_bootstrap_rust_toolchain() {
  if ctx_rust_tools_available; then
    return 0
  fi

  if ! command -v cargo >/dev/null 2>&1; then
    if ! command -v curl >/dev/null 2>&1; then
      printf 'cargo is missing and curl is unavailable to install rustup\n' >&2
      return 127
    fi
    local rustup_installer
    printf 'cargo not found; installing stable Rust toolchain with rustup\n' >&2
    rustup_installer="$(mktemp "${TMPDIR:-/tmp}/ctx-rustup-init.XXXXXX")"
    if ! curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "${rustup_installer}"; then
      rm -f "${rustup_installer}"
      return 1
    fi
    if ! sh "${rustup_installer}" -y --profile minimal --default-toolchain stable \
      --component rustfmt --component clippy; then
      rm -f "${rustup_installer}"
      return 1
    fi
    rm -f "${rustup_installer}"
    export PATH="${CARGO_HOME}/bin:${PATH}"
  fi

  if ctx_rust_tools_available; then
    return 0
  fi

  if command -v rustup >/dev/null 2>&1; then
    printf 'Rust toolchain found but rustfmt or clippy is missing; installing components\n' >&2
    rustup component add rustfmt clippy \
      || rustup toolchain install stable --profile minimal --component rustfmt --component clippy
  fi
}

ctx_ensure_rust_toolchain() {
  local cargo_home rustup_home lock_file

  cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
  rustup_home="${RUSTUP_HOME:-${HOME}/.rustup}"
  export CARGO_HOME="${cargo_home}"
  export RUSTUP_HOME="${rustup_home}"
  export PATH="${CARGO_HOME}/bin:${PATH}"

  if ctx_rust_tools_available; then
    return 0
  fi

  if ! ctx_truthy "${CTX_BOOTSTRAP_RUSTUP:-0}"; then
    printf 'Rust toolchain is incomplete; cargo, rustc, rustfmt, and clippy are required. Set CTX_BOOTSTRAP_RUSTUP=1 for explicit local rustup bootstrap.\n' >&2
    return 127
  fi

  lock_file="${CTX_RUSTUP_LOCK:-${TMPDIR:-${CTX_REPO_ROOT}/target/tmp}/ctx-rustup.lock}"
  mkdir -p "$(dirname "${lock_file}")"
  if command -v flock >/dev/null 2>&1; then
    (
      flock 9
      ctx_bootstrap_rust_toolchain
    ) 9>"${lock_file}"
  else
    ctx_bootstrap_rust_toolchain
  fi

  export PATH="${CARGO_HOME}/bin:${PATH}"
  if ! ctx_rust_tools_available; then
    printf 'Rust toolchain is incomplete after bootstrap; cargo, rustc, rustfmt, and clippy are required\n' >&2
    return 127
  fi
}

ctx_ensure_rust_build_toolchain() {
  local cargo_home rustup_home

  cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
  rustup_home="${RUSTUP_HOME:-${HOME}/.rustup}"
  export CARGO_HOME="${cargo_home}"
  export RUSTUP_HOME="${rustup_home}"
  export PATH="${CARGO_HOME}/bin:${PATH}"

  if ctx_rust_build_tools_available; then
    return 0
  fi

  if ! ctx_truthy "${CTX_BOOTSTRAP_RUSTUP:-0}"; then
    printf 'Rust build toolchain is missing; cargo and rustc are required. Set CTX_BOOTSTRAP_RUSTUP=1 for explicit local rustup bootstrap.\n' >&2
    return 127
  fi

  ctx_ensure_rust_toolchain
}

ctx_init_resource_env() {
  local cpu_count memory_gb memory_jobs default_jobs bazel_ram_mb

  cpu_count="${CTX_CPU_COUNT:-$(ctx_detect_cpu_count)}"
  memory_gb="${CTX_TOTAL_MEMORY_GB:-$(ctx_detect_memory_gb)}"

  if ! ctx_positive_int "${cpu_count}"; then
    cpu_count=2
  fi
  if ! ctx_positive_int "${memory_gb}"; then
    memory_gb=4
  fi

  memory_jobs=$(( memory_gb / 3 ))
  if (( memory_jobs < 1 )); then
    memory_jobs=1
  fi

  default_jobs="${cpu_count}"
  if (( memory_jobs < default_jobs )); then
    default_jobs="${memory_jobs}"
  fi
  if (( default_jobs < 1 )); then
    default_jobs=1
  fi
  if [[ "${CTX_LOCAL_RESOURCE_CAP:-1}" == "1" && -z "${CI:-}" && -z "${BUILDKITE:-}" && -z "${BUILDKITE_BUILD_ID:-}" ]] \
    && (( default_jobs > 2 )); then
    default_jobs=2
  fi

  export CTX_CPU_COUNT="${cpu_count}"
  export CTX_TOTAL_MEMORY_GB="${memory_gb}"
  export TMPDIR="${TMPDIR:-$(ctx_default_tmpdir)}"
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${CTX_CARGO_JOBS:-${default_jobs}}}"
  export RUST_TEST_THREADS="${RUST_TEST_THREADS:-${CTX_TEST_THREADS:-${CARGO_BUILD_JOBS}}}"
  export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
  if [[ "${CTX_USE_SCCACHE:-0}" != "1" && "${RUSTC_WRAPPER:-}" == *sccache* ]]; then
    unset RUSTC_WRAPPER
  fi
  mkdir -p "${TMPDIR}"

  export BAZEL_JOBS="${BAZEL_JOBS:-${CARGO_BUILD_JOBS}}"
  export BAZEL_LOCAL_CPU_RESOURCES="${BAZEL_LOCAL_CPU_RESOURCES:-${BAZEL_JOBS}}"
  bazel_ram_mb=$(( memory_gb * 512 ))
  if (( bazel_ram_mb < 1024 )); then
    bazel_ram_mb=1024
  fi
  export BAZEL_LOCAL_RAM_RESOURCES="${BAZEL_LOCAL_RAM_RESOURCES:-${bazel_ram_mb}}"
  export BAZELISK_HOME="${BAZELISK_HOME:-${CTX_REPO_ROOT}/target/tool-cache/bazelisk-home}"
  export BAZEL_OUTPUT_USER_ROOT="${BAZEL_OUTPUT_USER_ROOT:-${CTX_REPO_ROOT}/target/tool-cache/bazel-output}"
  mkdir -p "${BAZELISK_HOME}" "${BAZEL_OUTPUT_USER_ROOT}"
}

ctx_print_resource_env() {
  printf 'resource limits: cpu=%s memory_gb=%s cargo_jobs=%s test_threads=%s bazel_jobs=%s bazel_ram_mb=%s tmpdir=%s\n' \
    "${CTX_CPU_COUNT}" \
    "${CTX_TOTAL_MEMORY_GB}" \
    "${CARGO_BUILD_JOBS}" \
    "${RUST_TEST_THREADS}" \
    "${BAZEL_JOBS}" \
    "${BAZEL_LOCAL_RAM_RESOURCES}" \
    "${TMPDIR}"
}

ctx_json_escape() {
  local value="${1:-}"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "${value}"
}

ctx_timing_init() {
  CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/check}"
  CTX_TIMING_FILE="${CTX_TIMING_FILE:-${CTX_ARTIFACT_DIR}/timings.json}"
  CTX_TIMING_EVENTS="${CTX_TIMING_FILE}.events"
  mkdir -p "${CTX_ARTIFACT_DIR}" "$(dirname "${CTX_TIMING_FILE}")"
  : > "${CTX_TIMING_EVENTS}"
}

ctx_timing_finish() {
  if [[ -n "${CTX_TIMING_EVENTS:-}" && -f "${CTX_TIMING_EVENTS}" ]]; then
    {
      printf '[\n'
      awk 'NR == 1 { printf "  %s", $0; next } { printf ",\n  %s", $0 } END { if (NR > 0) printf "\n" }' "${CTX_TIMING_EVENTS}"
      printf ']\n'
    } > "${CTX_TIMING_FILE}"
    rm -f "${CTX_TIMING_EVENTS}"
    printf 'timing artifact: %s\n' "${CTX_TIMING_FILE}"
  fi
}

ctx_timing_record() {
  local name="$1"
  local status="$2"
  local started_at="$3"
  local ended_at="$4"
  local duration_s="$5"
  local exit_code="$6"
  local note="${7:-}"

  printf '{"name":"%s","status":"%s","started_at_unix_s":%s,"ended_at_unix_s":%s,"duration_s":%s,"exit_code":%s,"note":"%s"}\n' \
    "$(ctx_json_escape "${name}")" \
    "$(ctx_json_escape "${status}")" \
    "${started_at}" \
    "${ended_at}" \
    "${duration_s}" \
    "${exit_code}" \
    "$(ctx_json_escape "${note}")" >> "${CTX_TIMING_EVENTS}"
}

ctx_run_timed() {
  local name="$1"
  shift
  local started_at ended_at duration_s exit_code status command

  command="$*"
  started_at="$(date +%s)"
  printf '==> %s\n' "${name}"
  set +e
  "$@"
  exit_code=$?
  set -e
  ended_at="$(date +%s)"
  duration_s=$(( ended_at - started_at ))

  if (( exit_code == 0 )); then
    status="passed"
  else
    status="failed"
  fi

  ctx_timing_record "${name}" "${status}" "${started_at}" "${ended_at}" "${duration_s}" "${exit_code}" "${command}"
  return "${exit_code}"
}

ctx_record_skip() {
  local name="$1"
  local note="$2"
  local now

  now="$(date +%s)"
  printf '==> %s: skipped (%s)\n' "${name}" "${note}"
  ctx_timing_record "${name}" "skipped" "${now}" "${now}" "0" "0" "${note}"
}

ctx_bazelisk_asset() {
  local os arch

  os="$(uname -s 2>/dev/null || printf unknown)"
  arch="$(uname -m 2>/dev/null || printf unknown)"

  case "${os}:${arch}" in
    Linux:x86_64|Linux:amd64)
      printf 'bazelisk-linux-amd64'
      ;;
    Linux:aarch64|Linux:arm64)
      printf 'bazelisk-linux-arm64'
      ;;
    Darwin:x86_64|Darwin:amd64)
      printf 'bazelisk-darwin-amd64'
      ;;
    Darwin:arm64|Darwin:aarch64)
      printf 'bazelisk-darwin-arm64'
      ;;
    MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
      printf 'bazelisk-windows-amd64.exe'
      ;;
    *)
      printf 'unsupported Bazelisk host: %s %s\n' "${os}" "${arch}" >&2
      return 1
      ;;
  esac
}

ctx_bootstrap_bazelisk() {
  local bin_dir exe asset version url dest tmp

  if ! command -v curl >/dev/null 2>&1; then
    printf 'curl is required to bootstrap Bazelisk\n' >&2
    return 1
  fi

  asset="$(ctx_bazelisk_asset)" || return $?
  bin_dir="${CTX_BAZELISK_BIN_DIR:-${CTX_REPO_ROOT}/target/tool-cache/bazelisk/bin}"
  exe="bazelisk"
  if [[ "${asset}" == *.exe ]]; then
    exe="bazelisk.exe"
  fi
  dest="${bin_dir}/${exe}"

  if [[ -x "${dest}" ]]; then
    printf '%s\n' "${dest}"
    return 0
  fi

  mkdir -p "${bin_dir}"
  version="${CTX_BAZELISK_VERSION:-v1.29.0}"
  if [[ "${version}" == "latest" ]]; then
    url="https://github.com/bazelbuild/bazelisk/releases/latest/download/${asset}"
  else
    url="https://github.com/bazelbuild/bazelisk/releases/download/${version}/${asset}"
  fi
  tmp="${dest}.tmp"
  printf 'bazel/bazelisk not found; downloading Bazelisk from %s\n' "${url}" >&2
  rm -f "${tmp}"
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --connect-timeout 20 "${url}" -o "${tmp}"
  chmod 0755 "${tmp}" 2>/dev/null || true
  mv "${tmp}" "${dest}"
  printf '%s\n' "${dest}"
}

ctx_find_bazel() {
  if command -v bazel >/dev/null 2>&1; then
    command -v bazel
    return 0
  fi
  if command -v bazelisk >/dev/null 2>&1; then
    command -v bazelisk
    return 0
  fi
  if ctx_truthy "${CTX_BOOTSTRAP_BAZELISK:-0}"; then
    printf 'Bazelisk network bootstrap explicitly enabled with CTX_BOOTSTRAP_BAZELISK=1\n' >&2
    ctx_bootstrap_bazelisk
    return $?
  fi
  return 1
}
