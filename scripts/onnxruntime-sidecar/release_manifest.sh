#!/usr/bin/env bash

# Stable public sidecar shape. This file is data plus the concrete platform
# mapping; build tools source it and it can be audited directly from the CLI.
ONNXRUNTIME_VERSION="1.27.0"
ONNXRUNTIME_API_VERSION="24"
ONNXRUNTIME_COMMIT="8f0278c77bf44b0cc83c098c6c722b92a36ac4b5"
ONNXRUNTIME_MAX_GLIBC="2.39"
FREEBSD_BUILD_RECIPE="ctx-freebsd-source-v1"
FREEBSD_ABI_MAJOR="14"
SOURCE_DATE_EPOCH="1781827200"

configure_release_platform() {
  platform="$1"
  archive_kind="tar.zst"
  stage_kind="official"
  case "${platform}" in
    linux-x64)
      asset_name="ctx-onnxruntime-linux-x64.tar.zst"
      library_name="libonnxruntime.so"
      ;;
    linux-aarch64)
      asset_name="ctx-onnxruntime-linux-aarch64.tar.zst"
      library_name="libonnxruntime.so"
      ;;
    macos-arm64)
      asset_name="ctx-onnxruntime-macos-arm64.tar.zst"
      library_name="libonnxruntime.dylib"
      ;;
    macos-x64)
      asset_name="ctx-onnxruntime-macos-x64.tar.zst"
      library_name="libonnxruntime.dylib"
      stage_kind="macos-x64-source"
      ;;
    windows-x64)
      asset_name="ctx-onnxruntime-windows-x64.zip"
      library_name="onnxruntime.dll"
      archive_kind="zip"
      ;;
    freebsd-x64)
      asset_name="ctx-onnxruntime-freebsd-x64.tar.zst"
      library_name="libonnxruntime.so"
      stage_kind="freebsd-x64-source"
      ;;
    *)
      return 2
      ;;
  esac
}

print_release_platform() {
  configure_release_platform "$1" || {
    printf 'unsupported ONNX Runtime sidecar platform: %s\n' "$1" >&2
    return 2
  }
  printf '%s\n' \
    "version=${ONNXRUNTIME_VERSION}" \
    "api_version=${ONNXRUNTIME_API_VERSION}" \
    "commit=${ONNXRUNTIME_COMMIT}" \
    "max_glibc=${ONNXRUNTIME_MAX_GLIBC}" \
    "freebsd_build_recipe=${FREEBSD_BUILD_RECIPE}" \
    "freebsd_abi=${FREEBSD_ABI_MAJOR}" \
    "source_date_epoch=${SOURCE_DATE_EPOCH}" \
    "platform=${platform}" \
    "asset_name=${asset_name}" \
    "archive_kind=${archive_kind}" \
    "library_name=${library_name}" \
    "stage_kind=${stage_kind}"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  if [[ $# -ne 1 ]]; then
    printf 'usage: %s PLATFORM\n' "$0" >&2
    exit 2
  fi
  print_release_platform "$1"
fi
