#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/stage-github-release-assets.sh [ARTIFACT_DIR] [OUT_DIR]
       scripts/stage-github-release-assets.sh --transcode-runtime PLATFORM [ARTIFACT_DIR]

Stages public GitHub Release assets from built public CLI artifacts.

Inputs default to target/public-cli-artifacts.
Outputs default to target/github-release-assets.

Every ONNX Runtime sidecar is required. Release assembly fails closed when a
platform runtime is absent.

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

stage_runtime_asset() {
  local platform="$1"
  local asset_name

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
    printf 'required ONNX Runtime sidecar missing: %s\n' "${artifact_dir%/}/${asset_name}" >&2
    exit 1
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

required_runtime_assets=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
  ctx-onnxruntime-freebsd-x64.tar.gz
)
for required_runtime_asset in "${required_runtime_assets[@]}"; do
  if [[ ! -f "${artifact_dir%/}/${required_runtime_asset}" ]]; then
    printf 'required ONNX Runtime sidecar missing: %s\n' \
      "${artifact_dir%/}/${required_runtime_asset}" >&2
    exit 1
  fi
done

validate_authoritative_runtime_proof() {
  local platform="$1"
  local binary_name="$2"
  local proof_name="$3"
  local runtime="$4"
  local host_system="$5"
  local host_arch="$6"
  local host_native_arch="$7"
  local runtime_asset="${8:-}"
  local native_arch_probe="$9"
  local binary_sha_path="${artifact_dir%/}/${binary_name}.sha256"
  local proof_path="${artifact_dir%/}/${proof_name}"
  local expected_sha runtime_sha_path runtime_path expected_runtime_sha actual_runtime_sha duplicate_key

  [[ -s "${proof_path}" ]] || {
    printf 'required authoritative runtime proof missing: %s\n' "${proof_path}" >&2
    exit 1
  }
  [[ -s "${binary_sha_path}" ]] || {
    printf 'required binary checksum missing for runtime proof: %s\n' "${binary_sha_path}" >&2
    exit 1
  }
  duplicate_key="$(sed -n 's/^\([^=][^=]*\)=.*/\1/p' "${proof_path}" | sort | uniq -d | head -n 1)"
  [[ -z "${duplicate_key}" ]] || {
    printf 'runtime proof contains duplicate field %s: %s\n' "${duplicate_key}" "${proof_path}" >&2
    exit 1
  }
  expected_sha="$(tr -d '[:space:]' < "${binary_sha_path}")"
  [[ "${expected_sha}" =~ ^[0-9a-fA-F]{64}$ ]] || {
    printf 'invalid binary checksum for runtime proof: %s\n' "${binary_sha_path}" >&2
    exit 1
  }

  grep -Fxq "runtime=${runtime}" "${proof_path}" || {
    printf 'runtime proof has wrong runtime: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "platform=${platform}" "${proof_path}" || {
    printf 'runtime proof has wrong platform: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "host_system=${host_system}" "${proof_path}" || {
    printf 'runtime proof has wrong host system: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "host_arch=${host_arch}" "${proof_path}" || {
    printf 'runtime proof has wrong host architecture: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "host_native_arch=${host_native_arch}" "${proof_path}" || {
    printf 'runtime proof has wrong native host architecture: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq 'process_translated=0' "${proof_path}" || {
    printf 'runtime proof was produced by a translated process: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "native_arch_probe=${native_arch_probe}" "${proof_path}" || {
    printf 'runtime proof used the wrong native architecture probe: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq 'runtime_authority=authoritative' "${proof_path}" || {
    printf 'runtime proof is not authoritative: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "artifact_sha256=${expected_sha}" "${proof_path}" || {
    printf 'runtime proof does not match the exact release binary: %s\n' "${proof_path}" >&2
    exit 1
  }
  if [[ -n "${runtime_asset}" ]]; then
    runtime_sha_path="${artifact_dir%/}/${runtime_asset}.sha256"
    runtime_path="${artifact_dir%/}/${runtime_asset}"
    [[ -s "${runtime_sha_path}" ]] || {
      printf 'required runtime checksum missing for runtime proof: %s\n' "${runtime_sha_path}" >&2
      exit 1
    }
    expected_runtime_sha="$(tr -d '[:space:]' < "${runtime_sha_path}")"
    [[ "${expected_runtime_sha}" =~ ^[0-9a-fA-F]{64}$ ]] || {
      printf 'invalid runtime checksum for runtime proof: %s\n' "${runtime_sha_path}" >&2
      exit 1
    }
    actual_runtime_sha="$(sha256_file "${runtime_path}")"
    if [[ "$(printf '%s' "${actual_runtime_sha}" | tr 'A-F' 'a-f')" != \
      "$(printf '%s' "${expected_runtime_sha}" | tr 'A-F' 'a-f')" ]]; then
      printf 'runtime archive checksum mismatch for proof: %s\n' "${runtime_path}" >&2
      exit 1
    fi
    grep -Fxiq "runtime_archive_sha256=${actual_runtime_sha}" "${proof_path}" || {
      printf 'runtime proof does not match the exact runtime sidecar: %s\n' "${proof_path}" >&2
      exit 1
    }
  fi
  grep -Fxq 'semantic_search=passed' "${proof_path}" || {
    printf 'runtime proof does not record semantic search success: %s\n' "${proof_path}" >&2
    exit 1
  }
}

validate_authoritative_runtime_proof \
  linux-x64 ctx ctx-linux-x64.native-runtime-proof.txt \
  onnxruntime Linux x86_64 x86_64 ctx-onnxruntime-linux-x64.tar.gz uname
validate_authoritative_runtime_proof \
  linux-aarch64 ctx-linux-aarch64 ctx-linux-aarch64.native-runtime-proof.txt \
  onnxruntime Linux aarch64 aarch64 ctx-onnxruntime-linux-aarch64.tar.gz uname
validate_authoritative_runtime_proof \
  macos-arm64 ctx-macos-arm64 ctx-macos-arm64.native-runtime-proof.txt \
  onnxruntime Darwin arm64 arm64 ctx-onnxruntime-macos-arm64.tar.gz sysctl
validate_authoritative_runtime_proof \
  macos-x64 ctx-macos-x64 ctx-macos-x64.native-runtime-proof.txt \
  onnxruntime Darwin x86_64 x86_64 ctx-onnxruntime-macos-x64.tar.gz sysctl
validate_authoritative_runtime_proof \
  windows-x64 ctx.exe ctx-windows-x64.native-runtime-proof.txt \
  onnxruntime Windows_NT AMD64 AMD64 ctx-onnxruntime-windows-x64.zip iswow64process2
validate_authoritative_runtime_proof \
  freebsd-x64 ctx-freebsd-x64 ctx-freebsd-x64.native-runtime-proof.txt \
  onnxruntime FreeBSD amd64 amd64 ctx-onnxruntime-freebsd-x64.tar.gz uname

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
stage_runtime_asset linux-x64
stage_runtime_asset linux-aarch64
stage_runtime_asset macos-arm64
stage_runtime_asset macos-x64
stage_runtime_asset windows-x64
stage_runtime_asset freebsd-x64

printf 'staged GitHub release assets in %s\n' "${out_dir}"
