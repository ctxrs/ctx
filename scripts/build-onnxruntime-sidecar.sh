#!/usr/bin/env bash
set -euo pipefail

ONNXRUNTIME_VERSION="1.27.0"
ONNXRUNTIME_API_VERSION="24"
ONNXRUNTIME_COMMIT="8f0278c77bf44b0cc83c098c6c722b92a36ac4b5"
ONNXRUNTIME_RELEASE_BASE_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ONNXRUNTIME_VERSION}"
ONNXRUNTIME_SOURCE_URL="https://github.com/microsoft/onnxruntime/archive/refs/tags/v${ONNXRUNTIME_VERSION}.tar.gz"
ONNXRUNTIME_SOURCE_SHA256="b41d09905a3c2f3a25709d1dcce8ef3942a4c2799d1046f74be7b6bbebc45e6a"
ONNXRUNTIME_LICENSE_URL="https://raw.githubusercontent.com/microsoft/onnxruntime/${ONNXRUNTIME_COMMIT}/LICENSE"
ONNXRUNTIME_LICENSE_SHA256="2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c"
ONNXRUNTIME_NOTICES_URL="https://raw.githubusercontent.com/microsoft/onnxruntime/${ONNXRUNTIME_COMMIT}/ThirdPartyNotices.txt"
ONNXRUNTIME_NOTICES_SHA256="0e07b95f3a8d6230037707c5c4a2b554d12c4cb67369669ac255635528ffcee2"
ONNXRUNTIME_MAX_GLIBC="2.39"
ONNXRUNTIME_DEPS_SHA256="e411468ead299e3386b2e5e9d773e50e1939b5fc0baca599666ca5757eeb3f71"
FREEBSD_PORTS_COMMIT="7c1f125705820cd2b776056f2c492ed605f3b5e3"
FREEBSD_PORTS_PATCH_BASE_URL="https://cgit.freebsd.org/ports/plain/misc/onnxruntime/files"
FREEBSD_SPIN_PAUSE_PATCH="patch-onnxruntime_core_common_spin__pause.cc"
FREEBSD_SPIN_PAUSE_PATCH_SHA256="37f30419946cc3440859d4ce2bccf05b3a8961dd9b3b2dd9f9663b6a235282c1"
FREEBSD_POSIX_ENV_PATCH="patch-onnxruntime_core_platform_posix_env.cc"
FREEBSD_POSIX_ENV_PATCH_SHA256="d730c2fe1341654159f1068beaf224f06cffb5520593718681c96fb47e131033"
FREEBSD_DISTINFO_SHA256="ef17d849c2707c0db508504f982565238a80af66c33b3261973ec29bc7e72b5e"
FREEBSD_BUILD_RECIPE="ctx-freebsd-source-v1"
FREEBSD_ABI_MAJOR="14"
SOURCE_DATE_EPOCH="1781827200"

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/build-onnxruntime-sidecar.sh PLATFORM [OUTPUT_DIR]
  scripts/build-onnxruntime-sidecar.sh --validate PLATFORM ARCHIVE

Builds or validates the ONNX Runtime 1.27.0 CPU sidecar for one public ctx
platform. Official Microsoft release archives are checksum-pinned. macos-x64
is built from checksum-pinned source and requires a native Intel macOS host.
freebsd-x64 is built from that same checksum-pinned source on a native x64
FreeBSD 14 host. Its two compatibility patches are checksum-pinned to FreeBSD
ports commit 7c1f125705820cd2b776056f2c492ed605f3b5e3. CMake is forced to fetch
dependencies from a local mirror verified against that commit's SHA-256
distinfo instead of using mutable installed packages, and the resulting library
records its source, recipe, ABI, OS, compiler, and CMake provenance in
OrtGetBuildInfoString.

Platforms: linux-x64, linux-aarch64, macos-arm64, macos-x64, windows-x64,
freebsd-x64.

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

verify_size() {
  local path="$1"
  local expected="$2"
  local actual

  actual="$(wc -c < "${path}" | tr -d '[:space:]')"
  [[ "${actual}" == "${expected}" ]] || \
    die "size mismatch for ${path}: expected ${expected} bytes, got ${actual}"
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

configure_platform() {
  local requested_platform="$1"

  platform="${requested_platform}"
  archive_kind="tar.zst"
  upstream_kind=""
  upstream_asset=""
  upstream_sha256=""
  upstream_root=""
  upstream_library=""
  case "${platform}" in
    linux-x64)
      asset_name="ctx-onnxruntime-linux-x64.tar.zst"
      library_name="libonnxruntime.so"
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-linux-x64-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f"
      upstream_root="onnxruntime-linux-x64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
      ;;
    linux-aarch64)
      asset_name="ctx-onnxruntime-linux-aarch64.tar.zst"
      library_name="libonnxruntime.so"
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-linux-aarch64-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="3e4d83ac06924a32a07b6d7f91ce6f852876153fc0bbdf931bf517a140bfbe48"
      upstream_root="onnxruntime-linux-aarch64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
      ;;
    macos-arm64)
      asset_name="ctx-onnxruntime-macos-arm64.tar.zst"
      library_name="libonnxruntime.dylib"
      upstream_kind="tar.gz"
      upstream_asset="onnxruntime-osx-arm64-${ONNXRUNTIME_VERSION}.tgz"
      upstream_sha256="545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a"
      upstream_root="onnxruntime-osx-arm64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/libonnxruntime.dylib"
      ;;
    macos-x64)
      asset_name="ctx-onnxruntime-macos-x64.tar.zst"
      library_name="libonnxruntime.dylib"
      ;;
    windows-x64)
      asset_name="ctx-onnxruntime-windows-x64.zip"
      library_name="onnxruntime.dll"
      archive_kind="zip"
      upstream_kind="zip"
      upstream_asset="onnxruntime-win-x64-${ONNXRUNTIME_VERSION}.zip"
      upstream_sha256="c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc"
      upstream_root="onnxruntime-win-x64-${ONNXRUNTIME_VERSION}"
      upstream_library="lib/onnxruntime.dll"
      ;;
    freebsd-x64)
      asset_name="ctx-onnxruntime-freebsd-x64.tar.zst"
      library_name="libonnxruntime.so"
      ;;
    *)
      usage
      exit 2
      ;;
  esac
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

