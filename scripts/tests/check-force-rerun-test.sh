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
cp "${source_root}/scripts/tests/fixtures/fake-bazel.sh" "${repo_root}/scripts/fake-bazel"
cat >"${repo_root}/scripts/check-rust-crate-size.py" <<'PY'
#!/usr/bin/env python3
import os
from pathlib import Path
import re
import sys

arguments = sys.argv[1:]
if arguments == ["--preflight", str(Path.cwd())]:
    record = f"preflight={arguments[0]} {arguments[1]}"
elif (
    len(arguments) == 3
    and arguments[0] == "--exact-candidate"
    and re.fullmatch(r"[0-9a-f]{40}", arguments[1])
    and arguments[2] == str(Path.cwd())
):
    record = f"candidate={arguments[0]} {arguments[1]} {arguments[2]}"
else:
    raise SystemExit(f"unexpected crate-size arguments: {arguments!r}")
with open(os.environ["CTX_FAKE_BAZEL_LOG"], "a", encoding="utf-8") as output:
    print(record, file=output)
PY
cat >"${repo_root}/scripts/bazelw" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

script_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
"${script_root}/fake-bazel" "$@"
if [[ "$#" -ge 3 && "$1" == "run" && "$2" == "//:rust_crate_size_preflight" && "$3" == "--" ]]; then
  shift 3
  exec "${script_root}/check-rust-crate-size.py" "$@"
fi
SH
chmod +x \
  "${repo_root}/scripts/check.sh" \
  "${repo_root}/scripts/check-rust-crate-size.py" \
  "${repo_root}/scripts/fake-bazel" \
  "${repo_root}/scripts/bazelw"
git -C "${repo_root}" init -q -b main
git -C "${repo_root}" config user.email check-test@example.invalid
git -C "${repo_root}" config user.name 'Check Test'
git -C "${repo_root}" add -A
git -C "${repo_root}" commit -q -m fixture
candidate_commit="$(git -C "${repo_root}" rev-parse HEAD)"

export CTX_FAKE_BAZEL_LOG="${test_root}/fake-bazel.log"
unset RUST_TEST_THREADS

: >"${CTX_FAKE_BAZEL_LOG}"
"${repo_root}/scripts/check.sh" --mode ci --force-rerun \
  >"${test_root}/ci.out" 2>"${test_root}/ci.err"
[[ "$(grep -c '^preflight=' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'ci mode did not run exactly one local preflight'
expected_preflight="${test_root}/expected-preflight.log"
cat >"${expected_preflight}" <<EOF
arg=run
arg=//:rust_crate_size_preflight
arg=--
arg=--preflight
arg=${repo_root}
env=RUST_TEST_THREADS=
preflight=--preflight ${repo_root}
EOF
head -n 7 "${CTX_FAKE_BAZEL_LOG}" >"${test_root}/actual-preflight.log"
cmp -s "${expected_preflight}" "${test_root}/actual-preflight.log" \
  || fail 'ci mode did not run the exact local preflight before named-mode actions'
if grep -Fqx 'arg=query' "${CTX_FAKE_BAZEL_LOG}"; then
  fail 'ci mode ran a redundant Bazel query'
fi
[[ "$(grep -c '^arg=build$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'ci mode did not build exactly once'
[[ "$(grep -c '^arg=test$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'ci mode did not test exactly once'
[[ "$(grep -c '^arg=--cache_test_results=no$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'force flag was not added exactly once'
awk '
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
"${repo_root}/scripts/check.sh" --mode ci \
  >"${test_root}/normal.out" 2>"${test_root}/normal.err"
[[ "$(grep -c '^preflight=' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
  || fail 'normal mode did not run exactly one local preflight'
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
grep -Fq -- 'physical Rust crate-size gate runs locally' \
  <("${repo_root}/scripts/check.sh" --help) \
  || fail 'help does not document local crate-size behavior'

expected_modes="$(printf '%s\n' ci nightly release)"
[[ "$("${repo_root}/scripts/check.sh" --list-modes)" == "${expected_modes}" ]] \
  || fail 'mode inventory is not the canonical three-tier taxonomy'

for removed_mode in fast presubmit smoke; do
  if "${repo_root}/scripts/check.sh" --mode "${removed_mode}" \
    >"${test_root}/${removed_mode}.out" 2>"${test_root}/${removed_mode}.err"; then
    fail "removed ${removed_mode} mode still succeeds"
  fi
  grep -Fq "unknown check mode: ${removed_mode}" "${test_root}/${removed_mode}.err" \
    || fail "removed ${removed_mode} mode did not fail explicitly"
  if grep -Fq '^preflight=' "${CTX_FAKE_BAZEL_LOG}"; then
    fail "removed ${removed_mode} mode ran preflight before mode validation"
  fi
done

for mode in ci nightly release; do
  : >"${CTX_FAKE_BAZEL_LOG}"
  "${repo_root}/scripts/check.sh" --mode "${mode}" \
    >"${test_root}/${mode}.out" 2>"${test_root}/${mode}.err"
  if [[ "${mode}" == release ]]; then
    [[ "$(grep -c '^candidate=' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
      || fail 'release mode did not run exactly one exact-candidate gate'
    grep -Fqx "arg=--exact-candidate" "${CTX_FAKE_BAZEL_LOG}" \
      || fail 'release mode did not select exact-candidate validation'
    grep -Fqx "arg=${candidate_commit}" "${CTX_FAKE_BAZEL_LOG}" \
      || fail 'release mode did not bind the checked-out commit'
    if grep -Fqx 'arg=--preflight' "${CTX_FAKE_BAZEL_LOG}"; then
      fail 'release mode retained integration ancestry freshness'
    fi
  else
    [[ "$(grep -c '^preflight=' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
      || fail "${mode} mode did not run exactly one local preflight"
    grep -Fqx 'arg=--preflight' "${CTX_FAKE_BAZEL_LOG}" \
      || fail "${mode} mode did not retain integration ancestry freshness"
    if grep -Fqx 'arg=--exact-candidate' "${CTX_FAKE_BAZEL_LOG}"; then
      fail "${mode} mode unexpectedly selected exact-candidate validation"
    fi
  fi
  grep -Fqx 'arg=//...' "${CTX_FAKE_BAZEL_LOG}" \
    || fail "${mode} mode did not lint the full workspace"
  suite="${mode}_tests"
  if [[ "${mode}" == release ]]; then
    suite="nightly_tests"
  fi
  grep -Fqx "arg=//:${suite}" "${CTX_FAKE_BAZEL_LOG}" \
    || fail "${mode} mode did not execute its owning suite"
  [[ "$(grep -c '^arg=--config=ci$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
    || fail "${mode} mode did not use the inherited lint config exactly once"
  [[ "$(grep -c '^arg=--config=test$' "${CTX_FAKE_BAZEL_LOG}")" == "1" ]] \
    || fail "${mode} mode did not isolate deterministic tests from lint aspects"
  if grep -Fqx 'arg=--config=lint' "${CTX_FAKE_BAZEL_LOG}"; then
    fail "${mode} mode applied the lint aspect explicitly"
  fi
done

if grep -Eq '^test:ci --test_env=(BUILDKITE|BUILDKITE_BUILD_ID|CI|GITHUB_ACTIONS)$' \
  "${source_root}/.bazelrc"; then
  fail 'volatile CI identity is inherited by every test action'
fi

printf 'check force-rerun tests passed\n'
