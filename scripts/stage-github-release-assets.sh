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
  macos_signing_mode="${CTX_MACOS_RELEASE_SIGNING:-optional}"
  if [[ "${platform}" == macos-* && "${CTX_PUBLIC_CLI_ARTIFACT_MATRIX:-0}" == "1" ]]; then
    macos_signing_mode=required
  fi
  if [[ "${platform}" == macos-* && "${macos_signing_mode}" == required ]]; then
    signing_evidence="${artifact_dir%/}/ctx-onnxruntime-${platform}.signing.json"
    transcode_work="$(mktemp -d "${TMPDIR:-/tmp}/ctx-transcoded-runtime.XXXXXX")"
    nested_runtime="${transcode_work}/libonnxruntime.dylib"
    trap 'rm -rf "${transcode_work:-}"' EXIT
    python3 - "${dest_path}" "${nested_runtime}" <<'PY'
import shutil
import sys
import tarfile

archive, output = sys.argv[1:]
with tarfile.open(archive, "r:gz") as bundle:
    matches = [member for member in bundle.getmembers() if member.name == "lib/libonnxruntime.dylib"]
    if len(matches) != 1 or not matches[0].isfile():
        raise SystemExit("transcoded runtime must contain one regular lib/libonnxruntime.dylib")
    source = bundle.extractfile(matches[0])
    if source is None:
        raise SystemExit("could not read transcoded lib/libonnxruntime.dylib")
    with source, open(output, "wb") as destination:
        shutil.copyfileobj(source, destination)
PY
    python3 scripts/macos-release-signing-evidence.py bind-archive \
      --evidence "${signing_evidence}" \
      --platform "${platform}" \
      --archive "${dest_path}" \
      --checksum "${dest_path}.sha256" \
      --nested-artifact "${nested_runtime}" \
      --role release
    scripts/check-macos-release-signing.sh \
      "${platform}" runtime "${dest_path}" "${signing_evidence}"
    scripts/check-macos-release-signing.sh \
      "${platform}" cli "${artifact_dir%/}/ctx-${platform}"
    scripts/run-macos-release-signing.sh --attest-runtime-archive \
      "${platform}" "${dest_path}" "${nested_runtime}" "${artifact_dir}"
    rm -rf "${transcode_work}"
    trap - EXIT
  fi
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

