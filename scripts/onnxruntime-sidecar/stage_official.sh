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

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
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

download_verified() {
  local url="$1"
  local expected_sha256="$2"
  local destination="$3"
  local temporary="${destination}.tmp.$$"
  require_command curl
  mkdir -p "$(dirname "${destination}")"
  if [[ -f "${destination}" ]]; then
    if verify_sha256 "${destination}" "${expected_sha256}" 2>/dev/null; then
      printf 'using cached %s\n' "${destination}"
      return
    fi
    printf 'discarding checksum-mismatched cache entry %s\n' "${destination}" >&2
    rm -f "${destination}"
  fi
  rm -f "${temporary}"
  curl --fail --location --retry 4 --retry-all-errors --silent --show-error \
    "${url}" --output "${temporary}"
  verify_sha256 "${temporary}" "${expected_sha256}"
  mv "${temporary}" "${destination}"
}

extract_official_asset() {
  local archive="$1"
  local destination="$2"
  local selected_provider_libraries="$3"
  python3 - "${upstream_kind}" "${archive}" "${upstream_root}" \
    "${upstream_library}" "${library_name}" "${destination}" \
    "${ONNXRUNTIME_VERSION}" "${ONNXRUNTIME_COMMIT}" \
    "${selected_provider_libraries}" <<'PY'
import os
import posixpath
import shutil
import stat
import sys
import tarfile
import zipfile

kind, archive, expected_root, source_library, library_name, destination, version, commit, provider_libraries = sys.argv[1:]
required = {
    f"{expected_root}/{source_library}": f"lib/{library_name}",
    f"{expected_root}/LICENSE": "LICENSE",
    f"{expected_root}/ThirdPartyNotices.txt": "ThirdPartyNotices.txt",
    f"{expected_root}/VERSION_NUMBER": "VERSION_NUMBER",
    f"{expected_root}/GIT_COMMIT_ID": "GIT_COMMIT_ID",
}
for provider in provider_libraries.split():
    required[f"{expected_root}/lib/{provider}"] = f"lib/{provider}"


def canonical_name(raw):
    if not raw or "\\" in raw or raw.startswith("/"):
        raise SystemExit(f"unsafe upstream archive path: {raw!r}")
    while raw.startswith("./"):
        raw = raw[2:]
    normalized = posixpath.normpath(raw.rstrip("/"))
    if normalized in ("", "."):
        return ""
    if normalized == ".." or normalized.startswith("../"):
        raise SystemExit(f"unsafe upstream archive path: {raw!r}")
    return normalized


def validate_root(name):
    if name and name != expected_root and not name.startswith(expected_root + "/"):
        raise SystemExit(
            f"unexpected upstream archive root: {name!r}; expected {expected_root!r}"
        )


os.makedirs(os.path.join(destination, "lib"), exist_ok=True)
seen = set()
if kind == "tar.gz":
    with tarfile.open(archive, "r:gz") as bundle:
        members = {}
        for member in bundle.getmembers():
            name = canonical_name(member.name)
            validate_root(name)
            if not name:
                continue
            if name in seen:
                raise SystemExit(f"duplicate upstream archive entry: {name}")
            seen.add(name)
            if member.issym() or member.islnk():
                target = member.linkname
                if target.startswith("/"):
                    raise SystemExit(f"unsafe upstream archive link: {name} -> {target}")
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
                validate_root(resolved)
            elif not (member.isdir() or member.isfile()):
                raise SystemExit(f"unsupported upstream archive entry type: {name}")
            members[name] = member
        for source, target in required.items():
            member = members.get(source)
            if member is None or not member.isfile():
                raise SystemExit(f"required regular file missing from upstream archive: {source}")
            source_file = bundle.extractfile(member)
            if source_file is None:
                raise SystemExit(f"could not read upstream archive member: {source}")
            target_path = os.path.join(destination, *target.split("/"))
            with source_file, open(target_path, "wb") as output:
                shutil.copyfileobj(source_file, output)
elif kind == "zip":
    with zipfile.ZipFile(archive) as bundle:
        members = {}
        for member in bundle.infolist():
            name = canonical_name(member.filename)
            validate_root(name)
            if not name:
                continue
            if name in seen:
                raise SystemExit(f"duplicate upstream archive entry: {name}")
            seen.add(name)
            mode = member.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise SystemExit(f"upstream zip contains a symbolic link: {name}")
            members[name] = member
        for source, target in required.items():
            member = members.get(source)
            if member is None or member.is_dir():
                raise SystemExit(f"required regular file missing from upstream archive: {source}")
            target_path = os.path.join(destination, *target.split("/"))
            with bundle.open(member) as source_file, open(target_path, "wb") as output:
                shutil.copyfileobj(source_file, output)
else:
    raise SystemExit(f"unsupported upstream archive kind: {kind}")

for name in ("LICENSE", "ThirdPartyNotices.txt"):
    path = os.path.join(destination, name)
    with open(path, "rb") as handle:
        content = handle.read().replace(b"\r\n", b"\n")
    with open(path, "wb") as handle:
        handle.write(content)
for name, expected in (("VERSION_NUMBER", version), ("GIT_COMMIT_ID", commit)):
    path = os.path.join(destination, name)
    with open(path, "rb") as handle:
        actual = handle.read().decode("utf-8-sig").strip()
    if actual != expected:
        raise SystemExit(f"upstream {name} is {actual!r}, expected {expected!r}")
    with open(path, "wb") as handle:
        handle.write((expected + "\n").encode())

for name in (library_name, *provider_libraries.split()):
    os.chmod(os.path.join(destination, "lib", name), 0o755)
for name in ("LICENSE", "ThirdPartyNotices.txt", "VERSION_NUMBER", "GIT_COMMIT_ID"):
    os.chmod(os.path.join(destination, name), 0o644)
PY
}

