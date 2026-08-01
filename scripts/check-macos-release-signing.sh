#!/usr/bin/env bash
set -euo pipefail

EXPECTED_TEAM_ID="SJSNARH4TG"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-macos-release-signing.sh PLATFORM KIND ARTIFACT [EVIDENCE]

Verifies Developer ID, accepted notarization, checksum, cryptographic
attestation, and artifact-specific evidence for a standalone macOS CLI or
packaged ONNX Runtime sidecar. KIND is cli or runtime.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

authority_matches_expected_team() {
  case "$1" in
    "Developer ID Application: "*" (${EXPECTED_TEAM_ID})") return 0 ;;
    *) return 1 ;;
  esac
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

codesign_details_have_runtime() {
  awk '
    /^CodeDirectory[[:space:]]/ && match($0, /flags=[^[:space:]]*\([^)]*\)/) {
      value = substr($0, RSTART, RLENGTH)
      sub(/^flags=[^(]*\(/, "", value)
      sub(/\)$/, "", value)
      count = split(value, tokens, ",")
      for (i = 1; i <= count; i++) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", tokens[i])
        if (tokens[i] == "runtime") found = 1
      }
    }
    END { exit found ? 0 : 1 }
  ' "$1"
}

platform="${1:-}"
kind="${2:-}"
artifact="${3:-}"
evidence="${4:-}"
if [[ -z "${platform}" || -z "${kind}" || -z "${artifact}" ]]; then
  usage
  exit 2
fi
case "${platform}" in
  macos-arm64|macos-x64) ;;
  *) usage; exit 2 ;;
esac
case "${kind}" in
  cli) evidence_prefix="ctx-${platform}" ;;
  runtime) evidence_prefix="ctx-onnxruntime-${platform}" ;;
  *) usage; exit 2 ;;
esac
[[ -f "${artifact}" ]] || die "macOS release artifact not found: ${artifact}"
artifact="$(cd "$(dirname "${artifact}")" && pwd)/$(basename "${artifact}")"
if [[ -z "${evidence}" ]]; then
  evidence="$(dirname "${artifact}")/${evidence_prefix}.signing.json"
fi
[[ -s "${evidence}" ]] || die "macOS signing evidence missing: ${evidence}"
[[ -s "${artifact}.sha256" ]] || die "macOS release checksum missing: ${artifact}.sha256"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
require_command codesign
require_command python3
evidence_authority="$(python3 - "${evidence}" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source)["codesign"]["authority"]
except (OSError, KeyError, TypeError, json.JSONDecodeError):
    value = ""
if isinstance(value, str):
    print(value, end="")
PY
)"
authority_matches_expected_team "${evidence_authority}" || \
  die "macOS signing evidence does not contain the pinned ctx Apple authority"
evidence_dir="$(cd "$(dirname "${evidence}")" && pwd)"
attestation_json="${evidence_dir}/${evidence_prefix}.attestation.json"
attestation_cms="${evidence_dir}/${evidence_prefix}.attestation.cms"

verify_macho() {
  local path="$1"
  local details
  details="$(mktemp "${TMPDIR:-/tmp}/ctx-codesign-check.XXXXXX")"
  if ! codesign --verify --strict --verbose=4 "${path}" >/dev/null 2>&1; then
    rm -f "${details}"
    die "strict codesign verification failed: ${path}"
  fi
  if ! codesign -d --verbose=4 "${path}" >"${details}" 2>&1; then
    rm -f "${details}"
    die "could not inspect Developer ID signature: ${path}"
  fi
  grep -Fqx "Authority=${evidence_authority}" "${details}" || {
    rm -f "${details}"
    die "artifact does not have the pinned ctx Apple authority: ${path}"
  }
  grep -Fqx "TeamIdentifier=${EXPECTED_TEAM_ID}" "${details}" || {
    rm -f "${details}"
    die "artifact does not have the pinned ctx Apple Team ID: ${path}"
  }
  codesign_details_have_runtime "${details}" || {
    rm -f "${details}"
    die "artifact is missing hardened runtime flags: ${path}"
  }
  grep -Eq '^Timestamp=.+$' "${details}" || {
    rm -f "${details}"
    die "artifact is missing a secure timestamp: ${path}"
  }
  rm -f "${details}"
}

if [[ "${kind}" == "cli" ]]; then
  python3 "${root_dir}/scripts/macos-release-signing-evidence.py" verify-artifact \
    --evidence "${evidence}" \
    --platform "${platform}" \
    --kind cli \
    --artifact "${artifact}" \
    --checksum "${artifact}.sha256"
  "${root_dir}/scripts/verify-macos-release-attestation.sh" \
    "${platform}" cli "${artifact}" "${attestation_json}" "${attestation_cms}"
  verify_macho "${artifact}"
  build_info="${artifact}.build-info.json"
  if [[ -s "${build_info}" ]]; then
    python3 - "${artifact}" "${build_info}" <<'PY'
import hashlib
import json
import sys

artifact, build_info = sys.argv[1:]
with open(artifact, "rb") as source:
    digest = hashlib.sha256()
    for chunk in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(chunk)
    actual = digest.hexdigest()
with open(build_info, encoding="utf-8") as source:
    expected = json.load(source).get("artifact_sha256")
if expected != actual:
    raise SystemExit(
        f"build-info does not bind the signed CLI bytes: expected {expected}, got {actual}"
    )
PY
  fi
  printf 'macOS release signing ok: %s cli\n' "${platform}"
  exit 0
fi

require_command tar
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-runtime-check.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
inspection_archive="${artifact}"
case "${artifact}" in
  *.tar.zst)
    require_command zstd
    inspection_archive="${work_dir}/runtime.tar"
    zstd -q -d -f "${artifact}" -o "${inspection_archive}"
    package_role="builder"
    ;;
  *.tar.gz)
    package_role="release"
    ;;
  *) die "macOS runtime signing check requires .tar.zst or .tar.gz" ;;
esac
nested_artifact="${work_dir}/libonnxruntime.dylib"
python3 - "${inspection_archive}" "${nested_artifact}" <<'PY'
import shutil
import sys
import tarfile

archive, output = sys.argv[1:]
expected = "lib/libonnxruntime.dylib"
mode = "r:gz" if archive.endswith(".gz") else "r:"
with tarfile.open(archive, mode) as bundle:
    matches = [member for member in bundle.getmembers() if member.name == expected]
    if len(matches) != 1 or not matches[0].isfile():
        raise SystemExit(f"runtime archive must contain one regular {expected}")
    source = bundle.extractfile(matches[0])
    if source is None:
        raise SystemExit(f"could not read {expected}")
    with source, open(output, "wb") as destination:
        shutil.copyfileobj(source, destination)
PY
python3 "${root_dir}/scripts/macos-release-signing-evidence.py" verify-archive \
  --evidence "${evidence}" \
  --platform "${platform}" \
  --archive "${artifact}" \
  --checksum "${artifact}.sha256" \
  --nested-artifact "${nested_artifact}" \
  --role "${package_role}"
"${root_dir}/scripts/verify-macos-release-attestation.sh" \
  "${platform}" runtime "${nested_artifact}" \
  "${attestation_json}" "${attestation_cms}"
verify_macho "${nested_artifact}"
printf 'macOS release signing ok: %s runtime %s\n' "${platform}" "${package_role}"
