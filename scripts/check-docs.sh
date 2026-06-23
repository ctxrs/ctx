#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

required_paths=(
  README.md
  docs/getting-started.md
  docs/cli-reference.md
  docs/work-model.md
  docs/privacy-storage.md
  examples/local-record-workflow.sh
  examples/capture-spool-fixture.sh
)

for path in "${required_paths[@]}"; do
  test -f "${path}"
done

for script in examples/*.sh scripts/check-docs.sh; do
  bash -n "${script}"
done

rg -n "ctx capture import" README.md docs examples >/dev/null
rg -n "ctx vcs inspect" README.md docs examples >/dev/null
rg -n "ctx pr parse" README.md docs examples >/dev/null
rg -n "ctx dashboard export" README.md docs examples >/dev/null
rg -n "does not install|Not implemented yet|not yet" README.md docs >/dev/null

if rg -n "does not ship a local dashboard|does not include a dashboard|local dashboard;" docs README.md >/dev/null; then
  printf 'dashboard appears to be documented as missing\n' >&2
  exit 1
fi

if rg -n "ctx publish" docs README.md | rg -v "does not include|Not implemented yet|not ship|such as" >/dev/null; then
  printf 'publish appears to be documented as shipped\n' >&2
  exit 1
fi

echo "docs ok"
