#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
wrapper="${repo_root}/scripts/bazelw"
fake_bazel="${repo_root}/scripts/tests/fixtures/fake-bazel.sh"
fake_df="${repo_root}/scripts/tests/fixtures/fake-df.sh"
fake_governor="${repo_root}/scripts/tests/fixtures/fake-build-governor.sh"
test_root="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/ctx-bazelw-test.XXXXXXXX")"
trap 'rm -rf -- "${test_root}"' EXIT

fail() {
  printf 'bazelw test failed: %s\n' "$*" >&2
  exit 1
}

assert_log_line() {
  local expected="$1"
  grep -Fqx -- "${expected}" "${CTX_FAKE_BAZEL_LOG}" \
    || fail "missing fake Bazel log line: ${expected}"
}

export BAZEL="${fake_bazel}"
export CTX_FAKE_BAZEL_LOG="${test_root}/fake-bazel.log"
export CTX_BAZEL_CACHE_ROOT="${test_root}/explicit-cache"
export CTX_CPU_COUNT=32
export CTX_TOTAL_MEMORY_GB=128
# Exercise the wrapper's default derivation independently from the bounded
# thread count forwarded by an outer Bazel test invocation.
unset RUST_TEST_THREADS
unset CTX_HOST_BUILD_GOVERNOR CTX_HOST_BUILD_GOVERNOR_ACTIVE

if grep -Eq '\[\[[[:space:]]+-v[[:space:]]' "${wrapper}"; then
  echo 'bazel wrapper must remain compatible with the Bash 3.2 shipped by macOS' >&2
  exit 1
fi
unset CTX_BUILD_GOVERNOR_LEASE_ID CTX_BUILD_GOVERNOR_LEASE_CLASS XDG_CONFIG_HOME
export HOME="${test_root}/home"
export TMPDIR="${test_root}/tmp"
mkdir -p "${HOME}" "${TMPDIR}"

: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" test //:focused --config=test 2>"${test_root}/config.log"
[[ "$(grep -c '^ctx bazel:' "${test_root}/config.log")" == "1" ]] \
  || fail 'wrapper must print exactly one configuration line'
assert_log_line "arg=--repository_cache=${CTX_BAZEL_CACHE_ROOT}/bazel-9.2.0/repository-cache"
assert_log_line "arg=--disk_cache=${CTX_BAZEL_CACHE_ROOT}/bazel-9.2.0/action-test-cache"
assert_log_line "arg=--experimental_disk_cache_gc_max_size=100G"
assert_log_line "arg=--experimental_disk_cache_gc_max_age=30d"
assert_log_line "arg=--max_idle_secs=600"
assert_log_line "arg=--jobs=8"
assert_log_line "arg=--local_resources=cpu=8"
assert_log_line "arg=--local_resources=memory=65536"
assert_log_line "arg=--local_test_jobs=2"
assert_log_line "arg=--test_env=RUST_TEST_THREADS=4"
if grep -Fq 'arg=--test_env=TMPDIR=' "${CTX_FAKE_BAZEL_LOG}"; then
  fail 'wrapper forwarded TMPDIR without explicit release-test authority'
fi
assert_log_line "env=RUST_TEST_THREADS=4"
grep -Fq "arg=--output_user_root=${CTX_BAZEL_CACHE_ROOT}/output-roots/bazel-9.2.0/" "${CTX_FAKE_BAZEL_LOG}" \
  || fail 'output root is not versioned per worktree'
grep -Fq "arg=--sandbox_base=${CTX_BAZEL_CACHE_ROOT}/bazel-9.2.0/sandboxes/" "${CTX_FAKE_BAZEL_LOG}" \
  || fail 'sandbox is not placed in the spacious cache root'
sandbox_base="$(sed -n 's/^arg=--sandbox_base=//p' "${CTX_FAKE_BAZEL_LOG}" | head -n 1)"
mkdir -p "${sandbox_base}/stale-sandbox"
"${wrapper}" shutdown 2>"${test_root}/shutdown-config.log"
[[ ! -e "${sandbox_base}" ]] \
  || fail 'shutdown did not remove the disposable default sandbox base'

