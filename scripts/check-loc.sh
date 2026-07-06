#!/usr/bin/env bash
set -euo pipefail

source_limit="${CTX_SOURCE_LOC_LIMIT:-650}"
test_limit="${CTX_TEST_LOC_LIMIT:-650}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

is_code_file() {
  local path="$1"
  case "${path}" in
    BUILD.bazel|MODULE.bazel) return 0 ;;
    *.rs|*.swift|*.cs|*.go|*.py|*.js|*.ts|*.java|*.kt|*.kts|*.sh|*.ps1|*.bazel|*.bzl) return 0 ;;
    *) return 1 ;;
  esac
}

is_excluded_file() {
  local path="$1"
  case "${path}" in
    .git/*|target/*|*/target/*|bazel-*/*) return 0 ;;
    Cargo.lock|MODULE.bazel.lock) return 0 ;;
    docs/*|contracts/*|plugins/*/skills/*|skills/*) return 0 ;;
    *.json|*.jsonl|*.png|*.md|LICENSE|README.md|SECURITY.md) return 0 ;;
    *) return 1 ;;
  esac
}

is_test_file() {
  local path="$1"
  case "${path}" in
    */tests/*|*/Tests/*|*/test/*|*/examples/*|*/Examples/*) return 0 ;;
    *_test.go|*.test.js|*.test.ts|*Test.java|*Tests.swift|*Tests.cs|*test_*.py) return 0 ;;
    *) return 1 ;;
  esac
}

tracked_files() {
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git ls-files --cached --others --exclude-standard
    return 0
  fi

  find . -type f \
    -not -path './.git/*' \
    -not -path './target/*' \
    -not -path '*/target/*' \
    -not -path './bazel-*/*' \
    | sed 's#^\./##'
}

failures=0

while IFS= read -r path; do
  [[ -n "${path}" ]] || continue
  is_code_file "${path}" || continue
  is_excluded_file "${path}" && continue
  [[ -f "${path}" ]] || continue

  loc="$(wc -l <"${path}")"
  if is_test_file "${path}"; then
    kind="test"
    limit="${test_limit}"
  else
    kind="source"
    limit="${source_limit}"
  fi

  if (( loc > limit )); then
    printf '%s LOC limit exceeded: %s has %d lines (limit %d)\n' \
      "${kind}" "${path}" "${loc}" "${limit}" >&2
    failures=$((failures + 1))
  fi
done < <(tracked_files)

if (( failures > 0 )); then
  printf 'LOC gate failed with %d oversized file(s)\n' "${failures}" >&2
  exit 1
fi

printf 'LOC gate ok: source <= %d lines, tests/examples <= %d lines\n' \
  "${source_limit}" "${test_limit}"
