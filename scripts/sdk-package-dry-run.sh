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

resolve_input() {
  local input="$1"
  if [[ "$input" = /* ]]; then
    printf '%s\n' "$input"
  else
    printf '%s/%s\n' "$repo_root" "$input"
  fi
}

if [[ -z "${CTX_SDK_CARGO:-}" \
  || -z "${CTX_SDK_RUSTC:-}" \
  || -z "${CTX_SDK_CARGO_VENDOR_MANIFEST:-}" ]]; then
  if [[ -z "${TEST_SRCDIR:-}" ]]; then
    exec scripts/bazelw test \
      //:sdk_package_dry_run \
      //sdks/go:go_sdk_tests \
      //sdks/go/examples/dogfood:dogfood_tests \
      --config=ci --test_output=all --nocache_test_results
  fi
  printf 'Bazel SDK package dry-run requires declared Cargo, rustc, and vendor inputs\n' >&2
  exit 1
fi
cargo_bin="$(resolve_input "${CTX_SDK_CARGO:-}")"
rustc_bin="$(resolve_input "${CTX_SDK_RUSTC:-}")"
vendor_manifest="$(resolve_input "${CTX_SDK_CARGO_VENDOR_MANIFEST:-}")"
if [[ ! -x "$cargo_bin" || ! -x "$rustc_bin" || ! -f "$vendor_manifest" ]]; then
  printf 'Bazel SDK package dry-run requires declared Cargo, rustc, and vendor inputs\n' >&2
  exit 1
fi

run bash scripts/check-sdk-no-publish.sh
run python3 scripts/check-typescript-package-contents.py

if [[ -n "${TEST_SRCDIR:-}" ]]; then
  printf '\n==> TypeScript npm package contents were validated deterministically in Bazel runfiles\n'
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

run python3 scripts/prepare-sdk-cargo-workspace.py \
  Cargo.toml \
  Cargo.lock \
  crates/ctx-protocol \
  crates/ctx-sdk \
  "$vendor_manifest" \
  "$tmp_dir/cargo-workspace"
mkdir -p "$tmp_dir/home" "$tmp_dir/cargo-home" "$tmp_dir/rustup-home"
cargo_env=(
  env
  "HOME=$tmp_dir/home"
  "CARGO_HOME=$tmp_dir/cargo-home"
  "RUSTUP_HOME=$tmp_dir/rustup-home"
  "CARGO_NET_OFFLINE=true"
  "RUSTC=$rustc_bin"
  "PATH=/usr/bin:/bin"
)
run "${cargo_env[@]}" "$cargo_bin" --version
run "${cargo_env[@]}" "$rustc_bin" --version
run_in_dir "$tmp_dir/cargo-workspace" \
  "${cargo_env[@]}" "$cargo_bin" generate-lockfile --offline
run_in_dir "$tmp_dir/cargo-workspace" \
  "${cargo_env[@]}" "$cargo_bin" package --locked --offline --no-verify --allow-dirty \
  -p ctx-protocol --target-dir "$tmp_dir/cargo-target"
run_in_dir "$tmp_dir/cargo-workspace" \
  "${cargo_env[@]}" "$cargo_bin" check --locked --offline -p ctx-sdk \
  --target-dir "$tmp_dir/cargo-target"
skip "Rust ctx-sdk cargo package dry-run (depends on unpublished in-repo ctx-protocol)"

printf '\n==> Go module compilation and tests are modeled by pinned rules_go targets\n'

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
