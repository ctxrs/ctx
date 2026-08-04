#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  printf 'SDK publish guard failed: %s\n' "$*" >&2
  exit 1
}

require_file_contains() {
  local file="$1"
  local pattern="$2"
  local message="$3"
  if ! grep -Eq -- "$pattern" "$file"; then
    fail "$message"
  fi
}

require_file_contains sdks/typescript/package.json '"private"[[:space:]]*:[[:space:]]*true' \
  'TypeScript SDK package.json must remain private'
require_file_contains sdks/python/pyproject.toml '^publish[[:space:]]*=[[:space:]]*false$' \
  'Python SDK pyproject.toml must keep [tool.ctx] publish = false'
require_file_contains crates/ctx-sdk/Cargo.toml '^publish[[:space:]]*=[[:space:]]*false$' \
  'Rust SDK crate must keep publish = false'
require_file_contains crates/ctx-protocol/Cargo.toml '^publish[[:space:]]*=[[:space:]]*false$' \
  'Rust protocol crate must keep publish = false'
require_file_contains sdks/dotnet/src/Ctx.AgentHistory/Ctx.AgentHistory.csproj '<IsPackable>false</IsPackable>' \
  '.NET SDK project must keep IsPackable=false until NuGet publishing is intentional'

publish_pattern='(^|[[:space:]])(npm publish|twine upload|cargo publish|dotnet nuget push|gradle publish|mvn deploy|swift package-registry publish)([[:space:]]|$)'
publish_command_found=false
publish_scan_failed=false
publish_scan_manifest=""

cleanup_publish_scan_manifest() {
  if [[ -n "$publish_scan_manifest" ]]; then
    rm -f -- "$publish_scan_manifest" || true
  fi
}
trap cleanup_publish_scan_manifest EXIT

if ! publish_scan_manifest="$(mktemp "${TMPDIR:-/tmp}/ctx-sdk-no-publish.XXXXXX")"; then
  fail 'could not allocate SDK publish-policy traversal state'
fi

# Prune generated trees before descent. The remaining paths are NUL-delimited
# so every valid filename, including spaces and newlines, is scanned exactly.
if ! find . \
  \( \
    -path './.git' -o \
    -path './.buildkite-cache' -o \
    -path './target' -o \
    -path './bazel-*' \
  \) -prune -o -type f -print0 >"$publish_scan_manifest"; then
  fail 'could not traverse every SDK publish-policy input'
fi

while IFS= read -r -d '' discovered_file; do
  file="${discovered_file#./}"
  case "$file" in
    scripts/check-sdk-no-publish.sh | \
      contracts/agent-history-v1/README.md | \
      docs/sdk-production-readiness.md | \
      sdks/*/README.md | \
      crates/ctx-sdk/README.md)
      continue
      ;;
  esac

  if grep -nHE "$publish_pattern" -- "$file"; then
    publish_command_found=true
  else
    status=$?
    if [[ "$status" -ne 1 ]]; then
      printf 'SDK publish guard scan failed for %s (grep status %s)\n' \
        "$file" "$status" >&2
      publish_scan_failed=true
    fi
  fi
done <"$publish_scan_manifest"

if [[ "$publish_scan_failed" == true ]]; then
  fail 'could not inspect every SDK publish-policy input'
fi
if [[ "$publish_command_found" == true ]]; then
  fail 'live SDK package-manager publish command found outside docs/policy text'
fi

printf 'SDK publish guard passed\n'
