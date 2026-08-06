#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sidecar_tools="${script_dir}/onnxruntime-sidecar"
# shellcheck source=scripts/onnxruntime-sidecar/release_manifest.sh
source "${sidecar_tools}/release_manifest.sh"

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/build-onnxruntime-sidecar.sh PLATFORM [OUTPUT_DIR]
  scripts/build-onnxruntime-sidecar.sh --validate PLATFORM ARCHIVE

Builds or validates one pinned public ctx native runtime sidecar. CPU and CUDA
use ONNX Runtime 1.27.0. The legacy windows-x64 lane retains its pinned
app-local VC runtime; windows-x64-windowsml is a separate self-contained
Windows ML 2.1.74 asset. The CUDA artifact includes its pinned CUDA 12 and
cuDNN user-space libraries, leaving only the NVIDIA driver on the host. Every
official input is checksum-pinned. macos-x64 is built from checksum-pinned
source and requires a native Intel macOS host.
freebsd-x64 is built from that same checksum-pinned source on a native x64
FreeBSD 14 host. Its two compatibility patches are checksum-pinned to FreeBSD
ports commit 7c1f125705820cd2b776056f2c492ed605f3b5e3. CMake is forced to fetch
dependencies from a local mirror verified against that commit's SHA-256
distinfo instead of using mutable installed packages, and the resulting library
records its source, recipe, ABI, OS, compiler, and CMake provenance in
OrtGetBuildInfoString.

Platforms: linux-x64, linux-x64-cuda12, linux-aarch64, macos-arm64,
macos-x64, windows-x64, windows-x64-windowsml, freebsd-x64.

Environment:
  CTX_ONNXRUNTIME_CACHE_DIR       Download cache (default: target/onnxruntime-sidecar-cache)
  CTX_ONNXRUNTIME_BUILD_DIR       Source/build directory for source-built platforms
  CTX_ONNXRUNTIME_BUILD_JOBS      Parallel job count for source-built platforms
  CTX_ONNXRUNTIME_MAX_GLIBC       Maximum accepted Linux GLIBC symbol version

Native FreeBSD build requirements:
  FreeBSD 14 x64 userland, CMake >= 3.28, Python 3, clang/clang++, GNU patch
  (gpatch), make, and network access to the checksum-declared source inputs.
  The OS/compiler/CMake versions are recorded, not supplied by environment.

USAGE
}

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

mode="build"
if [[ "${1:-}" == "--validate" ]]; then
  mode="validate"
  shift
fi
if [[ -z "${1:-}" || "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  if [[ -z "${1:-}" ]]; then
    exit 2
  fi
  exit 0
fi
if ! configure_release_platform "$1"; then
  usage
  exit 2
fi
shift

if [[ "${mode}" == "validate" ]]; then
  [[ $# -eq 1 ]] || {
    usage
    exit 2
  }
  exec bash "${sidecar_tools}/validate_sidecar.sh" "${platform}" "$1"
fi
[[ $# -le 1 ]] || {
  usage
  exit 2
}
command -v python3 >/dev/null 2>&1 || die "python3 is required"

output_dir="${1:-target/public-cli-artifacts}"
cache_dir="${CTX_ONNXRUNTIME_CACHE_DIR:-target/onnxruntime-sidecar-cache}"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-onnxruntime-sidecar.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
stage_dir="${work_dir}/stage"
package_dir="${work_dir}/package"
mkdir -p "${stage_dir}/lib" "${package_dir}" "${cache_dir}"

case "${stage_kind}" in
  official)
    bash "${sidecar_tools}/stage_official.sh" \
      "${platform}" "${stage_dir}" "${cache_dir}" "${work_dir}"
    ;;
  macos-x64-source)
    bash "${sidecar_tools}/build_macos_x64.sh" \
      "${stage_dir}" "${cache_dir}" "${work_dir}"
    ;;
  freebsd-x64-source)
    bash "${sidecar_tools}/build_freebsd_x64.sh" \
      "${stage_dir}" "${cache_dir}" "${work_dir}"
    ;;
esac

# Signing and publication intentionally remain visible in this coordinator.
macos_signing_mode="${CTX_MACOS_RELEASE_SIGNING:-optional}"
if [[ "${platform}" == macos-* && "${CTX_PUBLIC_CLI_ARTIFACT_MATRIX:-0}" == "1" ]]; then
  macos_signing_mode=required
fi
if [[ "${platform}" == macos-* ]]; then
  case "${macos_signing_mode}" in
    required)
      "${script_dir}/run-macos-release-signing.sh" \
        "${platform}" runtime "${stage_dir}/lib/${library_name}" "${output_dir}"
      ;;
    optional) ;;
    *) die "CTX_MACOS_RELEASE_SIGNING must be optional or required, got ${macos_signing_mode}" ;;
  esac
