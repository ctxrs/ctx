#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: rust-target-inventory-live-test.sh ROOT_BUILD INVENTORY CHECKER" >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
inventory="$(readlink -f "$2")"
checker="$(readlink -f "$3")"
repo_root="$(dirname "$root_build")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-rust-target-inventory.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="$tmp/home" \
    BAZEL_OUTPUT_USER_ROOT="$tmp/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="$tmp/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="$repo_root" \
    "$repo_root/scripts/bazelw" query "$@"
}

query 'kind("(rust_binary|rust_library|rust_proc_macro|rust_test|ctx_rust_binary|ctx_rust_test|ctx_cli_integration_test) rule", //...)' \
  --output=label_kind | LC_ALL=C sort -u >"$tmp/live-labels.txt"

python3 - "$inventory" >"$tmp/production-labels.txt" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1], encoding="utf-8"))
labels = {
    variant["label"]
    for package in inventory["packages"].values()
    for variants in package["production_targets"].values()
    for variant in variants
    if variant["kind"] == "rust"
}
for package in inventory["packages"].values():
    labels.update(package["targets"][key] for key in package["test_only_targets"])
    for proof_labels in package["test_only_feature_targets"].values():
        labels.update(proof_labels)
print(*sorted(labels), sep="\n")
PY

: >"$tmp/live-builds.txt"
while IFS= read -r label; do
  printf '@@LABEL\t%s\n' "$label" >>"$tmp/live-builds.txt"
  query "$label" --output=build >>"$tmp/live-builds.txt"
  printf '@@END\n' >>"$tmp/live-builds.txt"
done <"$tmp/production-labels.txt"

python3 "$checker" \
  "$inventory" \
  "$repo_root/Cargo.toml" \
  --live-labels "$tmp/live-labels.txt" \
  --live-builds "$tmp/live-builds.txt"
