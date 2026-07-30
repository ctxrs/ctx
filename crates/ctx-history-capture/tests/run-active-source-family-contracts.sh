#!/usr/bin/env bash
set -euo pipefail

if (( "$#" != 2 )); then
  printf 'usage: %s UNIT_TEST_BINARY EXPECTED_TESTS\n' "$0" >&2
  exit 2
fi

test_binary="$1"
expected_tests="$2"
[[ -x "${test_binary}" ]] || {
  printf 'ctx-history-capture unit test binary is not executable: %s\n' "${test_binary}" >&2
  exit 2
}
[[ -f "${expected_tests}" ]] || {
  printf 'active source-family test inventory is missing: %s\n' "${expected_tests}" >&2
  exit 2
}
: "${TEST_TMPDIR:?Bazel must provide TEST_TMPDIR}"

mkdir -p -- "${TEST_TMPDIR}/tmp"
export TMPDIR="${TEST_TMPDIR}/tmp"

actual="${TEST_TMPDIR}/active-source-family-contract-tests.actual"
expected="${TEST_TMPDIR}/active-source-family-contract-tests.expected"
LC_ALL=C "${test_binary}" --list |
  awk '/active_source_family_contract_.*: test$/ { sub(/: test$/, ""); print }' |
  sort >"${actual}"
LC_ALL=C sort "${expected_tests}" >"${expected}"
if ! diff -u "${expected}" "${actual}"; then
  printf 'active source-family test inventory changed; update the reviewed manifest deliberately\n' >&2
  exit 1
fi
[[ -s "${actual}" ]] || {
  printf 'active source-family test inventory is empty\n' >&2
  exit 1
}

"${test_binary}" \
  active_source_family_contract_ \
  --test-threads=1