stage_pinned_documents() {
  local destination="$1"
  local cache_dir="$2"
  local license="${cache_dir}/onnxruntime-${ONNXRUNTIME_COMMIT}-LICENSE"
  local notices="${cache_dir}/onnxruntime-${ONNXRUNTIME_COMMIT}-ThirdPartyNotices.txt"

  download_verified "${ONNXRUNTIME_LICENSE_URL}" "${ONNXRUNTIME_LICENSE_SHA256}" "${license}"
  download_verified "${ONNXRUNTIME_NOTICES_URL}" "${ONNXRUNTIME_NOTICES_SHA256}" "${notices}"
  cp "${license}" "${destination}/LICENSE"
  cp "${notices}" "${destination}/ThirdPartyNotices.txt"
  printf '%s\n' "${ONNXRUNTIME_VERSION}" > "${destination}/VERSION_NUMBER"
  printf '%s\n' "${ONNXRUNTIME_COMMIT}" > "${destination}/GIT_COMMIT_ID"
  chmod 644 "${destination}/LICENSE" "${destination}/ThirdPartyNotices.txt" \
    "${destination}/VERSION_NUMBER" "${destination}/GIT_COMMIT_ID"
}

stage_official_release() {
  local destination="$1"
  local cache_dir="$2"
  local archive="${cache_dir}/${upstream_asset}"

  download_verified "${ONNXRUNTIME_RELEASE_BASE_URL}/${upstream_asset}" \
    "${upstream_sha256}" "${archive}"
  extract_official_asset "${archive}" "${destination}"
}

stage_macos_x64_source_build() {
  local destination="$1"
  local cache_dir="$2"
  local source_archive="${cache_dir}/onnxruntime-${ONNXRUNTIME_VERSION}-source.tar.gz"
  local build_root="${CTX_ONNXRUNTIME_BUILD_DIR:-${work_dir}/macos-x64-build}"
  local source_parent="${build_root}/source"
  local source_dir="${source_parent}/onnxruntime-${ONNXRUNTIME_VERSION}"
  local cmake_build_dir="${build_root}/build"
  local built_library="${cmake_build_dir}/Release/libonnxruntime.dylib"
  local deployment_target="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
  local jobs="${CTX_ONNXRUNTIME_BUILD_JOBS:-}"

  [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "x86_64" ]] || \
    die "macos-x64 ONNX Runtime must be built on a native Intel macOS host"
  require_command python3
  require_command cmake

  download_verified "${ONNXRUNTIME_SOURCE_URL}" "${ONNXRUNTIME_SOURCE_SHA256}" "${source_archive}"
  validate_source_archive_layout "${source_archive}"
  rm -rf "${source_parent}" "${cmake_build_dir}"
  mkdir -p "${source_parent}" "${cmake_build_dir}"
  tar -xzf "${source_archive}" -C "${source_parent}"

  build_args=(
    --config Release
    --build_dir "${cmake_build_dir}"
    --build_shared_lib
    --skip_tests
    --compile_no_warning_as_error
  )
  if [[ -n "${jobs}" ]]; then
    [[ "${jobs}" =~ ^[1-9][0-9]*$ ]] || die "CTX_ONNXRUNTIME_BUILD_JOBS must be a positive integer"
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
}

