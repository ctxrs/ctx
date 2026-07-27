#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${repo_root}"

base_ref="${1:-origin/main}"
cache_root="${CTX_BAZEL_DIFF_CACHE_ROOT:-${XDG_CACHE_HOME:-${HOME}/.cache}/ctx/bazel-diff}"
base_sha="$(git rev-parse --verify "${base_ref}^{commit}")"
base_worktree="${cache_root}/base-worktree"
hash_dir="${cache_root}/hashes"
mkdir -p "${cache_root}" "${hash_dir}"

fail_closed() {
  printf '//:presubmit\n' >"${hash_dir}/selected-tests.txt"
  printf 'bazel-diff failed; selecting //:presubmit\n' >&2
  if [[ "${CTX_AFFECTED_DRY_RUN:-0}" != "1" ]]; then
    scripts/bazelw test //:presubmit --config=test
  fi
}
trap fail_closed ERR

if [[ ! -e "${base_worktree}/.git" ]]; then
  git worktree add --detach "${base_worktree}" "${base_sha}"
else
  git -C "${base_worktree}" checkout --detach "${base_sha}"
fi

starting="${hash_dir}/${base_sha}.json"
final="${hash_dir}/working-tree.json"
impacted="${hash_dir}/impacted.txt"
filtered_impacted="${hash_dir}/filtered-impacted.txt"
changed="${hash_dir}/changed.txt"
selected="${hash_dir}/selected-tests.txt"

# Omitting --modified-filepaths is intentional: both graphs receive complete
# content hashing. The cached, detached base worktree is never a developer tree.
if [[ ! -s "${starting}" ]]; then
  scripts/bazelw run //:bazel-diff -- generate-hashes \
    --workspacePath="${base_worktree}" \
    --bazelPath="${repo_root}/scripts/bazelw" \
    --alwaysAffectedTags=non-rust-action \
    "${starting}"
fi
scripts/bazelw run //:bazel-diff -- generate-hashes \
  --workspacePath="${repo_root}" \
  --bazelPath="${repo_root}/scripts/bazelw" \
  --alwaysAffectedTags=non-rust-action \
  "${final}"
scripts/bazelw run //:bazel-diff -- get-impacted-targets \
  --workspacePath="${repo_root}" \
  --startingHashes="${starting}" \
  --finalHashes="${final}" \
  --output="${impacted}"

# bazel-diff deliberately reports every affected graph node. Let Bazel reduce
# that set to executable tests/test suites and exclude explicitly manual or
# external-harness targets before the fail-closed policy layer runs.
if [[ -s "${impacted}" ]]; then
  impacted_set="$(tr '\n' ' ' <"${impacted}")"
  scripts/bazelw query \
    "kind(\".*(_test|test_suite) rule\", set(${impacted_set})) except attr(\"tags\", \".*(manual|external-harness).*\", set(${impacted_set}))" \
    --output=label >"${filtered_impacted}"
else
  : >"${filtered_impacted}"
fi

{
  git diff --name-only "${base_sha}" --
  git ls-files --others --exclude-standard
} | sort -u >"${changed}"

python3 tools/bazel/select_affected_tests.py "${changed}" "${filtered_impacted}" "${selected}"
cat "${selected}"

if [[ "${CTX_AFFECTED_DRY_RUN:-0}" != "1" ]] && [[ -s "${selected}" ]]; then
  mapfile -t tests <"${selected}"
  scripts/bazelw test "${tests[@]}" --config=test
fi

trap - ERR