stage_cuda_dependencies() {
  local destination="$1"
  local cache_dir="$2"
  local cublas="${cache_dir}/${NVIDIA_CUBLAS_ASSET}"
  local cuda_runtime="${cache_dir}/${NVIDIA_CUDA_RUNTIME_ASSET}"
  local cuda_nvrtc="${cache_dir}/${NVIDIA_CUDA_NVRTC_ASSET}"
  local curand="${cache_dir}/${NVIDIA_CURAND_ASSET}"
  local cufft="${cache_dir}/${NVIDIA_CUFFT_ASSET}"
  local cudnn="${cache_dir}/${NVIDIA_CUDNN_ASSET}"

  download_verified "${NVIDIA_CUBLAS_URL}" "${NVIDIA_CUBLAS_SHA256}" "${cublas}"
  download_verified \
    "${NVIDIA_CUDA_RUNTIME_URL}" "${NVIDIA_CUDA_RUNTIME_SHA256}" "${cuda_runtime}"
  download_verified \
    "${NVIDIA_CUDA_NVRTC_URL}" "${NVIDIA_CUDA_NVRTC_SHA256}" "${cuda_nvrtc}"
  download_verified "${NVIDIA_CURAND_URL}" "${NVIDIA_CURAND_SHA256}" "${curand}"
  download_verified "${NVIDIA_CUFFT_URL}" "${NVIDIA_CUFFT_SHA256}" "${cufft}"
  download_verified "${NVIDIA_CUDNN_URL}" "${NVIDIA_CUDNN_SHA256}" "${cudnn}"

  python3 - "${destination}" \
    "${NVIDIA_CUDA_LICENSE_SHA256}" "${NVIDIA_CUDNN_LICENSE_SHA256}" \
    "${cublas}" "${cuda_runtime}" "${cuda_nvrtc}" "${curand}" "${cufft}" "${cudnn}" <<'PY'
import hashlib
import os
import posixpath
import shutil
import stat
import sys
import zipfile

destination, cuda_license_sha256, cudnn_license_sha256, *archives = sys.argv[1:]
specs = [
    (
        archives[0],
        (
            "nvidia/cublas/lib/libcublasLt.so.12",
            "nvidia/cublas/lib/libcublas.so.12",
        ),
        "nvidia_cublas_cu12-12.9.2.10.dist-info/licenses/License.txt",
        cuda_license_sha256,
        "NVIDIA-CUDA-LICENSE.txt",
    ),
    (
        archives[1],
        ("nvidia/cuda_runtime/lib/libcudart.so.12",),
        "nvidia_cuda_runtime_cu12-12.9.79.dist-info/licenses/License.txt",
        cuda_license_sha256,
        None,
    ),
    (
        archives[2],
        ("nvidia/cuda_nvrtc/lib/libnvrtc.so.12",),
        "nvidia_cuda_nvrtc_cu12-12.9.86.dist-info/licenses/License.txt",
        cuda_license_sha256,
        None,
    ),
    (
        archives[3],
        ("nvidia/curand/lib/libcurand.so.10",),
        "nvidia_curand_cu12-10.3.10.19.dist-info/License.txt",
        cuda_license_sha256,
        None,
    ),
    (
        archives[4],
        ("nvidia/cufft/lib/libcufft.so.11",),
        "nvidia_cufft_cu12-11.4.1.4.dist-info/licenses/License.txt",
        cuda_license_sha256,
        None,
    ),
    (
        archives[5],
        (
            "nvidia/cudnn/lib/libcudnn.so.9",
            "nvidia/cudnn/lib/libcudnn_graph.so.9",
            "nvidia/cudnn/lib/libcudnn_ops.so.9",
        ),
        "nvidia_cudnn_cu12-9.25.0.15.dist-info/licenses/License.txt",
        cudnn_license_sha256,
        "NVIDIA-CUDNN-LICENSE.txt",
    ),
]


def canonical_name(raw):
    if not raw or "\\" in raw or raw.startswith("/"):
        raise SystemExit(f"unsafe NVIDIA wheel path: {raw!r}")
    name = posixpath.normpath(raw.rstrip("/"))
    if name in ("", ".", "..") or name.startswith("../"):
        raise SystemExit(f"unsafe NVIDIA wheel path: {raw!r}")
    return name


for archive, libraries, license_member, expected_license, license_output in specs:
    with zipfile.ZipFile(archive) as wheel:
        members = {}
        for member in wheel.infolist():
            name = canonical_name(member.filename)
            if name in members:
                raise SystemExit(f"duplicate NVIDIA wheel path: {name}")
            mode = member.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise SystemExit(f"NVIDIA wheel contains a symbolic link: {name}")
            members[name] = member
        for source in libraries:
            member = members.get(source)
            if member is None or member.is_dir():
                raise SystemExit(f"NVIDIA wheel library is missing: {source}")
            target = os.path.join(destination, "lib", posixpath.basename(source))
            if os.path.exists(target):
                raise SystemExit(f"duplicate staged NVIDIA library: {target}")
            with wheel.open(member) as input_file, open(target, "wb") as output:
                shutil.copyfileobj(input_file, output)
            os.chmod(target, 0o755)
        member = members.get(license_member)
        if member is None or member.is_dir():
            raise SystemExit(f"NVIDIA wheel license is missing: {license_member}")
        license_bytes = wheel.read(member)
        actual_license = hashlib.sha256(license_bytes).hexdigest()
        if actual_license != expected_license:
            raise SystemExit(
                f"NVIDIA wheel license mismatch: expected {expected_license}, "
                f"got {actual_license}"
            )
        if license_output:
            target = os.path.join(destination, license_output)
            with open(target, "wb") as output:
                output.write(license_bytes)
            os.chmod(target, 0o644)
PY
}

