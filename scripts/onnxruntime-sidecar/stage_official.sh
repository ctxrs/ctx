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
  python3 - "${upstream_kind}" "${archive}" "${upstream_root}" \
    "${upstream_library}" "${library_name}" "${destination}" \
    "${ONNXRUNTIME_VERSION}" "${ONNXRUNTIME_COMMIT}" <<'PY'
import os
import posixpath
import shutil
import stat
import sys
import tarfile
import zipfile

kind, archive, expected_root, source_library, library_name, destination, version, commit = sys.argv[1:]
required = {
    f"{expected_root}/{source_library}": f"lib/{library_name}",
    f"{expected_root}/LICENSE": "LICENSE",
    f"{expected_root}/ThirdPartyNotices.txt": "ThirdPartyNotices.txt",
    f"{expected_root}/VERSION_NUMBER": "VERSION_NUMBER",
    f"{expected_root}/GIT_COMMIT_ID": "GIT_COMMIT_ID",
}


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

os.chmod(os.path.join(destination, "lib", library_name), 0o755)
for name in ("LICENSE", "ThirdPartyNotices.txt", "VERSION_NUMBER", "GIT_COMMIT_ID"):
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
configure_official_source "$1"
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
archive="${cache_dir}/${upstream_asset}"
download_verified "${ONNXRUNTIME_RELEASE_BASE_URL}/${upstream_asset}" \
  "${upstream_sha256}" "${archive}"
extract_official_asset "${archive}" "${destination}"
if [[ "${platform}" == "windows-x64" ]]; then
  stage_windows_vc_runtime "${destination}" "${cache_dir}"
fi
