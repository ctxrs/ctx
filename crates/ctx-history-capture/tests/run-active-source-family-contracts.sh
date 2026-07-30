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

run_runner_regressions() {
  local runner="$1"
  local fixture_root="${TEST_TMPDIR}/runner-regression"
  local fake_binary="${fixture_root}/fake-unit-tests"
  local fake_manifest="${fixture_root}/active-source-family-contract-tests.txt"

  mkdir -p -- "${fixture_root}/ignored" "${fixture_root}/count-mismatch"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'set -euo pipefail' \
    'test_name="fixture::active_source_family_contract_selected"' \
    'if [[ "${1:-}" == "--list" ]]; then' \
    '  printf "%s: test\n" "${test_name}"' \
    '  exit 0' \
    'fi' \
    'case "${CTX_ACTIVE_SOURCE_FAMILY_FAKE_RESULT:?}" in' \
    '  ignored)' \
    '    printf "running 1 test\n"' \
    '    printf "test %s ... ignored\n\n" "${test_name}"' \
    '    printf "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"' \
    '    ;;' \
    '  count-mismatch)' \
    '    printf "running 0 tests\n\n"' \
    '    printf "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n"' \
    '    ;;' \
    '  *) exit 2 ;;' \
    'esac' >"${fake_binary}"
  chmod +x "${fake_binary}"
  printf '%s\n' \
    'fixture::active_source_family_contract_selected' >"${fake_manifest}"

  if CTX_ACTIVE_SOURCE_FAMILY_RUNNER_REGRESSION=1 \
    CTX_ACTIVE_SOURCE_FAMILY_FAKE_RESULT=ignored \
    TEST_TMPDIR="${fixture_root}/ignored" \
    "${runner}" "${fake_binary}" "${fake_manifest}" \
    >"${fixture_root}/ignored.log" 2>&1; then
    printf 'runner regression failed: an ignored selected test returned success\n' >&2
    return 1
  fi
  grep -Fq 'selected contract tests were ignored: 1' \
    "${fixture_root}/ignored.log" || {
    printf 'runner regression failed without the ignored-test diagnostic\n' >&2
    sed -n '1,160p' "${fixture_root}/ignored.log" >&2
    return 1
  }

  if CTX_ACTIVE_SOURCE_FAMILY_RUNNER_REGRESSION=1 \
    CTX_ACTIVE_SOURCE_FAMILY_FAKE_RESULT=count-mismatch \
    TEST_TMPDIR="${fixture_root}/count-mismatch" \
    "${runner}" "${fake_binary}" "${fake_manifest}" \
    >"${fixture_root}/count-mismatch.log" 2>&1; then
    printf 'runner regression failed: a zero-pass selected run returned success\n' >&2
    return 1
  fi
  grep -Fq 'executed pass count 0 differs from manifest count 1' \
    "${fixture_root}/count-mismatch.log" || {
    printf 'runner regression failed without the pass-count diagnostic\n' >&2
    sed -n '1,160p' "${fixture_root}/count-mismatch.log" >&2
    return 1
  }
}

if [[ "${CTX_ACTIVE_SOURCE_FAMILY_RUNNER_REGRESSION:-0}" != 1 ]]; then
  run_runner_regressions "$0"
fi

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

manifest_count="$(wc -l <"${expected}")"
run_output="${TEST_TMPDIR}/active-source-family-contract-tests.output"
set +e
LC_ALL=C "${test_binary}" \
  active_source_family_contract_ \
  --color never \
  --test-threads=1 2>&1 |
  tee "${run_output}"
test_status="${PIPESTATUS[0]}"
set -e

summary_counts="$(
  sed -n -E \
    's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*/\2 \3 \4/p' \
    "${run_output}" |
    tail -n 1
)"
[[ -n "${summary_counts}" ]] || {
  printf 'active source-family test result summary is missing or unrecognized\n' >&2
  exit 1
}
read -r passed failed ignored extra <<<"${summary_counts}"
[[ -z "${extra:-}" ]] || {
  printf 'active source-family test result summary is ambiguous: %s\n' "${summary_counts}" >&2
  exit 1
}

result_failed=0
if (( ignored != 0 )); then
  printf 'active source-family selected contract tests were ignored: %s\n' "${ignored}" >&2
  result_failed=1
fi
if (( passed != manifest_count )); then
  printf 'active source-family executed pass count %s differs from manifest count %s\n' \
    "${passed}" "${manifest_count}" >&2
  result_failed=1
fi
if (( failed != 0 )); then
  printf 'active source-family selected contract tests failed: %s\n' "${failed}" >&2
  result_failed=1
fi
if (( test_status != 0 )); then
  printf 'active source-family unit test binary exited with status %s\n' "${test_status}" >&2
  result_failed=1
fi
(( result_failed == 0 ))