stage_windows_ml_nuget() {
  local destination="$1"
  local cache_dir="$2"
  local package="${cache_dir}/microsoft.windows.ai.machinelearning.${WINDOWS_ML_VERSION}.nupkg"
  download_verified "${WINDOWS_ML_NUGET_URL}" "${WINDOWS_ML_NUGET_SHA256}" "${package}"
  python3 - "${package}" "${destination}" \
    "${WINDOWS_ML_LICENSE_SIZE}" "${WINDOWS_ML_LICENSE_SHA256}" \
    "${WINDOWS_ML_NOTICES_SIZE}" "${WINDOWS_ML_NOTICES_SHA256}" \
    "${WINDOWS_ML_LIBRARY_SIZE}" "${WINDOWS_ML_LIBRARY_SHA256}" \
    "${WINDOWS_ML_ONNXRUNTIME_SIZE}" "${WINDOWS_ML_ONNXRUNTIME_SHA256}" \
    "${WINDOWS_ML_DIRECTML_SIZE}" "${WINDOWS_ML_DIRECTML_SHA256}" <<'PY'
import hashlib
import os
import stat
import sys
import zipfile

package, destination, *expected_values = sys.argv[1:]
expected = iter(expected_values)
required = {
    "license.txt": ("LICENSE", int(next(expected)), next(expected)),
    "ThirdPartyNotices.txt": (
        "ThirdPartyNotices.txt",
        int(next(expected)),
        next(expected),
    ),
    "runtimes/win-x64/native/Microsoft.Windows.AI.MachineLearning.dll": (
        "lib/Microsoft.Windows.AI.MachineLearning.dll",
        int(next(expected)),
        next(expected),
    ),
    "runtimes/win-x64/native/onnxruntime.dll": (
        "lib/onnxruntime.dll",
        int(next(expected)),
        next(expected),
    ),
    "runtimes/win-x64/native/DirectML.dll": (
        "lib/DirectML.dll",
        int(next(expected)),
        next(expected),
    ),
}
os.makedirs(os.path.join(destination, "lib"), exist_ok=True)
with zipfile.ZipFile(package) as bundle:
    members = {}
    for member in bundle.infolist():
        name = member.filename.replace("\\", "/").rstrip("/")
        if not name or name.startswith("/") or ".." in name.split("/"):
            raise SystemExit(f"unsafe NuGet path: {member.filename!r}")
        if name in members:
            raise SystemExit(f"duplicate NuGet path: {name}")
        mode = member.external_attr >> 16
        if stat.S_ISLNK(mode):
            raise SystemExit(f"NuGet package contains symbolic link: {name}")
        members[name] = member
    for source, (target, expected_size, expected_sha256) in required.items():
        member = members.get(source)
        if member is None or member.is_dir():
            raise SystemExit(f"required NuGet file is missing: {source}")
        if member.file_size != expected_size:
            raise SystemExit(
                f"NuGet file size mismatch for {source}: "
                f"expected {expected_size}, got {member.file_size}"
            )
        content = bundle.read(member)
        actual_sha256 = hashlib.sha256(content).hexdigest()
        if actual_sha256 != expected_sha256:
            raise SystemExit(
                f"NuGet file SHA-256 mismatch for {source}: "
                f"expected {expected_sha256}, got {actual_sha256}"
            )
        target_path = os.path.join(destination, *target.split("/"))
        with open(target_path, "wb") as output:
            output.write(content)
for name in (
    "Microsoft.Windows.AI.MachineLearning.dll",
    "onnxruntime.dll",
    "DirectML.dll",
):
    os.chmod(os.path.join(destination, "lib", name), 0o755)
for name in ("LICENSE", "ThirdPartyNotices.txt"):
    os.chmod(os.path.join(destination, name), 0o644)
PY
}

