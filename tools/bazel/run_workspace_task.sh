#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: $0 <workspace-relative-dir> <command> <args...>" >&2
  exit 64
fi

resolve_repo_root() {
  local candidate
  for candidate in \
    "${RUNFILES_DIR:-}/_main" \
    "${RUNFILES_DIR:-}/${TEST_WORKSPACE:-}" \
    "${BUILD_WORKSPACE_DIRECTORY:-}"
  do
    if [[ -n "${candidate}" && -f "${candidate}/core/package.json" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

resolve_script_repo_root() {
  local script_dir=""
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
  dirname "$(dirname "${script_dir}")"
}

normalize_workspace_root_candidate() {
  local candidate="${1:-}"
  local leaf=""
  [[ -n "${candidate}" ]] || return 1
  if [[ -f "${candidate}/core/package.json" ]]; then
    printf '%s\n' "${candidate}"
    return 0
  fi
  leaf="${candidate##*/}"
  if [[ "${leaf}" == "core" && -f "${candidate}/package.json" ]]; then
    dirname "${candidate}"
    return 0
  fi
  return 1
}

resolve_real_workspace_root() {
  local candidate=""
  local normalized=""
  for candidate in \
    "${BUILD_WORKSPACE_DIRECTORY:-}" \
    "${CTX_REAL_WORKSPACE_ROOT:-}" \
    "${INIT_CWD:-}" \
    "${PNPM_SCRIPT_SRC_DIR:-}" \
    "${PWD:-}" \
    "$(resolve_script_repo_root)"
  do
    normalized="$(normalize_workspace_root_candidate "${candidate}" || true)"
    if [[ -n "${normalized}" ]]; then
      printf '%s\n' "${normalized}"
      return 0
    fi
  done
  printf '%s\n' "${RUNFILES_REPO_ROOT}"
}

link_real_workspace_dir() {
  local real_root="$1"
  local temp_root="$2"
  local rel_path="$3"
  if [[ ! -e "${real_root}/${rel_path}" ]]; then
    return 0
  fi
  mkdir -p "$(dirname "${temp_root}/${rel_path}")"
  ln -s "${real_root}/${rel_path}" "${temp_root}/${rel_path}"
}

resolve_workspace_command() {
  local command_name="$1"
  local candidate=""

  if [[ "$command_name" == */* ]]; then
    if [[ -x "$command_name" ]]; then
      printf '%s\n' "$command_name"
      return 0
    fi
    echo "failed to locate executable '${command_name}' for workspace task" >&2
    return 127
  fi

  if [[ "$command_name" == "node" ]]; then
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
      if [[ -n "$candidate" && -x "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
      fi
    done
  fi

  candidate="$(command -v "$command_name" 2>/dev/null || true)"
  if [[ -n "$candidate" && -x "$candidate" ]]; then
    printf '%s\n' "$candidate"
    return 0
  fi

  echo "failed to locate executable '${command_name}' for workspace task" >&2
  return 127
}

RUNFILES_REPO_ROOT="$(resolve_repo_root)"
if [[ -z "${RUNFILES_REPO_ROOT}" ]]; then
  echo "failed to locate Bazel runfiles repo root" >&2
  exit 1
fi

REAL_WORKSPACE_ROOT="$(resolve_real_workspace_root)"
TMP_WORKSPACE="$(mktemp -d "${TMPDIR:-/tmp}/ctx-bazel-workspace.XXXXXX")"
trap 'rm -rf "${TMP_WORKSPACE}"' EXIT

RSYNC_EXCLUDES=(
  "--exclude=.git"
  "--exclude=.ctx"
  "--exclude=node_modules"
  "--exclude=bazel-bin"
  "--exclude=bazel-out"
  "--exclude=bazel-testlogs"
  "--exclude=core/node_modules"
  "--exclude=core/apps/web/node_modules"
  "--exclude=core/apps/desktop/node_modules"
  "--exclude=core/target"
  "--exclude=core/apps/web/dist"
  "--exclude=core/apps/desktop/src-tauri/bin"
  "--exclude=core/apps/web/playwright-report"
  "--exclude=core/apps/web/test-results"
  "--exclude=core/apps/web/e2e/playwright-report"
  "--exclude=core/apps/web/e2e/test-results"
)

mkdir -p "${TMP_WORKSPACE}"
if [[ "${OSTYPE:-}" == darwin* ]]; then
  # AppleDouble and xattr propagation can stall large workspace mirrors on macOS.
  COPYFILE_DISABLE=1 COPY_EXTENDED_ATTRIBUTES_DISABLE=1 \
    tar -C "${RUNFILES_REPO_ROOT}" "${RSYNC_EXCLUDES[@]}" -cf - . |
    COPYFILE_DISABLE=1 COPY_EXTENDED_ATTRIBUTES_DISABLE=1 \
      tar -C "${TMP_WORKSPACE}" -xf -
else
  tar -C "${RUNFILES_REPO_ROOT}" "${RSYNC_EXCLUDES[@]}" -cf - . |
    tar -C "${TMP_WORKSPACE}" -xf -
fi
# Keep repo-authored docs available to contract tests without copying all .ctx attachments.
link_real_workspace_dir "${RUNFILES_REPO_ROOT}" "${TMP_WORKSPACE}" ".ctx/docs"
link_real_workspace_dir "${REAL_WORKSPACE_ROOT}" "${TMP_WORKSPACE}" "core/node_modules"
link_real_workspace_dir "${REAL_WORKSPACE_ROOT}" "${TMP_WORKSPACE}" "core/apps/desktop/node_modules"
link_real_workspace_dir "${REAL_WORKSPACE_ROOT}" "${TMP_WORKSPACE}" "core/apps/web/node_modules"

REL_DIR="$1"
shift
COMMAND_NAME="$1"
shift
RESOLVED_COMMAND="$(resolve_workspace_command "$COMMAND_NAME")"

cd "${TMP_WORKSPACE}/${REL_DIR}"
exec "$RESOLVED_COMMAND" "$@"
