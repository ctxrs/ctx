#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
input_root="$repo_root"
if [[ -n "${RUNFILES_DIR:-}" ]]; then
  input_root="$RUNFILES_DIR/${TEST_WORKSPACE:-_main}"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  input_root="$TEST_SRCDIR/${TEST_WORKSPACE:-_main}"
fi

resolve_input() {
  if [[ "$1" = /* ]]; then
    printf '%s\n' "$1"
  else
    printf '%s/%s\n' "$input_root" "$1"
  fi
}

if [[ "${1:-}" == "--ctx-tool-bin" ]]; then
  shift
  (( $# == 6 )) || { printf 'Bazel cargo-shear wrapper is missing declared inputs\n' >&2; exit 64; }
  shear_bin="$(resolve_input "$1")"
  cargo_bin="$(resolve_input "$2")"
  rustc_bin="$(resolve_input "$3")"
  prepare_vendor="$(resolve_input "$4")"
  vendor_manifest="$(resolve_input "$5")"
  source_project="$input_root/$6"
  temp_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-cargo-shear.XXXXXXXX")"
  trap 'rm -rf "$temp_root"' EXIT
  project="$temp_root/project"
  cp -RL -- "$source_project" "$project"
  cargo_home="$temp_root/cargo-home"
  "$prepare_vendor" "$project/Cargo.lock" "$vendor_manifest" "$cargo_home"
  env \
    "HOME=$temp_root/home" \
    "CARGO_HOME=$cargo_home" \
    "CARGO_NET_OFFLINE=true" \
    "CARGO=$cargo_bin" \
    "RUSTC=$rustc_bin" \
    "PATH=/usr/bin:/bin" \
    "$shear_bin" --frozen --deny-warnings "$project"
  exit $?
fi

cd "$repo_root"
exec scripts/bazelw test //:cargo_shear_check --config=ci --test_output=all --nocache_test_results
