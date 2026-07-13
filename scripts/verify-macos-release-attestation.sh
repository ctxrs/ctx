#!/usr/bin/env bash
set -euo pipefail

EXPECTED_AUTHORITY="Developer ID Application: Profound Health Institute LLC (SJSNARH4TG)"
EXPECTED_TEAM_ID="SJSNARH4TG"
EXPECTED_CA_SHA256="F1:6C:D3:C5:4C:7F:83:CE:A4:BF:1A:3E:6A:08:19:C8:AA:A8:E4:A1:52:8F:D1:44:71:5F:35:06:43:D2:DF:3A"

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/verify-macos-release-attestation.sh PLATFORM KIND ARTIFACT ATTESTATION CMS
  scripts/verify-macos-release-attestation.sh --runtime-archive PLATFORM ARCHIVE NESTED_DYLIB ATTESTATION CMS

Verifies a detached Developer ID CMS statement against only the pinned Apple
Developer ID G2 CA, then checks its exact executable or final-archive binding.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

mode=artifact
if [[ "${1:-}" == "--runtime-archive" ]]; then
  [[ $# -eq 6 ]] || { usage; exit 2; }
  mode=runtime_archive
  platform="$2"
  artifact="$3"
  nested_artifact="$4"
  attestation="$5"
  cms="$6"
else
  [[ $# -eq 5 ]] || { usage; exit 2; }
  platform="$1"
  kind="$2"
  artifact="$3"
  attestation="$4"
  cms="$5"
  case "${kind}" in cli|runtime) ;; *) usage; exit 2 ;; esac
fi
case "${platform}" in macos-arm64|macos-x64) ;; *) usage; exit 2 ;; esac
[[ -f "${artifact}" ]] || die "attested macOS artifact missing: ${artifact}"
if [[ "${mode}" == "runtime_archive" ]]; then
  [[ -f "${nested_artifact}" ]] || die "attested macOS nested dylib missing: ${nested_artifact}"
  [[ "$(basename "${nested_artifact}")" == "libonnxruntime.dylib" ]] || \
    die "runtime archive attestation requires libonnxruntime.dylib"
fi
[[ -s "${attestation}" ]] || die "macOS attestation statement missing: ${attestation}"
[[ -s "${cms}" ]] || die "macOS attestation signature missing: ${cms}"
if [[ "${mode}" == "runtime_archive" ]]; then
  notary_submit="${attestation%.release-attestation.json}.notary-submit.json"
else
  notary_submit="${attestation%.attestation.json}.notary-submit.json"
fi
[[ -s "${notary_submit}" ]] || die "bound Apple notarization response missing: ${notary_submit}"
command -v openssl >/dev/null 2>&1 || die "openssl is required to verify macOS attestation"
command -v python3 >/dev/null 2>&1 || die "python3 is required to verify macOS attestation"
[[ "$(openssl version 2>/dev/null || true)" == OpenSSL\ 3.* ]] || \
  die "macOS attestation verification requires OpenSSL 3"
cms_help="$(openssl cms -help 2>&1 || true)"
for flag in -no-CApath -no-CAstore -ignore_critical; do
  [[ "${cms_help}" == *"${flag}"* ]] || \
    die "selected OpenSSL 3 lacks required exclusive-trust flag ${flag}"
done

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ca_file="${root_dir}/scripts/apple-developer-id-g2-ca.pem"
ca_fingerprint="$(openssl x509 -in "${ca_file}" -noout -fingerprint -sha256 2>/dev/null \
  | sed 's/^.*Fingerprint=//')"
[[ "${ca_fingerprint}" == "${EXPECTED_CA_SHA256}" ]] || \
  die "pinned Apple Developer ID G2 CA fingerprint mismatch"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-attestation-check.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
signer_cert="${work_dir}/signer.pem"
if ! openssl cms -verify \
  -binary \
  -inform DER \
  -in "${cms}" \
  -content "${attestation}" \
  -purpose any \
  -partial_chain \
  -no-CApath \
  -no-CAstore \
  -ignore_critical \
  -CAfile "${ca_file}" \
  -signer "${signer_cert}" \
  -out /dev/null >/dev/null 2>&1; then
  die "macOS release attestation CMS signature verification failed"
fi
signer_count="$(grep -c '^-----BEGIN CERTIFICATE-----$' "${signer_cert}" || true)"
[[ "${signer_count}" == "1" ]] || \
  die "macOS release attestation must have exactly one actual signer"
subject="$(openssl x509 \
  -in "${signer_cert}" -noout -subject -nameopt RFC2253 2>/dev/null || true)"
subject=",${subject#subject=},"
[[ "${subject}" == *",CN=${EXPECTED_AUTHORITY},"* ]] || \
  die "macOS attestation actual signer does not have the pinned ctx Apple authority"
[[ "${subject}" == *",OU=${EXPECTED_TEAM_ID},"* ]] || \
  die "macOS attestation actual signer does not have the pinned ctx Apple Team ID"
eku="$(openssl x509 -in "${signer_cert}" -noout -ext extendedKeyUsage 2>/dev/null || true)"
grep -Eq '(^|[ ,])(Code Signing|1\.3\.6\.1\.5\.5\.7\.3\.3)(,|$)' <<<"${eku}" || \
  die "macOS attestation actual signer certificate lacks the Code Signing EKU"
key_usage="$(openssl x509 -in "${signer_cert}" -noout -ext keyUsage 2>/dev/null || true)"
[[ "${key_usage}" == *"X509v3 Key Usage: critical"* \
  && "${key_usage}" == *"Digital Signature"* ]] || \
  die "macOS attestation actual signer lacks critical Digital Signature key usage"
certificate_profile="$(openssl x509 -in "${signer_cert}" -noout -text 2>/dev/null || true)"
[[ "${certificate_profile}" == *"1.2.840.113635.100.6.1.13: critical"* ]] || \
  die "macOS attestation actual signer lacks Apple's critical Developer ID extension"
openssl verify -purpose any -partial_chain -no-CApath -no-CAstore -ignore_critical \
  -CAfile "${ca_file}" "${signer_cert}" >/dev/null 2>&1 || \
  die "macOS attestation actual signer does not chain exclusively to the pinned Apple G2 CA"

source_commit="${CTX_MACOS_RELEASE_SOURCE_COMMIT:-}"
if [[ -z "${source_commit}" ]]; then
  source_commit="$(git -C "${root_dir}" rev-parse --verify HEAD)"
fi
[[ "${source_commit}" =~ ^[0-9a-f]{40}$ && ! "${source_commit}" =~ ^0{40}$ ]] || \
  die "macOS release source commit must be a non-placeholder 40-character Git commit"
if [[ "${mode}" == "runtime_archive" ]]; then
  python3 "${root_dir}/scripts/macos-release-signing-evidence.py" \
    verify-runtime-archive-attestation \
    --attestation "${attestation}" \
    --platform "${platform}" \
    --archive "${artifact}" \
    --nested-artifact "${nested_artifact}" \
    --notary-submit "${notary_submit}" \
    --source-commit "${source_commit}"
  printf 'macOS release attestation ok: %s runtime release archive\n' "${platform}"
else
  python3 "${root_dir}/scripts/macos-release-signing-evidence.py" verify-attestation \
    --attestation "${attestation}" \
    --platform "${platform}" \
    --kind "${kind}" \
    --artifact "${artifact}" \
    --notary-submit "${notary_submit}" \
    --source-commit "${source_commit}"
  printf 'macOS release attestation ok: %s %s\n' "${platform}" "${kind}"
fi
