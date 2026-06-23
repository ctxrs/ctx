#!/usr/bin/env bash
set -euo pipefail

find_repo_root() {
  local script_dir candidate

  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  for candidate in \
    "${BUILD_WORKSPACE_DIRECTORY:-}" \
    "$(pwd)" \
    "${RUNFILES_DIR:-}/_main" \
    "${RUNFILES_DIR:-}/ctx_work_record" \
    "${script_dir}/.." \
    "${script_dir}/../_main" \
    "${script_dir}/../ctx_work_record"; do
    if [[ -n "${candidate}" && -f "${candidate}/Cargo.toml" ]]; then
      cd "${candidate}"
      return 0
    fi
  done

  printf 'could not locate repo root containing Cargo.toml for Bazel test\n' >&2
  return 1
}

find_repo_root

cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
rustup_home="${RUSTUP_HOME:-${HOME}/.rustup}"
export CARGO_HOME="${cargo_home}"
export RUSTUP_HOME="${rustup_home}"
export PATH="${CARGO_HOME}/bin:${PATH}"
if ! command -v cargo >/dev/null 2>&1; then
  printf 'cargo is required for //:cargo_tests but was not found on PATH=%s\n' "${PATH}" >&2
  exit 127
fi

positive_int() {
  [[ "${1:-}" =~ ^[0-9]+$ ]] && (( "$1" > 0 ))
}

detect_cpu_count() {
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
    if positive_int "${cores}"; then
      printf '%s\n' "${cores}"
      return 0
    fi
  fi

  cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
  if positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  cores="$(sysctl -n hw.physicalcpu 2>/dev/null || true)"
  if positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  if positive_int "${NUMBER_OF_PROCESSORS:-}"; then
    printf '%s\n' "${NUMBER_OF_PROCESSORS}"
    return 0
  fi

  printf '2\n'
}

detect_memory_gb() {
  local kb bytes gb

  if [[ -r /proc/meminfo ]]; then
    kb="$(awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo)"
    if positive_int "${kb}"; then
      gb=$(( kb / 1048576 ))
      if (( gb < 1 )); then
        gb=1
      fi
      printf '%s\n' "${gb}"
      return 0
    fi
  fi

  bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
  if positive_int "${bytes}"; then
    gb=$(( bytes / 1073741824 ))
    if (( gb < 1 )); then
      gb=1
    fi
    printf '%s\n' "${gb}"
    return 0
  fi

  printf '4\n'
}

json_escape() {
  local value="${1:-}"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "${value}"
}

cpu_count="${CTX_CPU_COUNT:-$(detect_cpu_count)}"
memory_gb="${CTX_TOTAL_MEMORY_GB:-$(detect_memory_gb)}"
if ! positive_int "${cpu_count}"; then
  cpu_count=2
fi
if ! positive_int "${memory_gb}"; then
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
if [[ "${CTX_LOCAL_RESOURCE_CAP:-1}" == "1" && -z "${CI:-}" && -z "${BUILDKITE:-}" && -z "${BUILDKITE_BUILD_ID:-}" ]] \
  && (( default_jobs > 2 )); then
  default_jobs=2
fi

export CTX_CPU_COUNT="${cpu_count}"
export CTX_TOTAL_MEMORY_GB="${memory_gb}"
if [[ -n "${TEST_TMPDIR:-}" ]]; then
  export TMPDIR="${TMPDIR:-${TEST_TMPDIR}/tmp}"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TEST_TMPDIR}/cargo-target}"
else
  export TMPDIR="${TMPDIR:-$(pwd)/target/tmp}"
fi
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${CTX_CARGO_JOBS:-${default_jobs}}}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-${CTX_TEST_THREADS:-${CARGO_BUILD_JOBS}}}"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
if [[ "${CTX_USE_SCCACHE:-0}" != "1" && "${RUSTC_WRAPPER:-}" == *sccache* ]]; then
  unset RUSTC_WRAPPER
fi
mkdir -p "${TMPDIR}" "${CARGO_TARGET_DIR:-target}"

if [[ -n "${CTX_ARTIFACT_DIR:-}" ]]; then
  artifact_dir="${CTX_ARTIFACT_DIR}"
elif [[ -n "${TEST_TMPDIR:-}" ]]; then
  artifact_dir="${TEST_TMPDIR}/ctx-artifacts"
else
  artifact_dir="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/bazel-test}"
fi
mkdir -p "${artifact_dir}"
timing_file="${artifact_dir}/timings.json"

cargo_locked_args=()
if [[ "${CTX_CARGO_LOCKED:-1}" != "0" && -f Cargo.lock ]]; then
  cargo_locked_args+=(--locked)
fi

printf 'resource limits: cpu=%s memory_gb=%s cargo_jobs=%s test_threads=%s tmpdir=%s\n' \
  "${CTX_CPU_COUNT}" \
  "${CTX_TOTAL_MEMORY_GB}" \
  "${CARGO_BUILD_JOBS}" \
  "${RUST_TEST_THREADS}" \
  "${TMPDIR}"

started_at="$(date +%s)"
set +e
cargo test --workspace --all-targets "${cargo_locked_args[@]}" -- --test-threads "${RUST_TEST_THREADS}"
exit_code=$?
set -e
ended_at="$(date +%s)"
duration_s=$(( ended_at - started_at ))
if (( exit_code == 0 )); then
  status="passed"
else
  status="failed"
fi

cat > "${timing_file}" <<EOF
[
  {"name":"bazel-cargo-test","status":"$(json_escape "${status}")","started_at_unix_s":${started_at},"ended_at_unix_s":${ended_at},"duration_s":${duration_s},"exit_code":${exit_code},"note":"cargo test --workspace --all-targets"}
]
EOF
printf 'timing artifact: %s\n' "${timing_file}"

exit "${exit_code}"
