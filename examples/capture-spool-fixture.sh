#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_root="${CTX_EXAMPLE_TMPDIR:-${repo_root}/target/tmp}"
mkdir -p "${tmp_root}"
data_root="${CTX_EXAMPLE_DATA_ROOT:-$(mktemp -d "${tmp_root}/ctx-work-record-capture.XXXXXX")}"
export CTX_DATA_ROOT="${data_root}"

run_ctx() {
  if [[ -n "${CTX_BIN:-}" ]]; then
    "${CTX_BIN}" "$@"
  else
    cargo run -q -p ctx -- "$@"
  fi
}

echo "CTX_DATA_ROOT=${CTX_DATA_ROOT}"

run_ctx setup
run_ctx capture write-fixture \
  --title "fixture capture import" \
  --body "Fixture envelope imported from the local capture spool." \
  --tag capture \
  --tag fixture \
  --dedupe-key "example:capture-spool-fixture" \
  --cwd "${repo_root}"

run_ctx status
run_ctx capture import --json
run_ctx search capture
run_ctx validate
