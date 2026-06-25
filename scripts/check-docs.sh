#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

required_paths=(
  README.md
  docs/getting-started.md
  docs/cli-reference.md
  docs/storage.md
  docs/privacy-storage.md
  docs/providers.md
  docs/provider-support.md
  docs/provider-support-matrix.json
  docs/search.md
  docs/agent-usage.md
  docs/testing-taxonomy.md
  docs/troubleshooting.md
  docs/threat-model.md
  docs/provider-adapter-api.md
  docs/redaction-corpus.md
  docs/release-install.md
  docs/release-supply-chain.md
  docs/release-r2-layout.md
  docs/freebsd-release-worker.md
  docs/dependency-license-audit.md
  skills/ctx-agent-memory/SKILL.md
)

for path in "${required_paths[@]}"; do
  test -f "${path}"
done

if command -v jq >/dev/null 2>&1; then
  jq empty docs/provider-support-matrix.json
fi

if command -v rg >/dev/null 2>&1; then
  if rg -n -i 'dashboard|shim|shims|pull request|pr evidence|ctx pr|ctx publish|ctx evidence|hosted|ADE|automatic summar|\bMVP\b|recover prior decisions|ctx remembers everything|privacy-first|--until|ctx list --provider|ctx list --repo|ctx list --since' \
    README.md docs skills; then
    printf 'public docs contain removed or unsupported product surface wording\n' >&2
    exit 1
  fi
else
  if grep -R -n -i -E 'dashboard|shim|shims|pull request|pr evidence|ctx pr|ctx publish|ctx evidence|hosted|ADE|automatic summar|\bMVP\b|recover prior decisions|ctx remembers everything|privacy-first|--until|ctx list --provider|ctx list --repo|ctx list --since' \
    README.md docs skills; then
    printf 'public docs contain removed or unsupported product surface wording\n' >&2
    exit 1
  fi
fi

printf 'search MVP docs ok\n'