prepare_freebsd_dependency_mirror() {
  local source_dir="$1"
  local cache_dir="$2"
  local distinfo="${cache_dir}/freebsd-ports-${FREEBSD_PORTS_COMMIT}-onnxruntime-distinfo"
  local mirror="${cache_dir}/freebsd-deps-${FREEBSD_PORTS_COMMIT}"
  local manifest="${work_dir}/freebsd-dependencies.tsv"
  local url expected_sha256 expected_size relative destination

  download_verified \
    "https://cgit.freebsd.org/ports/plain/misc/onnxruntime/distinfo?id=${FREEBSD_PORTS_COMMIT}" \
    "${FREEBSD_DISTINFO_SHA256}" "${distinfo}"
  python3 - "${source_dir}/cmake/deps.txt" "${distinfo}" "${manifest}" <<'PY'
import pathlib
import re
import sys
import urllib.parse

deps_path, distinfo_path, manifest_path = map(pathlib.Path, sys.argv[1:])
sha256 = {}
sizes = {}
pattern = re.compile(r"^(SHA256|SIZE) \(onnxruntime/(.+)\) = (.+)$")
for line in distinfo_path.read_text().splitlines():
    match = pattern.fullmatch(line)
    if not match:
        continue
    kind, name, value = match.groups()
    target = sha256 if kind == "SHA256" else sizes
    if name in target:
        raise SystemExit(f"duplicate {kind} entry in pinned FreeBSD distinfo: {name}")
    target[name] = value

rows = []
seen_basenames = {}
for line in deps_path.read_text().splitlines():
    if not line or line.startswith("#"):
        continue
    fields = line.split(";")
    if len(fields) != 3:
        raise SystemExit(f"invalid ONNX Runtime dependency row: {line!r}")
    _name, url, _sha1 = fields
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "https" or not parsed.netloc or parsed.query or parsed.fragment:
        raise SystemExit(f"dependency URL is not a plain HTTPS URL: {url}")
    basename = pathlib.PurePosixPath(parsed.path).name
    previous = seen_basenames.setdefault(basename, url)
    if previous != url:
        raise SystemExit(
            f"ambiguous dependency basename {basename!r}: {previous!r} and {url!r}"
        )
    if basename not in sha256 or basename not in sizes:
        raise SystemExit(
            f"pinned FreeBSD distinfo has no SHA256/SIZE for dependency {basename!r}"
        )
    relative = f"{parsed.netloc}{parsed.path}"
    if "\t" in relative or "\n" in relative:
        raise SystemExit(f"unsafe dependency mirror path: {relative!r}")
    rows.append((url, sha256[basename], sizes[basename], relative))

if not rows:
    raise SystemExit("ONNX Runtime dependency manifest is empty")
manifest_path.write_text(
    "".join("\t".join(row) + "\n" for row in rows)
)
PY

  while IFS=$'\t' read -r url expected_sha256 expected_size relative; do
    [[ -n "${url}" && -n "${expected_sha256}" && -n "${expected_size}" && -n "${relative}" ]] || \
      die "invalid row in generated FreeBSD dependency manifest"
    destination="${mirror}/${relative}"
    download_verified "${url}" "${expected_sha256}" "${destination}"
    verify_size "${destination}" "${expected_size}"
  done < "${manifest}"

  python3 - "${source_dir}/cmake/deps.txt" "${mirror}" <<'PY'
import pathlib
import sys
import urllib.parse

deps_path = pathlib.Path(sys.argv[1])
mirror = pathlib.Path(sys.argv[2]).resolve()
rewritten = []
for line in deps_path.read_text().splitlines(keepends=True):
    ending = "\n" if line.endswith("\n") else ""
    body = line[:-1] if ending else line
    if not body or body.startswith("#"):
        rewritten.append(line)
        continue
    fields = body.split(";")
    if len(fields) != 3:
        raise SystemExit(f"invalid ONNX Runtime dependency row: {line!r}")
    parsed = urllib.parse.urlsplit(fields[1])
    relative = pathlib.PurePosixPath(parsed.netloc + parsed.path)
    local_path = mirror.joinpath(*relative.parts)
    if not local_path.is_file():
        raise SystemExit(f"verified dependency mirror entry is missing: {local_path}")
    fields[1] = local_path.as_uri()
    rewritten.append(";".join(fields) + ending)
deps_path.write_text("".join(rewritten))
PY
}

stage_freebsd_source_build() {
  local destination="$1"
  local cache_dir="$2"
  local source_archive="${cache_dir}/onnxruntime-${ONNXRUNTIME_VERSION}-source.tar.gz"
  local spin_pause_patch="${cache_dir}/${FREEBSD_SPIN_PAUSE_PATCH}-${FREEBSD_PORTS_COMMIT}"
  local posix_env_patch="${cache_dir}/${FREEBSD_POSIX_ENV_PATCH}-${FREEBSD_PORTS_COMMIT}"
  local build_root="${CTX_ONNXRUNTIME_BUILD_DIR:-${work_dir}/freebsd-x64-build}"
  local source_parent="${build_root}/source"
  local source_dir="${source_parent}/onnxruntime-${ONNXRUNTIME_VERSION}"
  local cmake_build_dir="${build_root}/build"
  local jobs="${CTX_ONNXRUNTIME_BUILD_JOBS:-}"
  local freebsd_userland cmake_version cc cxx make_program patch_program python_program
  local reproducible_root common_flags cxx_flags built_library candidate
  local -a cmake_args build_args library_candidates

  [[ "$(uname -s)" == "FreeBSD" ]] || \
    die "freebsd-x64 ONNX Runtime must be built on a native FreeBSD host"
  case "$(uname -m)" in
    x86_64|amd64) ;;
    *) die "freebsd-x64 ONNX Runtime requires an x64 FreeBSD host, got $(uname -m)" ;;
  esac
  require_command freebsd-version
  require_command cmake
  require_command python3
  require_command clang
  require_command clang++
  require_command make
  require_command gpatch

  freebsd_userland="$(freebsd-version -u)"
  case "${freebsd_userland}" in
    "${FREEBSD_ABI_MAJOR}."*) ;;
    *) die "freebsd-x64 ONNX Runtime requires a FreeBSD ${FREEBSD_ABI_MAJOR} userland, got ${freebsd_userland}" ;;
  esac
  [[ "${freebsd_userland}" =~ ^[A-Za-z0-9._+-]+$ ]] || \
    die "freebsd-version returned an unsafe userland identifier: ${freebsd_userland}"
  cmake_version="$(cmake --version | awk 'NR == 1 { print $3 }')"
  python3 - "${cmake_version}" <<'PY'
import sys

actual = tuple(int(part) for part in sys.argv[1].split("."))
if actual < (3, 28):
    raise SystemExit(f"CMake >= 3.28 is required, got {sys.argv[1]}")
