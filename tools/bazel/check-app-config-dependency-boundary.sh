#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-app-config-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-app-config-boundary.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT
mkdir -p "${tmp}/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="${tmp}/home" \
    BAZEL_OUTPUT_USER_ROOT="${tmp}/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="${tmp}/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="${repo_root}" \
    "${repo_root}/scripts/bazelw" query "$1" --output=label
}

for target in lib test_support_lib; do
  semantic_target=lib
  [[ "${target}" == test_support_lib ]] && semantic_target=test_support_lib
  printf '%s\n' \
    "//crates/ctx-app-config:${target}" \
    '//crates/ctx-history-capture:lib' \
    '//crates/ctx-history-core:lib' \
    '//crates/ctx-history-platform:lib' \
    "//crates/ctx-semantic-model:${semantic_target}" \
    | LC_ALL=C sort -u >"${tmp}/expected"
  query "kind(\"rust_library rule\", deps(//crates/ctx-app-config:${target}, 1)) intersect //crates/..." \
    | LC_ALL=C sort -u >"${tmp}/actual"
  diff -u "${tmp}/expected" "${tmp}/actual" || {
    echo "unexpected direct internal dependencies for ctx-app-config:${target}" >&2
    exit 1
  }
done

forbidden='set(
  //crates/ctx-agent-application:lib //crates/ctx-agent-integrations:lib
  //crates/ctx-cli:ctx //crates/ctx-cli-presentation:lib //crates/ctx-client-observability:lib
  //crates/ctx-companion-bridge:lib //crates/ctx-daemon-cli:lib
  //crates/ctx-daemon-application:lib //crates/ctx-daemon-runtime:lib //crates/ctx-daemon-service:lib
  //crates/ctx-history-cli:lib //crates/ctx-history-ingest-application:lib
  //crates/ctx-history-read-application:lib //crates/ctx-history-refresh:lib
  //crates/ctx-history-refresh-execution:lib //crates/ctx-protocol:lib //crates/ctx-sdk:lib
  //crates/ctx-semantic-index:lib //crates/ctx-terminal:lib
)'
if [[ -n "$(query "deps(//crates/ctx-app-config:lib) intersect ${forbidden}")" ]]; then
  echo 'ctx-app-config has a forbidden upward Bazel dependency path' >&2
  exit 1
fi

for consumer in '//crates/ctx-cli:ctx' '//crates/ctx-daemon-cli:lib'; do
  if [[ -z "$(query "somepath(${consumer}, //crates/ctx-app-config:lib)")" ]]; then
    echo "${consumer} has no downward Bazel dependency path to ctx-app-config" >&2
    exit 1
  fi
done

crate_root="${repo_root}/crates/ctx-app-config"
if grep -En 'ctx-(agent|cli|client|companion|daemon|protocol|sdk|terminal)[[:alnum:]_-]*|ctx-history-(cli|ingest|read|refresh)[[:alnum:]_-]*|ctx-semantic-index|(^|[^[:alnum:]_-])(clap|ureq)([^[:alnum:]_-]|$)' \
  "${crate_root}/Cargo.toml"; then
  echo 'upward product, application, runtime, or presentation dependency leaked into ctx-app-config' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_(agent|cli|client|companion|daemon|protocol|sdk|terminal)[[:alnum:]_]*::|ctx_history_(cli|ingest|read|refresh)[[:alnum:]_]*::|ctx_semantic_index::|(^|[^[:alnum:]_])(clap|ureq)::' \
  "${crate_root}/src"; then
  echo 'upward product, application, runtime, or presentation authority leaked into ctx-app-config source' >&2
  exit 1
fi

mapfile -t parser_definitions < <(grep -REl --include='*.rs' \
  'pub fn parse_capture_provider_name[[:space:]]*[(]' "${repo_root}/crates" | LC_ALL=C sort)
expected_parser="${repo_root}/crates/ctx-history-core/src/source.rs"
if [[ "${parser_definitions[*]}" != "${expected_parser}" ]]; then
  echo 'capture provider-name parser must be owned exactly by ctx-history-core/src/source.rs' >&2
  exit 1
fi

printf 'ctx-app-config dependency, parser ownership, and bounded-consumer boundary ok\n'
