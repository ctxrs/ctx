#!/usr/bin/env bash
set -euo pipefail

{
  printf 'jobs=%s\n' "${CARGO_BUILD_JOBS:-}"
  printf 'threads=%s\n' "${RUST_TEST_THREADS:-}"
  printf 'target=%s\n' "${CARGO_TARGET_DIR:-}"
  printf 'dev_debug=%s\n' "${CARGO_PROFILE_DEV_DEBUG:-}"
  printf 'test_debug=%s\n' "${CARGO_PROFILE_TEST_DEBUG:-}"
  printf 'args='
  printf '%q ' "$@"
  printf '\n'
} >"${CTX_CARGO_DIAGNOSTIC_TEST_CAPTURE:?}"
