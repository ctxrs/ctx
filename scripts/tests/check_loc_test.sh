#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
checker="$(cd "${script_dir}/.." && pwd)/check-loc.sh"
schema_header=$'path\tkind\tceiling\tdisposition\towner\trationale\texit_or_review\treview_by'
today=2026-07-22
future=2099-12-31
tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT

case_number=0
failures=0
current_case=''
gate_status=0
gate_output=''

fail() {
  failures=$((failures + 1))
  printf 'check-loc test failed: %s\n' "$*" >&2
}

new_case() {
  local name="$1"
  case_number=$((case_number + 1))
  current_case="${tmp}/case-${case_number}-${name}"
  mkdir -p "${current_case}/scripts" "${current_case}/src" "${current_case}/tests"
  git -C "${current_case}" init -q
  printf '%s\n' "${schema_header}" > "${current_case}/scripts/check-loc-exceptions.tsv"
  git -C "${current_case}" add scripts/check-loc-exceptions.tsv
}

make_lines() {
  local path="$1"
  local count="$2"
  mkdir -p "$(dirname "${path}")"
  awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "// fixture line " i }' > "${path}"
}

add_row() {
  local path="$1" kind="$2" ceiling="$3" disposition="$4"
  local owner="$5" rationale="$6" exit_or_review="$7" review_by="$8"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${path}" "${kind}" "${ceiling}" "${disposition}" "${owner}" "${rationale}" "${exit_or_review}" "${review_by}" \
    >> "${current_case}/scripts/check-loc-exceptions.tsv"
}

add_valid_temporary_row() {
  local path="$1" kind="$2" ceiling="$3" review_by="${4:-${future}}"
  local normal_limit=1000
  [[ "${kind}" == test ]] && normal_limit=1500
  add_row \
    "${path}" \
    "${kind}" \
    "${ceiling}" \
    temporary \
    component:test-fixture \
    'Parser and projection responsibilities remain together until the named extraction lands.' \
    "Remove after ${path} is reduced to at most ${normal_limit} physical lines and scripts/check-loc.sh passes." \
    "${review_by}"
}

run_gate() {
  local output_file="${current_case}/gate-output"
  set +e
  (
    cd "${current_case}"
    CTX_LOC_TODAY="${today}" bash "${checker}"
  ) > "${output_file}" 2>&1
  gate_status=$?
  set -e
  gate_output="$(cat "${output_file}")"
}

expect_pass() {
  local name="$1"
  run_gate
  if ((gate_status != 0)); then
    fail "${name}: expected pass, got status ${gate_status}: ${gate_output}"
  fi
}

expect_fail() {
  local name="$1" expected="$2"
  run_gate
  if ((gate_status == 0)); then
    fail "${name}: expected failure"
    return
  fi
  if ! grep -F -- "${expected}" <<< "${gate_output}" >/dev/null; then
    fail "${name}: output did not contain '${expected}': ${gate_output}"
  fi
}

new_case valid-temporary
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1001
expect_pass 'exact temporary ceiling'

new_case review-due-today
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1001 "${today}"
expect_pass 'review date remains valid through the named day'

new_case valid-cohesive
make_lines "${current_case}/tests/contract_matrix.rs" 1501
git -C "${current_case}" add tests/contract_matrix.rs
add_row \
  tests/contract_matrix.rs test 1501 cohesive component:test-fixture \
  'Splitting this contract matrix would duplicate fixture ordering and cross-provider invariants.' \
  'Review when the matrix no longer shares cross-provider ordering and invariant assertions.' \
  "${future}"
expect_pass 'specific cohesive exception'

new_case expired-review
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1001 2026-07-21
expect_fail 'expired review date' 'review_by expired on 2026-07-21'

new_case invalid-calendar-date
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1001 2026-02-30
expect_fail 'real calendar date' 'review_by must be a real YYYY-MM-DD date'

new_case glob-path
add_valid_temporary_row 'src/*.rs' source 1001
expect_fail 'glob path' 'path must be one normalized exact repository path'

new_case traversal-path
add_valid_temporary_row 'src/../large.rs' source 1001
expect_fail 'normalized path' 'path must be one normalized exact repository path'

new_case trailing-slash-path
add_valid_temporary_row 'src/' source 1001
expect_fail 'file path cannot end in slash' 'path must be one normalized exact repository path'

new_case missing-path
add_valid_temporary_row src/missing.rs source 1001
expect_fail 'missing path' 'exception path is not tracked in git'

new_case tracked-path-missing-from-worktree
make_lines "${current_case}/src/missing.rs" 1001
git -C "${current_case}" add src/missing.rs
rm -- "${current_case}/src/missing.rs"
add_valid_temporary_row src/missing.rs source 1001
expect_fail 'tracked path missing from worktree' 'exception path is missing from the worktree'

new_case duplicate-path
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1001
add_valid_temporary_row src/large.rs source 1001
expect_fail 'duplicate path' 'duplicate exception for src/large.rs'

