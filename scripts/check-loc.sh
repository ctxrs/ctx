#!/usr/bin/env bash
set -euo pipefail

SOURCE_LIMIT=1000
TEST_LIMIT=1500
EXCEPTIONS_FILE="${CTX_LOC_EXCEPTIONS_FILE:-scripts/check-loc-exceptions.tsv}"

fail() {
  printf 'loc gate failed: %s\n' "$*" >&2
  exit 1
}

find_repo_root() {
  local root
  root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
  if [[ -n "${root}" ]]; then
    cd "${root}"
    return 0
  fi

  local script_path
  local script_root
  script_path="$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null || true)"
  if [[ -n "${script_path}" ]]; then
    script_root="$(cd "$(dirname "${script_path}")/.." && pwd)"
    root="$(cd "${script_root}" && git rev-parse --show-toplevel 2>/dev/null || true)"
    if [[ -n "${root}" ]]; then
      cd "${root}"
      return 0
    fi
  fi

  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  if [[ -d "${root}/.git" || -f "${root}/Cargo.toml" ]]; then
    cd "${root}"
    return 0
  fi

  fail 'could not locate repo root'
}

is_non_source_path() {
  local path="$1"
  local base="${path##*/}"

  case "${path}" in
    docs/*|*/docs/*|contracts/*/fixtures/*|*/fixtures/*|fixtures/*|*/fixture/*|fixture/*)
      return 0
      ;;
    data/*|*/data/*|generated/*|*/generated/*|gen/*|*/gen/*)
      return 0
      ;;
    Cargo.lock|*/Cargo.lock|package-lock.json|*/package-lock.json|MODULE.bazel.lock|*.lock)
      return 0
      ;;
  esac

  case "${base}" in
    README|README.*|SECURITY.md|LICENSE|NOTICE|CHANGELOG|CHANGELOG.*)
      return 0
      ;;
    *.md|*.markdown|*.rst|*.txt|*.json|*.jsonl|*.yaml|*.yml|*.toml)
      return 0
      ;;
  esac

  return 1
}

is_counted_source_file() {
  local path="$1"
  local base="${path##*/}"

  case "${base}" in
    BUILD|BUILD.bazel|WORKSPACE|WORKSPACE.bazel|MODULE.bazel)
      return 0
      ;;
    *.bzl|*.rs|*.sh|*.bash|*.py|*.js|*.jsx|*.mjs|*.cjs|*.ts|*.tsx|*.swift|*.go|*.java|*.cs)
      return 0
      ;;
  esac

  return 1
}