PY
  if [[ -n "${jobs}" ]]; then
    [[ "${jobs}" =~ ^[1-9][0-9]*$ ]] || die "CTX_ONNXRUNTIME_BUILD_JOBS must be a positive integer"
  fi

  download_verified "${ONNXRUNTIME_SOURCE_URL}" "${ONNXRUNTIME_SOURCE_SHA256}" "${source_archive}"
  download_verified \
    "${FREEBSD_PORTS_PATCH_BASE_URL}/${FREEBSD_SPIN_PAUSE_PATCH}?id=${FREEBSD_PORTS_COMMIT}" \
    "${FREEBSD_SPIN_PAUSE_PATCH_SHA256}" "${spin_pause_patch}"
  download_verified \
    "${FREEBSD_PORTS_PATCH_BASE_URL}/${FREEBSD_POSIX_ENV_PATCH}?id=${FREEBSD_PORTS_COMMIT}" \
    "${FREEBSD_POSIX_ENV_PATCH_SHA256}" "${posix_env_patch}"
  validate_source_archive_layout "${source_archive}"
  rm -rf "${source_parent}" "${cmake_build_dir}"
  mkdir -p "${source_parent}" "${cmake_build_dir}"
  tar -xzf "${source_archive}" -C "${source_parent}"
  verify_sha256 "${source_dir}/cmake/deps.txt" "${ONNXRUNTIME_DEPS_SHA256}"
  prepare_freebsd_dependency_mirror "${source_dir}" "${cache_dir}"

  patch_program="$(command -v gpatch)"
  "${patch_program}" --batch --forward --fuzz=0 -p0 -d "${source_dir}" < "${spin_pause_patch}"
  "${patch_program}" --batch --forward --fuzz=0 -p0 -d "${source_dir}" < "${posix_env_patch}"
  python3 - "${source_dir}/cmake/CMakeLists.txt" \
    "${FREEBSD_BUILD_RECIPE}" "${ONNXRUNTIME_SOURCE_SHA256}" \
    "${FREEBSD_PORTS_COMMIT}" "${FREEBSD_DISTINFO_SHA256}" \
    "${FREEBSD_ABI_MAJOR}" "${freebsd_userland}" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
recipe, source_sha256, ports_commit, deps_sha256, abi, freebsd_userland = sys.argv[2:]
needle = 'string(APPEND ORT_BUILD_INFO "build type=${CMAKE_BUILD_TYPE}")'
provenance = (
    'string(APPEND ORT_BUILD_INFO '
    f'"ctx-recipe={recipe}, ctx-source-sha256={source_sha256}, '
    f'ctx-freebsd-ports={ports_commit}, ctx-deps-sha256={deps_sha256}, '
    f'ctx-freebsd-abi={abi}, ctx-freebsd-userland={freebsd_userland}, '
    'ctx-os=${CMAKE_SYSTEM_NAME}-${CMAKE_SYSTEM_VERSION}, '
    'ctx-compiler=${CMAKE_CXX_COMPILER_ID}-${CMAKE_CXX_COMPILER_VERSION}, '
    'ctx-cmake=${CMAKE_VERSION}, ")'
)
text = path.read_text()
if text.count(needle) != 1:
    raise SystemExit("could not locate the unique ONNX Runtime build-info insertion point")
path.write_text(text.replace(needle, provenance + "\n" + needle))
PY

  cc="$(command -v clang)"
  cxx="$(command -v clang++)"
  make_program="$(command -v make)"
  python_program="$(command -v python3)"
  reproducible_root="/usr/src/ctx-onnxruntime-${ONNXRUNTIME_VERSION}"
  common_flags="-ffile-prefix-map=${source_dir}=${reproducible_root} -ffile-prefix-map=${cmake_build_dir}=${reproducible_root}/build -fdebug-prefix-map=${source_dir}=${reproducible_root} -fdebug-prefix-map=${cmake_build_dir}=${reproducible_root}/build"
  cxx_flags="${common_flags} -Wno-array-bounds -Wno-deprecated-declarations -I${source_dir}/include/onnxruntime/core/common/logging -frtti"
  cmake_args=(
    --compile-no-warning-as-error
    -S "${source_dir}/cmake"
    -B "${cmake_build_dir}"
    -G "Unix Makefiles"
    "-DCMAKE_BUILD_TYPE=Release"
    "-DCMAKE_C_COMPILER=${cc}"
    "-DCMAKE_CXX_COMPILER=${cxx}"
    "-DCMAKE_MAKE_PROGRAM=${make_program}"
    "-DPython_EXECUTABLE=${python_program}"
    "-DPython3_EXECUTABLE=${python_program}"
    "-DCMAKE_C_FLAGS=${common_flags}"
    "-DCMAKE_CXX_FLAGS=${cxx_flags}"
    "-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON"
    "-DCMAKE_INSTALL_RPATH="
    "-DCMAKE_SKIP_INSTALL_RPATH=ON"
    "-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF"
    "-DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF"
    "-DCMAKE_DISABLE_FIND_PACKAGE_Git=TRUE"
    "-DFETCHCONTENT_TRY_FIND_PACKAGE_MODE=NEVER"
    "-DFETCHCONTENT_FULLY_DISCONNECTED=OFF"
    "-DPatch_EXECUTABLE=${patch_program}"
    "-Donnxruntime_BUILD_SHARED_LIB=ON"
    "-Donnxruntime_BUILD_UNIT_TESTS=OFF"
    "-Donnxruntime_BUILD_BENCHMARKS=OFF"
    "-Donnxruntime_BUILD_FOR_NATIVE_MACHINE=OFF"
    "-Donnxruntime_ENABLE_CPUINFO=OFF"
    "-Donnxruntime_ENABLE_PYTHON=OFF"
    "-Donnxruntime_GENERATE_TEST_REPORTS=OFF"
    "-Donnxruntime_RUN_ONNX_TESTS=OFF"
    "-Donnxruntime_USE_AVX=OFF"
    "-Donnxruntime_USE_AVX2=OFF"
    "-Donnxruntime_USE_AVX512=OFF"
    "-Donnxruntime_USE_MIMALLOC=OFF"
    "-Donnxruntime_USE_XNNPACK=OFF"
  )
  env -u CC -u CXX -u CFLAGS -u CXXFLAGS -u CPPFLAGS -u LDFLAGS \
    -u CPATH -u C_INCLUDE_PATH -u CPLUS_INCLUDE_PATH -u LIBRARY_PATH \
    -u CMAKE_GENERATOR -u CMAKE_GENERATOR_PLATFORM -u CMAKE_GENERATOR_TOOLSET \
    -u CMAKE_PREFIX_PATH -u CMAKE_TOOLCHAIN_FILE -u MAKEFLAGS -u MFLAGS \
    SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH}" ZERO_AR_DATE=1 TZ=UTC LC_ALL=C LANG=C \
    cmake "${cmake_args[@]}"

  build_args=(--build "${cmake_build_dir}" --config Release --target onnxruntime)
  if [[ -n "${jobs}" ]]; then
    build_args+=(--parallel "${jobs}")
  else
    build_args+=(--parallel)
  fi
  env -u CC -u CXX -u CFLAGS -u CXXFLAGS -u CPPFLAGS -u LDFLAGS \
    -u CPATH -u C_INCLUDE_PATH -u CPLUS_INCLUDE_PATH -u LIBRARY_PATH \
    -u MAKEFLAGS -u MFLAGS -u CMAKE_BUILD_PARALLEL_LEVEL \
    SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH}" ZERO_AR_DATE=1 TZ=UTC LC_ALL=C LANG=C \
    cmake "${build_args[@]}"

  library_candidates=(
    "${cmake_build_dir}/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
    "${cmake_build_dir}/libonnxruntime.so"
    "${cmake_build_dir}/Release/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
    "${cmake_build_dir}/Release/libonnxruntime.so"
    "${cmake_build_dir}/Release/lib/libonnxruntime.so.${ONNXRUNTIME_VERSION}"
    "${cmake_build_dir}/Release/lib/libonnxruntime.so"
  )
  built_library=""
  for candidate in "${library_candidates[@]}"; do
    if [[ -f "${candidate}" ]]; then
      built_library="${candidate}"
      break
    fi
  done
  [[ -n "${built_library}" ]] || \
    die "freebsd-x64 source build did not produce libonnxruntime.so"

  cp -L "${built_library}" "${destination}/lib/${library_name}"
  chmod 755 "${destination}/lib/${library_name}"
  stage_pinned_documents "${destination}" "${cache_dir}"
}

