#!/usr/bin/env bash
set -euo pipefail

if [[ $# -gt 1 ]]; then
  echo "usage: $0 [expected-host-arch]" >&2
  exit 64
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(dirname "${SCRIPT_DIR}")"
CORE_ROOT="${REPO_ROOT}/core"
DESKTOP_TAURI_ROOT="${CORE_ROOT}/apps/desktop/src-tauri"
EXPECTED_HOST_ARCH="${1:-${CTX_EXPECTED_HOST_ARCH:-}}"

require_command() {
  local command_name="$1"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "error: required command '${command_name}' is missing from PATH" >&2
    exit 127
  fi
}

require_pkg_config_one_of() {
  local label="$1"
  shift
  local package
  for package in "$@"; do
    if pkg-config --exists "${package}"; then
      return 0
    fi
  done
  echo "error: missing Linux desktop package dependency '${label}' (checked pkg-config packages: $*)" >&2
  exit 1
}

reject_release_signing_env() {
  local names=(
    APPLE_API_ISSUER
    APPLE_API_KEY
    APPLE_API_KEY_ID
    APPLE_CERTIFICATE
    APPLE_CERTIFICATE_PASSWORD
    APPLE_SIGNING_IDENTITY
    CSC_KEY_PASSWORD
    CSC_LINK
    CTX_DESKTOP_EMBEDDED_UPDATER_PUBKEY_B64
    TAURI_KEY_PASSWORD
    TAURI_PRIVATE_KEY
    TAURI_SIGNING_PRIVATE_KEY
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  )
  local name
  local present=()
  for name in "${names[@]}"; do
    if [[ -n "${!name:-}" ]]; then
      present+=("${name}")
    fi
  done
  if (( ${#present[@]} > 0 )); then
    echo "error: desktop release verification package lane refuses signing/notarization/updater env: ${present[*]}" >&2
    exit 2
  fi
}

verify_host_arch() {
  if [[ -z "${EXPECTED_HOST_ARCH}" ]]; then
    return 0
  fi

  local actual_arch
  actual_arch="$(uname -m)"
  if [[ "${actual_arch}" != "${EXPECTED_HOST_ARCH}" ]]; then
    echo "error: expected host architecture ${EXPECTED_HOST_ARCH}, got ${actual_arch}" >&2
    exit 1
  fi
}

verify_linux_packaging_deps() {
  require_command pkg-config
  require_command file
  require_command curl
  require_pkg_config_one_of "GTK 3" gtk+-3.0
  require_pkg_config_one_of "WebKitGTK" webkit2gtk-4.1 webkit2gtk-4.0
  require_pkg_config_one_of "app indicator" ayatana-appindicator3-0.1 appindicator3-0.1
  require_pkg_config_one_of "librsvg" librsvg-2.0
}

copy_tree() {
  local source="$1"
  local dest="$2"
  if [[ ! -d "${source}" ]]; then
    echo "error: expected directory ${source}" >&2
    exit 1
  fi
  rm -rf "${dest}"
  mkdir -p "$(dirname "${dest}")"
  cp -R "${source}" "${dest}"
}

copy_executable() {
  local source="$1"
  local dest="$2"
  if [[ ! -x "${source}" ]]; then
    echo "error: expected executable ${source}" >&2
    exit 1
  fi
  mkdir -p "$(dirname "${dest}")"
  cp "${source}" "${dest}"
  chmod 0755 "${dest}"
}

reject_release_signing_env
verify_host_arch
require_command cargo

case "$(uname -s)" in
  Linux)
    verify_linux_packaging_deps
    ;;
  Darwin)
    ;;
  *)
    echo "error: unsupported desktop package host OS $(uname -s)" >&2
    exit 1
    ;;
esac

corepack enable
pnpm -C "${CORE_ROOT}" install --frozen-lockfile --prefer-offline --store-dir "${PNPM_STORE_DIR:-${HOME}/.cache/pnpm-store}"

cd "${REPO_ROOT}"
".buildkite/run-bazel.sh" build \
  //core/apps/web:build \
  //core/crates/ctx-http:ctx \
  //core/crates/ctx-mcp:ctx-mcp

copy_tree "${REPO_ROOT}/bazel-bin/core/apps/web/dist" "${DESKTOP_TAURI_ROOT}/web/dist"

mkdir -p "${DESKTOP_TAURI_ROOT}/bin"
find "${DESKTOP_TAURI_ROOT}/bin" -mindepth 1 -maxdepth 1 ! -name ".gitkeep" -exec rm -rf {} +
copy_executable "${REPO_ROOT}/bazel-bin/core/crates/ctx-http/ctx" "${DESKTOP_TAURI_ROOT}/bin/ctx-daemon"
copy_executable "${REPO_ROOT}/bazel-bin/core/crates/ctx-mcp/ctx-mcp" "${DESKTOP_TAURI_ROOT}/bin/ctx-mcp"

pnpm -C "${CORE_ROOT}" desktop:build