classify_kind() {
  local path="$1"
  local base="${path##*/}"

  if is_non_source_path "${path}" || ! is_counted_source_file "${path}"; then
    return 1
  fi

  case "${path}" in
    tests/*|*/tests/*|Tests/*|*/Tests/*|src/test/*|*/src/test/*)
      printf 'test\n'
      return 0
      ;;
  esac

  case "${base}" in
    *_test.rs|*_tests.rs|tests.rs|*_test.go|*.test.ts|*.test.tsx|*.test.js|*.test.jsx|*Tests.swift)
      printf 'test\n'
      return 0
      ;;
  esac

  printf 'source\n'
}

line_limit_for_kind() {
  case "$1" in
    source) printf '%s\n' "${SOURCE_LIMIT}" ;;
    test) printf '%s\n' "${TEST_LIMIT}" ;;
    *) fail "internal error: unknown LOC kind '$1'" ;;
  esac
}

line_count() {
  wc -l < "$1" | tr -d '[:space:]'
}

find_repo_root

declare -A repo_file
while IFS= read -r path; do
  repo_file["${path}"]=1
done < <(git ls-files --cached --others --exclude-standard)

declare -A exception_max
declare -A exception_kind
declare -A exception_reason

if [[ -f "${EXCEPTIONS_FILE}" ]]; then
  line_no=0
  while IFS=$'\t' read -r path max_lines kind reason review_after extra; do
    line_no=$((line_no + 1))
    if [[ "${line_no}" -eq 1 && "${path}" == "path" ]]; then
      continue
    fi
    if [[ -z "${path}" && -z "${max_lines}" && -z "${kind}" && -z "${reason}" && -z "${review_after}" ]]; then
      continue
    fi
    if [[ -n "${extra:-}" ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: expected 5 tab-separated columns"
    fi
    if [[ -z "${path}" || -z "${max_lines}" || -z "${kind}" || -z "${reason}" || -z "${review_after}" ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: path, max_lines, kind, reason, and review_after are required"
    fi
    if [[ "${path}" == *'*'* || "${path}" == *'?'* || "${path}" == *'['* || "${path}" == *']'* ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: exception paths must be exact, not globs: ${path}"
    fi
    if [[ ! "${max_lines}" =~ ^[1-9][0-9]*$ ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: max_lines must be a positive integer"
    fi
    if [[ "${kind}" != "source" && "${kind}" != "test" ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: kind must be source or test"
    fi
    if [[ ! "${review_after}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: review_after must be YYYY-MM-DD"
    fi
    if [[ -n "${exception_max[${path}]:-}" ]]; then
      fail "${EXCEPTIONS_FILE}:${line_no}: duplicate exception for ${path}"
    fi

    exception_max["${path}"]="${max_lines}"
    exception_kind["${path}"]="${kind}"
    exception_reason["${path}"]="${reason}"
  done < "${EXCEPTIONS_FILE}"
fi

violations_tmp="$(mktemp)"
stale_tmp="$(mktemp)"
invalid_tmp="$(mktemp)"
trap 'rm -f "${violations_tmp}" "${stale_tmp}" "${invalid_tmp}"' EXIT

for path in "${!exception_max[@]}"; do
  if [[ -z "${repo_file[${path}]:-}" ]]; then
    printf '%s\t%s\n' "${path}" "exception path is not tracked or untracked in the worktree" >> "${invalid_tmp}"
    continue
  fi

  actual_kind="$(classify_kind "${path}" || true)"
  if [[ -z "${actual_kind}" ]]; then
    printf '%s\t%s\n' "${path}" "exception path is not a counted source/test file" >> "${invalid_tmp}"
    continue
  fi

  if [[ "${actual_kind}" != "${exception_kind[${path}]}" ]]; then
    printf '%s\t%s\n' "${path}" "exception kind is ${exception_kind[${path}]}, actual kind is ${actual_kind}" >> "${invalid_tmp}"
  fi
done

while IFS= read -r path; do
  if [[ ! -f "${path}" ]]; then
    continue
  fi

  kind="$(classify_kind "${path}" || true)"
  if [[ -z "${kind}" ]]; then
    continue
  fi

  lines="$(line_count "${path}")"
  limit="$(line_limit_for_kind "${kind}")"

  if [[ -n "${exception_max[${path}]:-}" ]]; then
    max_lines="${exception_max[${path}]}"
    if (( lines <= limit )); then
      printf '%s\t%s\t%s\t%s\n' "${path}" "${kind}" "${lines}" "${limit}" >> "${stale_tmp}"
    elif (( lines > max_lines )); then
      excess=$((lines - max_lines))
      printf '%09d\t%s\t%s\t%s\t%s\t%s\t%s\n' "${excess}" "${path}" "${kind}" "${lines}" "${max_lines}" "exception-max" "${exception_reason[${path}]}" >> "${violations_tmp}"
    fi
    continue
  fi

  if (( lines > limit )); then
    excess=$((lines - limit))
    printf '%09d\t%s\t%s\t%s\t%s\t%s\t-\n' "${excess}" "${path}" "${kind}" "${lines}" "${limit}" "limit" >> "${violations_tmp}"
  fi
done < <(git ls-files --cached --others --exclude-standard)

if [[ -s "${invalid_tmp}" ]]; then
  printf 'LOC exception file has invalid entries:\n' >&2
  sort "${invalid_tmp}" | awk -F '\t' '{printf "  %s: %s\n", $1, $2}' >&2
  exit 1
fi

if [[ -s "${stale_tmp}" ]]; then
  printf 'LOC exceptions are stale; remove entries now under the normal limit:\n' >&2
  sort "${stale_tmp}" | awk -F '\t' '{printf "  %s (%s): %s lines <= %s limit\n", $1, $2, $3, $4}' >&2
  exit 1
fi

if [[ -s "${violations_tmp}" ]]; then
  printf 'LOC gate failed; hard limits are source=%s lines, test=%s lines.\n' "${SOURCE_LIMIT}" "${TEST_LIMIT}" >&2
  printf 'Largest excess first:\n' >&2
  sort -r "${violations_tmp}" | awk -F '\t' '{
    excess = $1 + 0
    if ($6 == "exception-max") {
      printf "  %s (%s): %s lines > exception max %s (+%s); %s\n", $2, $3, $4, $5, excess, $7
    } else {
      printf "  %s (%s): %s lines > limit %s (+%s)\n", $2, $3, $4, $5, excess
    }
  }' >&2
  exit 1
fi

printf 'LOC gate passed (source <= %s, test <= %s; tracked and untracked non-ignored files from git ls-files).\n' "${SOURCE_LIMIT}" "${TEST_LIMIT}"
