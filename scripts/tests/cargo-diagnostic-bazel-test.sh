#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi

test_root="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/ctx-cargo-diagnostic-bazel-test.XXXXXXXX")"
trap 'rm -rf -- "${test_root}"' EXIT
mkdir -p "${test_root}/scripts/tests/fixtures"
cp "${source_root}/scripts/cargo-diagnostic.sh" "${test_root}/scripts/cargo-diagnostic.sh"
cp "${source_root}/scripts/tests/cargo-diagnostic-test.sh" "${test_root}/scripts/tests/cargo-diagnostic-test.sh"
cp "${source_root}/scripts/tests/fixtures/fake-cargo.sh" "${test_root}/scripts/tests/fixtures/fake-cargo.sh"
chmod +x \
  "${test_root}/scripts/cargo-diagnostic.sh" \
  "${test_root}/scripts/tests/cargo-diagnostic-test.sh" \
  "${test_root}/scripts/tests/fixtures/fake-cargo.sh"

"${test_root}/scripts/tests/cargo-diagnostic-test.sh"
