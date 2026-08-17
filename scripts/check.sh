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

run_rust_crate_size_gate() {
  local selected_mode="$1"
  if [[ "${selected_mode}" == release ]]; then
    local candidate_commit
    candidate_commit="$(git rev-parse --verify HEAD^{commit})"
    printf '==> local physical Rust crate-size exact-candidate validation\n'
    scripts/bazelw run //:rust_crate_size_preflight -- \
      --exact-candidate "${candidate_commit}" "${repo_root}"
  else
    printf '==> local physical Rust crate-size live-worktree preflight\n'
    scripts/bazelw run //:rust_crate_size_preflight -- --preflight "${repo_root}"
  fi
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

The physical Rust crate-size gate runs locally before each named mode. CI and
nightly count the live worktree directly; release additionally verifies the
exact clean checked-out candidate. The gate has one fixed limit and no
checked-in size inventory, warning tier, grandfather ledger, or moving-base
comparison.
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
  ci|nightly|release) ;;
  *) printf 'unknown check mode: %s\n' "${mode}" >&2; usage >&2; exit 2 ;;
esac

run_rust_crate_size_gate "${mode}"

case "${mode}" in
  ci)
    run_bazel build //... --config=ci
    run_bazel test //:ci_tests --config=test
    ;;
  nightly)
    run_bazel build //... --config=ci
    run_bazel test //:nightly_tests --config=test
    ;;
  release)
    run_bazel build //... --config=ci
    run_bazel test //:nightly_tests --config=test
    ;;
esac
