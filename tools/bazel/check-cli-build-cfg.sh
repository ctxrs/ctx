#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNFILES_DIR:-}" ]]; then
  root="${RUNFILES_DIR}/_main"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  root="${TEST_SRCDIR}/_main"
else
  root="${BUILD_WORKSPACE_DIRECTORY:-$(cd "$(dirname "$0")/../.." && pwd)}"
fi
build_rs="${root}/crates/ctx-cli/build.rs"
bazel_cfg="${root}/crates/ctx-cli/test_targets.bzl"

for cfg in ctx_cli_bazel_test; do
  grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${build_rs}" >/dev/null
  grep -F -- "--check-cfg=cfg(${cfg})" "${bazel_cfg}" >/dev/null
  grep -F -- "--cfg=${cfg}" "${bazel_cfg}" >/dev/null
done

printf 'ctx-cli build.rs/native Bazel cfg parity ok\n'
