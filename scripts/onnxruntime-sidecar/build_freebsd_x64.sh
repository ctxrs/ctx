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

prepare_dependency_mirror() {
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
manifest_path.write_text("".join("\t".join(row) + "\n" for row in rows))
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

if [[ $# -lt 2 || $# -gt 3 ]]; then
  printf 'usage: %s DESTINATION CACHE_DIR [WORK_DIR]\n' "$0" >&2
  exit 2
fi
configure_release_platform freebsd-x64
destination="$1"
cache_dir="$2"
if [[ $# -eq 3 ]]; then
  work_dir="$3"
else
  work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-onnxruntime-freebsd-x64.XXXXXX")"
  trap 'rm -rf "${work_dir}"' EXIT
fi
source_archive="${cache_dir}/onnxruntime-${ONNXRUNTIME_VERSION}-source.tar.gz"
spin_pause_patch="${cache_dir}/${FREEBSD_SPIN_PAUSE_PATCH}-${FREEBSD_PORTS_COMMIT}"
posix_env_patch="${cache_dir}/${FREEBSD_POSIX_ENV_PATCH}-${FREEBSD_PORTS_COMMIT}"
build_root="${CTX_ONNXRUNTIME_BUILD_DIR:-${work_dir}/freebsd-x64-build}"
source_parent="${build_root}/source"
source_dir="${source_parent}/onnxruntime-${ONNXRUNTIME_VERSION}"
cmake_build_dir="${build_root}/build"
jobs="${CTX_ONNXRUNTIME_BUILD_JOBS:-}"

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
require_command tar

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
  [[ "${jobs}" =~ ^[1-9][0-9]*$ ]] || \
    die "CTX_ONNXRUNTIME_BUILD_JOBS must be a positive integer"
fi

mkdir -p "${destination}/lib" "${cache_dir}" "${work_dir}"
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
prepare_dependency_mirror "${source_dir}" "${cache_dir}"

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
