#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-semantic-model-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-semantic-model-boundary.XXXXXX")"
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
  expected_internal="${tmp}/${target}-expected-internal.txt"
  printf '%s\n' \
    '//crates/ctx-history-core:lib' \
    "//crates/ctx-semantic-model:${target}" >"${expected_internal}"
  query "kind(\"rust_library rule\", deps(//crates/ctx-semantic-model:${target})) intersect //crates/..." \
    | LC_ALL=C sort -u >"${tmp}/${target}-internal.txt"
  if ! diff -u "${expected_internal}" "${tmp}/${target}-internal.txt"; then
    echo "unexpected internal dependency closure for ctx-semantic-model:${target}" >&2
    exit 1
  fi
done

if [[ -n "$(query 'somepath(//crates/ctx-semantic-model:lib, //crates/ctx-history-index:lib)')" ]]; then
  echo 'ctx-semantic-model must not depend on ctx-history-index' >&2
  exit 1
fi
if [[ -z "$(query 'somepath(//crates/ctx-history-index:lib, //crates/ctx-semantic-model:lib)')" ]]; then
  echo 'ctx-history-index must consume the ctx-semantic-model contract' >&2
  exit 1
fi

if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-semantic-model:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-semantic-model' >&2
  exit 1
fi

model_root="${repo_root}/crates/ctx-semantic-model"
if grep -En 'ctx-(semantic-index|history-index|history-refresh|cli)' "${model_root}/Cargo.toml"; then
  echo 'forbidden Cargo dependency in ctx-semantic-model' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_semantic_index::|ctx_history_index::|ctx_history_refresh::|crate::semantic::|crate::output::|crate::net::' \
  "${model_root}/src"; then
  echo 'forbidden source dependency in ctx-semantic-model' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'SemanticBackgroundOperation|IndexBatch|semantic_background_resource_deferred|SEMANTIC_INDEX_MIN_AVAILABLE' \
  "${model_root}/src"; then
  echo 'daemon/index admission policy leaked into ctx-semantic-model' >&2
  exit 1
fi

ambient_scan="${tmp}/ambient-model-source.txt"
while IFS= read -r source; do
  case "${source}" in
    *_tests.rs|*/tests.rs|*/tests/*) continue ;;
  esac
  if [[ "${source}" == */model_runtime/onnx.rs ]]; then
    sed '/^#\[cfg(test)\]$/,$d' "${source}" >>"${ambient_scan}"
  else
    cat "${source}" >>"${ambient_scan}"
  fi
done < <(find "${model_root}/src" -type f -name '*.rs' | LC_ALL=C sort)
if grep -En \
  'std::env|env::(var|var_os|current_dir|current_exe)|default_data_root' \
  "${ambient_scan}"; then
  echo 'ambient product path/environment authority leaked into ctx-semantic-model' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '(^|[[:space:]])mod[[:space:]]+(query_adapter|query_service|vector_store|indexing)[[:space:]]*;|(query_adapter|query_service|vector_store)::|semantic::indexing' \
  "${model_root}/src"; then
  echo 'CLI/indexing module leaked into ctx-semantic-model' >&2
  exit 1
fi

fetch_impls="$(grep -REl --include='*.rs' 'impl[[:space:]]+ArtifactFetcher[[:space:]]+for' "${repo_root}/crates/ctx-cli/src" | LC_ALL=C sort -u)"
if [[ "${fetch_impls}" != "${repo_root}/crates/ctx-cli/src/semantic/daemon_worker.rs" ]]; then
  echo 'the CLI daemon worker must be the sole ArtifactFetcher implementation' >&2
  printf '%s\n' "${fetch_impls}" >&2
  exit 1
fi
if [[ "$(grep -RE --include='*.rs' -c '\.acquire_for_daemon\(' "${repo_root}/crates/ctx-cli/src" | awk -F: '{ total += $2 } END { print total + 0 }')" -ne 1 ]]; then
  echo 'ctx-cli must have exactly one production daemon acquisition call' >&2
  exit 1
fi

printf 'ctx-semantic-model dependency and fetch-capability boundary ok\n'
