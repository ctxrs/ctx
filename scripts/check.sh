#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if [[ -z "${HOME:-}" ]]; then
  export HOME="${repo_root}/target/bazel-home"
fi
mkdir -p "${HOME}"

# shellcheck source=scripts/ci-common.sh
source "${repo_root}/scripts/ci-common.sh"

bazel_bin=""

init_bazel() {
  if [[ -n "${bazel_bin}" ]]; then
    return 0
  fi

  ctx_init_resource_env
  ctx_print_resource_env

  bazel_bin="$(ctx_find_bazel)" || {
    printf 'bazel or bazelisk is required; set CTX_BOOTSTRAP_BAZELISK=1 to allow bootstrap\n' >&2
    exit 127
  }
}

run_bazel() {
  init_bazel
  printf '==> %s' "${bazel_bin}"
  printf ' %q' "$@"
  printf '\n'
  "${bazel_bin}" "$@"
}

usage() {
  cat <<'USAGE'
usage: scripts/check.sh [--mode MODE]
       scripts/check.sh --list-modes
       scripts/check.sh -- BAZEL_ARGS...

Modes:
  fast       formatting, docs, static package surface, and CLI contracts
  presubmit  fast plus clippy, workspace tests, fresh-home, and provider smoke
  smoke      fast plus fresh-home and provider smoke
  ci         presubmit plus release/content gates used by Buildkite
USAGE
}

list_modes() {
  printf '%s\n' fast presubmit smoke ci
}

mode="ci"

while (( "$#" > 0 )); do
  case "$1" in
    --mode=*)
      mode="${1#--mode=}"
      shift
      ;;
    --mode)
      shift
      if (( "$#" == 0 )); then
        printf 'missing value for --mode\n' >&2
        usage >&2
        exit 2
      fi
      mode="$1"
      shift
      ;;
    --list-modes)
      list_modes
      exit 0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      if (( "$#" == 0 )); then
        printf 'missing Bazel arguments after --\n' >&2
        usage >&2
        exit 2
      fi
      run_bazel "$@"
      exit $?
      ;;
    *)
      run_bazel "$@"
      exit $?
      ;;
  esac
done

run_bazel query //...

case "${mode}" in
  fast)
    run_bazel test //:fast --config=ci
    ;;
  presubmit)
    run_bazel test //:presubmit --config=ci
    ;;
  ci)
    run_bazel test //:ci --config=ci
    ;;
  smoke)
    run_bazel test //:smoke --config=ci
    ;;
  *)
    printf 'unknown check mode: %s\n' "${mode}" >&2
    usage >&2
    exit 2
    ;;
esac
