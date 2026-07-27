#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
selector="${repo_root}/scripts/public-cli-freebsd-build-strategy.sh"
fixture="${repo_root}/scripts/tests/fixtures/public-cli-freebsd-build-strategies.tsv"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-freebsd-build-strategy.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

case_count=0
while IFS=$'\t' read -r name host_system host_arch expected_status expected_strategy; do
  if [[ "${name}" == "name" ]]; then
    continue
  fi
  stdout="${tmp_dir}/${name}.out"
  stderr="${tmp_dir}/${name}.err"
  status=0
  sh "${selector}" "${host_system}" "${host_arch}" \
    >"${stdout}" 2>"${stderr}" || status=$?
  [[ "${status}" == "${expected_status}" ]] || {
    printf 'unexpected status for %s: expected %s got %s\n' \
      "${name}" "${expected_status}" "${status}" >&2
    exit 1
  }
  if [[ "${expected_status}" == 0 ]]; then
    [[ "$(tr -d '\r\n' < "${stdout}")" == "${expected_strategy}" ]]
    [[ ! -s "${stderr}" ]]
  else
    [[ ! -s "${stdout}" ]]
    grep -Fq 'requires native x64 FreeBSD or a Linux cross host' "${stderr}"
  fi
  case_count=$((case_count + 1))
done < "${fixture}"

[[ "${case_count}" -eq 6 ]]
printf 'FreeBSD public CLI build-strategy tests passed: cases=%s\n' "${case_count}"
