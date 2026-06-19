#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

mem_total_gib() {
  awk '/MemTotal:/ { printf "%d\n", ($2 + 1048575) / 1048576 }' /proc/meminfo 2>/dev/null || echo 16
}

cpu_count() {
  getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2
}

min_int() {
  local value="$1"
  shift
  local candidate
  for candidate in "$@"; do
    if (( candidate < value )); then
      value="$candidate"
    fi
  done
  echo "$value"
}

max_int() {
  local value="$1"
  shift
  local candidate
  for candidate in "$@"; do
    if (( candidate > value )); then
      value="$candidate"
    fi
  done
  echo "$value"
}

contains_cargo_jobs_arg() {
  local arg
  for arg in "$@"; do
    case "$arg" in
      -j|--jobs|--jobs=*) return 0 ;;
    esac
  done
  return 1
}

supports_cargo_jobs_arg() {
  local arg
  for arg in "$@"; do
    case "$arg" in
      +*) continue ;;
      test|build|check|clippy|run|bench|rustc) return 0 ;;
      *) return 1 ;;
    esac
  done
  return 1
}

with_cargo_jobs_arg() {
  local jobs="$1"
  shift
  if ! supports_cargo_jobs_arg "$@" || contains_cargo_jobs_arg "$@"; then
    printf '%s\0' "$@"
    return
  fi

  local inserted=0
  local arg
  for arg in "$@"; do
    if [[ "$arg" == "--" && "$inserted" -eq 0 ]]; then
      printf '%s\0' "-j" "$jobs"
      inserted=1
    fi
    printf '%s\0' "$arg"
  done
  if [[ "$inserted" -eq 0 ]]; then
    printf '%s\0' "-j" "$jobs"
  fi
}

sanitize_appimage_env() {
  unset CTX_BUILD_IDENTITY_PATH
  unset CTX_BUNDLE_DIR
  unset CTX_APPIMAGE_PATH
  unset APPIMAGE
  unset APPDIR

  local var
  for var in LD_LIBRARY_PATH GST_PLUGIN_PATH GST_PLUGIN_SYSTEM_PATH GI_TYPELIB_PATH GDK_PIXBUF_MODULE_FILE GIO_MODULE_DIR XDG_DATA_DIRS; do
    local value="${!var:-}"
    if [[ "$value" == *"/tmp/.mount_ctx"* ]] || [[ "$value" == *"/usr/lib/ctx"* ]]; then
      unset "$var"
    fi
  done
}

TOTAL_GIB="${CTX_CARGO_TOTAL_GIB:-$(mem_total_gib)}"
CPU_COUNT="${CTX_CARGO_CPU_COUNT:-$(cpu_count)}"
MAX_JOBS="${CTX_CARGO_MAX_JOBS:-4}"
GIB_PER_JOB="${CTX_CARGO_GIB_PER_JOB:-12}"
RESERVED_GIB="${CTX_CARGO_RESERVED_GIB:-12}"

if (( TOTAL_GIB <= RESERVED_GIB )); then
  DEFAULT_MEMORY_MAX_GIB="$(max_int 4 $(( TOTAL_GIB / 2 )))"
else
  DEFAULT_MEMORY_MAX_GIB="$(( TOTAL_GIB - RESERVED_GIB ))"
  DEFAULT_MEMORY_MAX_GIB="$(min_int "$DEFAULT_MEMORY_MAX_GIB" $(( TOTAL_GIB * 55 / 100 )))"
  DEFAULT_MEMORY_MAX_GIB="$(max_int "$DEFAULT_MEMORY_MAX_GIB" 8)"
fi

MEMORY_MAX_GIB="${CTX_CARGO_MEMORY_MAX_GIB:-$DEFAULT_MEMORY_MAX_GIB}"
MEMORY_SWAP_MAX_GIB="${CTX_CARGO_MEMORY_SWAP_MAX_GIB:-4}"

MEMORY_JOBS="$(( MEMORY_MAX_GIB / GIB_PER_JOB ))"
if (( MEMORY_JOBS < 1 )); then
  MEMORY_JOBS=1
fi

