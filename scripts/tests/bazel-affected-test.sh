#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
test_root="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/ctx-bazel-affected-test.XXXXXXXX")"
repo_root="${test_root}/repo"
trap 'rm -rf -- "${test_root}"' EXIT

fail() {
  printf 'bazel-affected test failed: %s\n' "$*" >&2
  exit 1
}

mkdir -p "${repo_root}/scripts/tests/fixtures" "${repo_root}/tools/bazel" "${repo_root}/src"
cp "${source_root}/.bazelversion" "${repo_root}/.bazelversion"
cp "${source_root}/scripts/bazelw" "${repo_root}/scripts/bazelw"
cp "${source_root}/scripts/bazel-affected.sh" "${repo_root}/scripts/bazel-affected.sh"
cp "${source_root}/scripts/ci-common.sh" "${repo_root}/scripts/ci-common.sh"
cp "${source_root}/scripts/tests/fixtures/fake-bazel.sh" "${repo_root}/scripts/tests/fixtures/fake-bazel.sh"
cp "${source_root}/tools/bazel/select_affected_tests.py" "${repo_root}/tools/bazel/select_affected_tests.py"
chmod +x \
  "${repo_root}/scripts/bazelw" \
  "${repo_root}/scripts/bazel-affected.sh" \
  "${repo_root}/scripts/tests/fixtures/fake-bazel.sh"

printf 'module(name = "affected_contract_fixture")\n' >"${repo_root}/MODULE.bazel"
printf 'before\n' >"${repo_root}/src/input.txt"
git -C "${repo_root}" init -q
git -C "${repo_root}" config user.email ctx-tests@example.invalid
git -C "${repo_root}" config user.name 'ctx tests'
git -C "${repo_root}" add .
git -C "${repo_root}" commit -qm base
printf 'after\n' >"${repo_root}/src/input.txt"

impacted="${test_root}/impacted.txt"
query_output="${test_root}/query-output.txt"
fake_log="${test_root}/fake-bazel.log"
diff_cache="${test_root}/diff-cache"
cache_root="${test_root}/bazel-cache"
printf '%s\n' \
  '//pkg:focused_tests' \
  '//pkg:manual_smoke' \
  '//pkg:network_tests' \
  '//pkg:release_contract_tests' >"${impacted}"
cp "${impacted}" "${query_output}"
: >"${fake_log}"

run_affected() {
  local stdout="$1"
  local stderr="$2"
  (
    cd "${repo_root}"
    BAZEL="${repo_root}/scripts/tests/fixtures/fake-bazel.sh" \
    CTX_AFFECTED_DRY_RUN=1 \
    CTX_BAZEL_CACHE_ROOT="${cache_root}" \
    CTX_BAZEL_DIFF_CACHE_ROOT="${diff_cache}" \
    CTX_CPU_COUNT=8 \
    CTX_FAKE_BAZEL_DELAY=0.05 \
    CTX_FAKE_BAZEL_IMPACTED_FILE="${impacted}" \
    CTX_FAKE_BAZEL_LOG="${fake_log}" \
    CTX_FAKE_BAZEL_QUERY_FILE="${query_output}" \
    CTX_TOTAL_MEMORY_GB=16 \
      scripts/bazel-affected.sh HEAD
  ) >"${stdout}" 2>"${stderr}"
}

# Two cold selectors may both compute the immutable base, but their worktrees
# and transient outputs are isolated and either atomic publication is valid.
run_affected "${test_root}/concurrent-a.out" "${test_root}/concurrent-a.err" &
pid_a=$!
run_affected "${test_root}/concurrent-b.out" "${test_root}/concurrent-b.err" &
pid_b=$!
wait "${pid_a}" || fail 'first concurrent selector failed'
wait "${pid_b}" || fail 'second concurrent selector failed'

for output in "${test_root}/concurrent-a.out" "${test_root}/concurrent-b.out"; do
  [[ "$(cat "${output}")" == '//pkg:focused_tests' ]] \
    || fail "concurrent selector was not focused: ${output}"
done
base_sha="$(git -C "${repo_root}" rev-parse HEAD)"
[[ -s "${diff_cache}/hashes/${base_sha}.json" ]] \
  || fail 'commit-keyed base hash was not published'
if find "${diff_cache}/runs" -mindepth 1 -print -quit | grep -q .; then
  fail 'concurrent run directories were not cleaned'
fi
[[ "$(grep -c '^arg=shutdown$' "${fake_log}")" == "2" ]] \
  || fail 'ephemeral base-worktree Bazel servers were not shut down'
unique_output_roots="$(
  grep '^arg=--output_user_root=' "${fake_log}" | sort -u | wc -l
)"
(( unique_output_roots >= 3 )) \
  || fail 'concurrent base worktrees did not receive isolated output roots'
grep -Fq "arg=--bazelPath=${repo_root}/scripts/bazelw" "${fake_log}" \
  || fail 'bazel-diff impacted-target calculation bypassed the repository wrapper'
grep -Fq 'advisory|external|flaky-repetition|manual|network|no-cache|platform-native|release|requires-local-history|requires-signing|requires-vm|stress' "${fake_log}" \
  || fail 'query did not exclude non-routine tags'

generate_count_before="$(grep -c '^event=generate-hashes ' "${fake_log}")"
run_affected "${test_root}/warm.out" "${test_root}/warm.err"
generate_count_after="$(grep -c '^event=generate-hashes ' "${fake_log}")"
[[ "$(( generate_count_after - generate_count_before ))" == "1" ]] \
  || fail 'warm selector did not reuse the commit-keyed base hash'
[[ "$(cat "${test_root}/warm.out")" == '//pkg:focused_tests' ]] \
  || fail 'warm selector lost focused behavior'

(
  cd "${repo_root}"
  BAZEL="${repo_root}/scripts/tests/fixtures/fake-bazel.sh" \
  CTX_AFFECTED_DRY_RUN=1 \
  CTX_BAZEL_CACHE_ROOT="${cache_root}" \
  CTX_BAZEL_DIFF_CACHE_ROOT="${diff_cache}" \
  CTX_CPU_COUNT=8 \
  CTX_FAKE_BAZEL_FAIL_MODE=get-impacted-targets \
  CTX_FAKE_BAZEL_IMPACTED_FILE="${impacted}" \
  CTX_FAKE_BAZEL_LOG="${fake_log}" \
  CTX_FAKE_BAZEL_QUERY_FILE="${query_output}" \
  CTX_TOTAL_MEMORY_GB=16 \
    scripts/bazel-affected.sh HEAD
) >"${test_root}/failure.out" 2>"${test_root}/failure.err"
[[ "$(cat "${test_root}/failure.out")" == '//:presubmit' ]] \
  || fail 'bazel-diff failure did not select presubmit'
grep -Fq 'bazel-diff failed; selecting //:presubmit' "${test_root}/failure.err" \
  || fail 'fail-closed diagnostic was not emitted'

(
  cd "${repo_root}"
  CTX_AFFECTED_DRY_RUN=1 scripts/bazel-affected.sh refs/heads/missing
) >"${test_root}/missing-base.out" 2>"${test_root}/missing-base.err"
[[ "$(cat "${test_root}/missing-base.out")" == '//:presubmit' ]] \
  || fail 'missing base did not select presubmit'
grep -Fq 'could not resolve affected-test base' "${test_root}/missing-base.err" \
  || fail 'missing-base fail-closed diagnostic was not emitted'

printf 'bazel-affected tests passed\n'
