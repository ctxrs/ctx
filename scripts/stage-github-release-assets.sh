#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/stage-github-release-assets.sh [ARTIFACT_DIR] [OUT_DIR]
       scripts/stage-github-release-assets.sh --transcode-runtime PLATFORM [ARTIFACT_DIR]

Stages public GitHub Release assets from built public CLI artifacts.

Inputs default to target/public-cli-artifacts.
Outputs default to target/github-release-assets.

Optional ONNX Runtime sidecars are staged when present in ARTIFACT_DIR. Set
CTX_ONNXRUNTIME_ASSET_REQUIRED=1 to require every platform sidecar.

The transcode mode converts a validated builder-owned Unix .tar.zst sidecar
to the deterministic .tar.gz transport consumed by release installers.
USAGE
}

mode="stage"
if [[ "${1:-}" == "--transcode-runtime" ]]; then
  mode="transcode"
  transcode_platform="${2:-}"
  artifact_dir="${3:-target/public-cli-artifacts}"
  out_dir=""
else
  artifact_dir="${1:-target/public-cli-artifacts}"
  out_dir="${2:-target/github-release-assets}"
fi

if [[ "${artifact_dir}" == "-h" || "${artifact_dir}" == "--help" ]]; then
  usage
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
    return
  fi

  printf 'sha256sum or shasum is required\n' >&2
  exit 127
}

transcode_runtime_asset() {
  local platform="$1"
  local source_name dest_name source_path dest_path

  case "${platform}" in
    linux-x64|linux-aarch64|macos-arm64|macos-x64|freebsd-x64)
      source_name="ctx-onnxruntime-${platform}.tar.zst"
      dest_name="ctx-onnxruntime-${platform}.tar.gz"
      ;;
    *)
      printf 'transcode mode does not support runtime platform: %s\n' "${platform}" >&2
      exit 2
      ;;
  esac
  source_path="${artifact_dir%/}/${source_name}"
  dest_path="${artifact_dir%/}/${dest_name}"
  test -f "${source_path}" || {
    printf 'runtime source archive missing: %s\n' "${source_path}" >&2
    exit 1
  }
  command -v python3 >/dev/null 2>&1 || {
    printf 'python3 is required to transcode runtime archives\n' >&2
    exit 127
  }
  command -v zstd >/dev/null 2>&1 || {
    printf 'zstd is required on runtime producer hosts\n' >&2
    exit 127
  }

  bash scripts/build-onnxruntime-sidecar.sh --validate "${platform}" "${source_path}"
  python3 - "${source_path}" "${dest_path}.tmp" <<'PY'
import gzip
import shutil
import subprocess
import sys

source, destination = sys.argv[1:]
with open(destination, "wb") as raw_output:
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw_output, compresslevel=9, mtime=0) as output:
        process = subprocess.Popen(["zstd", "-q", "-d", "-c", source], stdout=subprocess.PIPE)
        assert process.stdout is not None
        with process.stdout:
            shutil.copyfileobj(process.stdout, output)
        status = process.wait()
        if status != 0:
            raise SystemExit(f"zstd decompression failed with status {status}")
PY
  mv "${dest_path}.tmp" "${dest_path}"
  sha256_file "${dest_path}" > "${dest_path}.sha256"
  rm -f "${source_path}" "${source_path}.sha256"
  printf 'transcoded runtime release asset %s\n' "${dest_path}"
}

if [[ "${mode}" == "transcode" ]]; then
  [[ -n "${transcode_platform}" ]] || {
    usage
    exit 2
  }
  transcode_runtime_asset "${transcode_platform}"
  exit 0
fi

stage_asset() {
  local source_name="$1"
  local dest_name="$2"
  local mode="${3:-0755}"
  local source_path="${artifact_dir%/}/${source_name}"
  local source_sha_path="${source_path}.sha256"
  local dest_path="${out_dir%/}/${dest_name}"
  local expected_sha actual_sha

  if [[ ! -f "${source_path}" ]]; then
    printf 'missing public CLI artifact: %s\n' "${source_path}" >&2
    exit 1
  fi
  if [[ ! -s "${source_sha_path}" ]]; then
    printf 'missing public artifact checksum: %s\n' "${source_sha_path}" >&2
    exit 1
  fi
  expected_sha="$(tr -d '[:space:]' < "${source_sha_path}")"
  if [[ ! "${expected_sha}" =~ ^[0-9a-fA-F]{64}$ ]]; then
    printf 'invalid public artifact checksum: %s\n' "${source_sha_path}" >&2
    exit 1
  fi
  actual_sha="$(sha256_file "${source_path}")"
  if [[ "$(printf '%s' "${actual_sha}" | tr 'A-F' 'a-f')" != "$(printf '%s' "${expected_sha}" | tr 'A-F' 'a-f')" ]]; then
    printf 'public artifact checksum mismatch for %s: expected %s got %s\n' \
      "${source_path}" "${expected_sha}" "${actual_sha}" >&2
    exit 1
  fi

  install -m "${mode}" "${source_path}" "${dest_path}"
  printf '%s  %s\n' "${actual_sha}" "${dest_name}" >> "${out_dir%/}/SHA256SUMS"
}