: >"${CTX_FAKE_BAZEL_LOG}"
CTX_TOTAL_MEMORY_GB=6 "${wrapper}" build //:focused 2>"${test_root}/memory-config.log"
assert_log_line "arg=--jobs=2"
assert_log_line "arg=--local_resources=cpu=2"
assert_log_line "arg=--local_resources=memory=3072"

: >"${CTX_FAKE_BAZEL_LOG}"
BAZEL_JOBS=5 \
BAZEL_LOCAL_CPU_RESOURCES=3 \
BAZEL_LOCAL_RAM_RESOURCES=4096 \
BAZEL_LOCAL_TEST_JOBS=1 \
RUST_TEST_THREADS=2 \
  "${wrapper}" test //:focused 2>"${test_root}/override-config.log"
assert_log_line "arg=--jobs=5"
assert_log_line "arg=--local_resources=cpu=3"
assert_log_line "arg=--local_resources=memory=4096"
assert_log_line "arg=--local_test_jobs=1"
assert_log_line "arg=--test_env=RUST_TEST_THREADS=2"

: >"${CTX_FAKE_BAZEL_LOG}"
release_test_tmpdir="${test_root}/release-test-tmp"
mkdir -p "${release_test_tmpdir}"
CTX_BAZEL_TEST_TMPDIR="${release_test_tmpdir}" \
  "${wrapper}" test //:release-focused --config=test \
  2>"${test_root}/release-tmpdir-config.log"
assert_log_line "arg=--test_env=TMPDIR=${release_test_tmpdir}"

if CTX_BAZEL_TEST_TMPDIR=relative \
  "${wrapper}" test //:invalid-release-tmpdir --config=test \
  >"${test_root}/invalid-release-tmpdir.out" \
  2>"${test_root}/invalid-release-tmpdir.err"; then
  fail 'relative release test TMPDIR unexpectedly succeeded'
fi
grep -Fq 'CTX_BAZEL_TEST_TMPDIR must be an existing absolute directory' \
  "${test_root}/invalid-release-tmpdir.err" \
  || fail 'invalid release test TMPDIR did not fail explicitly'

: >"${CTX_FAKE_BAZEL_LOG}"
CTX_BAZEL_DISK_CACHE_MAX_SIZE=12G \
CTX_BAZEL_DISK_CACHE_MAX_AGE=7d \
  "${wrapper}" build //:focused 2>"${test_root}/gc-override-config.log"
assert_log_line "arg=--experimental_disk_cache_gc_max_size=12G"
assert_log_line "arg=--experimental_disk_cache_gc_max_age=7d"

if CTX_FAKE_BAZEL_VERSION=8.0.0 "${wrapper}" info output_base \
  >"${test_root}/version-mismatch.out" 2>"${test_root}/version-mismatch.err"; then
  fail 'mismatched Bazel binary version unexpectedly succeeded'
fi
grep -Fq 'Bazel version mismatch: expected 9.2.0, got 8.0.0' \
  "${test_root}/version-mismatch.err" \
  || fail 'Bazel version mismatch diagnostic was not emitted'

unset CTX_BAZEL_CACHE_ROOT
mkdir -p "${test_root}/bin"
cp "${fake_df}" "${test_root}/bin/df"
chmod 0755 "${test_root}/bin/df"
export PATH="${test_root}/bin:${PATH}"
export CTX_FAKE_DF_FREE_KIB=9000000
export CTX_FAKE_DF_FREE_INODES=400000
export CTX_BAZEL_SPACIOUS_ROOT="${test_root}/spacious"
mkdir -p "${CTX_BAZEL_SPACIOUS_ROOT}"
: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" info output_base 2>"${test_root}/spacious-config.log"
grep -Fq "cache=${CTX_BAZEL_SPACIOUS_ROOT}/ctx-bazel/bazel-9.2.0" "${test_root}/spacious-config.log" \
  || fail 'usable spacious root was not preferred'

