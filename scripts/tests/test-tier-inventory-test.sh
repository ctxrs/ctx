#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: test-tier-inventory-test.sh ROOT_BUILD CHECKER" >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
checker="$(readlink -f "$2")"
repo_root="$(dirname "$root_build")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-test-tier-inventory.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="$tmp/home" \
    BAZEL_OUTPUT_USER_ROOT="$tmp/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="$tmp/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="$repo_root" \
    "$repo_root/scripts/bazelw" query "$1" --output=label
}

query 'kind(".*_test rule", //...)' | LC_ALL=C sort -u >"$tmp/all-tests.txt"
query 'tests(//:release) intersect kind(".*_test rule", //...)' \
  | LC_ALL=C sort -u >"$tmp/release-tests.txt"
query 'attr("tags", "manual", kind(".*_test rule", //...))' \
  | LC_ALL=C sort -u >"$tmp/manual-tests.txt"

python3 "$checker" \
  --all-tests "$tmp/all-tests.txt" \
  --release-tests "$tmp/release-tests.txt" \
  --manual-tests "$tmp/manual-tests.txt"