create_archive() {
  local source_dir="$1"
  local output="$2"

  if [[ "${archive_kind}" == "tar.zst" ]]; then
    require_command zstd
    python3 - "${source_dir}" "${output}.tar" "${library_name}" <<'PY'
import os
import sys
import tarfile

source, output, library = sys.argv[1:]
mtime = 1781827200
with tarfile.open(output, "w", format=tarfile.USTAR_FORMAT) as bundle:
    directory = tarfile.TarInfo("lib/")
    directory.type = tarfile.DIRTYPE
    directory.mode = 0o755
    directory.uid = directory.gid = 0
    directory.uname = directory.gname = "root"
    directory.mtime = mtime
    bundle.addfile(directory)
    for name in ("LICENSE", "ThirdPartyNotices.txt", "VERSION_NUMBER", "GIT_COMMIT_ID", f"lib/{library}"):
        path = os.path.join(source, *name.split("/"))
        info = tarfile.TarInfo(name)
        info.size = os.path.getsize(path)
        info.mode = 0o755 if name.startswith("lib/") else 0o644
        info.uid = info.gid = 0
        info.uname = info.gname = "root"
        info.mtime = mtime
        with open(path, "rb") as handle:
            bundle.addfile(info, handle)
PY
    zstd -q -19 --threads=0 -f "${output}.tar" -o "${output}"
    rm -f "${output}.tar"
  else
    python3 - "${source_dir}" "${output}" "${library_name}" <<'PY'
import os
import sys
import zipfile

source, output, library = sys.argv[1:]
timestamp = (2026, 6, 19, 0, 0, 0)


def info(name, mode):
    entry = zipfile.ZipInfo(name, timestamp)
    entry.create_system = 3
    entry.compress_type = zipfile.ZIP_DEFLATED
    entry.external_attr = mode << 16
    return entry


with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
    bundle.writestr(info("lib/", 0o40755), b"")
    for name in ("LICENSE", "ThirdPartyNotices.txt", "VERSION_NUMBER", "GIT_COMMIT_ID", f"lib/{library}"):
        path = os.path.join(source, *name.split("/"))
        mode = 0o100755 if name.startswith("lib/") else 0o100644
        with open(path, "rb") as handle:
            bundle.writestr(info(name, mode), handle.read())
PY
  fi
}