export CTX_BAZEL_SPACIOUS_ROOT="${test_root}/missing-spacious"
export XDG_CACHE_HOME="${test_root}/xdg-cache"
: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" info output_base 2>"${test_root}/xdg-config.log"
grep -Fq "cache=${XDG_CACHE_HOME}/ctx/bazel/bazel-9.2.0" "${test_root}/xdg-config.log" \
  || fail 'XDG cache fallback was not selected'

# A writable volume can still be unusable to Bazel when blocks or inodes are
# exhausted. Exercise the capacity decision directly with deterministic df
# output instead of requiring a dedicated filesystem in the test sandbox.
source "${repo_root}/scripts/ci-common.sh"
launcher_bin="${test_root}/launcher-bin"
mkdir -p "${launcher_bin}"
ln -s "${fake_bazel}" "${launcher_bin}/bazelisk"
ln -s "${fake_bazel}" "${launcher_bin}/bazel"
selected_launcher="$(PATH="${launcher_bin}:/usr/bin:/bin" ctx_find_bazel)"
[[ "${selected_launcher}" == "${launcher_bin}/bazelisk" ]] \
  || fail 'Bazelisk was not preferred over a direct Bazel binary'

export CTX_FAKE_DF_FREE_INODES=1
export CTX_BAZEL_SPACIOUS_ROOT="${test_root}/spacious"
export XDG_CACHE_HOME="${test_root}/xdg-low-inode"
selected_cache="$(ctx_bazel_cache_root)"
[[ "${selected_cache}" == "${XDG_CACHE_HOME}/ctx/bazel" ]] \
  || fail 'low-inode spacious root did not fall back before invoking Bazel'

# An explicitly activated generic host governor wraps build-capable commands,
# raises healthy single-build defaults, and leaves query/read commands light.
export XDG_CONFIG_HOME="${test_root}/governor-config"
export CTX_FAKE_GOVERNOR_LOG="${test_root}/fake-governor.log"
mkdir -p "${XDG_CONFIG_HOME}/ctx"
ln -s "${fake_governor}" "${XDG_CONFIG_HOME}/ctx/build-governor"
unset BAZEL_JOBS BAZEL_LOCAL_CPU_RESOURCES BAZEL_LOCAL_RAM_RESOURCES
unset CTX_BAZEL_JOBS CTX_BAZEL_LOCAL_CPU_RESOURCES CTX_BAZEL_LOCAL_RAM_RESOURCES
: >"${CTX_FAKE_GOVERNOR_LOG}"
: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" test //:governed --config=test 2>"${test_root}/governed-config.log"
grep -Fqx 'mode=bazel' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'heavy command did not use governor'
grep -Fqx 'command=test' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governor command classification missing'
grep -Fqx 'jobs=16' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governed jobs default is not 16'
grep -Fqx 'cpu=16' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governed CPU default is not 16'
grep -Fqx 'ram=49152' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governed memory default is not bounded'
assert_log_line 'arg=--jobs=16'
assert_log_line 'arg=--local_resources=cpu=16'

mapfile -t governed_argv < <(sed -n 's/^argv=//p' "${CTX_FAKE_GOVERNOR_LOG}")
mapfile -t executed_argv < <(sed -n 's/^arg=//p' "${CTX_FAKE_BAZEL_LOG}")
[[ "${governed_argv[0]:-}" == "${fake_bazel}" ]] \
  || fail 'governor did not receive the Bazel executable first'
[[ "${governed_argv[1]:-}" == --output_user_root=* ]] \
  || fail 'governor did not receive output_user_root as a startup option'
[[ "${governed_argv[2]:-}" == '--max_idle_secs=600' ]] \
  || fail 'governor did not receive max_idle_secs before the command'
[[ "${governed_argv[3]:-}" == 'test' ]] \
  || fail 'governor did not receive startup options before the Bazel command'
[[ "${#governed_argv[@]}" == "$(( ${#executed_argv[@]} + 1 ))" ]] \
  || fail 'governor did not receive the complete Bazel argv'
