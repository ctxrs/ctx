#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi

test_root="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/ctx-check-force-test.XXXXXXXX")"
repo_root="${test_root}/repo"
trap 'rm -rf -- "${test_root}"' EXIT

fail() {
  printf 'check force-rerun test failed: %s\n' "$*" >&2
  exit 1
}

mkdir -p "${repo_root}/scripts"
cp "${source_root}/scripts/check.sh" "${repo_root}/scripts/check.sh"
cp "${source_root}/scripts/tests/fixtures/fake-bazel.sh" "${repo_root}/scripts/bazelw"
chmod +x "${repo_root}/scripts/check.sh" "${repo_root}/scripts/bazelw"

export CTX_FAKE_BAZEL_LOG="${test_root}/fake-bazel.log"
unset RUST_TEST_THREADS

: >"${CTX_FAKE_BAZEL_LOG}"
"${repo_root}/scripts/check.sh" --mode ci --force-rerun \
  >"${test_root}/ci.out" 2>"${test_root}/ci.err"
[[ "$(grep -c '^arg=query$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'ci mode did not query exactly once'
[[ "$(grep -c '^arg=build$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'ci mode did not build exactly once'
[[ "$(grep -c '^arg=test$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'ci mode did not test exactly once'
[[ "$(grep -c '^arg=--cache_test_results=no$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'force flag was not added exactly once'
awk '
  /^arg=query$/ { action = "query" }
  /^arg=build$/ { action = "build" }
  /^arg=test$/ { action = "test" }
  /^arg=--cache_test_results=no$/ {
    if (action != "test") {
      exit 1
    }
    found = 1
  }
  END { if (!found) exit 1 }
' "${CTX_FAKE_BAZEL_LOG}" \
  || fail 'force flag was attached to a non-test action'
if grep -Eq '^arg=(clean|--expunge)$' "${CTX_FAKE_BAZEL_LOG}"; then
  fail 'force rerun attempted to clean compilation caches'
fi

: >"${CTX_FAKE_BAZEL_LOG}"
"${repo_root}/scripts/check.sh" --mode fast \
  >"${test_root}/normal.out" 2>"${test_root}/normal.err"
if grep -Fqx 'arg=--cache_test_results=no' "${CTX_FAKE_BAZEL_LOG}"; then
  fail 'normal mode disabled Bazel test-result reuse'
fi

: >"${CTX_FAKE_BAZEL_LOG}"
"${repo_root}/scripts/check.sh" --force-rerun -- test //:focused --config=ci \
  >"${test_root}/direct.out" 2>"${test_root}/direct.err"
expected="${test_root}/expected.log"
cat >"${expected}" <<'EOF'
arg=test
arg=--cache_test_results=no
arg=//:focused
arg=--config=ci
env=RUST_TEST_THREADS=
EOF
cmp -s "${expected}" "${CTX_FAKE_BAZEL_LOG}" \
  || fail 'direct test command did not receive the exact force-rerun argv'

grep -Fq -- '--force-rerun disables test-result reuse' \
  <("${repo_root}/scripts/check.sh" --help) \
  || fail 'help does not document force-rerun cache behavior'

printf 'check force-rerun tests passed\n'
