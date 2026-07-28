#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
cd -- "${repo_root}"

bazel build //crates/ctx-history-capture:unit_tests

test_binary="bazel-bin/crates/ctx-history-capture/unit_tests"
if [[ ! -x "${test_binary}" ]]; then
  printf 'source-root safety test binary is not executable: %s\n' "${test_binary}" >&2
  exit 2
fi

"${test_binary}" \
  source_root_safety \
  --nocapture \
  --test-threads=1