validate_runtime_proof() {
  local platform="$1"
  local binary_name="$2"
  local proof_name="$3"
  local runtime="$4"
  local host_system="$5"
  local host_arch="$6"
  local host_native_arch="$7"
  local runtime_asset="${8:-}"
  local native_arch_probe="$9"
  local expected_process_translated="${10:-0}"
  local expected_runtime_authority="${11:-authoritative}"
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
  if [[ "${runtime}" == "onnxruntime" ]]; then
    grep -Fxq 'embedding_backend=cpu' "${proof_path}" || {
      printf 'runtime proof did not exercise the ONNX CPU backend: %s\n' "${proof_path}" >&2
      exit 1
    }
  fi
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
  grep -Fxq "process_translated=${expected_process_translated}" "${proof_path}" || {
    printf 'runtime proof has the wrong process translation status: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "native_arch_probe=${native_arch_probe}" "${proof_path}" || {
    printf 'runtime proof used the wrong native architecture probe: %s\n' "${proof_path}" >&2
    exit 1
  }
  grep -Fxq "runtime_authority=${expected_runtime_authority}" "${proof_path}" || {
    printf 'runtime proof has the wrong authority classification: %s\n' "${proof_path}" >&2
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
  if [[ "${platform}" == "windows-x64" ]]; then
    local runtime_dylib windows_runtime_lib dependency dependency_path normalized_path expected_path
    runtime_dylib="$(sed -n 's/^runtime_dylib=//p' "${proof_path}")"
    [[ -n "${runtime_dylib}" ]] || {
      printf 'Windows runtime proof is missing runtime_dylib: %s\n' "${proof_path}" >&2
      exit 1
    }
    normalized_path="${runtime_dylib//\\//}"
    [[ "${normalized_path,,}" == */lib/onnxruntime.dll ]] || {
      printf 'Windows runtime proof has an invalid runtime_dylib: %s\n' "${proof_path}" >&2
      exit 1
    }
    windows_runtime_lib="${normalized_path%/*}"
    for dependency in msvcp140 msvcp140_1 vcruntime140 vcruntime140_1; do
      dependency_path="$(sed -n "s/^runtime_dependency_${dependency}=//p" "${proof_path}")"
      [[ -n "${dependency_path}" ]] || {
        printf 'Windows runtime proof is missing runtime_dependency_%s: %s\n' \
          "${dependency}" "${proof_path}" >&2
        exit 1
      }
      normalized_path="${dependency_path//\\//}"
      expected_path="${windows_runtime_lib}/${dependency}.dll"
      [[ "${normalized_path,,}" == "${expected_path,,}" ]] || {
        printf 'Windows runtime proof has an invalid runtime_dependency_%s path: %s\n' \
          "${dependency}" "${proof_path}" >&2
        exit 1
      }
    done
  fi
  grep -Fxq 'semantic_search=passed' "${proof_path}" || {
    printf 'runtime proof does not record semantic search success: %s\n' "${proof_path}" >&2
    exit 1
  }
}

validate_macos_x64_preview_candidate() {
  local version_path="${artifact_dir%/}/ctx-macos-x64.version"
  local build_info_path="${artifact_dir%/}/ctx-macos-x64.build-info.json"
  local binary_sha_path="${artifact_dir%/}/ctx-macos-x64.sha256"
  local expected_source_commit="228e05fa0fd058822be7a362acd65cacdad24356"
  local expected_binary_sha

  grep -Fxq 'ctx 0.25.0' "${version_path}" || {
    printf 'macOS x64 preview proof exception is restricted to ctx 0.25.0\n' >&2
    exit 1
  }
  [[ -s "${build_info_path}" ]] || {
    printf 'macOS x64 preview proof requires candidate build info: %s\n' "${build_info_path}" >&2
    exit 1
  }
  expected_binary_sha="$(tr -d '[:space:]' < "${binary_sha_path}")"
  python3 - "${build_info_path}" "${expected_binary_sha}" "${expected_source_commit}" <<'PY'
import json
import sys

path, expected_sha, expected_commit = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    payload = json.load(source)
if payload.get("schema_version") != 1:
    raise SystemExit("macOS x64 preview candidate has unsupported build-info schema")
if payload.get("platform") != "macos-x64" or payload.get("target") != "x86_64-apple-darwin":
    raise SystemExit("macOS x64 preview candidate has wrong build-info platform")
if payload.get("artifact_sha256") != expected_sha:
    raise SystemExit("macOS x64 preview candidate build info does not bind the release binary")
source = payload.get("source", {})
if source.get("commit") != expected_commit or source.get("clean") is not True:
    raise SystemExit("macOS x64 preview exception is restricted to the reviewed 0.25.0 source commit")
PY
}

validate_macos_signing_evidence() (
  set -euo pipefail
  local platform="$1"
  local binary="${artifact_dir%/}/ctx-${platform}"
  local runtime="${artifact_dir%/}/ctx-onnxruntime-${platform}.tar.gz"
  local cli_evidence="${artifact_dir%/}/ctx-${platform}.signing.json"
  local runtime_evidence="${artifact_dir%/}/ctx-onnxruntime-${platform}.signing.json"
  local cli_attestation="${artifact_dir%/}/ctx-${platform}.attestation.json"
  local cli_attestation_cms="${artifact_dir%/}/ctx-${platform}.attestation.cms"
  local runtime_attestation="${artifact_dir%/}/ctx-onnxruntime-${platform}.attestation.json"
  local runtime_attestation_cms="${artifact_dir%/}/ctx-onnxruntime-${platform}.attestation.cms"
  local release_attestation="${artifact_dir%/}/ctx-onnxruntime-${platform}.release-attestation.json"
  local release_attestation_cms="${artifact_dir%/}/ctx-onnxruntime-${platform}.release-attestation.cms"
  local build_info="${artifact_dir%/}/ctx-${platform}.build-info.json"
  local source_commit work nested

  # JSON records diagnostics and archive bindings. The Developer ID CMS
  # checks below are the cross-platform authorization for executable bytes.
  [[ -s "${cli_evidence}" ]] || {
    printf 'required macOS CLI signing evidence missing: %s\n' "${cli_evidence}" >&2
    exit 1
  }
  [[ -s "${runtime_evidence}" ]] || {
    printf 'required macOS runtime signing evidence missing: %s\n' "${runtime_evidence}" >&2
    exit 1
  }
  source_commit="$(python3 - "${build_info}" "${platform}" <<'PY'
import json
import re
import sys

path, platform = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    payload = json.load(source)
commit = payload.get("source", {}).get("commit", "")
if (
    payload.get("schema_version") != 1
    or payload.get("platform") != platform
    or payload.get("source", {}).get("clean") is not True
    or re.fullmatch(r"[0-9a-f]{40}", commit) is None
    or commit == "0" * 40
):
    raise SystemExit("macOS release build info has invalid source provenance")
print(commit)
PY
)"
  python3 scripts/macos-release-signing-evidence.py verify-artifact \
    --evidence "${cli_evidence}" \
    --platform "${platform}" \
    --kind cli \
    --artifact "${binary}" \
    --checksum "${binary}.sha256"
  CTX_MACOS_RELEASE_SOURCE_COMMIT="${source_commit}" \
    scripts/verify-macos-release-attestation.sh \
    "${platform}" cli "${binary}" "${cli_attestation}" "${cli_attestation_cms}"

  work="$(mktemp -d "${TMPDIR:-/tmp}/ctx-stage-macos-signing.XXXXXX")"
  trap 'rm -rf "${work}"' EXIT
  nested="${work}/libonnxruntime.dylib"
  python3 - "${runtime}" "${nested}" <<'PY'
import shutil
import sys
import tarfile

archive, output = sys.argv[1:]
with tarfile.open(archive, "r:gz") as bundle:
    matches = [member for member in bundle.getmembers() if member.name == "lib/libonnxruntime.dylib"]
    if len(matches) != 1 or not matches[0].isfile():
        raise SystemExit("macOS runtime must contain one regular lib/libonnxruntime.dylib")
    source = bundle.extractfile(matches[0])
    if source is None:
        raise SystemExit("could not read macOS runtime dylib")
    with source, open(output, "wb") as destination:
        shutil.copyfileobj(source, destination)
PY
  python3 scripts/macos-release-signing-evidence.py verify-archive \
    --evidence "${runtime_evidence}" \
    --platform "${platform}" \
    --archive "${runtime}" \
    --checksum "${runtime}.sha256" \
    --nested-artifact "${nested}" \
    --role release
  CTX_MACOS_RELEASE_SOURCE_COMMIT="${source_commit}" \
    scripts/verify-macos-release-attestation.sh \
    "${platform}" runtime "${nested}" \
    "${runtime_attestation}" "${runtime_attestation_cms}"
  CTX_MACOS_RELEASE_SOURCE_COMMIT="${source_commit}" \
    scripts/verify-macos-release-attestation.sh --runtime-archive \
    "${platform}" "${runtime}" "${nested}" \
    "${release_attestation}" "${release_attestation_cms}"
)

