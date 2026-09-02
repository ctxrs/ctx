#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${repo_root}"
# shellcheck source=scripts/ci-common.sh
source "${repo_root}/scripts/ci-common.sh"

base_ref="${1:-origin/main}"
if ! base_sha="$(git rev-parse --verify "${base_ref}^{commit}")"; then
  printf 'could not resolve affected-test base %s; selecting default CI tests\n' "${base_ref}" >&2
  printf '//...\n'
  if [[ "${CTX_AFFECTED_DRY_RUN:-0}" != "1" ]]; then
    scripts/bazelw test //... --config=test \
      --test_tag_filters=-manual,-tier-nightly,-tier-release
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
  local impacted_set label
  local -a impacted_labels=() selected_labels=()

  {
    git diff --name-only "${base_sha}" --
    git ls-files --others --exclude-standard
  } | sort -u >"${changed}" || return 1
  if [[ ! -s "${changed}" ]]; then
    : >"${selected}"
    return 0
  fi
  if grep -Eq \
    '(^|/)(BUILD|BUILD\.bazel|MODULE\.bazel|MODULE\.bazel\.lock|WORKSPACE|WORKSPACE\.bazel|Cargo\.lock|Cargo\.toml|\.bazelignore|\.bazelrc|\.bazelversion|[^/]+\.bzl)$|^scripts/(bazel-affected\.sh|bazelw|ci-common\.sh)$|^tools/bazel/' \
    "${changed}"; then
    return 12
  fi

  # Omitting --modified-filepaths is intentional: both graphs receive complete
  # content hashing. Base hashes are published atomically by commit; concurrent
  # cold runs may duplicate computation but never share a mutable worktree.
  if [[ ! -s "${starting}" ]]; then
    base_worktree_registered=1
    git worktree add --detach "${base_worktree}" "${base_sha}" >/dev/null || return 1
    scripts/bazelw run //:bazel-diff -- generate-hashes \
      --workspacePath="${base_worktree}" \
      --bazelPath="${repo_root}/scripts/bazelw" \
      --excludeExternalTargets \
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
    --excludeExternalTargets \
    --alwaysAffectedTags=non-rust-action \
    "${final}" || return 1
  scripts/bazelw run //:bazel-diff -- get-impacted-targets \
    --workspacePath="${repo_root}" \
    --bazelPath="${repo_root}/scripts/bazelw" \
    --startingHashes="${starting}" \
    --finalHashes="${final}" \
    --excludeExternalTargets \
    --output="${impacted}" || return 1

  # bazel-diff is the graph authority, but never interpolate its output until
  # each label is syntactically safe. tests() expands suites; kind() leaves
  # only executable test rules, and tags remain Bazel's authority.
  while IFS= read -r label || [[ -n "${label}" ]]; do
    [[ -z "${label}" ]] && continue
    if [[ ! "${label}" =~ ^//[A-Za-z0-9_@.+,=~/-]*:[A-Za-z0-9_@.+,=~/-]+$ ]]; then
      printf 'bazel-diff emitted an invalid affected label: %s\n' "${label}" >&2
      return 23
    fi
    impacted_labels+=("${label}")
  done <"${impacted}"
  (( ${#impacted_labels[@]} > 0 )) || return 24
  impacted_set="$(printf '%s\n' "${impacted_labels[@]}" | sort -u | paste -sd ' ')"

  local test_query="kind(\".*_test rule\", tests(set(${impacted_set})))"
  scripts/bazelw query \
    "${test_query} except attr(\"tags\", \".*(advisory|external|flaky-repetition|manual|network|no-cache|platform-native|release|requires-local-history|requires-signing|requires-vm|stress|tier-nightly|tier-release).*\", ${test_query})" \
    --output=label >"${filtered_impacted}" || return 25

  while IFS= read -r label || [[ -n "${label}" ]]; do
    [[ -z "${label}" ]] && continue
    if [[ ! "${label}" =~ ^//[A-Za-z0-9_@.+,=~/-]*:[A-Za-z0-9_@.+,=~/-]+$ ]]; then
      printf 'Bazel query emitted an invalid selected label: %s\n' "${label}" >&2
      return 26
    fi
    selected_labels+=("${label}")
  done <"${filtered_impacted}"
  (( ${#selected_labels[@]} > 0 )) || return 24
  printf '%s\n' "${selected_labels[@]}" | sort -u >"${selected}"
}

fail_closed() {
  local reason="$1"
  printf '//...\n' >"${selected}"
  printf 'affected test selection failed closed to //...: %s\n' "${reason}" >&2
}

selection_status=0
generate_selection || selection_status=$?
case "${selection_status}" in
  0) ;;
  12) fail_closed 'build configuration changed' ;;
  23|26) fail_closed 'received an invalid Bazel label' ;;
  24) fail_closed 'changed files have no eligible routine tests' ;;
  25) fail_closed 'Bazel query failed' ;;
  *) fail_closed "bazel-diff failed (status ${selection_status})" ;;
esac

cat "${selected}"
if [[ "${CTX_AFFECTED_DRY_RUN:-0}" != "1" ]] && [[ -s "${selected}" ]]; then
  tests=()
  while IFS= read -r test_label; do
    tests+=("${test_label}")
  done <"${selected}"
  scripts/bazelw test "${tests[@]}" --config=test \
    --test_tag_filters=-manual,-tier-nightly,-tier-release
fi
