#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-cargo-diagnostic-test.XXXXXX")"
trap 'find "${temp_root}" -depth -type f -delete; find "${temp_root}" -depth -type d -empty -delete' EXIT

fake_cargo="${repo_root}/scripts/tests/fixtures/fake-cargo.sh"
capture="${temp_root}/capture"

env -u CARGO_BUILD_JOBS -u RUST_TEST_THREADS -u CARGO_TARGET_DIR \
  -u CARGO_PROFILE_DEV_DEBUG -u CARGO_PROFILE_TEST_DEBUG \
  CARGO="${fake_cargo}" \
  CTX_CPU_COUNT=32 \
  CTX_TOTAL_MEMORY_GB=128 \
  CTX_CARGO_DIAGNOSTIC_TEST_CAPTURE="${capture}" \
  "${repo_root}/scripts/cargo-diagnostic.sh" test -p ctx-history-core >/dev/null

grep -Fx 'jobs=8' "${capture}"
grep -Fx 'threads=4' "${capture}"
grep -Fx "target=${repo_root}/target/cargo-diagnostic" "${capture}"
grep -Fx 'dev_debug=0' "${capture}"
grep -Fx 'test_debug=0' "${capture}"
grep -F 'args=test -p ctx-history-core ' "${capture}"

env -u CARGO_PROFILE_DEV_DEBUG -u CARGO_PROFILE_TEST_DEBUG \
  CARGO="${fake_cargo}" \
  CARGO_BUILD_JOBS=3 \
  RUST_TEST_THREADS=2 \
  CTX_CARGO_DIAGNOSTIC_DEBUG=1 \
  CTX_CARGO_DIAGNOSTIC_TEST_CAPTURE="${capture}" \
  "${repo_root}/scripts/cargo-diagnostic.sh" check >/dev/null

grep -Fx 'jobs=3' "${capture}"
grep -Fx 'threads=2' "${capture}"
grep -Fx 'dev_debug=' "${capture}"
grep -Fx 'test_debug=' "${capture}"

env -u CARGO_BUILD_JOBS -u RUST_TEST_THREADS \
  CARGO="${fake_cargo}" \
  CTX_CPU_COUNT=64 \
  CTX_TOTAL_MEMORY_GB=6 \
  CTX_CARGO_DIAGNOSTIC_TEST_CAPTURE="${capture}" \
  "${repo_root}/scripts/cargo-diagnostic.sh" check >/dev/null

grep -Fx 'jobs=2' "${capture}"
grep -Fx 'threads=2' "${capture}"

if CTX_CARGO_JOBS=invalid CARGO="${fake_cargo}" \
  CTX_CARGO_DIAGNOSTIC_TEST_CAPTURE="${capture}" \
  "${repo_root}/scripts/cargo-diagnostic.sh" check >/dev/null 2>&1; then
  printf 'invalid CTX_CARGO_JOBS unexpectedly succeeded\n' >&2
  exit 1
fi

printf 'cargo diagnostic wrapper tests passed\n'
