#!/usr/bin/env bash
set -euo pipefail

if (( "$#" != 3 )); then
  printf 'usage: %s TEST_BINARY FAULT_SHIM TEST_NAME\n' "$0" >&2
  exit 2
fi

test_binary="$1"
fault_shim="$2"
test_name="$3"

[[ -x "${test_binary}" ]] || {
  printf 'source-backed recovery test binary is not executable: %s\n' "${test_binary}" >&2
  exit 2
}
[[ -f "${fault_shim}" ]] || {
  printf 'source-backed recovery fault shim is missing: %s\n' "${fault_shim}" >&2
  exit 2
}
: "${TEST_TMPDIR:?Bazel must provide TEST_TMPDIR}"

test_binary="$(cd -- "$(dirname -- "${test_binary}")" && pwd -P)/$(basename -- "${test_binary}")"
fault_shim="$(cd -- "$(dirname -- "${fault_shim}")" && pwd -P)/$(basename -- "${fault_shim}")"
mkdir -p -- "${TEST_TMPDIR}/tmp"
export TMPDIR="${TEST_TMPDIR}/tmp"

test_args=()
if [[ "${test_name}" != "*" ]]; then
  test_args=(--exact "${test_name}")
fi

CTX_SOURCE_RECOVERY_FAULT_SHIM="${fault_shim}" \
  "${test_binary}" \
    "${test_args[@]}" \
    --ignored \
    --nocapture \
    --test-threads=1