stage_windows_vc_runtime() {
  local destination="$1"
  local cache_dir="$2"
  local redist="${cache_dir}/vc-redist-x64-${WINDOWS_VC_RUNTIME_VERSION}.exe"
  local outer="${work_dir}/vc-redist-outer"
  local minimum="${work_dir}/vc-redist-minimum"
  require_command cabextract
  download_verified "${WINDOWS_VC_REDIST_URL}" "${WINDOWS_VC_REDIST_SHA256}" "${redist}"
  rm -rf "${outer}" "${minimum}"
  mkdir -p "${outer}" "${minimum}"
  cabextract -q -d "${outer}" "${redist}"
  verify_sha256 "${outer}/a12" "${WINDOWS_VC_MINIMUM_CAB_SHA256}"
  verify_sha256 "${outer}/u4" "${WINDOWS_VC_LICENSE_SHA256}"
  cabextract -q -d "${minimum}" "${outer}/a12"
  verify_sha256 "${minimum}/msvcp140.dll_amd64" "${WINDOWS_MSVC_RUNTIME_SHA256}"
  verify_sha256 "${minimum}/msvcp140_1.dll_amd64" "${WINDOWS_MSVC_RUNTIME_1_SHA256}"
  verify_sha256 "${minimum}/vcruntime140.dll_amd64" "${WINDOWS_VCRUNTIME_SHA256}"
  verify_sha256 "${minimum}/vcruntime140_1.dll_amd64" "${WINDOWS_VCRUNTIME_1_SHA256}"
  cp "${minimum}/msvcp140.dll_amd64" "${destination}/lib/msvcp140.dll"
  cp "${minimum}/msvcp140_1.dll_amd64" "${destination}/lib/msvcp140_1.dll"
  cp "${minimum}/vcruntime140.dll_amd64" "${destination}/lib/vcruntime140.dll"
  cp "${minimum}/vcruntime140_1.dll_amd64" "${destination}/lib/vcruntime140_1.dll"
  cp "${outer}/u4" "${destination}/MICROSOFT_VC_RUNTIME_LICENSE.rtf"
  chmod 755 "${destination}/lib/msvcp140.dll" "${destination}/lib/msvcp140_1.dll" \
    "${destination}/lib/vcruntime140.dll" "${destination}/lib/vcruntime140_1.dll"
  chmod 644 "${destination}/MICROSOFT_VC_RUNTIME_LICENSE.rtf"
}

