#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-read-application-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-history-read-application-boundary.XXXXXX")"
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

history_cli_search="${repo_root}/crates/ctx-history-cli/src/source_index/search.rs"
history_cli_locate="${repo_root}/crates/ctx-history-cli/src/source_index/locate.rs"
history_cli_show="${repo_root}/crates/ctx-history-cli/src/source_index/show.rs"
history_cli_show_mcp="${repo_root}/crates/ctx-history-cli/src/source_index/show/mcp.rs"
history_cli_list_events="${repo_root}/crates/ctx-history-cli/src/list_events.rs"
history_cli_query_consumers=(
  "${history_cli_search}"
  "${history_cli_locate}"
  "${history_cli_show}"
  "${history_cli_show_mcp}"
  "${history_cli_list_events}"
)
for consumer in "${history_cli_query_consumers[@]}"; do
  if [[ ! -f "${consumer}" ]]; then
    echo "expected history CLI query consumer is missing: ${consumer#"${repo_root}/"}" >&2
    exit 1
  fi
done

check_contract_inventory() {
  local expected="$1"
  query 'kind("rust_test rule", //crates/ctx-history-read-application:all)' | LC_ALL=C sort -u >"${tmp}/actual-contracts.txt"
  if ! diff -u "${expected}" "${tmp}/actual-contracts.txt"; then
    echo 'ctx-history-read-application final-binary contract inventory drifted' >&2
    exit 1
  fi

  query 'kind("rust_test rule", //crates/ctx-history-read-application:all) intersect attr("data", ".*ctx-cli:ctx.*", //crates/ctx-history-read-application:all)' \
    | LC_ALL=C sort -u >"${tmp}/actual-binary-data-contracts.txt"
  sed '/:unit_tests$/d' "${expected}" >"${tmp}/expected-binary-data-contracts.txt"
  if ! diff -u "${tmp}/expected-binary-data-contracts.txt" "${tmp}/actual-binary-data-contracts.txt"; then
    echo 'ctx-history-read-application contracts must receive final ctx only as data' >&2
    exit 1
  fi

  if [[ -n "$(query 'kind("rust_test rule", //crates/ctx-history-read-application:all) intersect attr("deps", ".*ctx-cli:ctx.*", //crates/ctx-history-read-application:all)')" ]]; then
    echo 'ctx-history-read-application contracts must not compile against final ctx' >&2
    exit 1
  fi
  if [[ -n "$(query 'kind("rust_test rule", //crates/ctx-history-read-application:all) intersect attr("deps", ".*//crates/(ctx-cli|ctx-cli-contract-tests|ctx-cli-presentation)(:|/).*", //crates/ctx-history-read-application:all)')" ]]; then
    echo 'ctx-history-read-application contracts retain a ctx or shared-support Rust backedge' >&2
    exit 1
  fi
  if [[ -n "$(query 'kind("rust_test rule", //crates/ctx-history-read-application:all) intersect attr("deps", ".*//crates/ctx-[^:]*shared[^:]*(:|/).*", //crates/ctx-history-read-application:all)')" ]]; then
    echo 'ctx-history-read-application contracts retain a ctx or shared-support Rust backedge' >&2
    exit 1
  fi
}

expected_direct="${tmp}/expected-direct.txt"
printf '%s\n' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-index-format:lib' \
  '//crates/ctx-history-index-query:lib' \
  '//crates/ctx-history-read-application:lib' >"${expected_direct}"
query 'kind("rust_library rule", deps(//crates/ctx-history-read-application:lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-direct.txt"
if ! diff -u "${expected_direct}" "${tmp}/actual-direct.txt"; then
  echo 'ctx-history-read-application direct Bazel dependency inventory drifted' >&2
  exit 1
fi

manifest="${repo_root}/crates/ctx-history-read-application/Cargo.toml"
sed -n '/^\[dependencies\]$/,/^\[/p' "${manifest}" \
  | grep -E '^[[:space:]]*ctx-[[:alnum:]-]+[[:space:]]*=' \
  | sed -E 's/^[[:space:]]*([^[:space:]]+).*/\1/' \
  | LC_ALL=C sort -u >"${tmp}/actual-cargo.txt"
printf '%s\n' \
  'ctx-history-core' \
  'ctx-history-index-format' \
  'ctx-history-index-query' >"${tmp}/expected-cargo.txt"
if ! diff -u "${tmp}/expected-cargo.txt" "${tmp}/actual-cargo.txt"; then
  echo 'ctx-history-read-application direct Cargo dependency inventory drifted' >&2
  exit 1
fi

for forbidden in \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-cli:ctx'; do
  if [[ -n "$(query "somepath(//crates/ctx-history-read-application:lib, ${forbidden})")" ]]; then
    echo "ctx-history-read-application has forbidden Bazel dependency path to ${forbidden}" >&2
    exit 1
  fi
done
if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-history-read-application:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-history-read-application' >&2
  exit 1
fi

printf '%s\n' \
  '//crates/ctx-history-read-application:search_refresh_tests' \
  '//crates/ctx-history-read-application:search_show_tests' \
  '//crates/ctx-history-read-application:search_source_identity_filters_tests' \
  '//crates/ctx-history-read-application:unit_tests' >"${tmp}/expected-contracts.txt"
check_contract_inventory "${tmp}/expected-contracts.txt"

query_root="${repo_root}/crates/ctx-history-read-application"
if grep -En 'ctx-(history-capture|history-refresh|semantic-index|cli)|clap' "${manifest}"; then
  echo 'forbidden runtime, writer, or transport dependency in ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_history_(capture|refresh)::|ctx_semantic_index::|crate::(config|daemon|output|ui)::' \
  "${query_root}/src"; then
  echo 'forbidden source dependency in ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'std::env|std::process|process::Command|Command::new|CODEX_THREAD_ID|CaptureProvider::Codex' \
  "${query_root}/src"; then
  echo 'environment, process, or caller identity leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'SearchRefreshMode|RefreshArg|semantic_daemon|DaemonConfig|SearchConfig|SourceBackedRefresh' \
  "${query_root}/src"; then
  echo 'daemon, configuration, or refresh lifecycle interpretation leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'clap::|shell_quote|--[[:alnum:]][[:alnum:]-]*|ctx (search|show|setup|doctor)' \
  "${query_root}/src"; then
  echo 'transport or presentation behavior leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '#\[path[[:space:]]*=|include!|include_str!|include_bytes!' \
  "${query_root}/src"; then
  echo 'ctx-history-read-application source must remain package-local' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'dyn[[:space:]]+(GenerationReadPort|HistorySemanticPort)' \
  "${query_root}/src"; then
  echo 'history read application ports must use static dispatch' >&2
  exit 1
fi
for authority in \
  'execute_search(_observed)?' \
  execute_locate \
  execute_show_event \
  execute_show_session_page \
  execute_show_session_stream \
  execute_list_events_page \
  execute_list_events_stream; do
  if [[ "$(grep -REh --include='*.rs' "^pub fn ${authority}<" "${query_root}/src" | wc -l)" -ne 1 ]]; then
    echo "${authority} must have one production application authority" >&2
    exit 1
  fi
done
if grep -Eq 'PinnedHistoryQuery|\.search\(' \
  "${history_cli_search}" \
  || grep -Eq 'PinnedHistoryQuery|\.locate\(' \
    "${history_cli_locate}"; then
  echo 'ctx-history-cli bypasses the application-owned search or locate authority' >&2
  exit 1
fi
if grep -Eq 'PinnedHistoryQuery|\.show_(event|session|session_page)\(' \
  "${history_cli_show}" \
  "${history_cli_show_mcp}" \
  || grep -Eq 'PinnedHistoryQuery|\.list_events(_page)?\(' \
    "${history_cli_list_events}"; then
  echo 'ctx-history-cli bypasses the application-owned show or list authority' >&2
  exit 1
fi
for contract in \
  'EVENT_QUERY_PAGE_ITEMS: usize = 100' \
  'EVENT_QUERY_PAGE_BYTES: usize = 1024 \* 1024' \
  'MAX_EVENT_QUERY_LIMIT: u64 = 10_000_000' \
  'MAX_EVENT_QUERY_CURSOR_CHARS: usize = 512' \
  'SHOW_SESSION_PAGE_ITEMS: usize = 200'; do
  if ! grep -REq --include='*.rs' "${contract}" "${query_root}/src"; then
    echo "history read application budget contract drifted: ${contract}" >&2
    exit 1
  fi
done
if [[ -e "${repo_root}/crates/ctx-history-query" ]]; then
  echo 'legacy ctx-history-query production authority still exists' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '/home/[[:alnum:]_.-]+|/Users/[[:alnum:]_.-]+|ctx-private|ctx-multi-repo-workspace|\.ctx/worktrees' \
  "${query_root}/src"; then
  echo 'private host or workspace path leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '[Ww]ork [Rr]ecorder|ctx publish|ctx evidence|ctx link-pr|ctx context|ctx uninstall|auto[_-]update|CTX_UPDATE|provider-live|completion-certificate|dashboard export|upsert_github|write[_-]shim' \
  "${query_root}/src"; then
  echo 'retired product or legacy control surface leaked into ctx-history-read-application' >&2
  exit 1
fi

printf 'ctx-history-read-application dependency, contract, and locality boundary ok\n'
