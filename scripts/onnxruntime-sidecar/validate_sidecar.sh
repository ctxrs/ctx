#!/usr/bin/env bash
set -euo pipefail

tool_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/onnxruntime-sidecar/release_manifest.sh
source "${tool_dir}/release_manifest.sh"
# shellcheck source=scripts/onnxruntime-sidecar/source_inputs.sh
source "${tool_dir}/source_inputs.sh"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
  else
    die "sha256sum or shasum is required"
  fi
}

verify_sha256() {
  local path="$1"
  local expected="$2"
  local actual
  actual="$(sha256_file "${path}")"
  if [[ "$(printf '%s' "${actual}" | tr 'A-F' 'a-f')" != "$(printf '%s' "${expected}" | tr 'A-F' 'a-f')" ]]; then
    die "SHA-256 mismatch for ${path}: expected ${expected}, got ${actual}"
  fi
}

if [[ $# -ne 2 ]]; then
  printf 'usage: %s PLATFORM ARCHIVE\n' "$0" >&2
  exit 2
fi
if ! configure_release_platform "$1"; then
  printf 'unsupported ONNX Runtime sidecar platform: %s\n' "$1" >&2
  exit 2
fi
archive="$2"
[[ -f "${archive}" ]] || die "ONNX Runtime sidecar archive not found: ${archive}"
archive_base="$(basename "${archive}")"
[[ "${archive_base}" == "${asset_name}" ]] || \
  die "sidecar archive must be named ${asset_name}, got ${archive_base}"
command -v python3 >/dev/null 2>&1 || die "python3 is required"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-onnxruntime-validation.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
validation_dir="${work_dir}/validation"
mkdir -p "${validation_dir}"
python3 "${tool_dir}/archive_tool.py" extract \
  --kind "${archive_kind}" \
  --library "${library_name}" \
  --archive "${archive}" \
  --destination "${validation_dir}" \
  --work-dir "${work_dir}"

verify_sha256 "${validation_dir}/LICENSE" "${ONNXRUNTIME_LICENSE_SHA256}"
verify_sha256 "${validation_dir}/ThirdPartyNotices.txt" "${ONNXRUNTIME_NOTICES_SHA256}"
if [[ "${platform}" == "windows-x64" ]]; then
  verify_sha256 "${validation_dir}/MICROSOFT_VC_RUNTIME_LICENSE.rtf" \
    "${WINDOWS_VC_LICENSE_SHA256}"
  verify_sha256 "${validation_dir}/lib/msvcp140.dll" "${WINDOWS_MSVC_RUNTIME_SHA256}"
  verify_sha256 "${validation_dir}/lib/msvcp140_1.dll" "${WINDOWS_MSVC_RUNTIME_1_SHA256}"
  verify_sha256 "${validation_dir}/lib/vcruntime140.dll" "${WINDOWS_VCRUNTIME_SHA256}"
  verify_sha256 "${validation_dir}/lib/vcruntime140_1.dll" "${WINDOWS_VCRUNTIME_1_SHA256}"
fi
[[ "$(cat "${validation_dir}/VERSION_NUMBER")" == "${ONNXRUNTIME_VERSION}" ]] || \
  die "sidecar VERSION_NUMBER is not exactly ${ONNXRUNTIME_VERSION}"
[[ "$(wc -c < "${validation_dir}/VERSION_NUMBER" | tr -d '[:space:]')" == "7" ]] || \
  die "sidecar VERSION_NUMBER has unexpected whitespace or content"
[[ "$(cat "${validation_dir}/GIT_COMMIT_ID")" == "${ONNXRUNTIME_COMMIT}" ]] || \
  die "sidecar GIT_COMMIT_ID is not ${ONNXRUNTIME_COMMIT}"
[[ "$(wc -c < "${validation_dir}/GIT_COMMIT_ID" | tr -d '[:space:]')" == "41" ]] || \
  die "sidecar GIT_COMMIT_ID has unexpected whitespace or content"

library_path="${validation_dir}/lib/${library_name}"
[[ -s "${library_path}" ]] || \
  die "sidecar runtime library is missing or empty: lib/${library_name}"
[[ "$(wc -c < "${library_path}" | tr -d '[:space:]')" -ge 1048576 ]] || \
  die "sidecar runtime library is implausibly small: lib/${library_name}"
max_glibc="${CTX_ONNXRUNTIME_MAX_GLIBC:-${ONNXRUNTIME_MAX_GLIBC}}"
[[ "${max_glibc}" =~ ^[0-9]+\.[0-9]+$ ]] || \
  die "CTX_ONNXRUNTIME_MAX_GLIBC must be MAJOR.MINOR, got ${max_glibc}"
python3 "${tool_dir}/validate_runtime.py" \
  --platform "${platform}" \
  --library "${library_path}" \
  --version "${ONNXRUNTIME_VERSION}" \
  --api-version "${ONNXRUNTIME_API_VERSION}" \
  --max-glibc "${max_glibc}" \
  --freebsd-build-recipe "${FREEBSD_BUILD_RECIPE}" \
  --source-sha256 "${ONNXRUNTIME_SOURCE_SHA256}" \
  --freebsd-ports-commit "${FREEBSD_PORTS_COMMIT}" \
  --freebsd-deps-sha256 "${FREEBSD_DISTINFO_SHA256}" \
  --freebsd-abi "${FREEBSD_ABI_MAJOR}"
printf 'ONNX Runtime sidecar ok: %s version=%s\n' "${platform}" "${ONNXRUNTIME_VERSION}"
