#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
build_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-source-backed-recovery.XXXXXX")"
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
cargo test -p ctx-history-index --test source_backed_recovery -- --test-threads=1
CTX_SOURCE_RECOVERY_FAULT_SHIM="${shim}" \
  cargo test -p ctx-history-index --test source_backed_recovery -- \
    --ignored \
    --nocapture \
    --test-threads=1
