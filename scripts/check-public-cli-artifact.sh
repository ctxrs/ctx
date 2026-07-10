#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-public-cli-artifact.sh PLATFORM [ARTIFACT_DIR]

Checks one locally staged public ctx CLI artifact, including a present or
required ONNX Runtime sidecar. Runtime archives must have the exact loader
layout and native architecture/ABI expected for PLATFORM.
USAGE
}

platform="${1:-}"
artifact_dir="${2:-target/public-cli-artifacts}"
if [[ -z "${platform}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi

case "${platform}" in
  linux-x64)
    binary_name="ctx"
    ;;
  linux-aarch64)
    binary_name="ctx-linux-aarch64"
    ;;
  macos-arm64)
    binary_name="ctx-macos-arm64"
    ;;
  macos-x64)
    binary_name="ctx-macos-x64"
    ;;
  windows-x64)
    binary_name="ctx.exe"
    ;;
  freebsd-x64)
    binary_name="ctx-freebsd-x64"
    ;;
  *)
    usage
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

runtime_asset_name_for_platform() {
  case "$1" in
    linux-x64) printf 'ctx-onnxruntime-linux-x64.tar.zst\n' ;;
    linux-aarch64) printf 'ctx-onnxruntime-linux-aarch64.tar.zst\n' ;;
    macos-arm64) printf 'ctx-onnxruntime-macos-arm64.tar.zst\n' ;;
    macos-x64) printf 'ctx-onnxruntime-macos-x64.tar.zst\n' ;;
    windows-x64) printf 'ctx-onnxruntime-windows-x64.zip\n' ;;
    freebsd-x64) printf 'ctx-onnxruntime-freebsd-x64.tar.zst\n' ;;
    *) return 1 ;;
  esac
}

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return
  fi

  shasum -a 256 "${path}" | awk '{ print $1 }'
}

check_sha256_sidecar() {
  local artifact_path="$1"
  local sha_path="${artifact_path}.sha256"
  local expected_sha actual_sha actual_sha_lower expected_sha_lower

  if [[ ! -s "${sha_path}" ]]; then
    printf 'public artifact SHA-256 sidecar missing or empty: %s\n' "${sha_path}" >&2
    exit 1
  fi

  expected_sha="$(tr -d '[:space:]' < "${sha_path}")"
  if [[ ! "${expected_sha}" =~ ^[0-9a-fA-F]{64}$ ]]; then
    printf 'public artifact SHA-256 sidecar is not a digest: %s\n' "${sha_path}" >&2
    exit 1
  fi

  actual_sha="$(sha256_file "${artifact_path}")"
  actual_sha_lower="$(printf '%s' "${actual_sha}" | tr 'A-F' 'a-f')"
  expected_sha_lower="$(printf '%s' "${expected_sha}" | tr 'A-F' 'a-f')"
  if [[ "${actual_sha_lower}" != "${expected_sha_lower}" ]]; then
    printf 'public artifact checksum mismatch for %s: expected %s got %s\n' \
      "${artifact_path}" "${expected_sha}" "${actual_sha}" >&2
    exit 1
  fi

  printf '%s\n' "${actual_sha}"
}

check_optional_runtime_asset() {
  local required="${CTX_ONNXRUNTIME_ASSET_REQUIRED:-0}"
  local asset_name runtime_path actual_sha

  case "${required}" in
    0|1) ;;
    *)
      printf 'CTX_ONNXRUNTIME_ASSET_REQUIRED must be 0 or 1\n' >&2
      exit 2
      ;;
  esac

  asset_name="$(runtime_asset_name_for_platform "${platform}")"
  runtime_path="${artifact_dir%/}/${asset_name}"
  if [[ ! -f "${runtime_path}" ]]; then
    if [[ "${required}" == "1" ]]; then
      printf 'required ONNX Runtime sidecar missing: %s\n' "${runtime_path}" >&2
      exit 1
    fi
    return
  fi

  actual_sha="$(check_sha256_sidecar "${runtime_path}")"
  bash scripts/build-onnxruntime-sidecar.sh --validate "${platform}" "${runtime_path}"
  printf 'public ONNX Runtime sidecar ok: %s sha256=%s\n' "${asset_name}" "${actual_sha}"
}

version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
artifact="${artifact_dir%/}/${binary_name}"
version_file="${artifact}.version"

if [[ ! -f "${artifact}" ]]; then
  printf 'public CLI artifact missing: %s\n' "${artifact}" >&2
  exit 1
fi

actual_sha="$(check_sha256_sidecar "${artifact}")"

if [[ ! -s "${version_file}" ]]; then
  printf 'public CLI artifact version sidecar missing or empty: %s\n' "${version_file}" >&2
  exit 1
fi

actual_version="$(tr -d '\r' < "${version_file}" | sed 's/[[:space:]]*$//' | tail -n 1)"
can_run_on_host=0
case "${platform}" in
  linux-x64)
    if [[ "$(uname -s 2>/dev/null || true)" == "Linux" ]]; then
      case "$(uname -m 2>/dev/null || true)" in
        x86_64|amd64) can_run_on_host=1 ;;
      esac
    fi
    ;;
  linux-aarch64)
    if [[ "$(uname -s 2>/dev/null || true)" == "Linux" ]]; then
      case "$(uname -m 2>/dev/null || true)" in
        aarch64|arm64) can_run_on_host=1 ;;
      esac
    fi
    ;;
  macos-arm64)
    if [[ "$(uname -s 2>/dev/null || true)" == "Darwin" && "$(uname -m 2>/dev/null || true)" == "arm64" ]]; then
      can_run_on_host=1
    fi
    ;;
  macos-x64)
    if [[ "$(uname -s 2>/dev/null || true)" == "Darwin" ]] && /usr/bin/arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
      can_run_on_host=1
    fi
    ;;
  freebsd-x64)
    if [[ "$(uname -s 2>/dev/null || true)" == "FreeBSD" ]]; then
      case "$(uname -m 2>/dev/null || true)" in
        x86_64|amd64) can_run_on_host=1 ;;
      esac
    fi
    ;;
esac

case "${actual_version}" in
  "ctx ${version}") ;;
  "not run on this host: ${platform}")
    if [[ "${can_run_on_host}" == "1" ]]; then
      printf 'public CLI artifact version sidecar skipped a runnable host platform: %s\n' "${platform}" >&2
      exit 1
    fi
    ;;
  *)
    printf 'public CLI artifact version sidecar has unexpected content: %s\n' "${actual_version}" >&2
    exit 1
    ;;
esac

bash scripts/check-release-binary-compat.sh "${platform}" "${artifact}"
check_optional_runtime_asset

printf 'public CLI artifact ok: %s sha256=%s\n' "${platform}" "${actual_sha}"
