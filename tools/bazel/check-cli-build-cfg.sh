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
  x86_64-unknown-freebsd \
  x86_64-unknown-linux-gnu; do
  awk -v selector="@rules_rust//rust/platform:${triple}" '
    index($0, selector) {
      selected = 1
      active = 1
    }
    active && index($0, "--cfg=ctx_semantic_fastembed") {
      fastembed = 1
    }
    active && index($0, "--cfg=ctx_sqlite_vec") {
      sqlite_vec = 1
    }
    active && $0 == "    ]," {
      active = 0
    }
    END {
      exit !(selected && fastembed && sqlite_vec)
    }
  ' "${bazel_cfg}"
done

for platform_clause in \
  '("linux", "x86_64" | "aarch64")' \
  '("macos", "x86_64" | "aarch64")' \
  '("windows", "x86_64")' \
  '("freebsd", "x86_64")'; do
  grep -F "${platform_clause}" "${build_rs}" >/dev/null
done

printf 'ctx-cli build.rs/native Bazel cfg parity ok\n'
