#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

SOURCE_LIMIT=1000
TEST_LIMIT=1500
EXCEPTIONS_FILE="${CTX_LOC_EXCEPTIONS_FILE:-scripts/check-loc-exceptions.tsv}"
CURRENT_DATE="${CTX_LOC_TODAY:-$(date -u +%F)}"
SCHEMA_HEADER=$'path\tkind\tceiling\tdisposition\towner\trationale\texit_or_review\treview_by'

fail() {
  printf 'loc gate failed: %s\n' "$*" >&2
  exit 1
}

resolve_script_path() {
  local path target
  path="${BASH_SOURCE[0]}"
  while [[ -L "${path}" ]]; do
    target="$(readlink "${path}")" || return 1
    case "${target}" in
      /*) path="${target}" ;;
      *) path="$(dirname "${path}")/${target}" ;;
    esac
  done
  printf '%s/%s\n' "$(cd -P "$(dirname "${path}")" && pwd)" "$(basename "${path}")"
}

find_repo_root() {
  local root script_path script_root
  root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
  if [[ -n "${root}" ]]; then
    cd "${root}"
    return 0
  fi

  script_path="$(resolve_script_path 2>/dev/null || true)"
  if [[ -n "${script_path}" ]]; then
    script_root="$(cd "$(dirname "${script_path}")/.." && pwd)"
    root="$(cd "${script_root}" && git rev-parse --show-toplevel 2>/dev/null || true)"
    if [[ -n "${root}" ]]; then
      cd "${root}"
      return 0
    fi
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
    tests/*|*/tests/*|Tests/*|*/Tests/*|__tests__/*|*/__tests__/*|src/test/*|*/src/test/*|test_support/*|*/test_support/*)
      printf 'test\n'
      return 0
      ;;
  esac

  case "${base}" in
    *_test.rs|*_tests.rs|tests.rs|test_support*|*_test.go|*.test.ts|*.test.tsx|*.test.js|*.test.jsx|*Tests.swift)
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

is_real_iso_date() {
  local value="$1"
  local year month day max_day
  [[ "${value}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || return 1

  year=$((10#${value:0:4}))
  month=$((10#${value:5:2}))
  day=$((10#${value:8:2}))
  ((year >= 1 && month >= 1 && month <= 12 && day >= 1)) || return 1

  case "${month}" in
    1|3|5|7|8|10|12) max_day=31 ;;
    4|6|9|11) max_day=30 ;;
    2)
      max_day=28
      if ((year % 400 == 0 || (year % 4 == 0 && year % 100 != 0))); then
        max_day=29
      fi
      ;;
  esac
  ((day <= max_day))
}

is_exact_repo_path() {
  case "$1" in
    ''|/*|.|..|./*|../*|*/|*/.|*/..|*/./*|*/../*|*//*|*\\*|*'*'*|*'?'*|*'['*|*']'*|*$'\t'*|*$'\n'*|*$'\r'*)
      return 1
      ;;
  esac
  return 0
}

has_outer_whitespace() {
  [[ "$1" =~ ^[[:space:]] || "$1" =~ [[:space:]]$ ]]
}

is_placeholder_metadata() {
  local value="$1"
  local lower
  lower="$(printf '%s' "${value}" | tr '[:upper:]' '[:lower:]')"
  case "${lower}" in
    ''|-|n/a|none|todo|tbd|unknown|later|to\ be\ determined)
      return 0
      ;;
  esac
  return 1
}

has_temporary_exit_trigger() {
  local value="$1"
  local lower
  lower="$(printf '%s' "${value}" | tr '[:upper:]' '[:lower:]')"
  [[ "${lower}" =~ (^|[^[:alpha:]])(after|once|when)([^[:alpha:]]|$) ]]
}

has_specific_split_harm() {
  local value="$1"
  local lower
  lower="$(printf '%s' "${value}" | tr '[:upper:]' '[:lower:]')"
  [[ "${lower}" == *split* ]] || return 1
  [[ "${lower}" =~ (coupl|state|transaction|contract|chronolog) ]]
}

find_repo_root
is_real_iso_date "${CURRENT_DATE}" || fail "current date must be a real YYYY-MM-DD date: ${CURRENT_DATE}"
[[ -f "${EXCEPTIONS_FILE}" ]] || fail "exception registry does not exist: ${EXCEPTIONS_FILE}"

declare -A tracked_file
while IFS= read -r path; do
  tracked_file["${path}"]=1
done < <(git ls-files --cached)

# Registry schema (eight tab-separated columns):
# path, kind, ceiling, disposition, owner, rationale, exit_or_review, review_by.
# Disposition is temporary or cohesive. Owners use team:<slug> or
# component:<slug>; individual names are intentionally not accepted. Temporary
# exits contain after/once/when; cohesive rationales discuss splitting and its
# coupling, state, transaction, contract, or chronology cost. Ceiling is exact
# current physical LOC and review_by is a real, non-expired ISO date. Comment
# lines are '#' or begin '# '.
declare -A exception_ceiling
declare -A exception_kind
declare -A exception_rationale

header_seen=0
line_no=0
while IFS= read -r row || [[ -n "${row}" ]]; do
  line_no=$((line_no + 1))
  [[ -z "${row}" || "${row}" == \# || "${row}" == \#\ * ]] && continue

  if ((header_seen == 0)); then
    [[ "${row}" == "${SCHEMA_HEADER}" ]] || fail "${EXCEPTIONS_FILE}:${line_no}: expected exact schema header: ${SCHEMA_HEADER}"
    header_seen=1
    continue
  fi

  if [[ "${row}" == "${SCHEMA_HEADER}" ]]; then
    fail "${EXCEPTIONS_FILE}:${line_no}: duplicate schema header"
  fi

  row_without_tabs="${row//$'\t'/}"
  tab_count=$((${#row} - ${#row_without_tabs}))
  if ((tab_count != 7)); then
    fail "${EXCEPTIONS_FILE}:${line_no}: expected 8 tab-separated columns"
  fi
  if [[ "${row}" == $'\t'* || "${row}" == *$'\t\t'* || "${row}" == *$'\t' ]]; then
    fail "${EXCEPTIONS_FILE}:${line_no}: all 8 columns are required"
  fi

  IFS=$'\t' read -r path kind ceiling disposition owner rationale exit_or_review review_by <<< "${row}"
  is_exact_repo_path "${path}" || fail "${EXCEPTIONS_FILE}:${line_no}: path must be one normalized exact repository path: ${path}"
  if [[ -n "${exception_ceiling[${path}]:-}" ]]; then
    fail "${EXCEPTIONS_FILE}:${line_no}: duplicate exception for ${path}"
  fi
  [[ "${kind}" == source || "${kind}" == test ]] || fail "${EXCEPTIONS_FILE}:${line_no}: kind must be source or test"
  [[ "${ceiling}" =~ ^[1-9][0-9]*$ ]] || fail "${EXCEPTIONS_FILE}:${line_no}: ceiling must be a positive integer"
  [[ "${disposition}" == temporary || "${disposition}" == cohesive ]] || fail "${EXCEPTIONS_FILE}:${line_no}: disposition must be temporary or cohesive"
  [[ "${owner}" =~ ^(team|component):[a-z0-9][a-z0-9-]*(/[a-z0-9][a-z0-9-]*)*$ ]] || fail "${EXCEPTIONS_FILE}:${line_no}: owner must be a stable team:<slug> or component:<slug>"
  has_outer_whitespace "${rationale}" && fail "${EXCEPTIONS_FILE}:${line_no}: rationale must not have leading or trailing whitespace"
  has_outer_whitespace "${exit_or_review}" && fail "${EXCEPTIONS_FILE}:${line_no}: exit_or_review must not have leading or trailing whitespace"
  is_placeholder_metadata "${rationale}" && fail "${EXCEPTIONS_FILE}:${line_no}: rationale must not use a placeholder value"
  is_placeholder_metadata "${exit_or_review}" && fail "${EXCEPTIONS_FILE}:${line_no}: exit_or_review must not use a placeholder value"
  is_real_iso_date "${review_by}" || fail "${EXCEPTIONS_FILE}:${line_no}: review_by must be a real YYYY-MM-DD date"
  [[ "${review_by}" < "${CURRENT_DATE}" ]] && fail "${EXCEPTIONS_FILE}:${line_no}: review_by expired on ${review_by} (current date ${CURRENT_DATE})"

  if [[ "${disposition}" == temporary ]]; then
    has_temporary_exit_trigger "${exit_or_review}" || fail "${EXCEPTIONS_FILE}:${line_no}: temporary exit_or_review must contain after, once, or when"
  else
    has_specific_split_harm "${rationale}" || fail "${EXCEPTIONS_FILE}:${line_no}: cohesive rationale must discuss splitting and its coupling, state, transaction, contract, or chronology cost"
  fi

  exception_ceiling["${path}"]="${ceiling}"
  exception_kind["${path}"]="${kind}"
  exception_rationale["${path}"]="${rationale}"
done < "${EXCEPTIONS_FILE}"

((header_seen == 1)) || fail "${EXCEPTIONS_FILE}: missing exact schema header: ${SCHEMA_HEADER}"

violations_tmp="$(mktemp)"
stale_tmp="$(mktemp)"
invalid_tmp="$(mktemp)"
trap 'rm -f "${violations_tmp}" "${stale_tmp}" "${invalid_tmp}"' EXIT

for path in "${!exception_ceiling[@]}"; do
  if [[ -z "${tracked_file[${path}]:-}" ]]; then
    printf '%s\t%s\n' "${path}" "exception path is not tracked in git" >> "${invalid_tmp}"
    continue
  fi
  if [[ ! -f "${path}" ]]; then
    printf '%s\t%s\n' "${path}" "exception path is missing from the worktree" >> "${invalid_tmp}"
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
  [[ -f "${path}" ]] || continue

  kind="$(classify_kind "${path}" || true)"
  [[ -n "${kind}" ]] || continue

  lines="$(line_count "${path}")"
  limit="$(line_limit_for_kind "${kind}")"

  if [[ -n "${exception_ceiling[${path}]:-}" ]]; then
    ceiling="${exception_ceiling[${path}]}"
    if ((lines <= limit)); then
      printf '%s\t%s\t%s\t%s\t%s\n' "${path}" "below-limit" "${kind}" "${lines}" "${limit}" >> "${stale_tmp}"
    elif ((lines < ceiling)); then
      printf '%s\t%s\t%s\t%s\t%s\n' "${path}" "below-ceiling" "${kind}" "${lines}" "${ceiling}" >> "${stale_tmp}"
    elif ((lines > ceiling)); then
      excess=$((lines - ceiling))
      printf '%09d\t%s\t%s\t%s\t%s\t%s\t%s\n' "${excess}" "${path}" "${kind}" "${lines}" "${ceiling}" "exception-ceiling" "${exception_rationale[${path}]}" >> "${violations_tmp}"
    fi
    continue
  fi

  if ((lines > limit)); then
    excess=$((lines - limit))
    printf '%09d\t%s\t%s\t%s\t%s\t%s\t-\n' "${excess}" "${path}" "${kind}" "${lines}" "${limit}" "limit" >> "${violations_tmp}"
  fi
done < <(git ls-files --cached --others --exclude-standard)

if [[ -s "${invalid_tmp}" ]]; then
  printf 'LOC exception registry has invalid entries:\n' >&2
  sort "${invalid_tmp}" | awk -F '\t' '{printf "  %s: %s\n", $1, $2}' >&2
  exit 1
fi

if [[ -s "${stale_tmp}" ]]; then
  printf 'LOC exceptions are stale and require reviewed removal or ceiling refresh:\n' >&2
  sort "${stale_tmp}" | awk -F '\t' '{
    if ($2 == "below-limit") {
      printf "  %s (%s): %s lines <= %s normal limit; remove the exception\n", $1, $3, $4, $5
    } else {
      printf "  %s (%s): %s lines < approved ceiling %s; refresh the ceiling to exact LOC\n", $1, $3, $4, $5
    }
  }' >&2
  exit 1
fi

if [[ -s "${violations_tmp}" ]]; then
  printf 'LOC gate failed; hard limits are source=%s lines, test=%s lines.\n' "${SOURCE_LIMIT}" "${TEST_LIMIT}" >&2
  printf 'Largest excess first:\n' >&2
  sort -r "${violations_tmp}" | awk -F '\t' '{
    excess = $1 + 0
    if ($6 == "exception-ceiling") {
      printf "  %s (%s): %s lines > approved ceiling %s (+%s); %s\n", $2, $3, $4, $5, excess, $7
    } else {
      printf "  %s (%s): %s lines > limit %s (+%s)\n", $2, $3, $4, $5, excess
    }
  }' >&2
  exit 1
fi

printf 'LOC gate passed (source <= %s, test <= %s; exception ceilings equal current LOC; tracked and untracked non-ignored files scanned).\n' "${SOURCE_LIMIT}" "${TEST_LIMIT}"
