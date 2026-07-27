#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"
source_repo="${CTX_RELEASE_FIXTURE_SOURCE_REPO:-$repo_root}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-released-store-fixtures.XXXXXX")"
target_dir="${CTX_RELEASE_FIXTURE_CARGO_TARGET_DIR:-$tmp_dir/cargo-target}"

cleanup() {
  if [[ "${CTX_RELEASE_FIXTURE_KEEP_TMP:-0}" == "1" ]]; then
    echo "preserved generator workspace: $tmp_dir" >&2
    return
  fi
  rm -rf -- "$tmp_dir"
}
trap cleanup EXIT

command -v cargo >/dev/null
command -v git >/dev/null
command -v sha256sum >/dev/null
command -v tar >/dev/null
command -v zstd >/dev/null

releases=(
  "v0.24.0:460ad6f1c5fe5dd4465f0f1ddfb6c95c3d7a55c1"
  "v0.25.0:228e05fa0fd058822be7a362acd65cacdad24356"
)

staged="$tmp_dir/staged"
mkdir -p "$staged"

for release_spec in "${releases[@]}"; do
  release="${release_spec%%:*}"
  expected_commit="${release_spec#*:}"
  actual_commit="$(git -C "$source_repo" rev-parse "$release^{commit}")"
  if [[ "$actual_commit" != "$expected_commit" ]]; then
    echo "$release resolved to $actual_commit, expected $expected_commit" >&2
    exit 1
  fi
  schema_line="$(git -C "$source_repo" show "$release:crates/ctx-history-store/src/lib.rs" |
    grep -F 'const SCHEMA_VERSION: i64 = 46;' || true)"
  if [[ -z "$schema_line" ]]; then
    echo "$release is not the expected released schema v46 source" >&2
    exit 1
  fi
  if git -C "$source_repo" grep -q -E \
    'verified_content_locators_v1|provider_source_locators|capture_source_provider_routes' \
    "$release" -- crates/ctx-history-capture crates/ctx-history-store; then
    echo "$release unexpectedly contains the v0.26 verified-content route contract" >&2
    exit 1
  fi

  release_root="$tmp_dir/$release"
  source_root="$release_root/source"
  mkdir -p "$source_root"
  git -C "$source_repo" archive "$actual_commit" | tar -x -C "$source_root"
  wrapper_root="$release_root/wrapper"
  mkdir -p "$wrapper_root/src"
  cp "$script_dir/generator.rs" "$wrapper_root/src/main.rs"
  cp "$source_root/Cargo.lock" "$wrapper_root/Cargo.lock"
  cat >"$wrapper_root/Cargo.toml" <<EOF
[package]
name = "released-store-fixture-generator"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
chrono = { version = "0.4", default-features = false, features = ["std", "serde"] }
ctx-history-core = { path = "../source/crates/ctx-history-core" }
ctx-history-store = { path = "../source/crates/ctx-history-store" }
rusqlite = { version = "0.32", features = ["bundled", "hooks", "limits"] }
serde_json = "1.0"
uuid = { version = "1.10", features = ["serde", "v4", "v7"] }
EOF
  cat >>"$wrapper_root/Cargo.lock" <<'EOF'

[[package]]
name = "released-store-fixture-generator"
version = "0.0.0"
dependencies = [
 "chrono",
 "ctx-history-core",
 "ctx-history-store",
 "rusqlite",
 "serde_json",
 "uuid",
]
EOF
  CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="$target_dir" \
    cargo metadata --quiet --offline --format-version 1 \
    --manifest-path "$wrapper_root/Cargo.toml" >/dev/null

  first="$release_root/first.sqlite"
  second="$release_root/second.sqlite"
  for output in "$first" "$second"; do
    CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="$target_dir" \
      cargo run --quiet --locked --offline \
      --manifest-path "$wrapper_root/Cargo.toml" -- "$output" "$release"
  done
  if ! cmp --silent "$first" "$second"; then
    echo "$release fixture generation was not byte-reproducible" >&2
    exit 1
  fi

  compressed="$staged/$release-work.sqlite.zst"
  zstd -19 --threads=1 --no-progress --force "$first" -o "$compressed"
  sha256sum "$compressed" | sed "s#  .*/#  #" >>"$staged/SHA256SUMS"
  sha256sum "$first" | sed "s#  .*/first.sqlite#  $release-work.sqlite#" \
    >>"$staged/SHA256SUMS"
done

install -m 0644 "$staged/v0.24.0-work.sqlite.zst" "$script_dir/"
install -m 0644 "$staged/v0.25.0-work.sqlite.zst" "$script_dir/"
install -m 0644 "$staged/SHA256SUMS" "$script_dir/"
(cd "$script_dir" && sha256sum --check SHA256SUMS --ignore-missing)
