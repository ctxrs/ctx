#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNFILES_DIR:-}" ]]; then
  root="${RUNFILES_DIR}/_main"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  root="${TEST_SRCDIR}/_main"
else
  root="${BUILD_WORKSPACE_DIRECTORY:-$(cd "$(dirname "$0")/../.." && pwd)}"
fi

model_build_rs="${root}/crates/ctx-semantic-model/build.rs"
model_build="${root}/crates/ctx-semantic-model/BUILD.bazel"
model_manifest="${root}/crates/ctx-semantic-model/Cargo.toml"
cli_build_rs="${root}/crates/ctx-cli/build.rs"
cli_targets="${root}/crates/ctx-cli/test_targets.bzl"

grep -F 'cargo:rustc-check-cfg=cfg(ctx_semantic_fastembed)' "${model_build_rs}" >/dev/null
grep -F -- '--check-cfg=cfg(ctx_semantic_fastembed)' "${model_build}" >/dev/null

for selector in \
  '@rules_rust//rust/platform:aarch64-apple-darwin' \
  '@rules_rust//rust/platform:aarch64-unknown-linux-gnu' \
  '@rules_rust//rust/platform:x86_64-apple-darwin' \
  '@rules_rust//rust/platform:x86_64-pc-windows-msvc' \
  '//tools/bazel/platforms:x86_64-pc-windows-gnu' \
  '@rules_rust//rust/platform:x86_64-unknown-freebsd' \
  '@rules_rust//rust/platform:x86_64-unknown-linux-gnu' \
  '@rules_rust//rust/platform:x86_64-unknown-nixos-gnu'; do
  awk -v selector="${selector}" '
    index($0, selector) { selected = 1; active = 1 }
    active && index($0, "--cfg=ctx_semantic_fastembed") { fastembed = 1 }
    active && $0 == "    ]," { active = 0 }
    END { exit !(selected && fastembed) }
  ' "${model_build}"
done

for platform_clause in \
  '("linux", "x86_64" | "aarch64")' \
  '("macos", "x86_64" | "aarch64")' \
  '("windows", "x86_64")' \
  '("freebsd", "x86_64")'; do
  grep -F "${platform_clause}" "${model_build_rs}" >/dev/null
done

grep -F "target.'cfg(any(all(target_os = \"linux\", any(target_arch = \"x86_64\", target_arch = \"aarch64\"), target_env = \"gnu\"), all(target_os = \"macos\", any(target_arch = \"x86_64\", target_arch = \"aarch64\")), all(target_os = \"windows\", target_arch = \"x86_64\"), all(target_os = \"freebsd\", target_arch = \"x86_64\")))'.dependencies" "${model_manifest}" >/dev/null

if grep -F 'ctx_semantic_fastembed' "${cli_build_rs}" "${cli_targets}" >/dev/null; then
  echo 'ctx_semantic_fastembed authority leaked back into ctx-cli' >&2
  exit 1
fi

printf 'ctx-semantic-model Cargo/Bazel cfg parity ok\n'
