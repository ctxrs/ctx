#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

run_in_dir() {
  local dir="$1"
  shift
  printf '\n==> (cd %s && %s)\n' "$dir" "$*"
  (
    cd "$dir"
    "$@"
  )
}

skipped=0
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-sdk-check.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

skip() {
  printf '\n==> skip: %s\n' "$*"
  skipped=$((skipped + 1))
}

run python3 scripts/check-agent-history-contract.py
run bash scripts/check-sdk-no-publish.sh
run cargo test -p ctx-protocol -p ctx-sdk

if command -v npm >/dev/null 2>&1 && [ -f sdks/typescript/package.json ]; then
  if [ -f sdks/typescript/package-lock.json ]; then
    run npm ci --prefix sdks/typescript --ignore-scripts
  fi
  run npm test --prefix sdks/typescript
else
  skip "TypeScript SDK tests (npm unavailable or SDK absent)"
fi

if command -v python3 >/dev/null 2>&1 && [ -d sdks/python/tests ]; then
  run python3 -m unittest discover -s sdks/python/tests
else
  skip "Python SDK tests (python3 unavailable or SDK absent)"
fi

if command -v go >/dev/null 2>&1 && [ -f sdks/go/go.mod ]; then
  run_in_dir sdks/go go test ./...
else
  skip "Go SDK tests (go unavailable or SDK absent)"
fi

if command -v javac >/dev/null 2>&1 && [ -f sdks/jvm/README.md ]; then
  if [ -x sdks/jvm/scripts/test ]; then
    run sdks/jvm/scripts/test
  elif [ -x sdks/jvm/scripts/test.sh ]; then
    run sdks/jvm/scripts/test.sh
  else
    skip "JVM SDK tests (no executable sdks/jvm/scripts/test)"
  fi
else
  skip "JVM SDK tests (javac unavailable or SDK absent)"
fi

if command -v swift >/dev/null 2>&1 && [ -f sdks/swift/Package.swift ]; then
  run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"
else
  skip "Swift SDK tests (swift unavailable or SDK absent)"
fi

if command -v dotnet >/dev/null 2>&1 && [ -f sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj ]; then
  run dotnet run --project sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj
else
  skip ".NET SDK tests (dotnet unavailable or SDK absent)"
fi

if [ "${CTX_SDK_RUN_LOCAL_SMOKE:-0}" = "1" ]; then
  run bash scripts/sdk-local-smoke.sh
fi

if [ "${CTX_SDK_STRICT_TOOLCHAINS:-0}" = "1" ] && [ "$skipped" -gt 0 ]; then
  printf '\nstrict SDK toolchain mode failed: %s SDK test group(s) skipped\n' "$skipped" >&2
  exit 1
fi
