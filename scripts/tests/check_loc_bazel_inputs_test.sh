#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" ]]; then
  input_root="${TEST_SRCDIR}/${TEST_WORKSPACE:-_main}"
else
  input_root="$(git rev-parse --show-toplevel)"
fi

assert_reaches_gate() {
  local path="$1"
  local bytes
  [[ -f "${input_root}/${path}" ]] || {
    printf 'LOC Bazel input contract failed: missing runfile %s\n' "${path}" >&2
    exit 1
  }
  bytes="$(wc -c < "${input_root}/${path}" | tr -d '[:space:]')"
  ((bytes > 0)) || {
    printf 'LOC Bazel input contract failed: empty runfile %s\n' "${path}" >&2
    exit 1
  }
}

assert_reaches_gate crates/ctx-history-capture/src/provider/source_backed/driver.rs
assert_reaches_gate tools/bazel/release_routes_test.bzl

printf 'LOC Bazel input contract passed (Rust and nested Starlark runfiles present).\n'
