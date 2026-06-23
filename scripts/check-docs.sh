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
  docs/provider-support.md
  docs/threat-model.md
  docs/redaction-corpus.md
  docs/dependency-license-audit.md
  docs/hosted-sync-roadmap.md
  docs/troubleshooting.md
  tests/fixtures/redaction/redaction-corpus.jsonl
  docs/release-install.md
  docs/release-supply-chain.md
  docs/release-r2-layout.md
  docs/freebsd-release-worker.md
  examples/local-record-workflow.sh
  examples/capture-spool-fixture.sh
  apps/work-recorder-dashboard/site-preview.html
  release/install/ctx-release-metadata.env.template
  release/completion-certificate-template.md
  release/r2-artifact-layout.json
)

for path in "${required_paths[@]}"; do
  test -f "${path}"
done

for script in examples/*.sh scripts/check.sh scripts/check-docs.sh scripts/check-buildkite-pipeline.sh scripts/install.sh scripts/release-*.sh; do
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

public_docs=(
  README.md
  SECURITY.md
  docs
  apps/work-recorder-dashboard/README.md
  apps/work-recorder-dashboard/src/site-preview-data.ts
  apps/work-recorder-dashboard/src/site-preview.tsx
  apps/work-recorder-dashboard/site-preview.html
)

public_name_docs=(
  README.md
  SECURITY.md
  docs/getting-started.md
  docs/cli-reference.md
  docs/work-model.md
  docs/privacy-storage.md
  docs/hosted-sync-roadmap.md
  docs/troubleshooting.md
  docs/redaction-corpus.md
  docs/dependency-license-audit.md
  docs/provider-adapter-api.md
  docs/threat-model.md
  apps/work-recorder-dashboard/README.md
  apps/work-recorder-dashboard/src/site-preview-data.ts
  apps/work-recorder-dashboard/src/site-preview.tsx
  apps/work-recorder-dashboard/site-preview.html
)

doc_search "ctx capture import" README.md docs examples >/dev/null
doc_search "ctx vcs inspect" README.md docs examples >/dev/null
doc_search "ctx pr parse" README.md docs examples >/dev/null
doc_search "ctx dashboard export" README.md docs examples >/dev/null
doc_search "ctx publish pr-comment" README.md docs >/dev/null
doc_search "Hosted Option A" SECURITY.md README.md docs >/dev/null
doc_search "redaction-corpus.jsonl" docs tests >/dev/null
doc_search "not a general-purpose sanitizer|not claim a general-purpose redaction engine" SECURITY.md docs/redaction-corpus.md docs/privacy-storage.md docs/threat-model.md docs/provider-adapter-api.md >/dev/null
doc_search "Redaction is heuristic|heuristic redaction|heuristic evidence" SECURITY.md docs/redaction-corpus.md docs/privacy-storage.md docs/threat-model.md docs/provider-adapter-api.md >/dev/null
doc_search "provider-specific redaction tests|redaction corpus coverage" SECURITY.md docs/threat-model.md docs/provider-support.md docs/provider-adapter-api.md docs/release-supply-chain.md >/dev/null
doc_search "malformed-input" SECURITY.md docs/threat-model.md docs/provider-adapter-api.md docs/release-supply-chain.md >/dev/null
doc_search "raw-retention|raw retention" docs/threat-model.md docs/provider-support.md docs/provider-adapter-api.md docs/release-supply-chain.md >/dev/null
doc_search "explicit opt-in|opt-in" SECURITY.md docs/privacy-storage.md docs/threat-model.md >/dev/null
doc_search "cargo audit|cargo deny" docs/dependency-license-audit.md >/dev/null
doc_search "does not install|Not implemented yet|not yet" README.md docs >/dev/null
doc_search "0.1.0" README.md docs >/dev/null
doc_search "not the ctx ADE|not the ADE" README.md docs >/dev/null
doc_search "do not present them as live installer URLs" docs/release-install.md >/dev/null
doc_search "curl -fsSLO" docs/release-install.md >/dev/null
doc_search "SHA-256" docs/release-install.md docs/release-supply-chain.md >/dev/null
doc_search "SBOM" docs/release-supply-chain.md >/dev/null
doc_search "notarization" docs/release-supply-chain.md >/dev/null
doc_search "Finished-Product Evidence Matrix" docs/release-supply-chain.md >/dev/null
doc_search "provider fixture import" docs/release-supply-chain.md release/completion-certificate-template.md >/dev/null
doc_search "PR publish dry-run" docs/release-supply-chain.md >/dev/null
doc_search "installer dry-run smoke lane" docs/release-install.md >/dev/null
doc_search "jj e2e blocker status" docs/release-supply-chain.md release/completion-certificate-template.md >/dev/null
doc_search "R2 upload plan" docs/release-supply-chain.md docs/release-r2-layout.md release/completion-certificate-template.md >/dev/null
doc_search "CTX_LIVE_PROVIDER_E2E=1" docs/release-supply-chain.md >/dev/null
doc_search "freebsd-x64" docs/freebsd-release-worker.md docs/release-supply-chain.md >/dev/null
doc_search "work records|records agent work" README.md docs apps/work-recorder-dashboard/src/site-preview-data.ts apps/work-recorder-dashboard/src/site-preview.tsx >/dev/null
doc_search "~/.ctx/" README.md docs/getting-started.md docs/threat-model.md >/dev/null
doc_search "objects/" README.md docs/threat-model.md >/dev/null
doc_search "spool/" README.md docs/getting-started.md docs/threat-model.md >/dev/null
doc_search "shims/" README.md docs/getting-started.md docs/threat-model.md >/dev/null
doc_search "config.toml" README.md docs/threat-model.md >/dev/null
doc_search "logs/" README.md docs/threat-model.md >/dev/null
doc_search "ctx dashboard.*--no-open|headless" README.md docs apps/work-recorder-dashboard/src/site-preview-data.ts apps/work-recorder-dashboard/src/site-preview.tsx >/dev/null
doc_search "ctx service install|ctx setup --service" README.md docs apps/work-recorder-dashboard/src/site-preview-data.ts >/dev/null
doc_search "ctx uninstall --delete-data" README.md docs >/dev/null

if doc_search "~/.ctx/work-record|work-record/(shims|inbox)|work-record\\\\(shims|inbox)|\\bblobs/|\\binbox\\b|blob files|blob artifacts" "${public_docs[@]}" >/dev/null; then
  printf 'public docs contain stale Work Recorder layout wording\n' >&2
  exit 1
fi

if doc_search "Work Recorder" "${public_docs[@]}" \
  | doc_search_inverse "work-recorder-dashboard|work-recorder-dogfood|work-recorder-completion|work-recorder-finished-product|ctx-work-recorder-releases|work-record-core|work-record-capture|work-record-store|Work Recorder Completion Certificate" >/dev/null; then
  printf 'public docs contain Work Recorder as product branding; use ctx/work records wording\n' >&2
  exit 1
fi

if doc_search "work-record" "${public_name_docs[@]}" \
  | doc_search_inverse "assets/readme/work-record-banner.png" >/dev/null; then
  printf 'public docs contain avoidable work-record naming; use ctx/work records wording\n' >&2
  exit 1
fi

if doc_search "does not ship a local dashboard|does not include a dashboard|local dashboard;" docs README.md >/dev/null; then
  printf 'dashboard appears to be documented as missing\n' >&2
  exit 1
fi

if doc_search "hosted publish|hosted publishing|hosted/team pull request publishing" docs README.md \
  | doc_search_inverse "out|outside|not in launch|does not include|not shipped|remain outside|future" >/dev/null; then
  printf 'hosted publishing appears to be documented as shipped\n' >&2
  exit 1
fi

echo "docs ok"
