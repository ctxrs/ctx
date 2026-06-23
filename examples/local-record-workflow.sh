#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_root="${CTX_EXAMPLE_TMPDIR:-${repo_root}/target/tmp}"
mkdir -p "${tmp_root}"
data_root="${CTX_EXAMPLE_DATA_ROOT:-$(mktemp -d "${tmp_root}/ctx-work-record-example.XXXXXX")}"
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

record_json="$(run_ctx record \
  --title "dogfood local Work Recorder flow" \
  --body "Create a record, attach command evidence, search, render context, and export." \
  --tag dogfood \
  --tag local \
  --kind task \
  --workspace "${repo_root}" \
  --json)"

record_id="$(printf '%s\n' "${record_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p' | head -n 1)"
if [[ -z "${record_id}" ]]; then
  printf 'failed to read record id from ctx record output\n' >&2
  printf '%s\n' "${record_json}" >&2
  exit 1
fi

run_ctx evidence run --record "${record_id}" --timeout-seconds 30 rustc --version
run_ctx vcs inspect "${repo_root}" --json
run_ctx pr parse https://github.com/example/project/pull/42 --json
run_ctx link-pr "${record_id}" https://github.com/example/project/pull/42
run_ctx search dogfood
run_ctx context dogfood
run_ctx report --format json
run_ctx dashboard export --output "${data_root}/dashboard"
run_ctx export --output "${data_root}/work-records.json"
run_ctx validate

echo "archive=${data_root}/work-records.json"
echo "dashboard=${data_root}/dashboard/index.html"
