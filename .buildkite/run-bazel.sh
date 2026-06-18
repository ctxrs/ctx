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

if [[ -n "${BUILDKITE:-}" || ! -x "${BAZELISK_BIN}" ]]; then
  bootstrap_node_deps
fi

if [[ ! -x "${BAZELISK_BIN}" ]]; then
  echo "error: expected Bazelisk at ${BAZELISK_BIN}" >&2
  exit 127
fi

section "Run Bazel: $*"
exec "${BAZELISK_BIN}" "$@"