extract_and_check_archive_layout() {
  local archive="$1"
  local destination="$2"
  local inspection_archive="${archive}"

  if [[ "${archive_kind}" == "tar.zst" ]]; then
    require_command zstd
    zstd -q -t "${archive}"
    inspection_archive="${work_dir}/validated.tar"
    zstd -q -d -f "${archive}" -o "${inspection_archive}"
  fi

  python3 - "${archive_kind}" "${inspection_archive}" "${destination}" "${library_name}" <<'PY'
import os
import posixpath
import shutil
import stat
import sys
import tarfile
import zipfile

kind, archive, destination, library = sys.argv[1:]
expected_files = {
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "GIT_COMMIT_ID",
    f"lib/{library}",
}
expected_entries = expected_files | {"lib"}


def canonical_name(raw):
    if not raw or "\\" in raw or raw.startswith("/"):
        raise SystemExit(f"unsafe sidecar archive path: {raw!r}")
    while raw.startswith("./"):
        raw = raw[2:]
    name = posixpath.normpath(raw.rstrip("/"))
    if name in ("", ".", "..") or name.startswith("../"):
        raise SystemExit(f"unsafe sidecar archive path: {raw!r}")
    return name


os.makedirs(os.path.join(destination, "lib"), exist_ok=True)
seen = set()
if kind == "tar.zst":
    with tarfile.open(archive, "r:") as bundle:
        members = {}
        for member in bundle.getmembers():
            name = canonical_name(member.name)
            if name in seen:
                raise SystemExit(f"duplicate sidecar archive entry: {name}")
            seen.add(name)
            if name not in expected_entries:
                raise SystemExit(f"unexpected sidecar archive entry: {name}")
            if member.mode & 0o7000:
                raise SystemExit(f"unsafe permission bits on sidecar archive entry: {name}")
            if name == "lib":
                if not member.isdir():
                    raise SystemExit("sidecar lib entry is not a directory")
            elif not member.isfile():
                raise SystemExit(f"sidecar entry is not a regular file: {name}")
            members[name] = member
        if seen != expected_entries:
            missing = sorted(expected_entries - seen)
            raise SystemExit("sidecar archive entries missing: " + ", ".join(missing))
        for name in expected_files:
            member = members[name]
            source_file = bundle.extractfile(member)
            if source_file is None:
                raise SystemExit(f"could not read sidecar archive member: {name}")
            target = os.path.join(destination, *name.split("/"))
            with source_file, open(target, "wb") as output:
                shutil.copyfileobj(source_file, output)
elif kind == "zip":
    with zipfile.ZipFile(archive) as bundle:
        members = {}
        for member in bundle.infolist():
            name = canonical_name(member.filename)
            if name in seen:
                raise SystemExit(f"duplicate sidecar archive entry: {name}")
            seen.add(name)
            if name not in expected_entries:
                raise SystemExit(f"unexpected sidecar archive entry: {name}")
            if member.flag_bits & 1:
                raise SystemExit(f"encrypted sidecar archive entry: {name}")
            mode = member.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise SystemExit(f"sidecar zip contains a symbolic link: {name}")
            if mode & 0o7000:
                raise SystemExit(f"unsafe permission bits on sidecar archive entry: {name}")
            if name == "lib":
                if not member.is_dir():
                    raise SystemExit("sidecar lib entry is not a directory")
            elif member.is_dir():
                raise SystemExit(f"sidecar entry is not a regular file: {name}")
            members[name] = member
        if seen != expected_entries:
            missing = sorted(expected_entries - seen)
            raise SystemExit("sidecar archive entries missing: " + ", ".join(missing))
        for name in expected_files:
            target = os.path.join(destination, *name.split("/"))
            with bundle.open(members[name]) as source_file, open(target, "wb") as output:
                shutil.copyfileobj(source_file, output)
else:
    raise SystemExit(f"unsupported sidecar archive kind: {kind}")
PY
}

