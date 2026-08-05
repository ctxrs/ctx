#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/stage-github-release-assets.sh [ARTIFACT_DIR] [OUT_DIR]
       scripts/stage-github-release-assets.sh --with-semantic [ARTIFACT_DIR] [OUT_DIR]
       scripts/stage-github-release-assets.sh --transcode-runtime PLATFORM [ARTIFACT_DIR]

Stages public GitHub Release assets from built public CLI artifacts.

Inputs default to target/public-cli-artifacts.
Outputs default to target/github-release-assets.

Every ONNX Runtime sidecar is required. Release assembly fails closed when a
platform runtime is absent.

The additive --with-semantic mode validates and stages the ten signed Semantic
assets after preserving the six legacy runtime assets.

The transcode mode converts a validated builder-owned Unix .tar.zst sidecar
to the deterministic .tar.gz transport consumed by release installers. It is
only valid in private builder staging before a completion marker is created.
USAGE
}

mode="stage"
include_semantic="0"
case "${1:-}" in
  --transcode-runtime)
    [[ "$#" -ge 2 && "$#" -le 3 ]] || {
      usage
      exit 2
    }
    mode="transcode"
    transcode_platform="${2:-}"
    artifact_dir="${3:-target/public-cli-artifacts}"
    out_dir=""
    ;;
  --with-semantic)
    [[ "$#" -le 3 ]] || {
      usage
      exit 2
    }
    include_semantic="1"
    artifact_dir="${2:-target/public-cli-artifacts}"
    out_dir="${3:-target/github-release-assets}"
    ;;
  -h|--help)
    usage
    exit 2
    ;;
  -*)
    printf 'unknown staging mode: %s\n' "$1" >&2
    usage
    exit 2
    ;;
  *)
    [[ "$#" -le 2 ]] || {
      usage
      exit 2
    }
    artifact_dir="${1:-target/public-cli-artifacts}"
    out_dir="${2:-target/github-release-assets}"
    ;;
esac

if [[ "${artifact_dir}" == -* || "${out_dir}" == -* ]]; then
  printf 'staging modes cannot be combined\n' >&2
  usage
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
bundle_tool="${repo_root}/scripts/release/release_bundle.py"
resolve_checkout_commit() {
  env \
    -u GIT_ALTERNATE_OBJECT_DIRECTORIES \
    -u GIT_CEILING_DIRECTORIES \
    -u GIT_COMMON_DIR \
    -u GIT_DIR \
    -u GIT_DISCOVERY_ACROSS_FILESYSTEM \
    -u GIT_INDEX_FILE \
    -u GIT_NAMESPACE \
    -u GIT_OBJECT_DIRECTORY \
    -u GIT_WORK_TREE \
    git -C "${repo_root}" rev-parse --verify HEAD^{commit}
}
source_commit="$(resolve_checkout_commit)"
if [[ ! "${source_commit}" =~ ^[0-9a-f]{40}$ || "${source_commit}" == "0000000000000000000000000000000000000000" ]]; then
  printf 'could not resolve the exact public source commit\n' >&2
  exit 1
fi
if [[ -n "${CTX_PUBLIC_RELEASE_SOURCE_COMMIT:-}" && "${CTX_PUBLIC_RELEASE_SOURCE_COMMIT}" != "${source_commit}" ]]; then
  printf 'ambient public source commit conflicts with checkout HEAD\n' >&2
  exit 1
fi

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

require_regular_input() {
  local path="$1"
  local label="$2"

  [[ -f "${path}" && ! -L "${path}" ]] || {
    printf '%s must be a regular non-symlink file: %s\n' \
      "${label}" "${path}" >&2
    exit 1
  }
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
  require_regular_input "${source_path}" "runtime source archive"
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
  printf 'transcoded runtime release asset %s; retained semantic source %s\n' \
    "${dest_path}" "${source_path}"
}

