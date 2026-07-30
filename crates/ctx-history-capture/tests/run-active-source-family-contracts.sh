#!/usr/bin/env bash
set -euo pipefail

if (( "$#" != 1 )); then
  printf 'usage: %s UNIT_TEST_BINARY\n' "$0" >&2
  exit 2
fi

test_binary="$1"
[[ -x "${test_binary}" ]] || {
  printf 'ctx-history-capture unit test binary is not executable: %s\n' "${test_binary}" >&2
  exit 2
}
: "${TEST_TMPDIR:?Bazel must provide TEST_TMPDIR}"

mkdir -p -- "${TEST_TMPDIR}/tmp"
export TMPDIR="${TEST_TMPDIR}/tmp"

"${test_binary}" \
  active_source_family_contract_ \
  --test-threads=1
