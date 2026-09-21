#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

required_paths=(
  README.md
  LICENSE
  docs/product-contract.md
  docs/getting-started.md
  docs/first-10-minutes.md
  docs/cli-reference.md
  docs/event-queries.md
  docs/contracts/json.md
  docs/storage.md
  docs/privacy-storage.md
  docs/mcp-exchange-capture.md
  docs/mcp-tool-call-attribution.md
  docs/mcp-tool-call-attribution-evidence.md
  docs/mcp-tool-call-attribution-capabilities.json
  docs/providers.md
  docs/provider-support.md
  docs/provider-support-matrix.json
  docs/search.md
  docs/blame.md
  docs/slash-command-integrations.md
  docs/limitations.md
  docs/security-checks.md
  docs/agent-usage.md
  docs/testing-taxonomy.md
  docs/troubleshooting.md
  docs/threat-model.md
  docs/provider-adapter-api.md
  docs/agent-skill-install.md
  docs/unmanaged-installs.md
  docs/sdks.md
  skills/ctx/SKILL.md
  plugins/ctx/plugin.json
  plugins/ctx/.codex-plugin/plugin.json
  plugins/ctx/.cursor-plugin/plugin.json
  plugins/ctx/.claude-plugin/plugin.json
  plugins/ctx/skills/ctx/SKILL.md
  plugins/ctx/commands/ctx.md
  plugins/ctx/README.md
  scripts/sync-plugin-skills.sh
  scripts/tests/test_mcp_tool_call_attribution_capabilities.py
)

for path in "${required_paths[@]}"; do
  if ! test -f "${path}"; then
    printf 'missing required path: %s\n' "${path}" >&2
    exit 1
  fi
done

if command -v jq >/dev/null 2>&1; then
  jq empty docs/provider-support-matrix.json
  jq empty docs/mcp-tool-call-attribution-capabilities.json
fi
python3 scripts/check-provider-support-matrix.py
python3 scripts/check-mcp-tool-call-attribution-capabilities.py
python3 scripts/tests/test_mcp_tool_call_attribution_capabilities.py

