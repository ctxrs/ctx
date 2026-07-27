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

NVIDIA_CUBLAS_ASSET="nvidia_cublas_cu12-12.9.2.10-py3-none-manylinux_2_27_x86_64.whl"
NVIDIA_CUBLAS_URL="https://files.pythonhosted.org/packages/cb/c0/0a517bfe63ccd3b92eb254d264e28fca3c7cab75d07daea315250fb1bf73/${NVIDIA_CUBLAS_ASSET}"
NVIDIA_CUBLAS_SHA256="e4f53a8ca8c5d6e8c492d0d0a3d565ecb59a751b19cfdaa4f6da0ab2104c1702"
NVIDIA_CUDA_RUNTIME_ASSET="nvidia_cuda_runtime_cu12-12.9.79-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl"
NVIDIA_CUDA_RUNTIME_URL="https://files.pythonhosted.org/packages/bc/46/a92db19b8309581092a3add7e6fceb4c301a3fd233969856a8cbf042cd3c/${NVIDIA_CUDA_RUNTIME_ASSET}"
NVIDIA_CUDA_RUNTIME_SHA256="25bba2dfb01d48a9b59ca474a1ac43c6ebf7011f1b0b8cc44f54eb6ac48a96c3"
NVIDIA_CUDA_NVRTC_ASSET="nvidia_cuda_nvrtc_cu12-12.9.86-py3-none-manylinux2010_x86_64.manylinux_2_12_x86_64.whl"
NVIDIA_CUDA_NVRTC_URL="https://files.pythonhosted.org/packages/b8/85/e4af82cc9202023862090bfca4ea827d533329e925c758f0cde964cb54b7/${NVIDIA_CUDA_NVRTC_ASSET}"
NVIDIA_CUDA_NVRTC_SHA256="210cf05005a447e29214e9ce50851e83fc5f4358df8b453155d5e1918094dcb4"
NVIDIA_CURAND_ASSET="nvidia_curand_cu12-10.3.10.19-py3-none-manylinux_2_27_x86_64.whl"
NVIDIA_CURAND_URL="https://files.pythonhosted.org/packages/31/44/193a0e171750ca9f8320626e8a1f2381e4077a65e69e2fb9708bd479e34a/${NVIDIA_CURAND_ASSET}"
NVIDIA_CURAND_SHA256="49b274db4780d421bd2ccd362e1415c13887c53c214f0d4b761752b8f9f6aa1e"
NVIDIA_CUFFT_ASSET="nvidia_cufft_cu12-11.4.1.4-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl"
NVIDIA_CUFFT_URL="https://files.pythonhosted.org/packages/95/f4/61e6996dd20481ee834f57a8e9dca28b1869366a135e0d42e2aa8493bdd4/${NVIDIA_CUFFT_ASSET}"
NVIDIA_CUFFT_SHA256="c67884f2a7d276b4b80eb56a79322a95df592ae5e765cf1243693365ccab4e28"
NVIDIA_CUDNN_ASSET="nvidia_cudnn_cu12-9.25.0.15-py3-none-manylinux_2_27_x86_64.whl"
NVIDIA_CUDNN_URL="https://files.pythonhosted.org/packages/83/94/1e9882d2d4307560197881069dee9e4050cea8384ae77b330e1f8f722fdf/${NVIDIA_CUDNN_ASSET}"
NVIDIA_CUDNN_SHA256="4ea1ba443fa28ac6cf04b7a44a107dfd54cf355c2324938102ddb21778ab10ce"
NVIDIA_CUDA_LICENSE_SHA256="ad6f5853fba0ca0d159d0f58d49ae49830c2f8c93f7a92648b9ce90adb4c6ccd"
NVIDIA_CUDNN_LICENSE_SHA256="49cf79bdb35734b52fe6203013b3bd759f81e998cd32aa2c65c51db9a88c61d2"

WINDOWS_VC_RUNTIME_VERSION="14.44.35211.0"
WINDOWS_VC_REDIST_URL="https://download.visualstudio.microsoft.com/download/pr/7ebf5fdb-36dc-4145-b0a0-90d3d5990a61/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe"
WINDOWS_VC_REDIST_SHA256="cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b"
WINDOWS_VC_MINIMUM_CAB_SHA256="640aa6c516c72444523b8fbe034db46ff4e118ed02705340e3ccb62d426ff040"
WINDOWS_VC_LICENSE_SHA256="8099dc3cf9502c335da829e5c755948a12e3e6de490eb492a99deb673d883d8b"
WINDOWS_MSVC_RUNTIME_SHA256="0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9"
WINDOWS_MSVC_RUNTIME_1_SHA256="bfad5aef4c63a669e3c140655cdfdf395b6c979b400a447bd5dcb65ed8826c3d"
WINDOWS_VCRUNTIME_SHA256="d5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066"
WINDOWS_VCRUNTIME_1_SHA256="1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f"

