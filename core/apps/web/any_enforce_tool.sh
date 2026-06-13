#!/usr/bin/env bash
set -euo pipefail

resolve_repo_root() {
  local candidate
  for candidate in \
    "${RUNFILES_DIR:-}/_main" \
    "${RUNFILES_DIR:-}/${TEST_WORKSPACE:-}" \
    "${BUILD_WORKSPACE_DIRECTORY:-}"
  do
    if [[ -n "${candidate}" && -f "${candidate}/core/package.json" && -f "${candidate}/MODULE.bazel" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

resolve_node() {
  local candidate=""
  for candidate in \
    "${NODE:-}" \
    "$(command -v node 2>/dev/null || true)" \
    "${HOME:-}"/.local/node/*/bin/node \
    "/opt/homebrew/bin/node" \
    "/usr/local/bin/node" \
    "/usr/bin/node" \
    "/bin/node"
  do
    if [[ -n "${candidate}" && -x "${candidate}" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  echo "error: failed to locate node for any_enforce" >&2
  return 127
}

REPO_ROOT="$(resolve_repo_root)"
if [[ -z "${REPO_ROOT}" ]]; then
  echo "error: failed to locate Bazel runfiles repo root for any_enforce" >&2
  exit 1
fi

WEB_ROOT="${REPO_ROOT}/core/apps/web"
NODE_BIN="$(resolve_node)"

cd "${WEB_ROOT}"
exec "${NODE_BIN}" ./scripts/explicit-any-report.mjs --enforce --enforce-mode zero --format table
