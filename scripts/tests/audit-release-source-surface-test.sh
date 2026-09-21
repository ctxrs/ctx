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

if ! bash "${checker}" "${fixture_root}/retained-upgrade-status" >/dev/null; then
  fail 'retained upgrade availability status was rejected'
fi

if ! bash "${checker}" "${fixture_root}/retained-workspace-product-crate-version" >/dev/null; then
  fail 'workspace-inherited product crate version was rejected'
fi

if ! bash "${checker}" "${fixture_root}/retained-blame-docs" >/dev/null; then
  fail 'ordinary Blame docs or the explicit native uninstall disclaimer was rejected'
fi

for retired_case in \
  retired-uninstall-advice \
  retired-attribution-manifest \
  retired-ctx-attribution-model-surface \
  retired-ctx-repository-evidence-surface \
  retired-ctx-attribution-index-surface \
  retired-ctx-attribution-surface \
  retired-ctx-attribution-derivation-surface \
  mutated-hardcoded-product-crate-version \
  retired-top-level-uninstall \
  retired-command-surfaces \
  retired-presentation-command-surfaces \
  retired-task-documents-command-surface \
  retired-update-invocation \
  retired-update-route; do
  if [[ ! -d "${fixture_root}/${retired_case}" ]]; then
    fail "missing fixture directory: ${retired_case}"
    continue
  fi
  if bash "${checker}" "${fixture_root}/${retired_case}" >/dev/null 2>&1; then
    fail "removed surface passed: ${retired_case}"
  fi
done

if (( failures > 0 )); then
  exit 1
fi

printf 'release source surface audit tests ok\n'