WINDOWS_ML_VERSION="2.1.74"
WINDOWS_ML_ONNXRUNTIME_VERSION="1.24.6"
WINDOWS_ML_NUGET_URL="https://api.nuget.org/v3-flatcontainer/microsoft.windows.ai.machinelearning/2.1.74/microsoft.windows.ai.machinelearning.2.1.74.nupkg"
WINDOWS_ML_NUGET_SHA256="691165fa3c07a04b752cbf4a07e93ed13a418e9dea1ee89eb163d2225e2ba3af"
WINDOWS_ML_LICENSE_SIZE="13996"
WINDOWS_ML_LICENSE_SHA256="66395f8cb219087fae2bd025010bd9076b736c14f03b48f20295471c0c376814"
WINDOWS_ML_NOTICES_SIZE="331175"
WINDOWS_ML_NOTICES_SHA256="fb0af774b4d7cffc5b9d046f2aaeade2f37df2f80abf8033c95dfffcc77a8866"
WINDOWS_ML_LIBRARY_SIZE="903464"
WINDOWS_ML_LIBRARY_SHA256="bbbb34415d8ce303f8a2a2c524c46bd749e21423ccabeb7d38c5fc7334a7848f"
WINDOWS_ML_ONNXRUNTIME_SIZE="21659280"
WINDOWS_ML_ONNXRUNTIME_SHA256="3cffeff2d7c25b247a814212baab70eb1f37d727335d4c813ed73785df80a794"
WINDOWS_ML_DIRECTML_SIZE="18700224"
WINDOWS_ML_DIRECTML_SHA256="257c75b2f607940c986d0b96d9309a2c897e57ef3192b6c678d707c22d747611"

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
    linux-x64-cuda12)
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-linux-x64-gpu_cuda12-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="3fed2d2f45f01f8bc1c1597a31afe29efd692c7ea4648d58e1844a8a0d0a48cb"
      upstream_root="onnxruntime-linux-x64-gpu_cuda12-${ONNXRUNTIME_VERSION}"
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
    "nvidia_cublas_url=${NVIDIA_CUBLAS_URL}" \
    "nvidia_cublas_sha256=${NVIDIA_CUBLAS_SHA256}" \
    "nvidia_cuda_runtime_url=${NVIDIA_CUDA_RUNTIME_URL}" \
    "nvidia_cuda_runtime_sha256=${NVIDIA_CUDA_RUNTIME_SHA256}" \
    "nvidia_cuda_nvrtc_url=${NVIDIA_CUDA_NVRTC_URL}" \
    "nvidia_cuda_nvrtc_sha256=${NVIDIA_CUDA_NVRTC_SHA256}" \
    "nvidia_curand_url=${NVIDIA_CURAND_URL}" \
    "nvidia_curand_sha256=${NVIDIA_CURAND_SHA256}" \
    "nvidia_cufft_url=${NVIDIA_CUFFT_URL}" \
    "nvidia_cufft_sha256=${NVIDIA_CUFFT_SHA256}" \
    "nvidia_cudnn_url=${NVIDIA_CUDNN_URL}" \
    "nvidia_cudnn_sha256=${NVIDIA_CUDNN_SHA256}" \
    "nvidia_cuda_license_sha256=${NVIDIA_CUDA_LICENSE_SHA256}" \
    "nvidia_cudnn_license_sha256=${NVIDIA_CUDNN_LICENSE_SHA256}" \
    "windows_vc_runtime_version=${WINDOWS_VC_RUNTIME_VERSION}" \
    "windows_vc_redist_url=${WINDOWS_VC_REDIST_URL}" \
    "windows_vc_redist_sha256=${WINDOWS_VC_REDIST_SHA256}" \
    "windows_vc_minimum_cab_sha256=${WINDOWS_VC_MINIMUM_CAB_SHA256}" \
    "windows_vc_license_sha256=${WINDOWS_VC_LICENSE_SHA256}" \
    "windows_msvcp140_sha256=${WINDOWS_MSVC_RUNTIME_SHA256}" \
    "windows_msvcp140_1_sha256=${WINDOWS_MSVC_RUNTIME_1_SHA256}" \
    "windows_vcruntime140_sha256=${WINDOWS_VCRUNTIME_SHA256}" \
    "windows_vcruntime140_1_sha256=${WINDOWS_VCRUNTIME_1_SHA256}" \
    "windows_ml_version=${WINDOWS_ML_VERSION}" \
    "windows_ml_onnxruntime_version=${WINDOWS_ML_ONNXRUNTIME_VERSION}" \
    "windows_ml_nuget_url=${WINDOWS_ML_NUGET_URL}" \
    "windows_ml_nuget_sha256=${WINDOWS_ML_NUGET_SHA256}" \
    "windows_ml_license_size=${WINDOWS_ML_LICENSE_SIZE}" \
    "windows_ml_license_sha256=${WINDOWS_ML_LICENSE_SHA256}" \
    "windows_ml_notices_size=${WINDOWS_ML_NOTICES_SIZE}" \
    "windows_ml_notices_sha256=${WINDOWS_ML_NOTICES_SHA256}" \
    "windows_ml_library_size=${WINDOWS_ML_LIBRARY_SIZE}" \
    "windows_ml_library_sha256=${WINDOWS_ML_LIBRARY_SHA256}" \
    "windows_ml_onnxruntime_size=${WINDOWS_ML_ONNXRUNTIME_SIZE}" \
    "windows_ml_onnxruntime_sha256=${WINDOWS_ML_ONNXRUNTIME_SHA256}" \
    "windows_ml_directml_size=${WINDOWS_ML_DIRECTML_SIZE}" \
    "windows_ml_directml_sha256=${WINDOWS_ML_DIRECTML_SHA256}" \
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
