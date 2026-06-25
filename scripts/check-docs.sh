#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

required_paths=(
  README.md
  LICENSE
  docs/product-contract.md
  docs/getting-started.md
  docs/first-10-minutes.md
  docs/cli-reference.md
  docs/contracts/json.md
  docs/storage.md
  docs/privacy-storage.md
  docs/providers.md
  docs/provider-support.md
  docs/provider-support-matrix.json
  docs/search.md
  docs/limitations.md
  docs/security-checks.md
  docs/agent-usage.md
  docs/testing-taxonomy.md
  docs/troubleshooting.md
  docs/threat-model.md
  docs/provider-adapter-api.md
  docs/redaction-corpus.md
  skills/ctx-agent-memory/SKILL.md
)

for path in "${required_paths[@]}"; do
  test -f "${path}"
done

if command -v jq >/dev/null 2>&1; then
  jq empty docs/provider-support-matrix.json
fi

if command -v rg >/dev/null 2>&1; then
  if rg -n -i 'dashboard|shim|shims|pull request|pr evidence|ctx pr|ctx publish|ctx evidence|ctx update|ctx uninstall|hosted|ADE|automatic summar|\bMVP\b|recover prior decisions|ctx remembers everything|privacy-first|ctx context|--until|ctx list --provider|ctx list --repo|ctx list --since|default analytics enabled|with default analytics enabled|analytics (are )?enabled by default' \
    README.md docs skills; then
    printf 'public docs contain removed or unsupported product surface wording\n' >&2
    exit 1
  fi
else
  if grep -R -n -i -E 'dashboard|shim|shims|pull request|pr evidence|ctx pr|ctx publish|ctx evidence|ctx update|ctx uninstall|hosted|ADE|automatic summar|\bMVP\b|recover prior decisions|ctx remembers everything|privacy-first|ctx context|--until|ctx list --provider|ctx list --repo|ctx list --since|default analytics enabled|with default analytics enabled|analytics (are )?enabled by default' \
    README.md docs skills; then
    printf 'public docs contain removed or unsupported product surface wording\n' >&2
    exit 1
  fi
fi

printf 'public docs ok\n'
