#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <workspace-relative-dir> <node args...>" >&2
  exit 64
fi

resolve_repo_root() {
  local candidate=""
  local manifest_root=""
  local launcher_runfiles_root=""
  local manifest_line=""
  local logical_path=""
  local physical_path=""
  local workspace_manifest_path=""
  if [[ -n "${RUNFILES_MANIFEST_FILE:-}" ]]; then
    case "${RUNFILES_MANIFEST_FILE}" in
      */MANIFEST)
        manifest_root="${RUNFILES_MANIFEST_FILE%/MANIFEST}"
        ;;
      *.runfiles_manifest)
        manifest_root="${RUNFILES_MANIFEST_FILE%_manifest}"
        ;;
    esac
  fi
  if [[ -e "${0}.runfiles" ]]; then
    launcher_runfiles_root="${0}.runfiles"
  fi
  for candidate in \
    "${launcher_runfiles_root}/_main" \
    "${launcher_runfiles_root}/${TEST_WORKSPACE:-}" \
    "${RUNFILES_DIR:-}/_main" \
    "${RUNFILES_DIR:-}/${TEST_WORKSPACE:-}" \
    "${manifest_root}/_main" \
    "${manifest_root}/${TEST_WORKSPACE:-}"
  do
    if [[ -n "${candidate}" && -f "${candidate}/core/package.json" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  if [[ -f "${RUNFILES_MANIFEST_FILE:-}" ]]; then
    workspace_manifest_path="${TEST_WORKSPACE:+${TEST_WORKSPACE}/core/package.json}"
    while IFS= read -r manifest_line; do
      logical_path="${manifest_line%% *}"
      physical_path="${manifest_line#* }"
      if [[ "${logical_path}" == "${physical_path}" ]]; then
        continue
      fi
      if [[ "${logical_path}" != "_main/core/package.json" \
        && ( -z "${workspace_manifest_path}" || "${logical_path}" != "${workspace_manifest_path}" ) ]]; then
        continue
      fi
      if [[ "${physical_path}" == */core/package.json ]]; then
        candidate="${physical_path%/core/package.json}"
        if [[ -f "${candidate}/core/package.json" ]]; then
          printf '%s\n' "${candidate}"
          return 0
        fi
      fi
    done < "${RUNFILES_MANIFEST_FILE}"
  fi
  echo "failed to locate Bazel repo root for node task" >&2
  return 1
}

resolve_node_command() {
  local candidate=""
  for candidate in \
    "${NODE:-}" \
    "$(command -v node 2>/dev/null || true)" \
    "${HOME:-}"/.local/node/*/bin/node \
    "/var/lib/buildkite-agent/.local/node"/*/bin/node \
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

  echo "failed to locate node for Bazel node task" >&2
  return 127
}

REPO_ROOT="$(resolve_repo_root)"
NODE_BIN="$(resolve_node_command)"
REL_DIR="$1"
shift

cd "${REPO_ROOT}/${REL_DIR}"
exec "${NODE_BIN}" "$@"
