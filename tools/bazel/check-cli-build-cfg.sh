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

for cfg in ctx_semantic_fastembed ctx_sqlite_vec; do
  grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${build_rs}" >/dev/null
  grep -F -- "--check-cfg=cfg(${cfg})" "${bazel_cfg}" >/dev/null
  grep -F -- "--cfg=${cfg}" "${bazel_cfg}" >/dev/null
done

for triple in \
  aarch64-apple-darwin \
  aarch64-unknown-linux-gnu \
  x86_64-apple-darwin \
  x86_64-pc-windows-msvc \
  x86_64-unknown-linux-gnu; do
  grep -F "${triple}" "${bazel_cfg}" >/dev/null
done

for platform_clause in \
  '("linux", "x86_64" | "aarch64")' \
  '("macos", "x86_64" | "aarch64")' \
  '("windows", "x86_64")'; do
  grep -F "${platform_clause}" "${build_rs}" >/dev/null
done

printf 'ctx-cli build.rs/native Bazel cfg parity ok\n'
