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
CUDA_DEPENDENCY_LIBRARIES="libcudart.so.12 libcublasLt.so.12 libcublas.so.12 libcurand.so.10 libcufft.so.11 libnvrtc.so.12 libcudnn.so.9 libcudnn_graph.so.9 libcudnn_ops.so.9"

configure_release_platform() {
  platform="$1"
  archive_platform="${platform}"
  archive_kind="tar.zst"
  stage_kind="official"
  runtime_backend="cpu"
  catalog_role="cpu-runtime"
  catalog_backend="ort-cpu"
  provider_libraries=""
  upstream_provider_libraries=""
  extra_documents=""
  archive_exact_files=""
  semantic_catalog_asset="1"
  runtime_version="${ONNXRUNTIME_VERSION}"
  runtime_commit="${ONNXRUNTIME_COMMIT}"
  case "${platform}" in
    linux-x64)
      asset_id="linux_x64_cpu"
      asset_name="ctx-onnxruntime-linux-x64.tar.zst"
      library_name="libonnxruntime.so"
      ;;
    linux-x64-cuda12)
      asset_id="linux_cuda12"
      asset_name="ctx-onnxruntime-linux-x64-cuda12.tar.zst"
      library_name="libonnxruntime.so"
      upstream_provider_libraries="libonnxruntime_providers_shared.so libonnxruntime_providers_cuda.so"
      provider_libraries="${upstream_provider_libraries} ${CUDA_DEPENDENCY_LIBRARIES}"
      extra_documents="NVIDIA-CUDA-LICENSE.txt NVIDIA-CUDNN-LICENSE.txt"
      runtime_backend="cuda"
      catalog_role="accelerator"
      catalog_backend="ort-cuda"
      ;;
    linux-aarch64)
      asset_id="linux_aarch64_cpu"
      asset_name="ctx-onnxruntime-linux-aarch64.tar.zst"
      library_name="libonnxruntime.so"
      ;;
    macos-arm64)
      asset_id="macos_arm64_cpu"
      asset_name="ctx-onnxruntime-macos-arm64.tar.zst"
      library_name="libonnxruntime.dylib"
      ;;
    macos-x64)
      asset_id="macos_x64_cpu"
      asset_name="ctx-onnxruntime-macos-x64.tar.zst"
      library_name="libonnxruntime.dylib"
      stage_kind="macos-x64-source"
      ;;
    windows-x64)
      asset_id=""
      asset_name="ctx-onnxruntime-windows-x64.zip"
      library_name="onnxruntime.dll"
      archive_kind="zip"
      semantic_catalog_asset="0"
      ;;
    windows-x64-windowsml)
      asset_id="windows_ml"
      archive_platform="windows-x64"
      asset_name="ctx-windowsml-windows-x64.zip"
      library_name="Microsoft.Windows.AI.MachineLearning.dll"
      provider_libraries="onnxruntime.dll DirectML.dll"
      archive_exact_files="LICENSE ThirdPartyNotices.txt lib/Microsoft.Windows.AI.MachineLearning.dll lib/onnxruntime.dll lib/DirectML.dll"
      runtime_backend="windows-ml"
      catalog_backend="windows-ml"
      runtime_version="2.1.74"
      runtime_commit=""
      archive_kind="zip"
      ;;
    freebsd-x64)
      asset_id="freebsd_x64_cpu"
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
    "version=${runtime_version}" \
    "api_version=${ONNXRUNTIME_API_VERSION}" \
    "commit=${runtime_commit}" \
    "max_glibc=${ONNXRUNTIME_MAX_GLIBC}" \
    "freebsd_build_recipe=${FREEBSD_BUILD_RECIPE}" \
    "freebsd_abi=${FREEBSD_ABI_MAJOR}" \
    "source_date_epoch=${SOURCE_DATE_EPOCH}" \
    "builder_key=${platform}" \
    "platform=${archive_platform}" \
    "asset_name=${asset_name}" \
    "archive_kind=${archive_kind}" \
    "library_name=${library_name}" \
    "provider_libraries=${provider_libraries}" \
    "upstream_provider_libraries=${upstream_provider_libraries}" \
    "extra_documents=${extra_documents}" \
    "archive_exact_files=${archive_exact_files}" \
    "runtime_backend=${runtime_backend}" \
    "asset_id=${asset_id}" \
    "catalog_role=${catalog_role}" \
    "catalog_backend=${catalog_backend}" \
    "semantic_catalog_asset=${semantic_catalog_asset}" \
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