if [[ "${mode}" == "transcode" ]]; then
  [[ -n "${transcode_platform}" ]] || {
    usage
    exit 2
  }
  transcode_candidate_dir="${artifact_dir}"
  if [[ "${transcode_candidate_dir}" != /* ]]; then
    transcode_candidate_dir="${repo_root}/${transcode_candidate_dir}"
  fi
  python3 -I "${bundle_tool}" require-directory \
    --directory "${transcode_candidate_dir}"
  python3 -I "${bundle_tool}" require-unsealed \
    --candidate-dir "${transcode_candidate_dir}"
  artifact_dir="${transcode_candidate_dir}"
  transcode_runtime_asset "${transcode_platform}"
  exit 0
fi

requested_artifact_dir="${artifact_dir}"
if [[ "${requested_artifact_dir}" != /* ]]; then
  requested_artifact_dir="${repo_root}/${requested_artifact_dir}"
fi
python3 -I "${bundle_tool}" require-directory \
  --directory "${requested_artifact_dir}"

stage_asset() {
  local source_name="$1"
  local dest_name="$2"
  local mode="${3:-0755}"
  local source_path="${artifact_dir%/}/${source_name}"
  local source_sha_path="${source_path}.sha256"
  local dest_path="${out_dir%/}/${dest_name}"
  local expected_sha actual_sha staged_sha

  require_regular_input "${source_path}" "public release artifact"
  require_regular_input "${source_sha_path}" "public artifact checksum"
  if [[ ! -s "${source_sha_path}" ]]; then
    printf 'missing public artifact checksum: %s\n' "${source_sha_path}" >&2
    exit 1
  fi
  expected_sha="$(awk 'NR == 1 { print $1 }' "${source_sha_path}")"
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
  require_regular_input "${dest_path}" "staged release artifact"
  staged_sha="$(sha256_file "${dest_path}")"
  if [[ "$(printf '%s' "${staged_sha}" | tr 'A-F' 'a-f')" != "$(printf '%s' "${expected_sha}" | tr 'A-F' 'a-f')" ]]; then
    printf 'staged artifact checksum mismatch for %s: expected %s got %s\n' \
      "${dest_path}" "${expected_sha}" "${staged_sha}" >&2
    exit 1
  fi
  printf '%s  %s\n' "${staged_sha}" "${dest_name}" >> "${out_dir%/}/SHA256SUMS"
}

stage_cli_evidence() {
  local source_name="$1"
  local dest_name="$2"
  local source_path="${artifact_dir%/}/${source_name}"
  local evidence

  for evidence in \
    "${source_path}" \
    "${source_path}.sha256" \
    "${source_path}.build-info.json" \
    "${source_path}.candidate.json" \
    "${source_path}.cdx.json" \
    "${source_path}.cdx.json.sha256" \
    "${source_path}.size.json" \
    "${source_path}.third-party-notices.txt" \
    "${source_path}.third-party-notices.txt.sha256"; do
    require_regular_input "${evidence}" "public CLI producer input"
  done

  stage_asset \
    "${source_name}.cdx.json" "${dest_name}.cdx.json" 0644
  stage_asset \
    "${source_name}.third-party-notices.txt" \
    "${dest_name}.third-party-notices.txt" 0644
}

validate_staged_cli_evidence() {
  local source_name="$1"
  local dest_name="$2"
  local platform="$3"
  local source_path="${artifact_dir%/}/${source_name}"
  local staged_path="${out_dir%/}/${dest_name}"

  python3 -I scripts/check-public-cli-build-info.py \
    --artifact "${staged_path}" \
    --build-info "${source_path}.build-info.json" \
    --matrix contracts/release-targets-v1.json \
    --platform "${platform}" \
    --source-commit "${source_commit}" >/dev/null
  python3 -I scripts/release-sbom.py verify-bundle \
    --artifact "${staged_path}" \
    --build-info "${source_path}.build-info.json" \
    --sbom "${staged_path}.cdx.json" \
    --notices "${staged_path}.third-party-notices.txt" \
    --size-report "${source_path}.size.json" \
    --candidate-manifest "${source_path}.candidate.json"
}

runtime_asset_name() {
  local platform="$1"

  case "${platform}" in
    linux-x64) printf 'ctx-onnxruntime-linux-x64.tar.gz\n' ;;
    linux-aarch64) printf 'ctx-onnxruntime-linux-aarch64.tar.gz\n' ;;
    macos-arm64) printf 'ctx-onnxruntime-macos-arm64.tar.gz\n' ;;
    macos-x64) printf 'ctx-onnxruntime-macos-x64.tar.gz\n' ;;
    windows-x64) printf 'ctx-onnxruntime-windows-x64.zip\n' ;;
    freebsd-x64) printf 'ctx-onnxruntime-freebsd-x64.tar.gz\n' ;;
    *)
      printf 'unknown platform for ONNX Runtime staging: %s\n' "${platform}" >&2
      exit 2
      ;;
  esac
}

stage_runtime_asset() {
  local platform="$1"
  local asset_name

  asset_name="$(runtime_asset_name "${platform}")"

  require_regular_input \
    "${artifact_dir%/}/${asset_name}" "required ONNX Runtime sidecar"
  stage_asset "${asset_name}" "${asset_name}" 0644
}

validate_staged_runtime_asset() {
  local platform="$1"
  local asset_name archive

  asset_name="$(runtime_asset_name "${platform}")"
  archive="${out_dir%/}/${asset_name}"
  require_regular_input "${archive}" "staged ONNX Runtime sidecar"

  if [[ "${platform}" == "windows-x64" ]]; then
    bash scripts/build-onnxruntime-sidecar.sh --validate \
      "${platform}" "${archive}"
  else
    python3 - "${archive}" "${platform}" <<'PY'
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
}

stage_complete_candidate() {
local artifact_dir="$1"
local out_dir="$2"
local include_semantic="$3"
local source_commit="$4"
local repo_root="$5"
local required_runtime_asset cli_dest semantic_fields semantic_asset
local required_runtime_assets semantic_assets

[[ "${source_commit}" =~ ^[0-9a-f]{40}$ ]] || {
  printf 'completed GitHub staging source commit is invalid\n' >&2
  exit 1
}
cd "${repo_root}"
required_runtime_assets=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
  ctx-onnxruntime-freebsd-x64.tar.gz
)
for required_runtime_asset in "${required_runtime_assets[@]}"; do
  require_regular_input \
    "${artifact_dir%/}/${required_runtime_asset}" \
    "required ONNX Runtime sidecar"
done

validate_macos_signing_evidence() (
  set -euo pipefail
  local platform="$1"
  local binary="${out_dir%/}/ctx-${platform}"
  local runtime="${out_dir%/}/ctx-onnxruntime-${platform}.tar.gz"
  local binary_checksum="${artifact_dir%/}/ctx-${platform}.sha256"
  local runtime_checksum="${artifact_dir%/}/ctx-onnxruntime-${platform}.tar.gz.sha256"
  local cli_evidence="${artifact_dir%/}/ctx-${platform}.signing.json"
  local runtime_evidence="${artifact_dir%/}/ctx-onnxruntime-${platform}.signing.json"
  local cli_attestation="${artifact_dir%/}/ctx-${platform}.attestation.json"
  local cli_attestation_cms="${artifact_dir%/}/ctx-${platform}.attestation.cms"
  local runtime_attestation="${artifact_dir%/}/ctx-onnxruntime-${platform}.attestation.json"
  local runtime_attestation_cms="${artifact_dir%/}/ctx-onnxruntime-${platform}.attestation.cms"
  local release_attestation="${artifact_dir%/}/ctx-onnxruntime-${platform}.release-attestation.json"
  local release_attestation_cms="${artifact_dir%/}/ctx-onnxruntime-${platform}.release-attestation.cms"
  local build_info="${artifact_dir%/}/ctx-${platform}.build-info.json"
  local source_commit work nested producer_input

  # JSON records diagnostics and archive bindings. The Developer ID CMS
  # checks below are the cross-platform authorization for executable bytes.
  for producer_input in \
    "${binary_checksum}" "${runtime_checksum}" \
    "${cli_evidence}" "${runtime_evidence}" "${build_info}" \
    "${cli_attestation}" "${cli_attestation_cms}" \
    "${runtime_attestation}" "${runtime_attestation_cms}" \
    "${release_attestation}" "${release_attestation_cms}"; do
    require_regular_input "${producer_input}" "macOS release producer input"
    [[ -s "${producer_input}" ]] || {
      printf 'macOS release producer input is empty: %s\n' "${producer_input}" >&2
      exit 1
    }
  done
  require_regular_input "${binary}" "staged macOS CLI"
  require_regular_input "${runtime}" "staged macOS runtime"
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
    --checksum "${binary_checksum}"
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
    --checksum "${runtime_checksum}" \
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

mkdir -p "${out_dir}"
for cli_dest in \
  ctx-linux-aarch64 \
  ctx-linux-x64 \
  ctx-macos-arm64 \
  ctx-macos-x64 \
  ctx-windows-x64.exe \
  ctx-freebsd-x64; do
  rm -f \
    "${out_dir%/}/${cli_dest}.cdx.json" \
    "${out_dir%/}/${cli_dest}.third-party-notices.txt"
done
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
  "${out_dir%/}/ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz" \
  "${out_dir%/}/ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz" \
  "${out_dir%/}/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz" \
  "${out_dir%/}/ctx-onnxruntime-linux-x64.tar.zst" \
  "${out_dir%/}/ctx-onnxruntime-linux-aarch64.tar.zst" \
  "${out_dir%/}/ctx-onnxruntime-macos-arm64.tar.zst" \
  "${out_dir%/}/ctx-onnxruntime-macos-x64.tar.zst" \
  "${out_dir%/}/ctx-windowsml-windows-x64.zip" \
  "${out_dir%/}/ctx-onnxruntime-freebsd-x64.tar.zst" \
  "${out_dir%/}/ctx-onnxruntime-linux-x64-cuda12.tar.zst" \
  "${out_dir%/}/SHA256SUMS"

stage_asset ctx ctx-linux-x64
stage_cli_evidence ctx ctx-linux-x64
stage_asset ctx-linux-aarch64 ctx-linux-aarch64
stage_cli_evidence ctx-linux-aarch64 ctx-linux-aarch64
stage_asset ctx-macos-arm64 ctx-macos-arm64
stage_cli_evidence ctx-macos-arm64 ctx-macos-arm64
stage_asset ctx-macos-x64 ctx-macos-x64
stage_cli_evidence ctx-macos-x64 ctx-macos-x64
stage_asset ctx.exe ctx-windows-x64.exe
stage_cli_evidence ctx.exe ctx-windows-x64.exe
stage_asset ctx-freebsd-x64 ctx-freebsd-x64
stage_cli_evidence ctx-freebsd-x64 ctx-freebsd-x64
stage_runtime_asset linux-x64
stage_runtime_asset linux-aarch64
stage_runtime_asset macos-arm64
stage_runtime_asset macos-x64
stage_runtime_asset windows-x64
stage_runtime_asset freebsd-x64

if [[ "${include_semantic}" == "1" ]]; then
  semantic_fields="$(mktemp "${TMPDIR:-/tmp}/ctx-semantic-release.XXXXXX")"
  semantic_assets=(
    ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz
    ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz
    ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz
    ctx-onnxruntime-linux-x64.tar.zst
    ctx-onnxruntime-linux-aarch64.tar.zst
    ctx-onnxruntime-macos-arm64.tar.zst
    ctx-onnxruntime-macos-x64.tar.zst
    ctx-windowsml-windows-x64.zip
    ctx-onnxruntime-freebsd-x64.tar.zst
    ctx-onnxruntime-linux-x64-cuda12.tar.zst
  )
  for semantic_asset in "${semantic_assets[@]}"; do
    require_regular_input \
      "${artifact_dir%/}/${semantic_asset}.sha256" \
      "Semantic producer checksum"
    require_regular_input \
      "${artifact_dir%/}/${semantic_asset}.asset.json" \
      "Semantic producer record"
  done
  bash scripts/construct-semantic-release-catalog.sh \
    "${artifact_dir}" "${semantic_fields}"
  for semantic_asset in "${semantic_assets[@]}"; do
    stage_asset "${semantic_asset}" "${semantic_asset}" 0644
  done
  rm -f "${semantic_fields}"
fi

validate_staged_cli_evidence ctx ctx-linux-x64 linux-x64
validate_staged_cli_evidence ctx-linux-aarch64 ctx-linux-aarch64 linux-aarch64
validate_staged_cli_evidence ctx-macos-arm64 ctx-macos-arm64 macos-arm64
validate_staged_cli_evidence ctx-macos-x64 ctx-macos-x64 macos-x64
validate_staged_cli_evidence ctx.exe ctx-windows-x64.exe windows-x64
validate_staged_cli_evidence ctx-freebsd-x64 ctx-freebsd-x64 freebsd-x64
validate_staged_runtime_asset linux-x64
validate_staged_runtime_asset linux-aarch64
validate_staged_runtime_asset macos-arm64
validate_staged_runtime_asset macos-x64
validate_staged_runtime_asset windows-x64
validate_staged_runtime_asset freebsd-x64
validate_macos_signing_evidence macos-arm64
validate_macos_signing_evidence macos-x64

}

python3 -I "${bundle_tool}" verify \
  --candidate-dir "${requested_artifact_dir}" \
  --platform linux-x64 \
  --source-commit "${source_commit}" \
  --allow-extra
python3 -I "${bundle_tool}" verify \
  --candidate-dir "${requested_artifact_dir}" \
  --platform linux-aarch64 \
  --source-commit "${source_commit}" \
  --allow-extra

if [[ "${out_dir}" != /* ]]; then
  out_dir="${repo_root}/${out_dir}"
fi
[[ ! -e "${out_dir}" && ! -L "${out_dir}" ]] || {
  printf 'refusing to replace existing GitHub release staging: %s\n' "${out_dir}" >&2
  exit 1
}
mkdir -p "$(dirname "${out_dir}")"
staged_out="$(mktemp -d "$(dirname "${out_dir}")/.github-release-assets.XXXXXX")"
trap 'rm -rf -- "${staged_out}"' EXIT
artifact_dir="${requested_artifact_dir}"
stage_complete_candidate \
  "${artifact_dir}" "${staged_out}" "${include_semantic}" \
  "${source_commit}" "${repo_root}"
python3 -I "${bundle_tool}" commit-directory \
  --stage-dir "${staged_out}" \
  --output-dir "${out_dir}"
trap - EXIT
printf 'staged GitHub release assets in %s\n' "${out_dir}"