for (( index = 0; index < ${#executed_argv[@]}; index++ )); do
  [[ "${governed_argv[index + 1]}" == "${executed_argv[index]}" ]] \
    || fail "governor changed Bazel argument $index"
done

for governed_command in build coverage run clean; do
  : >"${CTX_FAKE_GOVERNOR_LOG}"
  : >"${CTX_FAKE_BAZEL_LOG}"
  "${wrapper}" "${governed_command}" //:governed \
    2>"${test_root}/${governed_command}-governed-config.log"
  grep -Fqx 'mode=bazel' "${CTX_FAKE_GOVERNOR_LOG}" \
    || fail "${governed_command} did not use governor"
  grep -Fqx "command=${governed_command}" "${CTX_FAKE_GOVERNOR_LOG}" \
    || fail "${governed_command} classification was not forwarded"
done

: >"${CTX_FAKE_GOVERNOR_LOG}"
: >"${CTX_FAKE_BAZEL_LOG}"
BAZEL_JOBS=5 \
BAZEL_LOCAL_CPU_RESOURCES=3 \
BAZEL_LOCAL_RAM_RESOURCES=4096 \
  "${wrapper}" build //:governed-override 2>"${test_root}/governed-override-config.log"
grep -Fqx 'jobs=5' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governor changed explicit jobs override'
grep -Fqx 'cpu=3' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governor changed explicit CPU override'
grep -Fqx 'ram=4096' "${CTX_FAKE_GOVERNOR_LOG}" || fail 'governor changed explicit memory override'
assert_log_line 'arg=--jobs=5'
assert_log_line 'arg=--local_resources=cpu=3'
assert_log_line 'arg=--local_resources=memory=4096'

: >"${CTX_FAKE_GOVERNOR_LOG}"
"${wrapper}" query //... >/dev/null 2>"${test_root}/query-config.log"
[[ ! -s "${CTX_FAKE_GOVERNOR_LOG}" ]] || fail 'light query unexpectedly acquired admission'

: >"${CTX_FAKE_GOVERNOR_LOG}"
"${wrapper}" shutdown 2>"${test_root}/governed-shutdown-config.log"
[[ ! -s "${CTX_FAKE_GOVERNOR_LOG}" ]] || fail 'shutdown unexpectedly acquired admission'

: >"${CTX_FAKE_GOVERNOR_LOG}"
set +e
CTX_HOST_BUILD_GOVERNOR_ACTIVE=1 "${wrapper}" build //:forged-active \
  >"${test_root}/forged-active.out" 2>"${test_root}/forged-active.err"
forged_active_status=$?
set -e
[[ "${forged_active_status}" == "125" ]] ||
  fail "forged active marker exited ${forged_active_status}, expected 125"
grep -Fq 'active marker is not backed by the current lease cgroup' \
  "${test_root}/forged-active.err" || fail 'forged active marker did not fail closed'

: >"${CTX_FAKE_GOVERNOR_LOG}"
set +e
CTX_HOST_BUILD_GOVERNOR= "${wrapper}" build //:empty-governor \
  >"${test_root}/empty-governor.out" 2>"${test_root}/empty-governor.err"
empty_governor_status=$?
set -e
[[ "${empty_governor_status}" == "125" ]] ||
  fail "empty governor override exited ${empty_governor_status}, expected 125"
grep -Fq 'configured CTX host build governor must not be empty' \
  "${test_root}/empty-governor.err" || fail 'empty governor override did not fail closed'

set +e
CTX_HOST_BUILD_GOVERNOR="${test_root}/missing-governor" \
  "${wrapper}" build //:must-not-run >"${test_root}/missing.out" 2>"${test_root}/missing.err"
missing_status=$?
set -e
[[ "${missing_status}" == "125" ]] || fail "invalid governor exited ${missing_status}, expected 125"
grep -Fq 'configured CTX host build governor must be an absolute executable' \
  "${test_root}/missing.err" || fail 'missing governor did not fail closed'

printf 'bazelw tests passed\n'
