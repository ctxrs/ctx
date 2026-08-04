#!/usr/bin/env bash
set -euo pipefail

source_commit="1f400fc7ac37e87c107e7665e8decccc8c30fe91"
expected_fingerprint="c5ad8c7bce69d5fd3f12d3b57e8e49403233db4a74f91882ed649a2bb117b19a"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(git -C "${script_dir}" rev-parse --show-toplevel)"
staging="$(mktemp -d "${TMPDIR:-/tmp}/ctx-predecessor-fixture.XXXXXX")"
trap 'rm -rf -- "${staging}"' EXIT

snapshot="${staging}/snapshot"
mkdir -- "${snapshot}"
git -C "${repo_root}" archive "${source_commit}" | tar -x -C "${snapshot}"
mkdir -p -- "${snapshot}/crates/ctx-history-index/examples"
cp -- \
  "${script_dir}/predecessor_fixture_generator.rs" \
  "${snapshot}/crates/ctx-history-index/examples/predecessor_fixture_generator.rs"

CTX_PREDECESSOR_SOURCE_COMMIT="${source_commit}" \
CARGO_TARGET_DIR="${staging}/target" \
  cargo run \
    --quiet \
    --manifest-path "${snapshot}/Cargo.toml" \
    -p ctx-history-index \
    --example predecessor_fixture_generator \
    -- "${staging}/generated"

actual_fingerprint="$(
  sed -n \
    's/.*"core_record_contract_fingerprint": "\([0-9a-f]*\)".*/\1/p' \
    "${staging}/generated/PROVENANCE.json"
)"
[[ "${actual_fingerprint}" == "${expected_fingerprint}" ]]

rm -rf -- "${script_dir}/index"
rm -f -- "${script_dir}/PROVENANCE.json"
mv -- "${staging}/generated/index" "${script_dir}/index"
mv -- "${staging}/generated/PROVENANCE.json" "${script_dir}/PROVENANCE.json"
