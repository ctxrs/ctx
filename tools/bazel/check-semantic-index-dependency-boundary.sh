#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-semantic-index-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-semantic-index-boundary.XXXXXX")"
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

python3 "${repo_root}/tools/bazel/check_semantic_index_boundary.py" "${repo_root}"

for target in lib test_support_lib; do
  expected_internal="${tmp}/${target}-expected-internal.txt"
  printf '%s\n' \
    '//crates/ctx-history-capture-model:lib' \
    '//crates/ctx-history-core:lib' \
    '//crates/ctx-history-index-format:lib' \
    '//crates/ctx-history-index-generation:lib' \
    '//crates/ctx-history-index-query:lib' \
    '//crates/ctx-history-index:lib' \
    '//crates/ctx-history-platform:lib' \
    "//crates/ctx-semantic-index:${target}" \
    '//crates/ctx-semantic-model:lib' >"${expected_internal}"
  query "kind(\"rust_library rule\", deps(//crates/ctx-semantic-index:${target})) intersect //crates/..." \
    | LC_ALL=C sort -u >"${tmp}/${target}-internal.txt"
  if ! diff -u "${expected_internal}" "${tmp}/${target}-internal.txt"; then
    echo "unexpected internal dependency closure for ctx-semantic-index:${target}" >&2
    exit 1
  fi
done

write_expected_external() {
  # `url` constructs percent-correct immutable SQLite file URIs; no network
  # client is admitted into ctx-semantic-index.
  cat <<'EOF'
@crates//anyhow-1.0.103:anyhow-1.0.103
@crates//fs2-0.4.3:fs2-0.4.3
@crates//memmap2-0.9.11:memmap2-0.9.11
@crates//rusqlite-0.32.1:rusqlite-0.32.1
@crates//serde-1.0.228:serde-1.0.228
@crates//serde_json-1.0.150:serde_json-1.0.150
@crates//sha2-0.10.9:sha2-0.10.9
@crates//thiserror-1.0.69:thiserror-1.0.69
@crates//url-2.5.8:url-2.5.8
@crates//uuid-1.23.4:uuid-1.23.4
EOF
}

for target in lib test_support_lib unit_tests; do
  write_expected_external >"${tmp}/${target}-expected-external.txt"
  if [[ "${target}" == unit_tests ]]; then
    printf '%s\n' '@crates//tempfile-3.27.0:tempfile-3.27.0' \
      >>"${tmp}/${target}-expected-external.txt"
  fi
  LC_ALL=C sort -u -o "${tmp}/${target}-expected-external.txt" \
    "${tmp}/${target}-expected-external.txt"
  query "deps(//crates/ctx-semantic-index:${target}, 1)" \
    | grep '^@crates//' | LC_ALL=C sort -u >"${tmp}/${target}-external.txt"
  if ! diff -u "${tmp}/${target}-expected-external.txt" "${tmp}/${target}-external.txt"; then
    echo "Cargo/Bazel external dependency inventory drift for ctx-semantic-index:${target}" >&2
    exit 1
  fi
done

