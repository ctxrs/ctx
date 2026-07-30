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

for cfg in ctx_semantic_fastembed; do
  grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${build_rs}" >/dev/null
  grep -F -- "--check-cfg=cfg(${cfg})" "${bazel_cfg}" >/dev/null
done

for cfg in ctx_cli_bazel_test; do
  grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${build_rs}" >/dev/null
  grep -F -- "--check-cfg=cfg(${cfg})" "${bazel_cfg}" >/dev/null
  grep -F -- "--cfg=${cfg}" "${bazel_cfg}" >/dev/null
done

for selector in \
  '@rules_rust//rust/platform:aarch64-apple-darwin' \
  '@rules_rust//rust/platform:aarch64-unknown-linux-gnu' \
  '@rules_rust//rust/platform:x86_64-apple-darwin' \
  '//tools/bazel/platforms:x86_64-pc-windows-gnu' \
  '@rules_rust//rust/platform:x86_64-pc-windows-msvc' \
  '@rules_rust//rust/platform:x86_64-unknown-freebsd' \
  '@rules_rust//rust/platform:x86_64-unknown-linux-gnu' \
  '@rules_rust//rust/platform:x86_64-unknown-nixos-gnu'; do
  awk -v selector="${selector}" '
    index($0, selector) {
      selected = 1
      active = 1
    }
    active && index($0, "--cfg=ctx_semantic_fastembed") {
      fastembed = 1
    }
    active && $0 == "    ]," {
      active = 0
    }
    END {
      exit !(selected && fastembed)
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
