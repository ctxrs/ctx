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

mkdir -p "${repo_root}"/{scripts/bazel,scripts/tests/fixtures,src}
cp "${source_root}/.bazelversion" "${repo_root}/.bazelversion"
cp "${source_root}/scripts/bazelw" "${repo_root}/scripts/bazelw"
cp "${source_root}/scripts/bazel-affected.sh" "${repo_root}/scripts/bazel-affected.sh"
cp "${source_root}/scripts/bazel/workspace-status.sh" "${repo_root}/scripts/bazel/workspace-status.sh"
cp "${source_root}/scripts/ci-common.sh" "${repo_root}/scripts/ci-common.sh"
cp "${source_root}/scripts/tests/fixtures/fake-bazel.sh" "${repo_root}/scripts/tests/fixtures/fake-bazel.sh"
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
  '//pkg:focused_suite' \
  '//pkg:non_test_tool' >"${impacted}"
# The fake query result stands in for Bazel's evaluated graph: it contains a
# leaf test with an intentionally unfamiliar name, not a target-name guess.
printf '%s\n' '//pkg:unfamiliar_routine' >"${query_output}"
: >"${fake_log}"
affected_impacted="${impacted}"
affected_query="${query_output}"

# This real Bazel fixture proves the selector's suite, routing-tag, and quoted
# punctuation-label query semantics without another classifier.
real_repo="${test_root}/real-query"
mkdir -p "${real_repo}"
cp "${source_root}/.bazelversion" "${real_repo}/.bazelversion"
printf 'module(name = "affected_query_fixture")\nbazel_dep(name = "rules_shell", version = "0.8.0")\n' >"${real_repo}/MODULE.bazel"
printf '#!/usr/bin/env bash\nexit 0\n' >"${real_repo}/pass.sh"
chmod +x "${real_repo}/pass.sh"
cat >"${real_repo}/BUILD.bazel" <<'EOF'
load("@rules_shell//shell:sh_test.bzl", "sh_test")
EOF
real_tests=()
for test in \
  'unfamiliar+comma,equals=target:' \
  'release_gate_test:release-gate' \
  'no_cache_test:no-cache' \
  'manual_test:manual' \
  'nightly_test:tier-nightly' \
  'release_test:tier-release'; do
  name="${test%%:*}"
  tags="${test#*:}"
  real_tests+=(":${name}")
  printf '\nsh_test(name = "%s", srcs = ["pass.sh"]' "${name}" >>"${real_repo}/BUILD.bazel"
  [[ -z "${tags}" ]] || printf ', tags = ["%s"]' "${tags}" >>"${real_repo}/BUILD.bazel"
  printf ')\n' >>"${real_repo}/BUILD.bazel"
done
printf '\ntest_suite(name = "affected_suite", tests = [%s])\n' \
  "$(printf '"%s", ' "${real_tests[@]}")" >>"${real_repo}/BUILD.bazel"
real_test_query='kind(".*_test rule", tests(set("//:affected_suite" "//:unfamiliar+comma,equals=target")))'
real_excluded_tags='(^|\[|, )(manual|tier[-]nightly|tier[-]release)(, |\])'
CTX_BAZEL_WORKSPACE="${real_repo}" \
  "${source_root}/scripts/bazelw" query \
    "${real_test_query} except attr(\"tags\", \"${real_excluded_tags}\", ${real_test_query})" \
    --output=label | LC_ALL=C sort >"${test_root}/real-query.out"
cat >"${test_root}/real-query.expected" <<'EOF'
//:no_cache_test
//:release_gate_test
//:unfamiliar+comma,equals=target
EOF
cmp -s "${test_root}/real-query.expected" "${test_root}/real-query.out" \
  || fail 'real Bazel suite/tag query did not preserve default-CI tests exactly'

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
    CTX_FAKE_BAZEL_IMPACTED_FILE="${affected_impacted}" \
    CTX_FAKE_BAZEL_LOG="${fake_log}" \
    CTX_FAKE_BAZEL_QUERY_FILE="${affected_query}" \
    CTX_FAKE_BAZEL_REQUIRE_EXCLUDE_EXTERNAL=1 \
    CTX_TOTAL_MEMORY_GB=16 \
      scripts/bazel-affected.sh HEAD
  ) >"${stdout}" 2>"${stderr}"
}

assert_fallback() {
  local name="$1" stdout="$2" stderr="$3" diagnostic="$4"
  [[ "$(cat "${stdout}")" == '//...' ]] || fail "${name} did not select ci"
  grep -Fq "${diagnostic}" "${stderr}" || fail "${name} diagnostic was not emitted"
}

