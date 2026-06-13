#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <output-dir>" >&2
  exit 64
fi

OUTPUT_DIR="$1"
if [[ -z "${BUILD_WORKSPACE_DIRECTORY:-}" ]]; then
  echo "error: BUILD_WORKSPACE_DIRECTORY is required" >&2
  exit 1
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

copy_runfiles_file() {
  local runfiles_root="$1"
  local temp_root="$2"
  local rel_path="$3"
  if [[ ! -f "${runfiles_root}/${rel_path}" ]]; then
    return 0
  fi
  mkdir -p "$(dirname "${temp_root}/${rel_path}")"
  cp "${runfiles_root}/${rel_path}" "${temp_root}/${rel_path}"
}

link_runfiles_path() {
  local runfiles_root="$1"
  local temp_root="$2"
  local rel_path="$3"
  if [[ ! -e "${runfiles_root}/${rel_path}" ]]; then
    return 0
  fi
  mkdir -p "$(dirname "${temp_root}/${rel_path}")"
  ln -s "${runfiles_root}/${rel_path}" "${temp_root}/${rel_path}"
}

prepare_minimal_workspace() {
  local runfiles_root="$1"
  local real_root="$2"
  local temp_root="$3"

  mkdir -p "${temp_root}/core/apps/web"
  copy_runfiles_file "${runfiles_root}" "${temp_root}" "core/package.json"
  copy_runfiles_file "${runfiles_root}" "${temp_root}" "core/pnpm-lock.yaml"
  copy_runfiles_file "${runfiles_root}" "${temp_root}" "core/pnpm-workspace.yaml"
  link_runfiles_path "${runfiles_root}" "${temp_root}" "core/packages"

  while IFS= read -r entry; do
    local name
    name="$(basename "${entry}")"
    case "${name}" in
      dist|node_modules|coverage|playwright-report|test-results)
        continue
        ;;
    esac
    if [[ -d "${entry}" ]]; then
      link_runfiles_path "${runfiles_root}" "${temp_root}" "core/apps/web/${name}"
    elif [[ -f "${entry}" ]]; then
      copy_runfiles_file "${runfiles_root}" "${temp_root}" "core/apps/web/${name}"
    fi
  done < <(find "${runfiles_root}/core/apps/web" -mindepth 1 -maxdepth 1 | sort)

  link_real_workspace_dir "${real_root}" "${temp_root}" "core/node_modules"
  link_real_workspace_dir "${real_root}" "${temp_root}" "core/apps/web/node_modules"
}

RUNFILES_REPO_ROOT="$(resolve_repo_root)"
if [[ -z "${RUNFILES_REPO_ROOT}" ]]; then
  echo "failed to locate Bazel runfiles repo root" >&2
  exit 1
fi

REAL_WORKSPACE_ROOT="${BUILD_WORKSPACE_DIRECTORY}"
TMP_WORKSPACE="$(mktemp -d "${TMPDIR:-/tmp}/ctx-bazel-web-dist.XXXXXX")"
trap 'rm -rf "${TMP_WORKSPACE}"' EXIT

mkdir -p "${TMP_WORKSPACE}"
prepare_minimal_workspace "${RUNFILES_REPO_ROOT}" "${REAL_WORKSPACE_ROOT}" "${TMP_WORKSPACE}"

VITE_BIN="${TMP_WORKSPACE}/core/apps/web/node_modules/.bin/vite"
if [[ ! -x "${VITE_BIN}" ]]; then
  echo "error: expected web-local vite at ${VITE_BIN}" >&2
  exit 1
fi

cd "${TMP_WORKSPACE}/core/apps/web"
"${VITE_BIN}" build

if [[ ! -d dist ]]; then
  echo "error: expected apps/web/dist after pnpm build" >&2
  exit 1
fi

rm -rf "${OUTPUT_DIR}"
mkdir -p "$(dirname "${OUTPUT_DIR}")"
cp -R dist "${OUTPUT_DIR}"