stage_optional_runtime_asset() {
  local platform="$1"
  local asset_name required
  required="${CTX_ONNXRUNTIME_ASSET_REQUIRED:-0}"

  case "${required}" in
    0|1) ;;
    *)
      printf 'CTX_ONNXRUNTIME_ASSET_REQUIRED must be 0 or 1\n' >&2
      exit 2
      ;;
  esac

  case "${platform}" in
    linux-x64) asset_name="ctx-onnxruntime-linux-x64.tar.gz" ;;
    linux-aarch64) asset_name="ctx-onnxruntime-linux-aarch64.tar.gz" ;;
    macos-arm64) asset_name="ctx-onnxruntime-macos-arm64.tar.gz" ;;
    macos-x64) asset_name="ctx-onnxruntime-macos-x64.tar.gz" ;;
    windows-x64) asset_name="ctx-onnxruntime-windows-x64.zip" ;;
    freebsd-x64) asset_name="ctx-onnxruntime-freebsd-x64.tar.gz" ;;
    *)
      printf 'unknown platform for ONNX Runtime staging: %s\n' "${platform}" >&2
      exit 2
      ;;
  esac

  if [[ ! -f "${artifact_dir%/}/${asset_name}" ]]; then
    if [[ "${required}" == "1" ]]; then
      printf 'required ONNX Runtime sidecar missing: %s\n' "${artifact_dir%/}/${asset_name}" >&2
      exit 1
    fi
    return
  fi

  if [[ "${platform}" == "windows-x64" ]]; then
    bash scripts/build-onnxruntime-sidecar.sh --validate \
      "${platform}" "${artifact_dir%/}/${asset_name}"
  else
    python3 - "${artifact_dir%/}/${asset_name}" "${platform}" <<'PY'
import posixpath
import stat
import sys
import tarfile

archive, platform = sys.argv[1:]
library = "libonnxruntime.dylib" if platform.startswith("macos-") else "libonnxruntime.so"
expected_files = {
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "GIT_COMMIT_ID",
    f"lib/{library}",
}
expected = expected_files | {"lib"}
seen = set()
with tarfile.open(archive, "r:gz") as bundle:
    for member in bundle.getmembers():
        raw = member.name
        name = posixpath.normpath(raw.rstrip("/"))
        if (
            not raw
            or "\\" in raw
            or raw.startswith("/")
            or name in ("", ".", "..")
            or name.startswith("../")
            or raw != name
        ):
            raise SystemExit(f"unsafe runtime archive path: {raw!r}")
        if name in seen:
            raise SystemExit(f"duplicate runtime archive entry: {name}")
        seen.add(name)
        if name not in expected:
            raise SystemExit(f"unexpected runtime archive entry: {name}")
        if member.mode & 0o7000:
            raise SystemExit(f"unsafe permission bits on runtime archive entry: {name}")
        if name == "lib":
            if not member.isdir():
                raise SystemExit("runtime lib entry is not a directory")
        elif not member.isfile():
            raise SystemExit(f"runtime archive entry is not a regular file: {name}")
    if seen != expected:
        raise SystemExit("runtime archive entries do not exactly match the expected layout")
PY
  fi
  stage_asset "${asset_name}" "${asset_name}" 0644
}

mkdir -p "${out_dir}"
rm -f \
  "${out_dir%/}/ctx-linux-aarch64" \
  "${out_dir%/}/ctx-linux-x64" \
  "${out_dir%/}/ctx-macos-arm64" \
  "${out_dir%/}/ctx-macos-x64" \
  "${out_dir%/}/ctx-windows-x64.exe" \
  "${out_dir%/}/ctx-freebsd-x64" \
  "${out_dir%/}/ctx-onnxruntime-linux-x64.tar.gz" \
  "${out_dir%/}/ctx-onnxruntime-linux-aarch64.tar.gz" \
  "${out_dir%/}/ctx-onnxruntime-macos-arm64.tar.gz" \
  "${out_dir%/}/ctx-onnxruntime-macos-x64.tar.gz" \
  "${out_dir%/}/ctx-onnxruntime-windows-x64.zip" \
  "${out_dir%/}/ctx-onnxruntime-freebsd-x64.tar.gz" \
  "${out_dir%/}/SHA256SUMS"

stage_asset ctx ctx-linux-x64
stage_asset ctx-linux-aarch64 ctx-linux-aarch64
stage_asset ctx-macos-arm64 ctx-macos-arm64
stage_asset ctx-macos-x64 ctx-macos-x64
stage_asset ctx.exe ctx-windows-x64.exe
stage_asset ctx-freebsd-x64 ctx-freebsd-x64
stage_optional_runtime_asset linux-x64
stage_optional_runtime_asset linux-aarch64
stage_optional_runtime_asset macos-arm64
stage_optional_runtime_asset macos-x64
stage_optional_runtime_asset windows-x64
stage_optional_runtime_asset freebsd-x64

printf 'staged GitHub release assets in %s\n' "${out_dir}"