public_docs=(
  README.md
  SECURITY.md
  docs/*.md
  docs/contracts/*.md
  skills/ctx/SKILL.md
  plugins/ctx/skills/ctx/SKILL.md
  plugins/ctx/commands/ctx.md
  plugins/ctx/README.md
)

analytics_scope=()
for path in "${public_docs[@]}"; do
  if [[ "${path}" != "docs/storage.md" ]]; then
    analytics_scope+=("${path}")
  fi
done

scan_docs() {
  local pattern="$1"
  shift

  if command -v rg >/dev/null 2>&1; then
    rg -n -i -e "${pattern}" "$@"
  else
    grep -R -n -i -E -e "${pattern}" "$@"
  fi
}

unsupported_surface_pattern='dashboard|shim|shims|ctx pr([^[:alnum:]_]|$)|ctx publish|ctx evidence|ctx skill (install|status)([^[:alnum:]_]|$)|ctx update|\bADE\b|automatic summar|\bMVP\b|recover prior decisions|ctx remembers everything|privacy-first|ctx context|ctx export|ctx validate|normalized-only|normalized only|normalized_import_only|normalized provider JSONL|CTX_PROVIDER_NORMALIZED_IMPORT_DEV|[W]ork Recorder|[w]ork recorder|\bwork-[r]ecord\b'
private_path_pattern='/home/[^[:space:]/]+/(code|Documents|Desktop)|/Users/[^[:space:]/]+/(code|Documents|Desktop)'
private_path_pattern+='|multi[-_]repo[-_]workspace'
private_path_pattern+='|(conformance|internal)[^[:space:]/]*/[^[:space:]/]*(proof|evidence)[-_](packet|packets|bundle)'
private_path_pattern+='|\.ctx/worktrees'

if scan_docs "${unsupported_surface_pattern}" "${public_docs[@]}"; then
    printf 'public docs contain removed or unsupported product surface wording\n' >&2
    exit 1
fi

# `--until` is public only for the paired `ctx list events` chronology range.
# Keep rejecting the former search and single-record show spellings.
if scan_docs 'ctx search.*--until|ctx show (session|event)([[:space:]]|<).*--until' "${public_docs[@]}"; then
  printf 'public docs attach --until to an unsupported command\n' >&2
  exit 1
fi

if scan_docs "${private_path_pattern}" "${public_docs[@]}"; then
  printf 'public docs contain private host/workspace paths\n' >&2
  exit 1
fi

python3 - "${public_docs[@]}" <<'PY'
import re
import sys
from pathlib import Path

from scripts.check_mcp_tool_call_attribution_capabilities_lib import public_boundary_violation

for name in sys.argv[1:]:
    text = Path(name).read_text(encoding="utf-8")
    # The hosted uninstaller is supported; there is no native uninstall command.
    # Permit that explicit distinction while rejecting runnable recommendations.
    command_text = re.sub(r"There is no\s+native\s+`ctx uninstall`\s+command\.", "", text)
    if re.search(r"\bctx uninstall\b", command_text, re.IGNORECASE):
        raise SystemExit(f"{name} recommends the unsupported native ctx uninstall command")
    violation = public_boundary_violation(text)
    if violation is not None:
        raise SystemExit(f"{name} crosses the public documentation boundary: {violation}")
PY

if scan_docs 'analytics|telemetry' "${analytics_scope[@]}"; then
  printf 'public analytics copy must stay limited to docs/storage.md\n' >&2
  exit 1
fi

bash scripts/sync-plugin-skills.sh --check

python3 <<'PY'
import json
import tomllib
from pathlib import Path

root = Path(".")
version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
description = "Fast local search and 'git blame' for agent sessions."
author = {"name": "ctx engineering inc", "url": "https://ctx.rs"}
keywords = [
    "ctx", "agent-sessions", "agent-history", "transcripts",
    "local-search", "code-provenance",
]

portable_path = root / "plugins/ctx/plugin.json"
portable = json.loads(portable_path.read_text(encoding="utf-8"))
portable_keys = {
    "$schema", "name", "version", "description", "author", "homepage",
    "repository", "license", "keywords", "extensions",
}
unknown = set(portable) - portable_keys
if unknown:
    raise SystemExit(f"{portable_path} has non-portable fields: {sorted(unknown)}")
expected_portable = {
    "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
    "name": "ctx",
    "version": version,
    "description": description,
    "author": author,
    "homepage": "https://ctx.rs",
    "repository": "https://github.com/ctxrs/ctx",
    "license": "Apache-2.0",
    "keywords": keywords,
}
for key, expected in expected_portable.items():
    if portable.get(key) != expected:
        raise SystemExit(f"{portable_path} {key} must be {expected!r}")

native_paths = [
    root / "plugins/ctx/.codex-plugin/plugin.json",
    root / "plugins/ctx/.cursor-plugin/plugin.json",
    root / "plugins/ctx/.claude-plugin/plugin.json",
]
for path in native_paths:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    expected = {
        "name": "ctx",
        "version": version,
        "description": description,
        "author": author,
        "homepage": "https://ctx.rs",
        "repository": "https://github.com/ctxrs/ctx",
        "license": "Apache-2.0",
        "keywords": keywords,
        "skills": "./skills/",
    }
    for key, value in expected.items():
        if manifest.get(key) != value:
            raise SystemExit(f"{path} {key} must match portable metadata")

codex = json.loads(native_paths[0].read_text(encoding="utf-8"))
interface = codex.get("interface", {})
if interface.get("displayName") != "ctx" or interface.get("developerName") != "ctx engineering inc":
    raise SystemExit("Codex display and developer names must match ctx publisher metadata")

catalog_expectations = {
    root / ".agents/plugins/marketplace.json": ("ctx", "./plugins/ctx"),
    root / ".cursor-plugin/marketplace.json": ("ctx", "plugins/ctx"),
    root / ".claude-plugin/marketplace.json": ("ctx", "./plugins/ctx"),
}
for path, (name, source) in catalog_expectations.items():
    catalog = json.loads(path.read_text(encoding="utf-8"))
    entries = catalog.get("plugins", [])
    if len(entries) != 1 or entries[0].get("name") != name:
        raise SystemExit(f"{path} must contain exactly the ctx plugin")
    actual_source = entries[0].get("source")
    if isinstance(actual_source, dict):
        actual_source = actual_source.get("path")
    if actual_source != source:
        raise SystemExit(f"{path} source must be {source}")
    if "version" in entries[0] and entries[0]["version"] != version:
        raise SystemExit(f"{path} version must match Cargo workspace version")
    if "description" in entries[0] and entries[0]["description"] != description:
        raise SystemExit(f"{path} description must match portable metadata")
    if "keywords" in entries[0] and entries[0]["keywords"] != keywords:
        raise SystemExit(f"{path} keywords must match portable metadata")

cursor_catalog = json.loads((root / ".cursor-plugin/marketplace.json").read_text(encoding="utf-8"))
if cursor_catalog.get("metadata", {}).get("description") != description:
    raise SystemExit("Cursor marketplace description must match portable metadata")
PY

if ! grep -F -q 'Use the `ctx` skill' plugins/ctx/commands/ctx.md; then
  printf 'ctx command must delegate to the ctx skill\n' >&2
  exit 1
fi

if scan_docs 'ctx search "[^"]*" --format json[[:space:]]*$' docs/agent-usage.md docs/getting-started.md docs/first-10-minutes.md skills/ctx/SKILL.md plugins/ctx/skills/ctx/SKILL.md plugins/ctx/commands/ctx.md; then
  printf 'agent-facing docs should not recommend ctx search --format json for normal reading\n' >&2
  exit 1
fi

printf 'public docs ok\n'
