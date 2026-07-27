#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "${script_dir}/../.." && pwd)"
fi
checker="${repo_root}/scripts/check-release-binary-strings.sh"
fixture_dir="${repo_root}/scripts/tests/fixtures/release-binary-strings"
failures=0

fail() {
  failures=$((failures + 1))
  printf 'release binary string audit test failed: %s\n' "$*" >&2
}

while IFS= read -r signature; do
  [[ -n "${signature}" ]] || continue
  if printf '%s\n' "${signature}" | bash "${checker}" >/dev/null 2>&1; then
    fail "removed signature passed: ${signature}"
  fi
done <"${fixture_dir}/removed-cloud-history.txt"

if ! bash "${checker}" "${fixture_dir}/protocol-v1-local-runtime.txt"; then
  fail 'Protocol V1 and retained local-runtime strings were rejected'
fi

if (( failures > 0 )); then
  exit 1
fi

printf 'release binary string audit tests ok\n'
