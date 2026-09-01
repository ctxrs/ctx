#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/validate-public-cli-factory-artifact.sh PLATFORM ARTIFACT_DIR OUTPUT_DIR

Validates one exact Core artifact downloaded from the Linux release factory on
its native platform. This command never compiles or replaces the candidate bytes.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

[[ $# -eq 3 ]] || { usage; exit 2; }
platform="$1"
artifact_dir="$2"
output_dir="$3"
case "${platform}" in
  linux-x64) binary="ctx" ;;
  linux-aarch64) binary="ctx-linux-aarch64" ;;
  macos-arm64) binary="ctx-macos-arm64" ;;
  macos-x64) binary="ctx-macos-x64" ;;
  windows-x64) binary="ctx.exe" ;;
  *) usage; exit 2 ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
[[ -n "${artifact_dir}" ]] || die "factory artifact directory is unavailable"
case "${artifact_dir}" in
  /*) artifact_dir_operand="${artifact_dir}" ;;
  *) artifact_dir_operand="./${artifact_dir}" ;;
esac
artifact_dir="$(CDPATH= cd "${artifact_dir_operand}" 2>/dev/null && pwd -P)" || \
  die "factory artifact directory is unavailable"
artifact="${artifact_dir}/${binary}"
[[ -f "${artifact}" && ! -L "${artifact}" ]] || die "factory artifact is missing"
[[ -s "${artifact}.sha256" ]] || die "factory checksum is missing"
before="$(sha256_file "${artifact}")"
expected_sha256="$(tr -d '[:space:]' <"${artifact}.sha256")"
[[ "${expected_sha256}" =~ ^[0-9a-f]{64}$ ]] || die "factory checksum is malformed"
[[ "${before}" == "${expected_sha256}" ]] || \
  die "factory artifact checksum mismatch"
source_commit="$(git rev-parse --verify HEAD^{commit})"
version="$(python3 -I scripts/check-public-cli-build-info.py \
  --artifact "${artifact}" \
  --build-info "${artifact}.build-info.json" \
  --matrix contracts/release-targets-v1.json \
  --platform "${platform}" \
  --source-commit "${source_commit}" \
  --cargo-lock Cargo.lock \
  --factory-inputs contracts/release-factory-inputs-v1.json \
  --candidate-manifest "${artifact}.candidate.json" \
  --version-file "${artifact}.version")" || \
  die "factory candidate identity or version contract failed"

# Buildkite artifact downloads preserve bytes but not Unix executable mode.
# Restore only owner execute permission after the exact artifact, source,
# candidate, build-info, and construction-version bindings have passed.
if [[ "${platform}" != windows-x64 ]]; then
  # BSD chmod rejects the GNU-only extra option terminator. The canonical
  # absolute path above cannot be mistaken for an option.
  chmod u+x "${artifact}" || die "could not establish factory artifact executable mode"
  [[ -f "${artifact}" && ! -L "${artifact}" && -x "${artifact}" ]] || \
    die "factory artifact is not an executable regular file"
fi

if [[ "${platform}" == linux-* ]]; then
  IFS=$'\t' read -r \
    host_system host_arch host_native_arch process_translated _native_arch_probe \
    hardware_identity emulation hypervisor evidence_complete \
    < <(scripts/public-cli-host-runtime-evidence.sh)
  IFS=$'\t' read -r os_identity os_version os_product_type \
    < <(scripts/public-cli-host-runtime-evidence.sh --os-baseline-only)
  runtime_authority="$(scripts/public-cli-runtime-authority.sh \
    "${platform}" "${host_system}" "${host_arch}" passed \
    "${host_native_arch}" "${process_translated}" "${hardware_identity}" \
    "${emulation}" "${hypervisor}" "${evidence_complete}" "" \
    "${os_identity}" "${os_version}" "${os_product_type}" ubuntu-24.04)"
  [[ "${runtime_authority}" == authoritative ]] || \
    die "native Linux validation requires authoritative Ubuntu 24.04 execution"
fi

case "${platform}" in
  macos-arm64|macos-x64)
    scripts/verify-macos-signed-cli.sh "${platform}" "${artifact}" "${version}" \
      "${artifact_dir%/}/ctx-${platform}.signing.json"
    scripts/check-macos-release-signing.sh "${platform}" cli "${artifact}"
    mkdir -p "${output_dir%/}"
    scripts/run-native-candidate-smoke.sh \
      "${artifact}" tests/fixtures/custom-history-jsonl/basic.jsonl "${version}" \
      "${output_dir%/}/candidate-smoke.json"
    ;;
  windows-x64)
    command -v powershell.exe >/dev/null 2>&1 || die "PowerShell is required"
    powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
      -File scripts/verify-windows-authenticode.ps1 \
      -Artifact "${artifact}" \
      -Evidence "${artifact}.authenticode.json"
    printf '%s\n' "${version}" >"${artifact}.expected-version"
    mkdir -p "${output_dir}"
    powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
      -File scripts/run-native-candidate-smoke.ps1 \
      -Binary "${artifact}" \
      -Fixture tests/fixtures/custom-history-jsonl/basic.jsonl \
      -ExpectedVersion "${version}" \
      -ResultPath "${output_dir%/}/candidate-smoke.json"
    ;;
  *)
    mkdir -p "${output_dir}"
    scripts/run-native-candidate-smoke.sh \
      "${artifact}" tests/fixtures/custom-history-jsonl/basic.jsonl "${version}" \
      "${output_dir%/}/candidate-smoke.json"
    ;;
esac

after="$(sha256_file "${artifact}")"
[[ "${after}" == "${before}" ]] || die "native validation mutated candidate bytes"
python3 -I scripts/native-execution-proof.py create \
  --platform "${platform}" \
  --artifact "${artifact}" \
  --smoke-result "${output_dir%/}/candidate-smoke.json" \
  --output "${output_dir%/}/ctx-${platform}.native-execution.json"
if [[ "${platform}" == macos-* ]]; then
  signing_evidence="${artifact_dir%/}/ctx-${platform}.signing.json"
  [[ -f "${signing_evidence}" && ! -L "${signing_evidence}" ]] || \
    die "passed macOS signing evidence is not a regular file"
  install -m 0644 "${signing_evidence}" \
    "${output_dir%/}/ctx-${platform}.signing.json"
fi
printf 'native exact-byte validation passed: %s sha256=%s\n' "${platform}" "${after}"
