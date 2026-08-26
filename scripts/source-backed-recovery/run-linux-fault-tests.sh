#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
build_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-source-backed-recovery.XXXXXX")"
fault_filter="${1:-}"
trap 'rm -rf -- "${build_root}"' EXIT

shim="${build_root}/libctx_source_recovery_fault.so"
cc \
  -std=c11 \
  -O2 \
  -fPIC \
  -shared \
  -Wall \
  -Wextra \
  -Werror \
  "${script_dir}/fault_shim.c" \
  -ldl \
  -o "${shim}"

cd -- "${repo_root}"
cargo test -p ctx-history-index-qualification --test source_backed_recovery -- --test-threads=1
fault_args=()
if [[ -n "${fault_filter}" ]]; then
  fault_args=("${fault_filter}")
fi
if (( "${#fault_args[@]}" > 0 )); then
  CTX_SOURCE_RECOVERY_FAULT_SHIM="${shim}" \
    cargo test -p ctx-history-index-qualification --test source_backed_recovery "${fault_args[@]}" -- \
      --exact \
      --ignored \
      --nocapture \
      --test-threads=1
else
  CTX_SOURCE_RECOVERY_FAULT_SHIM="${shim}" \
    cargo test -p ctx-history-index-qualification --test source_backed_recovery -- \
      --ignored \
      --skip actual_bounded_filesystem_enospc_preserves_previous_generation \
      --nocapture \
      --test-threads=1
fi
