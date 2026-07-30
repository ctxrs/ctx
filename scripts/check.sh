#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

run_bazel() {
  local command_name="$1"
  shift
  local command=("${command_name}")
  if (( force_rerun )) && [[ "${command_name}" == "test" || "${command_name}" == "coverage" ]]; then
    command+=(--cache_test_results=no)
  fi
  command+=("$@")
  printf '==> scripts/bazelw'
  printf ' %q' "${command[@]}"
  printf '\n'
  scripts/bazelw "${command[@]}"
}

usage() {
  cat <<'USAGE'
usage: scripts/check.sh [--mode MODE] [--force-rerun]
       scripts/check.sh --list-modes
       scripts/check.sh -- BAZEL_ARGS...

Modes:
  ci         merge validation: lint plus deterministic tests and audits
  nightly    ci plus serialized upgrade, daemon, and fault qualification
  release    nightly qualification for a release candidate

Cargo is not invoked by these modes; Bazel is the build and test authority.
--force-rerun disables test-result reuse without deleting compilation caches.
USAGE
}

mode="ci"
force_rerun=0
while (( "$#" > 0 )); do
  case "$1" in
    --mode=*) mode="${1#--mode=}"; shift ;;
    --mode)
      shift
      (( "$#" > 0 )) || { printf 'missing value for --mode\n' >&2; exit 2; }
      mode="$1"
      shift
      ;;
    --force-rerun) force_rerun=1; shift ;;
    --list-modes) printf '%s\n' ci nightly release; exit 0 ;;
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

case "${mode}" in
  ci)
    run_bazel build //... --config=ci --config=lint
    run_bazel test //:ci --config=ci
    ;;
  nightly)
    run_bazel build //... --config=ci --config=lint
    run_bazel test //:nightly --config=ci
    ;;
  release)
    run_bazel build //... --config=ci --config=lint
    run_bazel test //:release --config=ci
    ;;
  *) printf 'unknown check mode: %s\n' "${mode}" >&2; usage >&2; exit 2 ;;
esac
