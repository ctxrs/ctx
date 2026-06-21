#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <bazel args...>" >&2
  exit 64
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(dirname "${SCRIPT_DIR}")"
CORE_ROOT="${REPO_ROOT}/core"
BAZELISK_BIN="${CORE_ROOT}/node_modules/.bin/bazelisk"

section() {
  if [[ -n "${BUILDKITE:-}" ]]; then
    echo "--- $*"
  else
    echo "$*"
  fi
}

bootstrap_node_deps() {
  section "Bootstrap pnpm dependencies"
  corepack enable
  pnpm -C "${CORE_ROOT}" install --frozen-lockfile --prefer-offline --store-dir "${PNPM_STORE_DIR:-${HOME}/.cache/pnpm-store}"
}

cd "${REPO_ROOT}"

if [[ -n "${BUILDKITE:-}" ]]; then
  export CTX_E2E_AUTH_TOKEN="ctx-buildkite-local-e2e-token"
fi

if [[ -n "${BUILDKITE:-}" || ! -x "${BAZELISK_BIN}" ]]; then
  bootstrap_node_deps
fi

if [[ ! -x "${BAZELISK_BIN}" ]]; then
  echo "error: expected Bazelisk at ${BAZELISK_BIN}" >&2
  exit 127
fi

has_jobs_arg() {
  local arg
  for arg in "$@"; do
    if [[ "${arg}" == "--jobs" || "${arg}" == --jobs=* ]]; then
      return 0
    fi
  done
  return 1
}

bazel_args=("$@")
if [[ -n "${BUILDKITE:-}" && "$#" -gt 0 && ( "$1" == "test" || "$1" == "build" ) ]] \
  && ! has_jobs_arg "$@"; then
  bazel_args=("$1" "--jobs=${CTX_BAZEL_JOBS:-4}" "${@:2}")
fi

section "Run Bazel: ${bazel_args[*]}"
exec "${BAZELISK_BIN}" "${bazel_args[@]}"