if [[ $# -lt 3 || $# -gt 4 ]]; then
  printf 'usage: %s PLATFORM DESTINATION CACHE_DIR [WORK_DIR]\n' "$0" >&2
  exit 2
fi
if ! configure_release_platform "$1" || [[ "${stage_kind}" != "official" ]]; then
  printf 'platform has no official ONNX Runtime release stage: %s\n' "$1" >&2
  exit 2
fi
destination="$2"
cache_dir="$3"
if [[ $# -eq 4 ]]; then
  work_dir="$4"
else
  work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-onnxruntime-official.XXXXXX")"
  trap 'rm -rf "${work_dir}"' EXIT
fi
require_command python3
mkdir -p "${destination}/lib" "${cache_dir}" "${work_dir}"
if [[ "${platform}" == "windows-x64-windowsml" ]]; then
  stage_windows_ml_nuget "${destination}" "${cache_dir}"
else
  configure_official_source "$1"
  archive="${cache_dir}/${upstream_asset}"
  download_verified "${ONNXRUNTIME_RELEASE_BASE_URL}/${upstream_asset}" \
    "${upstream_sha256}" "${archive}"
  extract_official_asset \
    "${archive}" "${destination}" "${upstream_provider_libraries}"
  if [[ "${platform}" == "linux-x64-cuda12" ]]; then
    stage_cuda_dependencies "${destination}" "${cache_dir}"
  fi
  if [[ "${platform}" == "windows-x64" ]]; then
    stage_windows_vc_runtime "${destination}" "${cache_dir}"
  fi
fi