JOBS="${CTX_CARGO_JOBS:-$(min_int "$CPU_COUNT" "$MAX_JOBS" "$MEMORY_JOBS")}"
JOBS="$(max_int "$JOBS" 1)"
TEST_THREADS="${CTX_RUST_TEST_THREADS:-$JOBS}"
CARGO_BIN="${CTX_CARGO_BIN:-cargo}"
CARGO_LOCK_ENABLED="${CTX_CARGO_LOCK:-1}"
CARGO_LOCK_PATH="${CTX_CARGO_LOCK_PATH:-/tmp/ctx-cargo.lock}"
CARGO_LOW_IO_ENABLED="${CTX_CARGO_LOW_IO:-1}"
CARGO_NICE_LEVEL="${CTX_CARGO_NICE:-10}"
CARGO_IONICE_CLASS="${CTX_CARGO_IONICE_CLASS:-2}"
CARGO_IONICE_LEVEL="${CTX_CARGO_IONICE_LEVEL:-7}"

if [[ "$#" -eq 0 ]]; then
  set -- test --manifest-path Cargo.toml --workspace --locked
fi

sanitize_appimage_env

if [[ "${CTX_CARGO_USE_CGROUP:-1}" != "0" && "${CTX_CARGO_SAFE_IN_SCOPE:-0}" != "1" ]] && command -v systemd-run >/dev/null 2>&1; then
  if systemctl --user show-environment >/dev/null 2>&1; then
    echo "[ctx-cargo-safe] running in user cgroup: MemoryMax=${MEMORY_MAX_GIB}G MemorySwapMax=${MEMORY_SWAP_MAX_GIB}G jobs=${JOBS} test_threads=${TEST_THREADS}" >&2
    exec systemd-run --user --scope --quiet \
      -p "MemoryMax=${MEMORY_MAX_GIB}G" \
      -p "MemorySwapMax=${MEMORY_SWAP_MAX_GIB}G" \
      env \
        CTX_CARGO_SAFE_IN_SCOPE=1 \
        CTX_CARGO_TOTAL_GIB="$TOTAL_GIB" \
        CTX_CARGO_CPU_COUNT="$CPU_COUNT" \
        CTX_CARGO_MEMORY_MAX_GIB="$MEMORY_MAX_GIB" \
        CTX_CARGO_MEMORY_SWAP_MAX_GIB="$MEMORY_SWAP_MAX_GIB" \
        CTX_CARGO_JOBS="$JOBS" \
        CTX_RUST_TEST_THREADS="$TEST_THREADS" \
        "$0" "$@"
  fi
fi

cd "$CORE_DIR"

mapfile -d '' CARGO_ARGS < <(with_cargo_jobs_arg "$JOBS" "$@")

export CARGO_BUILD_JOBS="$JOBS"
export RUST_TEST_THREADS="$TEST_THREADS"

RUNNER=("$CARGO_BIN" "${CARGO_ARGS[@]}")
if [[ "$CARGO_LOW_IO_ENABLED" != "0" ]]; then
  if command -v nice >/dev/null 2>&1; then
    RUNNER=(nice -n "$CARGO_NICE_LEVEL" "${RUNNER[@]}")
  fi
  if command -v ionice >/dev/null 2>&1; then
    if ionice -c "$CARGO_IONICE_CLASS" -n "$CARGO_IONICE_LEVEL" true >/dev/null 2>&1; then
      RUNNER=(ionice -c "$CARGO_IONICE_CLASS" -n "$CARGO_IONICE_LEVEL" "${RUNNER[@]}")
    else
      echo "[ctx-cargo-safe] warning: ionice is present but unavailable; continuing without ionice" >&2
    fi
  fi
fi

echo "[ctx-cargo-safe] ${CARGO_BIN} ${CARGO_ARGS[*]} (test_threads=${TEST_THREADS}, memory_max=${MEMORY_MAX_GIB}G, lock=${CARGO_LOCK_ENABLED}, low_io=${CARGO_LOW_IO_ENABLED})" >&2

if [[ "$CARGO_LOCK_ENABLED" != "0" ]]; then
  if command -v flock >/dev/null 2>&1; then
    echo "[ctx-cargo-safe] waiting for host cargo lock: ${CARGO_LOCK_PATH}" >&2
    exec flock "$CARGO_LOCK_PATH" "${RUNNER[@]}"
  fi
  echo "[ctx-cargo-safe] warning: flock not found; running without host cargo lock" >&2
fi

exec "${RUNNER[@]}"
