#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-upgrade-engine-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-upgrade-engine-boundary.XXXXXX")"
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

for target in lib test_support_lib qualification_lib; do
  expected="${tmp}/${target}-expected.txt"
  printf '%s\n' \
    '//crates/ctx-companion-bridge:lib' \
    '//crates/ctx-history-core:lib' \
    '//crates/ctx-history-platform:lib' \
    '//crates/ctx-managed-pair-engine:lib' \
    '//crates/ctx-terminal:lib' \
    "//crates/ctx-upgrade-engine:${target}" >"${expected}"
  query "kind(\"rust_library rule\", deps(//crates/ctx-upgrade-engine:${target})) intersect //crates/..." \
    | LC_ALL=C sort -u >"${tmp}/${target}-actual.txt"
  if ! diff -u "${expected}" "${tmp}/${target}-actual.txt"; then
    echo "unexpected internal dependency closure for ctx-upgrade-engine:${target}" >&2
    exit 1
  fi
done

for forbidden in \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-history-index:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-semantic-model:lib'; do
  if [[ -n "$(query "somepath(//crates/ctx-upgrade-engine:lib, ${forbidden})")" ]]; then
    echo "ctx-upgrade-engine has forbidden Bazel dependency path to ${forbidden}" >&2
    exit 1
  fi
done
if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-upgrade-engine:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-upgrade-engine' >&2
  exit 1
fi

engine_root="${repo_root}/crates/ctx-upgrade-engine"
managed_pair_root="${repo_root}/crates/ctx-managed-pair-engine"
if grep -En 'ctx-(cli|history-index|history-refresh|semantic)|(^|[^[:alnum:]_-])(clap|ureq)([^[:alnum:]_-]|$)' \
  "${engine_root}/Cargo.toml" "${managed_pair_root}/Cargo.toml"; then
  echo 'forbidden Cargo dependency in ctx-upgrade-engine' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_(history_index|history_refresh|semantic)|crate::(analytics|net|output|semantic|ui|process_environment)::|(^|[^[:alnum:]_])(clap|ureq)::' \
  "${engine_root}/src" "${managed_pair_root}/src"; then
  echo 'forbidden source dependency in ctx-upgrade-engine' >&2
  exit 1
fi
if grep -REn --include='*.rs' 'env!\("CARGO_PKG_VERSION"\)' \
  "${engine_root}/src" "${managed_pair_root}/src"; then
  echo 'package identity leaked into ctx-upgrade-engine product behavior' >&2
  exit 1
fi

transport_impls="$(grep -REl --include='*.rs' 'impl[[:space:]]+ReleaseTransport[[:space:]]+for' \
  "${repo_root}/crates/ctx-cli/src" \
  "${repo_root}/crates/ctx-cli-presentation/src" | LC_ALL=C sort -u)"
if [[ "${transport_impls}" != "${repo_root}/crates/ctx-cli/src/upgrade/ports.rs" ]]; then
  echo 'ctx-cli upgrade ports must be the sole production ReleaseTransport implementation' >&2
  printf '%s\n' "${transport_impls}" >&2
  exit 1
fi

printf 'ctx-upgrade-engine dependency and composition boundary ok\n'
