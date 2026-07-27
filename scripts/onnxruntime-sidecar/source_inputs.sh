#!/usr/bin/env bash

# Exact checksum-pinned network inputs for the ONNX Runtime sidecar release.
# This is intentionally a release-specific manifest, not a shell helper library.
tool_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -z "${ONNXRUNTIME_VERSION:-}" || -z "${ONNXRUNTIME_COMMIT:-}" ]]; then
  # shellcheck source=scripts/onnxruntime-sidecar/release_manifest.sh
  source "${tool_dir}/release_manifest.sh"
fi
ONNXRUNTIME_RELEASE_BASE_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ONNXRUNTIME_VERSION}"
ONNXRUNTIME_SOURCE_URL="https://github.com/microsoft/onnxruntime/archive/refs/tags/v${ONNXRUNTIME_VERSION}.tar.gz"
ONNXRUNTIME_SOURCE_SHA256="b41d09905a3c2f3a25709d1dcce8ef3942a4c2799d1046f74be7b6bbebc45e6a"
ONNXRUNTIME_LICENSE_URL="https://raw.githubusercontent.com/microsoft/onnxruntime/${ONNXRUNTIME_COMMIT}/LICENSE"
ONNXRUNTIME_LICENSE_SHA256="2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c"
ONNXRUNTIME_NOTICES_URL="https://raw.githubusercontent.com/microsoft/onnxruntime/${ONNXRUNTIME_COMMIT}/ThirdPartyNotices.txt"
ONNXRUNTIME_NOTICES_SHA256="0e07b95f3a8d6230037707c5c4a2b554d12c4cb67369669ac255635528ffcee2"
ONNXRUNTIME_DEPS_SHA256="e411468ead299e3386b2e5e9d773e50e1939b5fc0baca599666ca5757eeb3f71"

WINDOWS_VC_RUNTIME_VERSION="14.44.35211.0"
WINDOWS_VC_REDIST_URL="https://download.visualstudio.microsoft.com/download/pr/7ebf5fdb-36dc-4145-b0a0-90d3d5990a61/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe"
WINDOWS_VC_REDIST_SHA256="cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b"
WINDOWS_VC_MINIMUM_CAB_SHA256="640aa6c516c72444523b8fbe034db46ff4e118ed02705340e3ccb62d426ff040"
WINDOWS_VC_LICENSE_SHA256="8099dc3cf9502c335da829e5c755948a12e3e6de490eb492a99deb673d883d8b"
WINDOWS_MSVC_RUNTIME_SHA256="0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9"
WINDOWS_MSVC_RUNTIME_1_SHA256="bfad5aef4c63a669e3c140655cdfdf395b6c979b400a447bd5dcb65ed8826c3d"
WINDOWS_VCRUNTIME_SHA256="d5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066"
WINDOWS_VCRUNTIME_1_SHA256="1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f"

FREEBSD_PORTS_COMMIT="7c1f125705820cd2b776056f2c492ed605f3b5e3"
FREEBSD_PORTS_PATCH_BASE_URL="https://cgit.freebsd.org/ports/plain/misc/onnxruntime/files"
FREEBSD_SPIN_PAUSE_PATCH="patch-onnxruntime_core_common_spin__pause.cc"
FREEBSD_SPIN_PAUSE_PATCH_SHA256="37f30419946cc3440859d4ce2bccf05b3a8961dd9b3b2dd9f9663b6a235282c1"
FREEBSD_POSIX_ENV_PATCH="patch-onnxruntime_core_platform_posix_env.cc"
FREEBSD_POSIX_ENV_PATCH_SHA256="d730c2fe1341654159f1068beaf224f06cffb5520593718681c96fb47e131033"
FREEBSD_DISTINFO_SHA256="ef17d849c2707c0db508504f982565238a80af66c33b3261973ec29bc7e72b5e"