check_native_library() {
  local library="$1"
  local max_glibc="${CTX_ONNXRUNTIME_MAX_GLIBC:-${ONNXRUNTIME_MAX_GLIBC}}"

  [[ "${max_glibc}" =~ ^[0-9]+\.[0-9]+$ ]] || \
    die "CTX_ONNXRUNTIME_MAX_GLIBC must be MAJOR.MINOR, got ${max_glibc}"
  python3 - "${platform}" "${library}" "${ONNXRUNTIME_VERSION}" "${max_glibc}" \
    "${FREEBSD_BUILD_RECIPE}" "${ONNXRUNTIME_SOURCE_SHA256}" \
    "${FREEBSD_PORTS_COMMIT}" "${FREEBSD_DISTINFO_SHA256}" \
    "${FREEBSD_ABI_MAJOR}" <<'PY'
import re
import struct
import sys

(
    platform,
    path,
    version,
    max_glibc,
    recipe,
    source_sha256,
    ports_commit,
    deps_sha256,
    freebsd_abi,
) = sys.argv[1:]
with open(path, "rb") as handle:
    data = handle.read()
if len(data) < 64:
    raise SystemExit(f"native runtime library is implausibly small: {len(data)} bytes")
if version.encode() not in data:
    raise SystemExit(f"native runtime library does not contain version marker {version}")

if platform.startswith("linux-") or platform == "freebsd-x64":
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise SystemExit("native runtime library is not 64-bit little-endian ELF")
    elf_type, machine = struct.unpack_from("<HH", data, 16)
    expected_machine = 183 if platform == "linux-aarch64" else 62
    if elf_type != 3:
        raise SystemExit(f"native runtime ELF type is {elf_type}, expected ET_DYN (3)")
    if machine != expected_machine:
        raise SystemExit(
            f"native runtime ELF machine is {machine}, expected {expected_machine} for {platform}"
        )
    osabi = data[7]
    if platform == "freebsd-x64":
        if osabi != 9:
            raise SystemExit(f"native runtime ELF OSABI is {osabi}, expected FreeBSD (9)")
        required_provenance = (
            f"ctx-recipe={recipe}",
            f"ctx-source-sha256={source_sha256}",
            f"ctx-freebsd-ports={ports_commit}",
            f"ctx-deps-sha256={deps_sha256}",
            f"ctx-freebsd-abi={freebsd_abi}",
            f"ctx-freebsd-userland={freebsd_abi}.",
            "ctx-os=FreeBSD-",
            "ctx-compiler=Clang-",
            "ctx-cmake=",
            "build type=Release",
        )
        missing = [marker for marker in required_provenance if marker.encode() not in data]
        if missing:
            raise SystemExit(
                "native FreeBSD runtime is missing pinned build provenance: "
                + ", ".join(missing)
            )
    elif osabi not in (0, 3):
        raise SystemExit(f"native runtime ELF OSABI is {osabi}, expected System V or GNU/Linux")
    if platform.startswith("linux-"):
        versions = {
            (int(match.group(1)), int(match.group(2)))
            for match in re.finditer(rb"GLIBC_(\d+)\.(\d+)", data)
        }
        if not versions:
            raise SystemExit("native Linux runtime has no GLIBC symbol versions")
        allowed = tuple(int(part) for part in max_glibc.split("."))
        required = max(versions)
        if required > allowed:
            raise SystemExit(
                f"native Linux runtime requires GLIBC_{required[0]}.{required[1]}, "
                f"newer than allowed GLIBC_{allowed[0]}.{allowed[1]}"
            )
elif platform.startswith("macos-"):
    magic, cpu_type, _cpu_subtype, file_type = struct.unpack_from("<IIII", data, 0)
    expected_cpu = 0x0100000C if platform == "macos-arm64" else 0x01000007
    if magic != 0xFEEDFACF:
        raise SystemExit("native runtime library is not a thin 64-bit little-endian Mach-O")
    if cpu_type != expected_cpu:
        raise SystemExit(
            f"native runtime Mach-O CPU type is 0x{cpu_type:08x}, "
            f"expected 0x{expected_cpu:08x} for {platform}"
        )
    if file_type != 6:
        raise SystemExit(f"native runtime Mach-O file type is {file_type}, expected MH_DYLIB (6)")
elif platform == "windows-x64":
    if data[:2] != b"MZ" or len(data) < 0x40:
        raise SystemExit("native runtime library is not a PE image")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset + 26 > len(data) or data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise SystemExit("native runtime library has an invalid PE header")
    machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
    characteristics = struct.unpack_from("<H", data, pe_offset + 22)[0]
    optional_magic = struct.unpack_from("<H", data, pe_offset + 24)[0]
    if machine != 0x8664:
        raise SystemExit(f"native runtime PE machine is 0x{machine:04x}, expected AMD64")
    if optional_magic != 0x20B:
        raise SystemExit("native runtime PE image is not PE32+")
    if not characteristics & 0x2000:
        raise SystemExit("native runtime PE image is not marked as a DLL")
else:
    raise SystemExit(f"unsupported platform: {platform}")
PY
}

host_matches_platform() {
  local system machine

  system="$(uname -s 2>/dev/null || true)"
  machine="$(uname -m 2>/dev/null || true)"
  case "${platform}:${system}:${machine}" in
    linux-x64:Linux:x86_64|linux-x64:Linux:amd64) return 0 ;;
    linux-aarch64:Linux:aarch64|linux-aarch64:Linux:arm64) return 0 ;;
    macos-arm64:Darwin:arm64|macos-x64:Darwin:x86_64) return 0 ;;
    freebsd-x64:FreeBSD:x86_64|freebsd-x64:FreeBSD:amd64) return 0 ;;
    windows-x64:MINGW*:x86_64|windows-x64:MSYS*:x86_64|windows-x64:CYGWIN*:x86_64) return 0 ;;
    *) return 1 ;;
  esac
}

check_native_runtime_version() {
  local library="$1"

  if ! host_matches_platform; then
    printf 'native ONNX Runtime load check skipped on %s/%s for %s\n' \
      "$(uname -s 2>/dev/null || printf unknown)" "$(uname -m 2>/dev/null || printf unknown)" "${platform}"
    return
  fi

  python3 - "${platform}" "${library}" "${ONNXRUNTIME_VERSION}" \
    "${ONNXRUNTIME_API_VERSION}" "${FREEBSD_BUILD_RECIPE}" \
    "${ONNXRUNTIME_SOURCE_SHA256}" "${FREEBSD_PORTS_COMMIT}" \
    "${FREEBSD_DISTINFO_SHA256}" "${FREEBSD_ABI_MAJOR}" <<'PY'
import ctypes
import os
import sys

(
    platform,
    path,
    expected,
    api_version,
    recipe,
    source_sha256,
    ports_commit,
    deps_sha256,
    freebsd_abi,
) = sys.argv[1:]
if os.name == "nt":
    os.add_dll_directory(os.path.dirname(os.path.abspath(path)))
runtime = ctypes.CDLL(os.path.abspath(path))
runtime.OrtGetApiBase.argtypes = []
runtime.OrtGetApiBase.restype = ctypes.c_void_p
base = runtime.OrtGetApiBase()
if not base:
    raise SystemExit("OrtGetApiBase returned null")
entries = ctypes.cast(base, ctypes.POINTER(ctypes.c_void_p))
callback_type = getattr(ctypes, "WINFUNCTYPE", ctypes.CFUNCTYPE) if os.name == "nt" else ctypes.CFUNCTYPE
get_api = callback_type(ctypes.c_void_p, ctypes.c_uint32)(entries[0])
api = get_api(int(api_version))
if not api:
    raise SystemExit(f"OrtApiBase::GetApi({api_version}) returned null")
get_version = callback_type(ctypes.c_char_p)(entries[1])
actual_bytes = get_version()
actual = actual_bytes.decode("utf-8") if actual_bytes else ""
if actual != expected:
    raise SystemExit(f"OrtGetVersionString returned {actual!r}, expected {expected!r}")
if platform == "freebsd-x64":
    api_entries = ctypes.cast(api, ctypes.POINTER(ctypes.c_void_p))
    get_build_info_address = api_entries[254]
    if not get_build_info_address:
        raise SystemExit("OrtApi::GetBuildInfoString is null")
    get_build_info = callback_type(ctypes.c_char_p)(get_build_info_address)
    build_info_bytes = get_build_info()
    build_info = build_info_bytes.decode("utf-8") if build_info_bytes else ""
    required_provenance = (
        f"ctx-recipe={recipe}",
        f"ctx-source-sha256={source_sha256}",
        f"ctx-freebsd-ports={ports_commit}",
        f"ctx-deps-sha256={deps_sha256}",
        f"ctx-freebsd-abi={freebsd_abi}",
        f"ctx-freebsd-userland={freebsd_abi}.",
        "ctx-os=FreeBSD-",
        "ctx-compiler=Clang-",
        "ctx-cmake=",
        "build type=Release",
    )
    missing = [marker for marker in required_provenance if marker not in build_info]
    if missing:
        raise SystemExit(
            "OrtGetBuildInfoString is missing pinned FreeBSD provenance: "
            + ", ".join(missing)
        )
PY
}

