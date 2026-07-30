#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "${script_dir}/../.." && pwd)"
fi

checker="${repo_root}/scripts/check-release-source-surface.sh"
fixture_root="${repo_root}/scripts/tests/fixtures/release-source-surface"
failures=0

fail() {
  failures=$((failures + 1))
  printf 'release source surface audit test failed: %s\n' "$*" >&2
}

if ! bash "${checker}" "${fixture_root}/retained-pro-uninstall" >/dev/null; then
  fail 'retained Local Pro lifecycle uninstall was rejected'
fi

if ! bash "${checker}" "${fixture_root}/retained-upgrade-status" >/dev/null; then
  fail 'retained upgrade availability status was rejected'
fi

for retired_case in \
  retired-top-level-uninstall \
  retired-command-surfaces \
  retired-misplaced-pro-uninstall \
  retired-update-invocation \
  retired-update-route; do
  if bash "${checker}" "${fixture_root}/${retired_case}" >/dev/null 2>&1; then
    fail "removed surface passed: ${retired_case}"
  fi
done

if (( failures > 0 )); then
  exit 1
fi

printf 'release source surface audit tests ok\n'
