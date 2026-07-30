#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-public-cli-artifact.sh PLATFORM [ARTIFACT_DIR] [DECLARED_LLVM_READOBJ]

Checks one locally staged public ctx CLI artifact. This validates construction
outputs: artifact presence, SHA-256 consistency, the construction-time version
sidecar, and the static binary contract. A version sidecar is never native
runtime proof; promotion consumes separate exact-byte native evidence.
USAGE
}

platform="${1:-}"
artifact_dir="${2:-target/public-cli-artifacts}"
declared_llvm_readobj="${3:-}"
if [[ $# -gt 3 ]]; then
  usage
  exit 2
fi
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

version="${CTX_PUBLIC_CLI_EXPECTED_VERSION:-}"
if [[ -z "${version}" ]]; then
  version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
fi
artifact="${artifact_dir%/}/${binary_name}"
sha_file="${artifact}.sha256"
version_file="${artifact}.version"

if [[ ! -f "${artifact}" ]]; then
  printf 'public CLI artifact missing: %s\n' "${artifact}" >&2
  exit 1
fi

if [[ ! -s "${sha_file}" ]]; then
  printf 'public CLI artifact SHA-256 sidecar missing or empty: %s\n' "${sha_file}" >&2
  exit 1
fi

expected_sha="$(tr -d '[:space:]' < "${sha_file}")"
if [[ ! "${expected_sha}" =~ ^[0-9a-fA-F]{64}$ ]]; then
  printf 'public CLI artifact SHA-256 sidecar is not a digest: %s\n' "${sha_file}" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual_sha="$(sha256sum "${artifact}" | awk '{ print $1 }')"
else
  actual_sha="$(shasum -a 256 "${artifact}" | awk '{ print $1 }')"
fi

actual_sha_lower="$(printf '%s' "${actual_sha}" | tr 'A-F' 'a-f')"
expected_sha_lower="$(printf '%s' "${expected_sha}" | tr 'A-F' 'a-f')"
if [[ "${actual_sha_lower}" != "${expected_sha_lower}" ]]; then
  printf 'public CLI artifact checksum mismatch for %s: expected %s got %s\n' \
    "${artifact}" "${expected_sha}" "${actual_sha}" >&2
  exit 1
fi

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

construction_version_status=""
case "${actual_version}" in
  "ctx ${version}")
    construction_version_status="executed-on-construction-host"
    ;;
  "not run on this host: ${platform}")
    if [[ "${can_run_on_host}" == "1" ]]; then
      printf 'public CLI artifact version sidecar skipped a runnable host platform: %s\n' "${platform}" >&2
      exit 1
    fi
    construction_version_status="not-executed-on-construction-host"
    ;;
  *)
    printf 'public CLI artifact version sidecar has unexpected content: %s\n' "${actual_version}" >&2
    exit 1
    ;;
esac

if [[ -n "${declared_llvm_readobj}" ]]; then
  bash scripts/check-release-binary-compat.sh \
    "${platform}" "${artifact}" "${declared_llvm_readobj}"
else
  bash scripts/check-release-binary-compat.sh "${platform}" "${artifact}"
fi

printf 'public CLI artifact ok: %s sha256=%s construction_version=%s native_runtime_proof=not-claimed\n' \
  "${platform}" "${actual_sha}" "${construction_version_status}"
