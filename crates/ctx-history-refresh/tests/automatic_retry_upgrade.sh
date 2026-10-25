#!/usr/bin/env bash
set -euo pipefail

export CTX_TEST_RETRY_UPGRADE_ROOT="${TEST_TMPDIR}/retry-upgrade"
mkdir -p "${CTX_TEST_RETRY_UPGRADE_ROOT}"
test_name=engine::tests::additional::build_upgrade::automatic_retry_across_builds

run_phase() {
  local binary="$1" phase="$2" version="$3" output
  output="$(CTX_TEST_RETRY_UPGRADE_PHASE="$phase" CTX_TEST_RETRY_UPGRADE_VERSION="$version" \
    "$binary" --ignored --exact "$test_name" --nocapture)"
  printf '%s\n' "$output"
  [[ "$output" == *"1 passed; 0 failed"* ]]
}

run_phase "$1" pause 2.2.8
run_phase "$1" same-build 2.2.8
run_phase "$2" upgrade 2.3.0
