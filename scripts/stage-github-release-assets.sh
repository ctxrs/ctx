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
publisher="${repo_root}/scripts/release/publish-linux-bazel-release.py"
source_commit="${CTX_PUBLIC_RELEASE_SOURCE_COMMIT:-}"
if [[ -z "${source_commit}" ]]; then
  source_commit="$(git rev-parse --verify HEAD^{commit})"
fi
if [[ ! "${source_commit}" =~ ^[0-9a-f]{40}$ || "${source_commit}" == "0000000000000000000000000000000000000000" ]]; then
  printf 'could not resolve the exact public source commit\n' >&2
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
  python3 -I "${publisher}" require-unsealed \
    --candidate-dir "${transcode_candidate_dir}"
  artifact_dir="${transcode_candidate_dir}"
  transcode_runtime_asset "${transcode_platform}"
  exit 0
fi

requested_artifact_dir="${artifact_dir}"
if [[ "${requested_artifact_dir}" != /* ]]; then
  requested_artifact_dir="${repo_root}/${requested_artifact_dir}"
fi
if [[ "${CTX_RELEASE_PINNED_CONSUMER:-0}" != "1" ]]; then
  consumer_command=(
    env CTX_RELEASE_PINNED_CONSUMER=1
    /bin/bash "${BASH_SOURCE[0]}"
  )
  if [[ "${include_semantic}" == "1" ]]; then
    consumer_command+=(--with-semantic)
  fi
  consumer_command+=("{candidate}" "${out_dir}")
  python3 -I "${publisher}" consume-complete \
    --candidate-dir "${requested_artifact_dir}" \
    --snapshot-root "${TMPDIR:-/tmp}" \
    --platform linux-x64 \
    --platform linux-aarch64 \
    --source-commit "${source_commit}" \
    --allow-extra -- "${consumer_command[@]}"
  exit $?
fi
artifact_dir="${requested_artifact_dir}"

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
  printf '%s  %s\n' "${actual_sha}" "${dest_name}" >> "${out_dir%/}/SHA256SUMS"
}

verify_and_stage_cli_evidence() {
  local source_name="$1"
  local dest_name="$2"
  local platform="$3"
  local source_path="${artifact_dir%/}/${source_name}"

  python3 -I scripts/check-public-cli-build-info.py \
    --artifact "${source_path}" \
    --build-info "${source_path}.build-info.json" \
    --matrix contracts/release-targets-v1.json \
    --platform "${platform}" \
    --source-commit "${source_commit}" >/dev/null
  python3 -I scripts/release-sbom.py verify-bundle \
    --artifact "${source_path}" \
    --build-info "${source_path}.build-info.json" \
    --sbom "${source_path}.cdx.json" \
    --notices "${source_path}.third-party-notices.txt" \
    --size-report "${source_path}.size.json" \
    --candidate-manifest "${source_path}.candidate.json"
  stage_asset \
    "${source_name}.cdx.json" "${dest_name}.cdx.json" 0644
  stage_asset \
    "${source_name}.third-party-notices.txt" \
    "${dest_name}.third-party-notices.txt" 0644
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

validate_macos_signing_evidence macos-arm64
validate_macos_signing_evidence macos-x64

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
verify_and_stage_cli_evidence ctx ctx-linux-x64 linux-x64
stage_asset ctx-linux-aarch64 ctx-linux-aarch64
verify_and_stage_cli_evidence ctx-linux-aarch64 ctx-linux-aarch64 linux-aarch64
stage_asset ctx-macos-arm64 ctx-macos-arm64
verify_and_stage_cli_evidence ctx-macos-arm64 ctx-macos-arm64 macos-arm64
stage_asset ctx-macos-x64 ctx-macos-x64
verify_and_stage_cli_evidence ctx-macos-x64 ctx-macos-x64 macos-x64
stage_asset ctx.exe ctx-windows-x64.exe
verify_and_stage_cli_evidence ctx.exe ctx-windows-x64.exe windows-x64
stage_asset ctx-freebsd-x64 ctx-freebsd-x64
verify_and_stage_cli_evidence ctx-freebsd-x64 ctx-freebsd-x64 freebsd-x64
stage_runtime_asset linux-x64
stage_runtime_asset linux-aarch64
stage_runtime_asset macos-arm64
stage_runtime_asset macos-x64
stage_runtime_asset windows-x64
stage_runtime_asset freebsd-x64

if [[ "${include_semantic}" == "1" ]]; then
  semantic_fields="$(mktemp "${TMPDIR:-/tmp}/ctx-semantic-release.XXXXXX")"
  bash scripts/construct-semantic-release-catalog.sh \
    "${artifact_dir}" "${semantic_fields}"
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
    stage_asset "${semantic_asset}" "${semantic_asset}" 0644
  done
  rm -f "${semantic_fields}"
fi

printf 'staged GitHub release assets in %s\n' "${out_dir}"