fi

package_path="${package_dir}/${asset_name}"
archive_command=(
  python3 "${sidecar_tools}/archive_tool.py" create
  --kind "${archive_kind}"
  --library "${library_name}"
  --source "${stage_dir}"
  --output "${package_path}"
  --source-date-epoch "${SOURCE_DATE_EPOCH}"
)
for provider_library in ${provider_libraries}; do
  archive_command+=(--extra-library "${provider_library}")
done
for extra_document in ${extra_documents}; do
  archive_command+=(--extra-document "${extra_document}")
done
for exact_file in ${archive_exact_files}; do
  archive_command+=(--exact-file "${exact_file}")
done
"${archive_command[@]}"
bash "${sidecar_tools}/validate_sidecar.sh" "${platform}" "${package_path}"

signed_files=()
if [[ -n "${archive_exact_files}" ]]; then
  read -r -a signed_files <<<"${archive_exact_files}"
else
  signed_files=(
    LICENSE
    ThirdPartyNotices.txt
    VERSION_NUMBER
    GIT_COMMIT_ID
  )
  for extra_document in ${extra_documents}; do
    signed_files+=("${extra_document}")
  done
  signed_files+=("lib/${library_name}")
  for provider_library in ${provider_libraries}; do
    signed_files+=("lib/${provider_library}")
  done
fi
metadata_args=()
while IFS= read -r signed_file; do
  metadata_args+=(--file "${signed_file}")
done < <(printf '%s\n' "${signed_files[@]}" | LC_ALL=C sort -u)
package_metadata=""
if [[ "${semantic_catalog_asset}" == "1" ]]; then
  package_metadata="${package_path}.asset.json"
  python3 "${script_dir}/semantic-release-assets.py" record \
    --asset-id "${asset_id}" \
    --role "${catalog_role}" \
    --backend "${catalog_backend}" \
    --version "${runtime_version}" \
    --platform "${archive_platform}" \
    --archive-format "${archive_kind}" \
    --archive "${package_path}" \
    --root "${stage_dir}" \
    --output "${package_metadata}" \
    "${metadata_args[@]}"
fi

mkdir -p "${output_dir}"
temporary_output="${output_dir%/}/.${asset_name}.tmp.$$"
rm -f "${temporary_output}"
cp "${package_path}" "${temporary_output}"
chmod 644 "${temporary_output}"
mv "${temporary_output}" "${output_dir%/}/${asset_name}"
sha256_file "${output_dir%/}/${asset_name}" > "${output_dir%/}/${asset_name}.sha256.tmp.$$"
mv "${output_dir%/}/${asset_name}.sha256.tmp.$$" "${output_dir%/}/${asset_name}.sha256"
if [[ -n "${package_metadata}" ]]; then
  temporary_metadata="${output_dir%/}/.${asset_name}.asset.json.tmp.$$"
  rm -f "${temporary_metadata}"
  cp "${package_metadata}" "${temporary_metadata}"
  chmod 644 "${temporary_metadata}"
  mv "${temporary_metadata}" "${output_dir%/}/${asset_name}.asset.json"
fi
if [[ "${platform}" == macos-* && "${macos_signing_mode}" == required ]]; then
  python3 "${script_dir}/macos-release-signing-evidence.py" bind-archive \
    --evidence "${output_dir%/}/ctx-onnxruntime-${platform}.signing.json" \
    --platform "${platform}" \
    --archive "${output_dir%/}/${asset_name}" \
    --checksum "${output_dir%/}/${asset_name}.sha256" \
    --nested-artifact "${stage_dir}/lib/${library_name}" \
    --role builder
  "${script_dir}/check-macos-release-signing.sh" \
    "${platform}" runtime "${output_dir%/}/${asset_name}" \
    "${output_dir%/}/ctx-onnxruntime-${platform}.signing.json"
fi
printf 'built %s sha256=%s\n' \
  "${output_dir%/}/${asset_name}" "$(cat "${output_dir%/}/${asset_name}.sha256")"
