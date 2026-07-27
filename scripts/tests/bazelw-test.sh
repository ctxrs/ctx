#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
wrapper="${repo_root}/scripts/bazelw"
fake_bazel="${repo_root}/scripts/tests/fixtures/fake-bazel.sh"
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
export HOME="${test_root}/home"
export TMPDIR="${test_root}/tmp"
mkdir -p "${HOME}" "${TMPDIR}"

: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" test //:focused --config=test 2>"${test_root}/config.log"
[[ "$(grep -c '^ctx bazel:' "${test_root}/config.log")" == "1" ]] \
  || fail 'wrapper must print exactly one configuration line'
assert_log_line "arg=--repository_cache=${CTX_BAZEL_CACHE_ROOT}/bazel-7.4.1/repository-cache"
assert_log_line "arg=--disk_cache=${CTX_BAZEL_CACHE_ROOT}/bazel-7.4.1/action-test-cache"
assert_log_line "arg=--experimental_disk_cache_gc_max_size=100G"
assert_log_line "arg=--experimental_disk_cache_gc_max_age=30d"
assert_log_line "arg=--max_idle_secs=600"
assert_log_line "arg=--jobs=8"
assert_log_line "arg=--local_resources=cpu=8"
assert_log_line "arg=--local_resources=memory=65536"
assert_log_line "arg=--local_test_jobs=2"
assert_log_line "arg=--test_env=RUST_TEST_THREADS=4"
assert_log_line "env=RUST_TEST_THREADS=4"
grep -Fq "arg=--output_user_root=${CTX_BAZEL_CACHE_ROOT}/output-roots/bazel-7.4.1/" "${CTX_FAKE_BAZEL_LOG}" \
  || fail 'output root is not versioned per worktree'
grep -Fq "arg=--sandbox_base=${CTX_BAZEL_CACHE_ROOT}/bazel-7.4.1/sandboxes/" "${CTX_FAKE_BAZEL_LOG}" \
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
CTX_BAZEL_DISK_CACHE_MAX_SIZE=12G \
CTX_BAZEL_DISK_CACHE_MAX_AGE=7d \
  "${wrapper}" build //:focused 2>"${test_root}/gc-override-config.log"
assert_log_line "arg=--experimental_disk_cache_gc_max_size=12G"
assert_log_line "arg=--experimental_disk_cache_gc_max_age=7d"

if CTX_FAKE_BAZEL_VERSION=8.0.0 "${wrapper}" info output_base \
  >"${test_root}/version-mismatch.out" 2>"${test_root}/version-mismatch.err"; then
  fail 'mismatched Bazel binary version unexpectedly succeeded'
fi
grep -Fq 'Bazel version mismatch: expected 7.4.1, got 8.0.0' \
  "${test_root}/version-mismatch.err" \
  || fail 'Bazel version mismatch diagnostic was not emitted'

unset CTX_BAZEL_CACHE_ROOT
export CTX_BAZEL_SPACIOUS_ROOT="${test_root}/spacious"
mkdir -p "${CTX_BAZEL_SPACIOUS_ROOT}"
: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" info output_base 2>"${test_root}/spacious-config.log"
grep -Fq "cache=${CTX_BAZEL_SPACIOUS_ROOT}/ctx-bazel/bazel-7.4.1" "${test_root}/spacious-config.log" \
  || fail 'usable spacious root was not preferred'

export CTX_BAZEL_SPACIOUS_ROOT="${test_root}/missing-spacious"
export XDG_CACHE_HOME="${test_root}/xdg-cache"
: >"${CTX_FAKE_BAZEL_LOG}"
"${wrapper}" info output_base 2>"${test_root}/xdg-config.log"
grep -Fq "cache=${XDG_CACHE_HOME}/ctx/bazel/bazel-7.4.1" "${test_root}/xdg-config.log" \
  || fail 'XDG cache fallback was not selected'

printf 'bazelw tests passed\n'
