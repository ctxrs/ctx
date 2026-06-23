#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

required_paths=(
  README.md
  SECURITY.md
  docs/getting-started.md
  docs/cli-reference.md
  docs/work-model.md
  docs/privacy-storage.md
  docs/threat-model.md
  docs/redaction-corpus.md
  docs/dependency-license-audit.md
  examples/local-record-workflow.sh
  examples/capture-spool-fixture.sh
  tests/fixtures/redaction/redaction-corpus.jsonl
)

for path in "${required_paths[@]}"; do
  test -f "${path}"
done

for script in examples/*.sh scripts/check-docs.sh; do
  bash -n "${script}"
done

doc_search() {
  if command -v rg >/dev/null 2>&1; then
    rg -n "$@"
  else
    grep -R -n -E "$@"
  fi
}

doc_search_inverse() {
  if command -v rg >/dev/null 2>&1; then
    rg -v "$@"
  else
    grep -v -E "$@"
  fi
}

doc_search "ctx capture import" README.md docs examples >/dev/null
doc_search "ctx vcs inspect" README.md docs examples >/dev/null
doc_search "ctx pr parse" README.md docs examples >/dev/null
doc_search "ctx dashboard export" README.md docs examples >/dev/null
doc_search "Hosted Option A" SECURITY.md README.md docs >/dev/null
doc_search "redaction-corpus.jsonl" docs tests >/dev/null
doc_search "cargo audit|cargo deny" docs/dependency-license-audit.md >/dev/null
doc_search "does not install|Not implemented yet|not yet" README.md docs >/dev/null

if doc_search "does not ship a local dashboard|does not include a dashboard|local dashboard;" docs README.md >/dev/null; then
  printf 'dashboard appears to be documented as missing\n' >&2
  exit 1
fi

if doc_search "ctx publish" docs README.md | doc_search_inverse "does not include|Not implemented yet|not ship|such as" >/dev/null; then
  printf 'publish appears to be documented as shipped\n' >&2
  exit 1
fi

echo "docs ok"
