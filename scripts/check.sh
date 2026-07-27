#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

run_bazel() {
  printf '==> scripts/bazelw'
  printf ' %q' "$@"
  printf '\n'
  scripts/bazelw "$@"
}

usage() {
  cat <<'USAGE'
usage: scripts/check.sh [--mode MODE]
       scripts/check.sh --list-modes
       scripts/check.sh -- BAZEL_ARGS...

Modes:
  fast       format, policy, SDK, and native Rust smoke tests
  presubmit  fast plus the complete native Rust test graph
  smoke      fast plus fresh-home and provider fixture flows
  ci         presubmit plus native clippy and release/content gates

Cargo is not invoked by these modes; Bazel is the build and test authority.
USAGE
}

mode="ci"
while (( "$#" > 0 )); do
  case "$1" in
    --mode=*) mode="${1#--mode=}"; shift ;;
    --mode)
      shift
      (( "$#" > 0 )) || { printf 'missing value for --mode\n' >&2; exit 2; }
      mode="$1"
      shift
      ;;
    --list-modes) printf '%s\n' fast presubmit smoke ci; exit 0 ;;
    -h|--help) usage; exit 0 ;;
    --)
      shift
      (( "$#" > 0 )) || { printf 'missing Bazel arguments after --\n' >&2; exit 2; }
      run_bazel "$@"
      exit $?
      ;;
    *) run_bazel "$@"; exit $? ;;
  esac
done

run_bazel query //...

case "${mode}" in
  fast) run_bazel test //:fast --config=ci ;;
  presubmit) run_bazel test //:presubmit --config=ci ;;
  smoke) run_bazel test //:smoke --config=ci ;;
  ci)
    run_bazel build //... --config=ci --config=lint
    run_bazel test //:ci --config=ci
    ;;
  *) printf 'unknown check mode: %s\n' "${mode}" >&2; usage >&2; exit 2 ;;
esac
