#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/stage-github-release-assets.sh [ARTIFACT_DIR] [OUT_DIR] [AUTHORITY_DIR] [NATIVE_PROOF_DIR]
       scripts/stage-github-release-assets.sh --transcode-runtime PLATFORM [ARTIFACT_DIR]

Stages the five public Core GitHub Release assets and their existing
SBOM/notices evidence from one sealed factory candidate.

Inputs default to target/public-cli-artifacts.
Core outputs default to target/github-core-release-assets.

The transcode mode converts a validated builder-owned Unix .tar.zst sidecar
to the deterministic .tar.gz transport consumed by release installers. It is
only valid in private builder staging before a completion marker is created.
USAGE
}

mode="stage"
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
    authority_dir=""
    native_proof_dir=""
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
    [[ "$#" -le 4 ]] || {
      usage
      exit 2
    }
    artifact_dir="${1:-target/public-cli-artifacts}"
    out_dir="${2:-target/github-core-release-assets}"
    authority_dir="${3:-${out_dir}.authority}"
    native_proof_dir="${4:-${CTX_PUBLIC_NATIVE_PROOF_DIR:-target/public-cli-native-smoke}}"
    ;;
esac

if [[ "${artifact_dir}" == -* || "${out_dir}" == -* || "${authority_dir}" == -* || "${native_proof_dir}" == -* ]]; then
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
    linux-x64|linux-aarch64|macos-arm64|macos-x64)
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
    if [[ "$(uname -s)" == "Darwin" ]]; then
      # Runtime producer workspaces intentionally contain no CLI artifacts.
      # The native validator joins and verifies the separate factory CLI.
      scripts/check-macos-release-signing.sh \
        "${platform}" runtime "${dest_path}" "${signing_evidence}"
    else
      python3 scripts/macos-release-signing-evidence.py verify-archive \
        --evidence "${signing_evidence}" --platform "${platform}" \
        --archive "${dest_path}" --checksum "${dest_path}.sha256" \
        --nested-artifact "${nested_runtime}" --role release
    fi
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
requested_native_proof_dir="${native_proof_dir}"
if [[ "${requested_native_proof_dir}" != /* ]]; then
  requested_native_proof_dir="${repo_root}/${requested_native_proof_dir}"
fi
python3 -I "${bundle_tool}" require-directory \
  --directory "${requested_native_proof_dir}"

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

stage_macos_cli_verifier_inputs() {
  local source_name="$1"
  local dest_name="$2"
  local source_path="${artifact_dir%/}/${source_name}"
  local suffix source destination source_digest destination_digest

  for suffix in \
    .sha256 \
    .build-info.json \
    .signing.json \
    .attestation.json \
    .attestation.cms \
    .notary-submit.json; do
    source="${source_path}${suffix}"
    destination="${out_dir%/}/${dest_name}${suffix}"
    require_regular_input "${source}" "macOS CLI verifier input"
    [[ -s "${source}" ]] || {
      printf 'macOS CLI verifier input is empty: %s\n' "${source}" >&2
      exit 1
    }
    source_digest="$(sha256_file "${source}")"
    install -m 0644 "${source}" "${destination}"
    destination_digest="$(sha256_file "${destination}")"
    [[ "${source_digest}" == "${destination_digest}" ]] || {
      printf 'staged macOS CLI verifier input changed: %s\n' "${source}" >&2
      exit 1
    }
  done
}

validate_staged_cli_evidence() {
  local source_name="$1"
  local dest_name="$2"
  local platform="$3"
  local candidate_manifest="$4"
  local verification_name="${5:-${dest_name}}"
  local verification_dir="${6:-${out_dir}}"
  local source_path="${artifact_dir%/}/${source_name}"
  local staged_path="${verification_dir%/}/${verification_name}"
  local build_info_path="${source_path}.build-info.json"
  local sbom_path="${verification_dir%/}/${verification_name}.cdx.json"
  local notices_path="${verification_dir%/}/${verification_name}.third-party-notices.txt"
  local size_report_path="${source_path}.size.json"

  if [[ "${verification_dir}" == "${authority_dir}" ]]; then
    build_info_path="${verification_dir%/}/${verification_name}.build-info.json"
    size_report_path="${verification_dir%/}/${verification_name}.size.json"
  fi

  python3 -I scripts/check-public-cli-build-info.py \
    --artifact "${staged_path}" \
    --build-info "${build_info_path}" \
    --matrix contracts/release-targets-v1.json \
    --platform "${platform}" \
    --source-commit "${source_commit}" >/dev/null
  python3 -I scripts/release-sbom.py verify-bundle \
    --artifact "${staged_path}" \
    --candidate-artifact-name "${source_name}" \
    --build-info "${build_info_path}" \
    --sbom "${sbom_path}" \
    --notices "${notices_path}" \
    --size-report "${size_report_path}" \
    --candidate-manifest "${candidate_manifest}"
}

stage_complete_candidate() {
local artifact_dir="$1"
local out_dir="$2"
local source_commit="$3"
local repo_root="$4"
local authority_dir="$5"
local native_proof_dir="$6"
local authority_candidate cli_dest
local authority_candidates

[[ "${source_commit}" =~ ^[0-9a-f]{40}$ ]] || {
  printf 'completed GitHub staging source commit is invalid\n' >&2
  exit 1
}
cd "${repo_root}"

stage_authority_leaf() {
  local source_path="$1"
  local destination_name="$2"
  local destination_path="${authority_dir%/}/${destination_name}"
  local before staged after

  require_regular_input "${source_path}" "candidate authority input"
  before="$(sha256_file "${source_path}")"
  install -m 0644 "${source_path}" "${destination_path}"
  staged="$(sha256_file "${destination_path}")"
  after="$(sha256_file "${source_path}")"
  if [[ "${before}" != "${staged}" || "${before}" != "${after}" ]]; then
    printf 'candidate authority input changed while staged: %s\n' \
      "${source_path}" >&2
    exit 1
  fi
}

validate_macos_cli_signing_evidence() (
  set -euo pipefail
  local platform="$1"
  local binary="${out_dir%/}/ctx-${platform}"
  local binary_checksum="${artifact_dir%/}/ctx-${platform}.sha256"
  local cli_evidence="${artifact_dir%/}/ctx-${platform}.signing.json"
  local cli_attestation="${artifact_dir%/}/ctx-${platform}.attestation.json"
  local cli_attestation_cms="${artifact_dir%/}/ctx-${platform}.attestation.cms"
  local build_info="${artifact_dir%/}/ctx-${platform}.build-info.json"
  local source_commit producer_input

  # JSON records diagnostics and archive bindings. The Developer ID CMS
  # checks below are the cross-platform authorization for executable bytes.
  for producer_input in \
    "${binary_checksum}" "${cli_evidence}" "${build_info}" \
    "${cli_attestation}" "${cli_attestation_cms}"; do
    require_regular_input "${producer_input}" "macOS release producer input"
    [[ -s "${producer_input}" ]] || {
      printf 'macOS release producer input is empty: %s\n' "${producer_input}" >&2
      exit 1
    }
  done
  require_regular_input "${binary}" "staged macOS CLI"
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
)

mkdir -p "${out_dir}"
for cli_dest in \
  ctx-linux-aarch64 \
  ctx-linux-x64 \
  ctx-macos-arm64 \
  ctx-macos-x64 \
  ctx-windows-x64.exe; do
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
  "${out_dir%/}/SHA256SUMS"

stage_asset ctx ctx-linux-x64
stage_cli_evidence ctx ctx-linux-x64
stage_asset ctx-linux-aarch64 ctx-linux-aarch64
stage_cli_evidence ctx-linux-aarch64 ctx-linux-aarch64
stage_asset ctx-macos-arm64 ctx-macos-arm64
stage_cli_evidence ctx-macos-arm64 ctx-macos-arm64
stage_macos_cli_verifier_inputs ctx-macos-arm64 ctx-macos-arm64
stage_asset ctx-macos-x64 ctx-macos-x64
stage_cli_evidence ctx-macos-x64 ctx-macos-x64
stage_macos_cli_verifier_inputs ctx-macos-x64 ctx-macos-x64
stage_asset ctx.exe ctx-windows-x64.exe
stage_cli_evidence ctx.exe ctx-windows-x64.exe

authority_candidates=(
  ctx.candidate.json
  ctx-linux-aarch64.candidate.json
  ctx-macos-arm64.candidate.json
  ctx-macos-x64.candidate.json
  ctx.exe.candidate.json
)
for authority_candidate in "${authority_candidates[@]}"; do
  authority_source="${artifact_dir%/}/${authority_candidate}"
  stage_authority_leaf "${authority_source}" "${authority_candidate}"
done

# The authority handoff retains the exact Windows construction names. The
# public release executable remains ctx-windows-x64.exe in SHA256SUMS; both are
# copied from the already verified Core bytes.
stage_authority_leaf "${out_dir%/}/ctx-windows-x64.exe" ctx.exe
stage_authority_leaf \
  "${artifact_dir%/}/ctx.exe.build-info.json" ctx.exe.build-info.json
stage_authority_leaf \
  "${out_dir%/}/ctx-windows-x64.exe.cdx.json" ctx.exe.cdx.json
stage_authority_leaf \
  "${artifact_dir%/}/ctx.exe.size.json" ctx.exe.size.json
stage_authority_leaf \
  "${out_dir%/}/ctx-windows-x64.exe.third-party-notices.txt" \
  ctx.exe.third-party-notices.txt
stage_authority_leaf "${out_dir%/}/SHA256SUMS" SHA256SUMS
stage_authority_leaf \
  "${artifact_dir%/}/ctx-release-factory.json" ctx-release-factory.json
stage_authority_leaf \
  "${artifact_dir%/}/ctx-core.release-complete.json" \
  ctx-core.release-complete.json

validate_staged_cli_evidence \
  ctx ctx-linux-x64 linux-x64 \
  "${authority_dir%/}/ctx.candidate.json"
validate_staged_cli_evidence \
  ctx-linux-aarch64 ctx-linux-aarch64 linux-aarch64 \
  "${authority_dir%/}/ctx-linux-aarch64.candidate.json"
validate_staged_cli_evidence \
  ctx-macos-arm64 ctx-macos-arm64 macos-arm64 \
  "${authority_dir%/}/ctx-macos-arm64.candidate.json"
validate_staged_cli_evidence \
  ctx-macos-x64 ctx-macos-x64 macos-x64 \
  "${authority_dir%/}/ctx-macos-x64.candidate.json"
validate_staged_cli_evidence \
  ctx.exe ctx-windows-x64.exe windows-x64 \
  "${authority_dir%/}/ctx.exe.candidate.json" \
  ctx.exe "${authority_dir}"
for native_platform in linux-x64 linux-aarch64 macos-arm64 macos-x64 windows-x64; do
  native_artifact="ctx-${native_platform}"
  [[ "${native_platform}" == "windows-x64" ]] && native_artifact="ctx-windows-x64.exe"
  python3 -I scripts/native-execution-proof.py verify \
    --platform "${native_platform}" \
    --artifact "${out_dir%/}/${native_artifact}" \
    --proof "${native_proof_dir%/}/${native_platform}/ctx-${native_platform}.native-execution.json"
done
validate_macos_cli_signing_evidence macos-arm64
validate_macos_cli_signing_evidence macos-x64

for authority_candidate in "${authority_candidates[@]}"; do
  printf '%s\n' \
    "$(sha256_file "${authority_dir%/}/${authority_candidate}")" \
    >"${authority_dir%/}/${authority_candidate}.sha256"
done
python3 - "${authority_dir}" "${source_commit}" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import re
import sys

root = Path(sys.argv[1])
source_commit = sys.argv[2]
candidates = (
    "ctx-linux-aarch64.candidate.json",
    "ctx.candidate.json",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-x64.candidate.json",
    "ctx.exe.candidate.json",
)
release_assets = {
    "ctx-linux-aarch64",
    "ctx-linux-aarch64.cdx.json",
    "ctx-linux-aarch64.third-party-notices.txt",
    "ctx-linux-x64",
    "ctx-linux-x64.cdx.json",
    "ctx-linux-x64.third-party-notices.txt",
    "ctx-macos-arm64",
    "ctx-macos-arm64.cdx.json",
    "ctx-macos-arm64.third-party-notices.txt",
    "ctx-macos-x64",
    "ctx-macos-x64.cdx.json",
    "ctx-macos-x64.third-party-notices.txt",
    "ctx-windows-x64.exe",
    "ctx-windows-x64.exe.cdx.json",
    "ctx-windows-x64.exe.third-party-notices.txt",
}


def record(name):
    path = root / name
    raw = path.read_bytes()
    return {
        "file": name,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "size_bytes": len(raw),
    }


sums_record = record("SHA256SUMS")
sums = {}
for line in (root / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
    match = re.fullmatch(r"([0-9a-f]{64})  ([^/]+)", line)
    if match is None or match.group(2) in sums:
        raise SystemExit("Core SHA256SUMS is malformed")
    sums[match.group(2)] = match.group(1)
if set(sums) != release_assets:
    raise SystemExit("Core SHA256SUMS inventory is not exact")
document = {
    "candidate_manifests": [record(name) for name in candidates],
    "factory_completion": record("ctx-core.release-complete.json"),
    "factory_manifest": record("ctx-release-factory.json"),
    "kind": "ctx-public-core-github-handoff",
    "release_sums": sums_record,
    "schema_version": 1,
    "source_commit": source_commit,
}
encoded = (json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n").encode()
output = root / "ctx-core-github-handoff.json"
descriptor = os.open(
    output,
    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
    0o644,
)
try:
    with os.fdopen(descriptor, "wb", closefd=False) as destination:
        destination.write(encoded)
        destination.flush()
        os.fsync(destination.fileno())
finally:
    os.close(descriptor)
PY
printf '%s\n' \
  "$(sha256_file "${authority_dir%/}/ctx-core-github-handoff.json")" \
  >"${authority_dir%/}/ctx-core-github-handoff.json.sha256"
authority_expected="$({
  for authority_candidate in "${authority_candidates[@]}"; do
    printf '%s\n%s.sha256\n' \
      "${authority_candidate}" "${authority_candidate}"
  done
  printf '%s\n' \
    SHA256SUMS \
    ctx-core-github-handoff.json \
    ctx-core-github-handoff.json.sha256 \
    ctx-core.release-complete.json \
    ctx.exe \
    ctx.exe.build-info.json \
    ctx.exe.cdx.json \
    ctx.exe.size.json \
    ctx.exe.third-party-notices.txt \
    ctx-release-factory.json
} | LC_ALL=C sort)"
authority_actual="$(find "${authority_dir}" -mindepth 1 -maxdepth 1 \
  -printf '%f\n' | LC_ALL=C sort)"
if [[ "${authority_actual}" != "${authority_expected}" ]]; then
  printf 'candidate authority handoff inventory is not exact\n' >&2
  exit 1
fi
}

python3 -I scripts/release/seal-linux-factory-candidate.py \
  --verify --candidate-dir "${requested_artifact_dir}" \
  --source-commit "${source_commit}" >/dev/null

if [[ "${out_dir}" != /* ]]; then
  out_dir="${repo_root}/${out_dir}"
fi
if [[ "${authority_dir}" != /* ]]; then
  authority_dir="${repo_root}/${authority_dir}"
fi
python3 -I "${bundle_tool}" preflight-publication \
  --input-dir "${requested_artifact_dir}" \
  --output-dir "${out_dir}" \
  --output-dir "${authority_dir}"
staged_out="$(mktemp -d "$(dirname "${out_dir}")/.github-release-assets.XXXXXX")"
staged_authority="$(mktemp -d \
  "$(dirname "${authority_dir}")/.github-release-authority.XXXXXX")"
trap 'rm -rf -- "${staged_out}" "${staged_authority}"' EXIT
artifact_dir="${requested_artifact_dir}"
stage_complete_candidate \
  "${artifact_dir}" "${staged_out}" \
  "${source_commit}" "${repo_root}" "${staged_authority}" \
  "${requested_native_proof_dir}"
python3 -I "${bundle_tool}" commit-publication \
  --stage-dir "${staged_authority}" \
  --output-dir "${authority_dir}" \
  --stage-dir "${staged_out}" \
  --output-dir "${out_dir}"
trap - EXIT
printf 'staged GitHub release assets in %s; candidate authority handoff in %s\n' \
  "${out_dir}" "${authority_dir}"
