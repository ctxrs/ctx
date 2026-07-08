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

skip() {
  printf '\n==> skip: %s\n' "$*"
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-sdk-package-dry-run.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

run bash scripts/check-sdk-no-publish.sh

if [[ -n "${TEST_SRCDIR:-}" ]]; then
  skip "TypeScript npm pack dry-run (npm pack is not reliable in Bazel runfiles)"
elif command -v npm >/dev/null 2>&1; then
  run npm pack --dry-run ./sdks/typescript
else
  skip "TypeScript npm pack dry-run (npm unavailable)"
fi

if command -v python3 >/dev/null 2>&1; then
  run env PYTHONPYCACHEPREFIX="$tmp_dir/python-pycache" python3 -m compileall -q sdks/python/src sdks/python/tests
  if python3 -c 'import build' >/dev/null 2>&1; then
    run env PYTHONPYCACHEPREFIX="$tmp_dir/python-pycache" python3 -m build sdks/python --outdir "$tmp_dir/python"
  else
    skip "Python wheel/sdist dry-run (python build module unavailable)"
  fi
else
  skip "Python package dry-run (python3 unavailable)"
fi

if command -v cargo >/dev/null 2>&1; then
  run cargo package --locked --no-verify --allow-dirty -p ctx-protocol --target-dir "$tmp_dir/cargo-target"
  run cargo check --locked -p ctx-sdk
  skip "Rust ctx-sdk cargo package dry-run (depends on unpublished in-repo ctx-protocol)"
else
  skip "Rust cargo package dry-run (cargo unavailable)"
fi

if command -v go >/dev/null 2>&1; then
  run_in_dir sdks/go go list ./...
else
  skip "Go module dry-run (go unavailable)"
fi

if command -v javac >/dev/null 2>&1; then
  run sdks/jvm/scripts/test
else
  skip "JVM jar/test dry-run (javac unavailable)"
fi

if command -v swift >/dev/null 2>&1; then
  run swift package --package-path sdks/swift --scratch-path "$tmp_dir/swift-build" describe
  run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"
else
  skip "Swift package describe (swift unavailable)"
fi

if command -v dotnet >/dev/null 2>&1; then
  run dotnet run --project sdks/dotnet/tests/Ctx.AgentHistory.Tests/Ctx.AgentHistory.Tests.csproj
else
  skip ".NET pack/test dry-run (dotnet unavailable)"
fi

find sdks/python -type d -name __pycache__ -prune -exec rm -rf {} +
