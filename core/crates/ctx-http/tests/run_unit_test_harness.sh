#!/usr/bin/env bash
set -euo pipefail

resolve_runfile() {
  local logical_path="$1"
  local candidate=""
  local manifest_line=""
  local manifest_logical=""
  local manifest_physical=""

  for candidate in \
    "${logical_path}" \
    "${RUNFILES_DIR:-}/${logical_path}" \
    "${RUNFILES_DIR:-}/_main/${logical_path}" \
    "${RUNFILES_DIR:-}/${TEST_WORKSPACE:-}/${logical_path}"
  do
    if [[ -n "${candidate}" && -x "${candidate}" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  if [[ -f "${RUNFILES_MANIFEST_FILE:-}" ]]; then
    while IFS= read -r manifest_line; do
      manifest_logical="${manifest_line%% *}"
      manifest_physical="${manifest_line#* }"
      if [[ "${manifest_logical}" != "${logical_path}" \
        && "${manifest_logical}" != "_main/${logical_path}" \
        && ( -z "${TEST_WORKSPACE:-}" || "${manifest_logical}" != "${TEST_WORKSPACE}/${logical_path}" ) ]]; then
        continue
      fi
      if [[ -x "${manifest_physical}" ]]; then
        printf '%s\n' "${manifest_physical}"
        return 0
      fi
    done < "${RUNFILES_MANIFEST_FILE}"
  fi

  echo "failed to locate executable ctx-http unit test harness: ${logical_path}" >&2
  return 1
}

if [[ $# -lt 1 ]]; then
  echo "usage: run_unit_test_harness.sh <harness-runfile> [libtest args...]" >&2
  exit 2
fi

if [[ -n "${TESTBRIDGE_TEST_ONLY:-}" ]]; then
  echo "Bazel --test_filter is not supported for ctx-http filtered wrapper shards; use the checked-in shard target filters" >&2
  exit 2
fi

HARNESS="$(resolve_runfile "$1")"
shift

exec "${HARNESS}" "$@"