new_case duplicate-rationale
make_lines "${current_case}/src/large.rs" 1001
make_lines "${current_case}/src/other.rs" 1001
git -C "${current_case}" add src/large.rs src/other.rs
add_valid_temporary_row src/large.rs source 1001
add_valid_temporary_row src/other.rs source 1001
expect_pass 'duplicate rationale is allowed'

new_case short-metadata
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 temporary component:test-fixture \
  'Parser seam' \
  'When reviewed.' \
  "${future}"
expect_pass 'short non-placeholder metadata with temporary trigger'

new_case punctuated-temporary-trigger
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 temporary component:test-fixture \
  'Parser seam' \
  'Once: reviewed.' \
  "${future}"
expect_pass 'punctuated temporary trigger'

new_case wrong-kind
make_lines "${current_case}/tests/large.rs" 1501
git -C "${current_case}" add tests/large.rs
add_valid_temporary_row tests/large.rs source 1501
expect_fail 'wrong kind' 'exception kind is source, actual kind is test'

new_case test-support-kind
make_lines "${current_case}/src/test_support.rs" 1501
git -C "${current_case}" add src/test_support.rs
add_valid_temporary_row src/test_support.rs test 1501
expect_pass 'test_support classification'

new_case stale-below-limit
make_lines "${current_case}/src/large.rs" 1000
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1000
expect_fail 'stale below normal limit' 'remove the exception'

new_case stale-below-ceiling
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1002
expect_fail 'ceiling must equal current LOC after shrinkage' 'refresh the ceiling to exact LOC'

new_case ceiling-growth
make_lines "${current_case}/src/large.rs" 1002
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source 1001
expect_fail 'ceiling growth' '1002 lines > approved ceiling 1001'

new_case malformed-ceiling
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_valid_temporary_row src/large.rs source zero
expect_fail 'malformed ceiling' 'ceiling must be a positive integer'

new_case untracked-over-limit
make_lines "${current_case}/src/large.rs" 1001
expect_fail 'untracked oversized file' 'src/large.rs (source): 1001 lines > limit 1000'

new_case untracked-exception
make_lines "${current_case}/src/large.rs" 1001
add_valid_temporary_row src/large.rs source 1001
expect_fail 'untracked exception cannot be approved' 'exception path is not tracked in git'

new_case legacy-header
printf '%s\n' $'path\tmax_lines\tkind\treason\treview_after' > "${current_case}/scripts/check-loc-exceptions.tsv"
expect_fail 'legacy schema rejected' 'expected exact schema header'

new_case malformed-columns
printf '%s\n' $'src/large.rs\tsource\t1001\ttemporary' >> "${current_case}/scripts/check-loc-exceptions.tsv"
expect_fail 'malformed column count' 'expected 8 tab-separated columns'

new_case empty-metadata
printf '%s\n' $'src/large.rs\tsource\t1001\ttemporary\tcomponent:test-fixture\t\tWhen reviewed.\t2099-12-31' >> "${current_case}/scripts/check-loc-exceptions.tsv"
expect_fail 'empty metadata' 'all 8 columns are required'

new_case invalid-disposition
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 permanent component:test-fixture \
  'Parser and projection responsibilities remain together until the named extraction lands.' \
  'Review when the parser and projection no longer share one bounded state transition.' \
  "${future}"
expect_fail 'invalid disposition' 'disposition must be temporary or cohesive'

new_case individual-owner
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 temporary Alice \
  'Parser and projection responsibilities remain together until the named extraction lands.' \
  'Remove after src/large.rs is reduced to at most 1000 physical lines and scripts/check-loc.sh passes.' \
  "${future}"
expect_fail 'stable component owner' 'owner must be a stable team:<slug> or component:<slug>'

new_case generic-rationale
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 temporary component:test-fixture \
  TODO \
  'Remove after src/large.rs is reduced to at most 1000 physical lines and scripts/check-loc.sh passes.' \
  "${future}"
expect_fail 'placeholder rationale' 'rationale must not use a placeholder value'

new_case placeholder-exit
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 temporary component:test-fixture \
  'Parser and projection responsibilities remain together until the named extraction lands.' \
  TBD \
  "${future}"
expect_fail 'placeholder exit text' 'exit_or_review must not use a placeholder value'

new_case missing-temporary-trigger
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 temporary component:test-fixture \
  'Parser seam' \
  'Review the extraction.' \
  "${future}"
expect_fail 'temporary exit trigger' 'temporary exit_or_review must contain after, once, or when'

new_case generic-cohesive-harm
make_lines "${current_case}/src/large.rs" 1001
git -C "${current_case}" add src/large.rs
add_row \
  src/large.rs source 1001 cohesive component:test-fixture \
  'Splitting this file would merely be inconvenient for the current maintainers.' \
  'Review when the module gains an independently testable responsibility boundary.' \
  "${future}"
expect_fail 'cohesive split harm' 'cohesive rationale must discuss splitting'

if ((failures > 0)); then
  exit 1
fi

printf 'check-loc policy tests passed (%s cases)\n' "${case_number}"
