#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-daemon-runtime-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-daemon-runtime-boundary.XXXXXX")"
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

workspace_members="${tmp}/workspace-members.txt"
sed -n '/^members = \[/,/^\]/p' "${repo_root}/Cargo.toml" \
  | sed -nE 's/^[[:space:]]*"(crates\/[^"[:space:]]+)",?[[:space:]]*$/\1/p' \
  | LC_ALL=C sort -u >"${workspace_members}"
if [[ ! -s "${workspace_members}" ]]; then
  echo 'Cargo workspace crate inventory is empty or unreadable' >&2
  exit 1
fi

visible_manifests="${tmp}/visible-manifests.txt"
find "${repo_root}/crates" -mindepth 2 -maxdepth 2 -name Cargo.toml -type f -printf '%h\n' \
  | sed "s#^${repo_root}/##" \
  | LC_ALL=C sort -u >"${visible_manifests}"
if ! diff -u "${workspace_members}" "${visible_manifests}"; then
  echo 'boundary runfiles do not expose the complete Cargo workspace crate inventory' >&2
  exit 1
fi
while IFS= read -r member; do
  if [[ ! -f "${repo_root}/${member}/BUILD.bazel" ]]; then
    echo "boundary runfiles omit the Bazel BUILD graph for ${member}" >&2
    exit 1
  fi
done <"${workspace_members}"

expected_internal="${tmp}/expected-internal.txt"
printf '%s\n' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-platform:lib' >"${expected_internal}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-runtime:lib)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-internal.txt"
if ! diff -u "${expected_internal}" "${tmp}/actual-internal.txt"; then
  echo 'unexpected internal dependency closure for ctx-daemon-runtime' >&2
  exit 1
fi

if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-daemon-runtime:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-daemon-runtime' >&2
  exit 1
fi

# Keep production and qualification consumers as independent exact inventories
# so a consumer cannot migrate between variants without failing this check.
expected_reverse_lib="${tmp}/expected-reverse-lib.txt"
printf '%s\n' \
  '//crates/ctx-cli-presentation:lib' \
  '//crates/ctx-cli-presentation:qualification_lib' \
  '//crates/ctx-cli-presentation:test_support_lib' \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-application:lib' \
  '//crates/ctx-daemon-application:test_support_lib' \
  '//crates/ctx-daemon-cli:lib' \
  '//crates/ctx-daemon-cli:qualification_test_support_lib' \
  '//crates/ctx-daemon-cli:test_support_lib' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-daemon-service:lib' \
  '//crates/ctx-daemon-service:test_support_lib' \
  '//crates/ctx-history-cli:lib' \
  '//crates/ctx-history-cli:test_support_lib' >"${expected_reverse_lib}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-runtime:lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-runtime:lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-lib.txt"
if ! diff -u "${expected_reverse_lib}" "${tmp}/actual-reverse-lib.txt"; then
  echo 'unexpected reverse production consumer of ctx-daemon-runtime' >&2
  exit 1
fi

expected_reverse_qualification="${tmp}/expected-reverse-qualification.txt"
printf '%s\n' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-application:qualification_lib' \
  '//crates/ctx-daemon-cli:qualification_lib' \
  '//crates/ctx-daemon-runtime:qualification_lib' \
  '//crates/ctx-daemon-service:qualification_lib' >"${expected_reverse_qualification}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-runtime:qualification_lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-runtime:qualification_lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-qualification.txt"
if ! diff -u "${expected_reverse_qualification}" "${tmp}/actual-reverse-qualification.txt"; then
  echo 'unexpected reverse qualification consumer of ctx-daemon-runtime' >&2
  exit 1
fi

runtime_root="${repo_root}/crates/ctx-daemon-runtime"
actual_internal_cargo="${tmp}/actual-internal-cargo.txt"
grep -E '^[[:space:]]*ctx-[[:alnum:]-]+[[:space:]]*=' "${runtime_root}/Cargo.toml" \
  | sed -E 's/^[[:space:]]*([^[:space:]]+).*/\1/' \
  | LC_ALL=C sort -u >"${actual_internal_cargo}"
printf '%s\n' 'ctx-history-core' 'ctx-history-platform' >"${tmp}/expected-internal-cargo.txt"
if ! diff -u "${tmp}/expected-internal-cargo.txt" "${actual_internal_cargo}"; then
  echo 'unexpected internal Cargo dependency for ctx-daemon-runtime' >&2
  exit 1
fi

actual_reverse_cargo="${tmp}/actual-reverse-cargo.txt"
while IFS= read -r manifest; do
  if [[ "${manifest}" != "${runtime_root}/Cargo.toml" ]] && grep -q 'ctx-daemon-runtime' "${manifest}"; then
    printf '%s\n' "${manifest#${repo_root}/}"
  fi
done < <(find "${repo_root}/crates" -mindepth 2 -maxdepth 2 -name Cargo.toml -type f | LC_ALL=C sort) \
  >"${actual_reverse_cargo}"
printf '%s\n' \
  'crates/ctx-daemon-application/Cargo.toml' \
  'crates/ctx-daemon-cli/Cargo.toml' \
  'crates/ctx-daemon-service/Cargo.toml' >"${tmp}/expected-reverse-cargo.txt"
if ! diff -u "${tmp}/expected-reverse-cargo.txt" "${actual_reverse_cargo}"; then
  echo 'unexpected reverse Cargo consumer of ctx-daemon-runtime' >&2
  exit 1
fi

if grep -REn --include='*.rs' \
  'ctx_(history_capture|history_index|history_refresh|pro_host_protocol|semantic_index|semantic_model|upgrade_engine)::|crate::(analytics|output|semantic|ui)::|(^|[^[:alnum:]_])clap::|AppConfig' \
  "${runtime_root}/src"; then
  echo 'product policy or composition dependency leaked into ctx-daemon-runtime' >&2
  exit 1
fi

printf 'ctx-daemon-runtime dependency and composition boundary ok: workspace_crates=%s\n' \
  "$(wc -l <"${workspace_members}")"