validate_archive() {
  local archive="$1"
  local archive_base validation_dir library_path

  [[ -f "${archive}" ]] || die "ONNX Runtime sidecar archive not found: ${archive}"
  archive_base="$(basename "${archive}")"
  [[ "${archive_base}" == "${asset_name}" ]] || \
    die "sidecar archive must be named ${asset_name}, got ${archive_base}"

  validation_dir="${work_dir}/validation"
  rm -rf "${validation_dir}"
  mkdir -p "${validation_dir}"
  extract_and_check_archive_layout "${archive}" "${validation_dir}"

  verify_sha256 "${validation_dir}/LICENSE" "${ONNXRUNTIME_LICENSE_SHA256}"
  verify_sha256 "${validation_dir}/ThirdPartyNotices.txt" "${ONNXRUNTIME_NOTICES_SHA256}"
  [[ "$(cat "${validation_dir}/VERSION_NUMBER")" == "${ONNXRUNTIME_VERSION}" ]] || \
    die "sidecar VERSION_NUMBER is not exactly ${ONNXRUNTIME_VERSION}"
  [[ "$(wc -c < "${validation_dir}/VERSION_NUMBER" | tr -d '[:space:]')" == "7" ]] || \
    die "sidecar VERSION_NUMBER has unexpected whitespace or content"
  [[ "$(cat "${validation_dir}/GIT_COMMIT_ID")" == "${ONNXRUNTIME_COMMIT}" ]] || \
    die "sidecar GIT_COMMIT_ID is not ${ONNXRUNTIME_COMMIT}"
  [[ "$(wc -c < "${validation_dir}/GIT_COMMIT_ID" | tr -d '[:space:]')" == "41" ]] || \
    die "sidecar GIT_COMMIT_ID has unexpected whitespace or content"

  library_path="${validation_dir}/lib/${library_name}"
  [[ -s "${library_path}" ]] || die "sidecar runtime library is missing or empty: lib/${library_name}"
  [[ "$(wc -c < "${library_path}" | tr -d '[:space:]')" -ge 1048576 ]] || \
    die "sidecar runtime library is implausibly small: lib/${library_name}"
  check_native_library "${library_path}"
  check_native_runtime_version "${library_path}"
  printf 'ONNX Runtime sidecar ok: %s version=%s\n' "${platform}" "${ONNXRUNTIME_VERSION}"
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

configure_platform "$1"
shift
require_command python3
require_command tar
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-onnxruntime-sidecar.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

if [[ "${mode}" == "validate" ]]; then
  [[ $# -eq 1 ]] || {
    usage
    exit 2
  }
  validate_archive "$1"
  exit 0
fi

[[ $# -le 1 ]] || {
  usage
  exit 2
}
output_dir="${1:-target/public-cli-artifacts}"
cache_dir="${CTX_ONNXRUNTIME_CACHE_DIR:-target/onnxruntime-sidecar-cache}"
stage_dir="${work_dir}/stage"
package_dir="${work_dir}/package"
mkdir -p "${stage_dir}/lib" "${package_dir}" "${cache_dir}"

case "${platform}" in
  linux-x64|linux-aarch64|macos-arm64|windows-x64)
    stage_official_release "${stage_dir}" "${cache_dir}"
    ;;
  macos-x64)
    stage_macos_x64_source_build "${stage_dir}" "${cache_dir}"
    ;;
  freebsd-x64)
    stage_freebsd_source_build "${stage_dir}" "${cache_dir}"
    ;;
esac

package_path="${package_dir}/${asset_name}"
create_archive "${stage_dir}" "${package_path}"
validate_archive "${package_path}"

mkdir -p "${output_dir}"
temporary_output="${output_dir%/}/.${asset_name}.tmp.$$"
rm -f "${temporary_output}"
cp "${package_path}" "${temporary_output}"
chmod 644 "${temporary_output}"
mv "${temporary_output}" "${output_dir%/}/${asset_name}"
sha256_file "${output_dir%/}/${asset_name}" > "${output_dir%/}/${asset_name}.sha256.tmp.$$"
mv "${output_dir%/}/${asset_name}.sha256.tmp.$$" "${output_dir%/}/${asset_name}.sha256"
printf 'built %s sha256=%s\n' \
  "${output_dir%/}/${asset_name}" "$(cat "${output_dir%/}/${asset_name}.sha256")"
