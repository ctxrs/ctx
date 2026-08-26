#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
input_root="$repo_root"
if [[ -n "${RUNFILES_DIR:-}" ]]; then
  input_root="$RUNFILES_DIR/${TEST_WORKSPACE:-_main}"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  input_root="$TEST_SRCDIR/${TEST_WORKSPACE:-_main}"
elif [[ -d "${BASH_SOURCE[0]}.runfiles" ]]; then
  input_root="${BASH_SOURCE[0]}.runfiles/${TEST_WORKSPACE:-_main}"
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
  (( $# >= 4 )) || { printf 'Bazel cargo-fixit wrapper is missing declared inputs\n' >&2; exit 64; }
  fixit_bin="$(resolve_input "$1")"
  cargo_bin="$(resolve_input "$2")"
  rustc_bin="$(resolve_input "$3")"
  project="$4"
  shift 4
  workspace="${BUILD_WORKSPACE_DIRECTORY:-$repo_root}"
  args=(--clippy --workspace --all-targets --locked "$@")
  cd "$workspace/$project"
  export CARGO="$cargo_bin"
  export RUSTC="$rustc_bin"
  export PATH="$(dirname "$cargo_bin"):/usr/bin:/bin"
  exec "$fixit_bin" fixit "${args[@]}"
fi

cd "$repo_root"
exec scripts/bazelw run //:cargo_fixit -- "$@"