assert_global_fallback() {
  local path="$1"
  local restore="${test_root}/$(basename "${path}").restore"
  if [[ -e "${path}" ]]; then
    cp "${path}" "${restore}"
  fi
  mkdir -p "$(dirname "${path}")"
  printf 'changed global input\n' >>"${path}"
  run_affected "${test_root}/global.out" "${test_root}/global.err"
  assert_fallback "global input ${path}" "${test_root}/global.out" "${test_root}/global.err" 'build configuration changed'
  if [[ -e "${restore}" ]]; then
    mv "${restore}" "${path}"
  else
    rm -f -- "${path}"
  fi
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
  [[ "$(cat "${output}")" == '//pkg:unfamiliar_routine' ]] \
    || fail "concurrent selector did not preserve Bazel's unfamiliar test name: ${output}"
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
grep -Fq 'arg=--excludeExternalTargets' "${fake_log}" \
  || fail 'bazel-diff was not told to exclude non-buildable //external targets'
grep -Fq 'tests(set(' "${fake_log}" \
  || fail 'affected query did not expand test suites'
grep -Fq 'kind(".*_test rule"' "${fake_log}" \
  || fail 'affected query did not discard non-test rules'
grep -Fq '(^|\[|, )(manual|tier[-]nightly|tier[-]release)(, |\])' "${fake_log}" \
  || fail 'Bazel query did not use exact public-CI routing tags'

generate_count_before="$(grep -c '^event=generate-hashes ' "${fake_log}")"
run_affected "${test_root}/warm.out" "${test_root}/warm.err"
generate_count_after="$(grep -c '^event=generate-hashes ' "${fake_log}")"
[[ "$(( generate_count_after - generate_count_before ))" == "1" ]] \
  || fail 'warm selector did not reuse the commit-keyed base hash'
[[ "$(cat "${test_root}/warm.out")" == '//pkg:unfamiliar_routine' ]] \
  || fail 'warm selector lost focused behavior'

CTX_FAKE_BAZEL_FAIL_MODE=get-impacted-targets \
  run_affected "${test_root}/failure.out" "${test_root}/failure.err"
assert_fallback 'bazel-diff failure' "${test_root}/failure.out" "${test_root}/failure.err" \
  'affected test selection failed closed to //...: bazel-diff failed'

for global_input in \
  "${repo_root}/BUILD.bazel" \
  "${repo_root}/tools/selection.bzl" \
  "${repo_root}/MODULE.bazel" \
  "${repo_root}/MODULE.bazel.lock" \
  "${repo_root}/Cargo.lock" \
  "${repo_root}/.bazelrc" \
  "${repo_root}/scripts/bazel/workspace-status.sh"; do
  assert_global_fallback "${global_input}"
done

printf 'not-a-bazel-label\n' >"${impacted}"
run_affected "${test_root}/malformed.out" "${test_root}/malformed.err"
assert_fallback 'malformed bazel-diff output' "${test_root}/malformed.out" "${test_root}/malformed.err" 'invalid affected label'

printf '%s\n' '//pkg:punctuation+comma,equals=target' >"${impacted}"
printf '%s\n' '//pkg:punctuation+comma,equals=target' >"${query_output}"
run_affected "${test_root}/punctuation.out" "${test_root}/punctuation.err"
[[ "$(cat "${test_root}/punctuation.out")" == '//pkg:punctuation+comma,equals=target' ]] \
  || fail 'query-safe punctuation label did not survive selection'
grep -Fq '"//pkg:punctuation+comma,equals=target"' "${fake_log}" \
  || fail 'query-safe punctuation label was not rendered as a quoted Bazel query word'

printf '%s\n' '//pkg:focused_suite) union //...' >"${impacted}"
run_affected "${test_root}/injection.out" "${test_root}/injection.err"
assert_fallback 'query injection-shaped label' "${test_root}/injection.out" "${test_root}/injection.err" 'invalid affected label'

printf '%s\n' '//pkg:focused_suite' >"${impacted}"
printf '%s\n' '//pkg:unfamiliar_routine' >"${query_output}"
affected_query="${test_root}/missing-query-output"
run_affected "${test_root}/query-failure.out" "${test_root}/query-failure.err"
assert_fallback 'query failure' "${test_root}/query-failure.out" "${test_root}/query-failure.err" 'Bazel query failed'

affected_query="${query_output}"
: >"${query_output}"
run_affected "${test_root}/empty.out" "${test_root}/empty.err"
assert_fallback 'empty eligible result' "${test_root}/empty.out" "${test_root}/empty.err" 'changed files have no eligible routine tests'

(
  cd "${repo_root}"
  CTX_AFFECTED_DRY_RUN=1 scripts/bazel-affected.sh refs/heads/missing
) >"${test_root}/missing-base.out" 2>"${test_root}/missing-base.err"
assert_fallback 'missing base' "${test_root}/missing-base.out" "${test_root}/missing-base.err" 'could not resolve affected-test base'

printf 'bazel-affected tests passed\n'
