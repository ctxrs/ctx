#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

positive_int() {
  [[ "${1:-}" =~ ^[1-9][0-9]*$ ]]
}

detect_cpu_count() {
  if positive_int "${CTX_CPU_COUNT:-}"; then
    printf '%s\n' "${CTX_CPU_COUNT}"
  elif command -v getconf >/dev/null 2>&1; then
    getconf _NPROCESSORS_ONLN 2>/dev/null || printf '2\n'
  elif command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.ncpu 2>/dev/null || printf '2\n'
  else
    printf '2\n'
  fi
}

detect_memory_gb() {
  local memory_kb memory_bytes
  if positive_int "${CTX_TOTAL_MEMORY_GB:-}"; then
    printf '%s\n' "${CTX_TOTAL_MEMORY_GB}"
  elif [[ -r /proc/meminfo ]]; then
    memory_kb="$(awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo)"
    if positive_int "${memory_kb}"; then
      printf '%s\n' "$(( memory_kb / 1024 / 1024 ))"
    fi
  elif command -v sysctl >/dev/null 2>&1; then
    memory_bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
    if positive_int "${memory_bytes}"; then
      printf '%s\n' "$(( memory_bytes / 1024 / 1024 / 1024 ))"
    fi
  fi
}

default_jobs() {
  local cpu_count jobs memory_gb memory_jobs
  cpu_count="$(detect_cpu_count)"
  if ! positive_int "${cpu_count}"; then
    cpu_count=2
  fi
  jobs=$(( cpu_count / 4 ))
  if (( jobs < 1 )); then
    jobs=1
  elif (( jobs > 8 )); then
    jobs=8
  fi
  memory_gb="$(detect_memory_gb)"
  if positive_int "${memory_gb}"; then
    memory_jobs=$(( memory_gb / 3 ))
    if (( memory_jobs < 1 )); then
      memory_jobs=1
    fi
    if (( memory_jobs < jobs )); then
      jobs="${memory_jobs}"
    fi
  fi
  printf '%s\n' "${jobs}"
}

if (( "$#" == 0 )); then
  printf 'usage: scripts/cargo-diagnostic.sh <cargo arguments...>\n' >&2
  exit 64
fi

cargo_jobs="${CARGO_BUILD_JOBS:-${CTX_CARGO_JOBS:-$(default_jobs)}}"
if ! positive_int "${cargo_jobs}"; then
  printf 'CARGO_BUILD_JOBS/CTX_CARGO_JOBS must be a positive integer\n' >&2
  exit 64
fi

test_threads="${RUST_TEST_THREADS:-${CTX_TEST_THREADS:-${cargo_jobs}}}"
if ! positive_int "${test_threads}"; then
  printf 'RUST_TEST_THREADS/CTX_TEST_THREADS must be a positive integer\n' >&2
  exit 64
fi
if [[ -z "${RUST_TEST_THREADS:-}" && -z "${CTX_TEST_THREADS:-}" ]] && (( test_threads > 4 )); then
  test_threads=4
fi

export CARGO_BUILD_JOBS="${cargo_jobs}"
export RUST_TEST_THREADS="${test_threads}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${repo_root}/target/cargo-diagnostic}"

if [[ "${CTX_CARGO_DIAGNOSTIC_DEBUG:-0}" != "1" ]]; then
  export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
  export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
fi

cargo_bin="${CARGO:-}"
if [[ -z "${cargo_bin}" ]]; then
  cargo_bin="$(command -v cargo 2>/dev/null || true)"
fi
if [[ -z "${cargo_bin}" ]]; then
  printf 'cargo is required (or set CARGO)\n' >&2
  exit 127
fi

printf 'cargo diagnostic: jobs=%s test_threads=%s target_dir=%s debug=%s\n' \
  "${CARGO_BUILD_JOBS}" \
  "${RUST_TEST_THREADS}" \
  "${CARGO_TARGET_DIR}" \
  "${CTX_CARGO_DIAGNOSTIC_DEBUG:-0}" >&2

cd "${repo_root}"
exec "${cargo_bin}" "$@"