validate_runtime_proof \
  linux-x64 ctx ctx-linux-x64.native-runtime-proof.txt \
  onnxruntime Linux x86_64 x86_64 ctx-onnxruntime-linux-x64.tar.gz uname
validate_runtime_proof \
  linux-aarch64 ctx-linux-aarch64 ctx-linux-aarch64.native-runtime-proof.txt \
  onnxruntime Linux aarch64 aarch64 ctx-onnxruntime-linux-aarch64.tar.gz uname
validate_runtime_proof \
  macos-arm64 ctx-macos-arm64 ctx-macos-arm64.native-runtime-proof.txt \
  onnxruntime Darwin arm64 arm64 ctx-onnxruntime-macos-arm64.tar.gz sysctl
case "${CTX_RELEASE_ALLOW_MACOS_X64_PREVIEW_PROOF:-0}" in
  0)
    validate_runtime_proof \
      macos-x64 ctx-macos-x64 ctx-macos-x64.native-runtime-proof.txt \
      onnxruntime Darwin x86_64 x86_64 ctx-onnxruntime-macos-x64.tar.gz sysctl
    ;;
  1)
    validate_macos_x64_preview_candidate
    validate_runtime_proof \
      macos-x64 ctx-macos-x64 ctx-macos-x64.native-runtime-proof.txt \
      onnxruntime Darwin x86_64 x86_64 ctx-onnxruntime-macos-x64.tar.gz sysctl \
      0 non_authoritative
    ;;
  *)
    printf 'CTX_RELEASE_ALLOW_MACOS_X64_PREVIEW_PROOF must be 0 or 1\n' >&2
    exit 2
    ;;
esac
validate_runtime_proof \
  windows-x64 ctx.exe ctx-windows-x64.native-runtime-proof.txt \
  onnxruntime Windows_NT AMD64 AMD64 ctx-onnxruntime-windows-x64.zip iswow64process2
validate_runtime_proof \
  freebsd-x64 ctx-freebsd-x64 ctx-freebsd-x64.native-runtime-proof.txt \
  onnxruntime FreeBSD amd64 amd64 ctx-onnxruntime-freebsd-x64.tar.gz uname
validate_macos_signing_evidence macos-arm64
validate_macos_signing_evidence macos-x64

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
