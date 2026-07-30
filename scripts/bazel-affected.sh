#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${repo_root}"
# shellcheck source=scripts/ci-common.sh
source "${repo_root}/scripts/ci-common.sh"

base_ref="${1:-origin/main}"
if ! base_sha="$(git rev-parse --verify "${base_ref}^{commit}")"; then
  printf 'could not resolve affected-test base %s; selecting //:ci\n' "${base_ref}" >&2
  printf '//:ci\n'
  if [[ "${CTX_AFFECTED_DRY_RUN:-0}" != "1" ]]; then
    scripts/bazelw test //:ci --config=test
  fi
  exit 0
fi
bazel_version="$(ctx_bazel_version)"
cache_root="${CTX_BAZEL_DIFF_CACHE_ROOT:-$(ctx_bazel_cache_root)/bazel-${bazel_version}/bazel-diff}"
hash_dir="${cache_root}/hashes"
run_root="${cache_root}/runs"
mkdir -p "${hash_dir}" "${run_root}"
run_dir="$(mktemp -d "${run_root}/run.XXXXXXXX")"
base_worktree="${run_dir}/base-worktree"
starting="${hash_dir}/${base_sha}.json"
starting_tmp="${run_dir}/${base_sha}.json"
final="${run_dir}/working-tree.json"
impacted="${run_dir}/impacted.txt"
filtered_impacted="${run_dir}/filtered-impacted.txt"
changed="${run_dir}/changed.txt"
selected="${run_dir}/selected-tests.txt"
base_worktree_registered=0

cleanup() {
  if (( base_worktree_registered == 1 )); then
    (
      cd "${base_worktree}"
      CTX_BAZEL_WORKSPACE="${base_worktree}" "${repo_root}/scripts/bazelw" shutdown
    ) >/dev/null 2>&1 || true
    git worktree remove --force "${base_worktree}" >/dev/null 2>&1 || true
  fi
  if [[ "${run_dir}" == "${run_root}"/run.* ]]; then
    rm -rf -- "${run_dir}"
  fi
}
trap cleanup EXIT

generate_selection() {
  local impacted_set

  # Omitting --modified-filepaths is intentional: both graphs receive complete
  # content hashing. Base hashes are published atomically by commit; concurrent
  # cold runs may duplicate computation but never share a mutable worktree.
  if [[ ! -s "${starting}" ]]; then
    base_worktree_registered=1
    git worktree add --detach "${base_worktree}" "${base_sha}" >/dev/null || return 1
    scripts/bazelw run //:bazel-diff -- generate-hashes \
      --workspacePath="${base_worktree}" \
      --bazelPath="${repo_root}/scripts/bazelw" \
      --alwaysAffectedTags=non-rust-action \
      "${starting_tmp}" || return 1
    if [[ ! -s "${starting_tmp}" ]]; then
      printf 'bazel-diff produced an empty base hash file\n' >&2
      return 1
    fi
    mv -f "${starting_tmp}" "${starting}" || return 1
  fi

  scripts/bazelw run //:bazel-diff -- generate-hashes \
    --workspacePath="${repo_root}" \
    --bazelPath="${repo_root}/scripts/bazelw" \
    --alwaysAffectedTags=non-rust-action \
    "${final}" || return 1
  scripts/bazelw run //:bazel-diff -- get-impacted-targets \
    --workspacePath="${repo_root}" \
    --bazelPath="${repo_root}/scripts/bazelw" \
    --startingHashes="${starting}" \
    --finalHashes="${final}" \
    --output="${impacted}" || return 1

  # bazel-diff deliberately reports every affected graph node. Let Bazel reduce
  # that set to executable tests/test suites and keep manual, network,
  # external-harness, and release targets out of routine affected runs.
  if [[ -s "${impacted}" ]]; then
    impacted_set="$(tr '\n' ' ' <"${impacted}")"
    scripts/bazelw query \
      "kind(\".*(_test|test_suite) rule\", set(${impacted_set})) except attr(\"tags\", \".*(advisory|external|flaky-repetition|manual|network|no-cache|platform-native|release|requires-local-history|requires-signing|requires-vm|stress).*\", set(${impacted_set}))" \
      --output=label >"${filtered_impacted}" || return 1
  else
    : >"${filtered_impacted}"
  fi

  {
    git diff --name-only "${base_sha}" --
    git ls-files --others --exclude-standard
  } | sort -u >"${changed}" || return 1

  python3 tools/bazel/select_affected_tests.py "${changed}" "${filtered_impacted}" "${selected}" || return 1
}

fail_closed() {
  printf '//:ci\n' >"${selected}"
  printf 'bazel-diff failed; selecting //:ci\n' >&2
}

if ! generate_selection; then
  fail_closed
fi

cat "${selected}"
if [[ "${CTX_AFFECTED_DRY_RUN:-0}" != "1" ]] && [[ -s "${selected}" ]]; then
  tests=()
  while IFS= read -r test_label; do
    tests+=("${test_label}")
  done <"${selected}"
  scripts/bazelw test "${tests[@]}" --config=test
fi
