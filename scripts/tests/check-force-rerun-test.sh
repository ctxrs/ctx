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

expected_named_mode_transcript() {
  local mode="$1"
  local tag_filter
  case "${mode}" in
    ci) tag_filter='-manual,-tier-nightly,-tier-release' ;;
    nightly) tag_filter='-manual,-tier-release' ;;
    release) tag_filter='-manual' ;;
    *) fail "missing expected transcript for ${mode}" ;;
  esac

  if [[ "${mode}" == release ]]; then
    printf 'arg=%s\n' run //:rust_crate_size_preflight -- --exact-candidate \
      "${candidate_commit}" "${repo_root}"
    printf 'env=RUST_TEST_THREADS=\n'
    printf 'candidate=--exact-candidate %s %s\n' "${candidate_commit}" "${repo_root}"
  else
    printf 'arg=%s\n' run //:rust_crate_size_preflight -- --preflight "${repo_root}"
    printf 'env=RUST_TEST_THREADS=\n'
    printf 'preflight=--preflight %s\n' "${repo_root}"
  fi
  printf 'arg=%s\n' build //... --config=ci
  printf 'env=RUST_TEST_THREADS=\n'
  printf 'arg=%s\n' test --cache_test_results=no //... --config=test \
    "--test_tag_filters=${tag_filter}"
  printf 'env=RUST_TEST_THREADS=\n'
}

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
grep -Fq -- 'Independent per-package limits are 21,000' \
  <("${repo_root}/scripts/check.sh" --help) \
  || fail 'help does not document the independent production crate-size limit'
grep -Fq -- 'production CLOC and 21,000 test-surface CLOC.' \
  <("${repo_root}/scripts/check.sh" --help) \
  || fail 'help does not document the independent test-surface limit'

expected_modes="$(printf '%s\n' ci nightly release)"
[[ "$("${repo_root}/scripts/check.sh" --list-modes)" == "${expected_modes}" ]] \
  || fail 'mode inventory is not the canonical three-tier taxonomy'

for removed_mode in fast presubmit smoke; do
  : >"${CTX_FAKE_BAZEL_LOG}"
  if "${repo_root}/scripts/check.sh" --mode "${removed_mode}" \
    >"${test_root}/${removed_mode}.out" 2>"${test_root}/${removed_mode}.err"; then
    fail "removed ${removed_mode} mode still succeeds"
  fi
  grep -Fq "unknown check mode: ${removed_mode}" "${test_root}/${removed_mode}.err" \
    || fail "removed ${removed_mode} mode did not fail explicitly"
  [[ ! -s "${CTX_FAKE_BAZEL_LOG}" ]] \
    || fail "removed ${removed_mode} mode ran a command before mode validation"
done

for mode in ci nightly release; do
  : >"${CTX_FAKE_BAZEL_LOG}"
  "${repo_root}/scripts/check.sh" --mode "${mode}" --force-rerun \
    >"${test_root}/${mode}.out" 2>"${test_root}/${mode}.err"
  cmp -s <(expected_named_mode_transcript "${mode}") "${CTX_FAKE_BAZEL_LOG}" \
    || fail "${mode} mode did not run the exact ordered force-rerun transcript"
done

: >"${CTX_FAKE_BAZEL_LOG}"
"${repo_root}/scripts/check.sh" --mode ci \
  >"${test_root}/normal.out" 2>"${test_root}/normal.err"
if grep -Fqx 'arg=--cache_test_results=no' "${CTX_FAKE_BAZEL_LOG}"; then
  fail 'normal mode disabled Bazel test-result reuse'
fi

if grep -Eq '^test:ci --test_env=(BUILDKITE|BUILDKITE_BUILD_ID|CI|GITHUB_ACTIONS)$' \
  "${source_root}/.bazelrc"; then
  fail 'volatile CI identity is inherited by every test action'
fi

printf 'check force-rerun tests passed\n'
