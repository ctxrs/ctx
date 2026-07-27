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

validate_source_archive_layout() {
  local archive="$1"
  local expected_root="onnxruntime-${ONNXRUNTIME_VERSION}"
  python3 - "${archive}" "${expected_root}" <<'PY'
import posixpath
import sys
import tarfile

archive, expected_root = sys.argv[1:]
seen = set()
required = {
    f"{expected_root}/build.sh",
    f"{expected_root}/LICENSE",
    f"{expected_root}/ThirdPartyNotices.txt",
    f"{expected_root}/VERSION_NUMBER",
}
with tarfile.open(archive, "r:gz") as bundle:
    for member in bundle.getmembers():
        raw = member.name
        if not raw or "\\" in raw or raw.startswith("/"):
            raise SystemExit(f"unsafe source archive path: {raw!r}")
        while raw.startswith("./"):
            raw = raw[2:]
        name = posixpath.normpath(raw.rstrip("/"))
        if name == ".." or name.startswith("../"):
            raise SystemExit(f"unsafe source archive path: {raw!r}")
        if name != expected_root and not name.startswith(expected_root + "/"):
            raise SystemExit(
                f"unexpected source archive root: {name!r}; expected {expected_root!r}"
            )
        if name in seen:
            raise SystemExit(f"duplicate source archive entry: {name}")
        seen.add(name)
        if member.issym() or member.islnk():
            target = member.linkname
            if target.startswith("/"):
                raise SystemExit(f"unsafe source archive link: {name} -> {target}")
            resolved = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
            if resolved != expected_root and not resolved.startswith(expected_root + "/"):
                raise SystemExit(f"source archive link escapes root: {name} -> {target}")
        elif not (member.isdir() or member.isfile()):
            raise SystemExit(f"unsupported source archive entry type: {name}")
missing = sorted(required - seen)
if missing:
    raise SystemExit("source archive is missing required entries: " + ", ".join(missing))
PY
}

if [[ $# -lt 2 || $# -gt 3 ]]; then
  printf 'usage: %s DESTINATION CACHE_DIR [WORK_DIR]\n' "$0" >&2
  exit 2
fi
configure_release_platform macos-x64
destination="$1"
cache_dir="$2"
if [[ $# -eq 3 ]]; then
  work_dir="$3"
else
  work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-onnxruntime-macos-x64.XXXXXX")"
  trap 'rm -rf "${work_dir}"' EXIT
fi
source_archive="${cache_dir}/onnxruntime-${ONNXRUNTIME_VERSION}-source.tar.gz"
build_root="${CTX_ONNXRUNTIME_BUILD_DIR:-${work_dir}/macos-x64-build}"
source_parent="${build_root}/source"
source_dir="${source_parent}/onnxruntime-${ONNXRUNTIME_VERSION}"
cmake_build_dir="${build_root}/build"
built_library="${cmake_build_dir}/Release/libonnxruntime.dylib"
deployment_target="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
jobs="${CTX_ONNXRUNTIME_BUILD_JOBS:-}"

[[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "x86_64" ]] || \
  die "macos-x64 ONNX Runtime must be built on a native Intel macOS host"
require_command python3
require_command cmake
require_command tar
download_verified "${ONNXRUNTIME_SOURCE_URL}" "${ONNXRUNTIME_SOURCE_SHA256}" "${source_archive}"
validate_source_archive_layout "${source_archive}"
rm -rf "${source_parent}" "${cmake_build_dir}"
mkdir -p "${destination}/lib" "${cache_dir}" "${source_parent}" "${cmake_build_dir}"
tar -xzf "${source_archive}" -C "${source_parent}"

build_args=(
  --config Release
  --build_dir "${cmake_build_dir}"
  --build_shared_lib
  --skip_tests --skip_submodule_sync
  --compile_no_warning_as_error
)
if [[ -n "${jobs}" ]]; then
  [[ "${jobs}" =~ ^[1-9][0-9]*$ ]] || \
    die "CTX_ONNXRUNTIME_BUILD_JOBS must be a positive integer"
  build_args+=(--parallel "${jobs}")
else
  build_args+=(--parallel)
fi
build_args+=(
  --cmake_extra_defines
  "CMAKE_OSX_ARCHITECTURES=x86_64"
  "CMAKE_OSX_DEPLOYMENT_TARGET=${deployment_target}"
  "onnxruntime_BUILD_UNIT_TESTS=OFF"
)
(cd "${source_dir}" && ./build.sh "${build_args[@]}")

if [[ ! -f "${built_library}" ]]; then
  alternate_library="${cmake_build_dir}/Release/lib/libonnxruntime.dylib"
  if [[ -f "${alternate_library}" ]]; then
    built_library="${alternate_library}"
  else
    die "macos-x64 build did not produce ${cmake_build_dir}/Release/libonnxruntime.dylib"
  fi
fi

cp -L "${built_library}" "${destination}/lib/${library_name}"
chmod 755 "${destination}/lib/${library_name}"
cp "${source_dir}/LICENSE" "${destination}/LICENSE"
cp "${source_dir}/ThirdPartyNotices.txt" "${destination}/ThirdPartyNotices.txt"
printf '%s\n' "${ONNXRUNTIME_VERSION}" > "${destination}/VERSION_NUMBER"
printf '%s\n' "${ONNXRUNTIME_COMMIT}" > "${destination}/GIT_COMMIT_ID"
chmod 644 "${destination}/LICENSE" "${destination}/ThirdPartyNotices.txt" \
  "${destination}/VERSION_NUMBER" "${destination}/GIT_COMMIT_ID"