for forbidden in \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-cli:ctx'; do
  if [[ -n "$(query "somepath(//crates/ctx-semantic-index:lib, ${forbidden})")" ]]; then
    echo "ctx-semantic-index has forbidden Bazel dependency path to ${forbidden}" >&2
    exit 1
  fi
done
if [[ -n "$(query 'somepath(//crates/ctx-history-index:lib, //crates/ctx-semantic-index:lib)')" ]]; then
  echo 'ctx-history-index must not depend on ctx-semantic-index' >&2
  exit 1
fi
if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-semantic-index:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-semantic-index' >&2
  exit 1
fi
if [[ -z "$(query 'somepath(//crates/ctx-daemon-service:lib, //crates/ctx-semantic-index:lib)')" ]]; then
  echo 'ctx-daemon-service has no Bazel dependency path to ctx-semantic-index' >&2
  exit 1
fi

index_root="${repo_root}/crates/ctx-semantic-index"
if grep -En 'ctx-(history-refresh|cli)|fastembed|hf-hub|coreml-native|tokenizers|(^|[^a-z])ort([^a-z]|$)' \
  "${index_root}/Cargo.toml"; then
  echo 'forbidden runtime/composition dependency in ctx-semantic-index' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_history_refresh::|crate::semantic::|crate::output::|crate::net::' \
  "${index_root}/src"; then
  echo 'forbidden source dependency in ctx-semantic-index' >&2
  exit 1
fi

production_sources="${tmp}/production-sources.txt"
while IFS= read -r source; do
  case "${source}" in
    *_tests.rs|*/tests.rs|*/tests/*|*/test_support*.rs|*/test_support/*) continue ;;
  esac
  printf '%s\n' "${source}" >>"${production_sources}"
done < <(find "${index_root}/src" -type f -name '*.rs' | LC_ALL=C sort)

expected_source_labels() {
  local source relative
  while IFS= read -r source; do
    relative="${source#${index_root}/}"
    printf '%s\n' "//crates/ctx-semantic-index:${relative}"
  done <"$1"
}

all_sources="${tmp}/all-sources.txt"
find "${index_root}/src" -type f -name '*.rs' | LC_ALL=C sort >"${all_sources}"
for target in lib test_support_lib unit_tests; do
  expected_input="${production_sources}"
  if [[ "${target}" == unit_tests ]]; then
    expected_input="${all_sources}"
  fi
  expected_source_labels "${expected_input}" | LC_ALL=C sort -u \
    >"${tmp}/${target}-expected-sources.txt"
  query "filter(\"^//crates/ctx-semantic-index:\", kind(\"source file\", deps(//crates/ctx-semantic-index:${target}, 1)))" \
    | LC_ALL=C sort -u >"${tmp}/${target}-sources.txt"
  if ! diff -u "${tmp}/${target}-expected-sources.txt" "${tmp}/${target}-sources.txt"; then
    echo "Cargo/Bazel source inventory drift for ctx-semantic-index:${target}" >&2
    exit 1
  fi
done

expected_model_imports="${tmp}/expected-model-imports.txt"
printf '%s\n' \
  'SemanticModelContract' \
  'semantic_model_contract' | LC_ALL=C sort >"${expected_model_imports}"

actual_model_imports="${tmp}/actual-model-imports.txt"
fully_qualified_model_uses="${tmp}/fully-qualified-model-uses.txt"
while IFS= read -r source; do
  perl -0777 -e '
    my $source = shift;
    local $/;
    my $text = <>;
    while ($text =~ /use\s+ctx_semantic_model::\{(.*?)\};/sg) {
      my $group = $1;
      $group =~ s/\s//g;
      print "$_\n" for grep { length } split /,/, $group;
    }
    while ($text =~ /use\s+ctx_semantic_model::([A-Za-z_][A-Za-z0-9_]*)\s*;/g) {
      print "$1\n";
    }
    $text =~ s/use\s+ctx_semantic_model::\{.*?\};//sg;
    $text =~ s/use\s+ctx_semantic_model::[A-Za-z_][A-Za-z0-9_]*\s*;//g;
    print STDERR "$source\n" if $text =~ /ctx_semantic_model::/;
  ' "${source}" "${source}" >>"${actual_model_imports}" 2>>"${fully_qualified_model_uses}"
done <"${production_sources}"
LC_ALL=C sort -u -o "${actual_model_imports}" "${actual_model_imports}"

if ! diff -u "${expected_model_imports}" "${actual_model_imports}"; then
  echo 'ctx-semantic-index may consume only the frozen model contract/E5 projection API' >&2
  exit 1
fi
if [[ -s "${fully_qualified_model_uses}" ]]; then
  echo 'ctx-semantic-index contains non-allowlisted fully qualified model calls' >&2
  cat "${fully_qualified_model_uses}" >&2
  exit 1
fi
if xargs grep -En \
  'SharedSemanticRuntime|SemanticModelConfig|SemanticDaemon|ArtifactFetch|ArtifactFetcher|model_runtime|model_acquisition|embed_documents|embed_query|ensure_loaded|acquire_for_daemon|load_model' \
  <"${production_sources}"; then
  echo 'model loading, acquisition, or embedding execution leaked into ctx-semantic-index' >&2
  exit 1
fi
if xargs grep -En \
  'impl([[:space:]]|<[^>]+>)*SemanticBatchEmbedder' \
  <"${production_sources}"; then
  echo 'ctx-semantic-index must accept embeddings through its adapter, never implement one' >&2
  exit 1
fi

printf 'ctx-semantic-index dependency and no-model-execution boundary ok\n'