configure_official_source() {
  upstream_kind=""
  upstream_asset=""
  upstream_sha256=""
  upstream_root=""
  upstream_library=""
  case "$1" in
    linux-x64)
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-linux-x64-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f"
      upstream_root="onnxruntime-linux-x64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
      ;;
    linux-aarch64)
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-linux-aarch64-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="3e4d83ac06924a32a07b6d7f91ce6f852876153fc0bbdf931bf517a140bfbe48"
      upstream_root="onnxruntime-linux-aarch64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
      ;;
    macos-arm64)
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-osx-arm64-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a"
      upstream_root="onnxruntime-osx-arm64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/libonnxruntime.dylib"
      ;;
    windows-x64)
      upstream_kind="zip"
      upstream_asset="onnxruntime-win-x64-${ONNXRUNTIME_VERSION}.zip"
      upstream_sha256="c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc"
      upstream_root="onnxruntime-win-x64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/onnxruntime.dll"
      ;;
    *)
      return 2
      ;;
  esac
}

print_source_inputs() {
  printf '%s\n' \
    "release_base_url=${ONNXRUNTIME_RELEASE_BASE_URL}" \
    "source_url=${ONNXRUNTIME_SOURCE_URL}" \
    "source_sha256=${ONNXRUNTIME_SOURCE_SHA256}" \
    "license_url=${ONNXRUNTIME_LICENSE_URL}" \
    "license_sha256=${ONNXRUNTIME_LICENSE_SHA256}" \
    "notices_url=${ONNXRUNTIME_NOTICES_URL}" \
    "notices_sha256=${ONNXRUNTIME_NOTICES_SHA256}" \
    "deps_sha256=${ONNXRUNTIME_DEPS_SHA256}" \
    "windows_vc_runtime_version=${WINDOWS_VC_RUNTIME_VERSION}" \
    "windows_vc_redist_url=${WINDOWS_VC_REDIST_URL}" \
    "windows_vc_redist_sha256=${WINDOWS_VC_REDIST_SHA256}" \
    "windows_vc_minimum_cab_sha256=${WINDOWS_VC_MINIMUM_CAB_SHA256}" \
    "windows_vc_license_sha256=${WINDOWS_VC_LICENSE_SHA256}" \
    "windows_msvcp140_sha256=${WINDOWS_MSVC_RUNTIME_SHA256}" \
    "windows_msvcp140_1_sha256=${WINDOWS_MSVC_RUNTIME_1_SHA256}" \
    "windows_vcruntime140_sha256=${WINDOWS_VCRUNTIME_SHA256}" \
    "windows_vcruntime140_1_sha256=${WINDOWS_VCRUNTIME_1_SHA256}" \
    "freebsd_ports_commit=${FREEBSD_PORTS_COMMIT}" \
    "freebsd_ports_patch_base_url=${FREEBSD_PORTS_PATCH_BASE_URL}" \
    "freebsd_spin_pause_patch=${FREEBSD_SPIN_PAUSE_PATCH}" \
    "freebsd_spin_pause_patch_sha256=${FREEBSD_SPIN_PAUSE_PATCH_SHA256}" \
    "freebsd_posix_env_patch=${FREEBSD_POSIX_ENV_PATCH}" \
    "freebsd_posix_env_patch_sha256=${FREEBSD_POSIX_ENV_PATCH_SHA256}" \
    "freebsd_distinfo_sha256=${FREEBSD_DISTINFO_SHA256}"
  if [[ $# -eq 1 ]]; then
    configure_official_source "$1" || {
      printf 'platform has no official ONNX Runtime release input: %s\n' "$1" >&2
      return 2
    }
    printf '%s\n' \
      "upstream_kind=${upstream_kind}" \
      "upstream_asset=${upstream_asset}" \
      "upstream_sha256=${upstream_sha256}" \
      "upstream_root=${upstream_root}" \
      "upstream_library=${upstream_library}"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  if [[ $# -gt 1 ]]; then
    printf 'usage: %s [OFFICIAL_PLATFORM]\n' "$0" >&2
    exit 2
  fi
  print_source_inputs "$@"
fi
