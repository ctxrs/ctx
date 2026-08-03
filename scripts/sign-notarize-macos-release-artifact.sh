#!/usr/bin/env bash
set -euo pipefail
case "$-" in
  *x*) set +x ;;
esac

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/sign-notarize-macos-release-artifact.sh PLATFORM KIND ARTIFACT [EVIDENCE_DIR]

Signs one standalone macOS release Mach-O with Developer ID, submits a
temporary ZIP to Apple notarization, and records sanitized verification
evidence. KIND is cli or runtime.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

authority_matches_team() {
  local authority="$1"
  local team_id="$2"
  case "${authority}" in
    "Developer ID Application: "*" (${team_id})") return 0 ;;
    *) return 1 ;;
  esac
}

subject_authority() {
  local subject="$1"
  [[ "${subject}" == *",CN="* ]] || return 1
  subject="${subject#*,CN=}"
  printf '%s\n' "${subject%%,*}"
}

subject_organizational_unit() {
  local subject="$1"
  [[ "${subject}" == *",OU="* ]] || return 1
  subject="${subject#*,OU=}"
  printf '%s\n' "${subject%%,*}"
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

decode_b64_file() {
  local label="$1"
  local input="$2"
  local output="$3"

  rm -f "${output}"
  if base64 --decode <"${input}" >"${output}" 2>/dev/null \
    || base64 -d <"${input}" >"${output}" 2>/dev/null \
    || base64 -D <"${input}" >"${output}" 2>/dev/null; then
    chmod 0600 "${output}"
    [[ -s "${output}" ]] || die "decoded ${label} was empty"
    return 0
  fi
  rm -f "${output}"
  die "failed to decode ${label}"
}

path_mode() {
  local path="$1"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    stat -f '%Lp' "${path}"
  else
    stat -c '%a' "${path}"
  fi
}

extract_codesign_certificate() {
  local p12_path="$1"
  local password_path="$2"
  local certificate_path="$3"

  rm -f "${certificate_path}"
  if openssl pkcs12 \
    -in "${p12_path}" -passin "file:${password_path}" \
    -clcerts -nokeys -out "${certificate_path}" >/dev/null 2>&1; then
    chmod 0600 "${certificate_path}"
    return 0
  fi
  rm -f "${certificate_path}"
  if openssl pkcs12 -legacy \
    -in "${p12_path}" -passin "file:${password_path}" \
    -clcerts -nokeys -out "${certificate_path}" >/dev/null 2>&1; then
    chmod 0600 "${certificate_path}"
    return 0
  fi
  die "APPLE_CODESIGN_CERT_P12_B64 could not be opened with APPLE_CODESIGN_CERT_PASSWORD"
}

extract_codesign_private_key() {
  local p12_path="$1"
  local password_path="$2"
  local private_key_path="$3"

  rm -f "${private_key_path}"
  if openssl pkcs12 \
    -in "${p12_path}" -passin "file:${password_path}" \
    -nocerts -nodes -out "${private_key_path}" >/dev/null 2>&1; then
    chmod 0600 "${private_key_path}"
    return 0
  fi
  rm -f "${private_key_path}"
  if openssl pkcs12 -legacy \
    -in "${p12_path}" -passin "file:${password_path}" \
    -nocerts -nodes -out "${private_key_path}" >/dev/null 2>&1; then
    chmod 0600 "${private_key_path}"
    return 0
  fi
  die "APPLE_CODESIGN_CERT_P12_B64 did not contain an importable private key"
}

json_field() {
  local path="$1"
  local name="$2"
  python3 - "${path}" "${name}" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source).get(sys.argv[2])
except (OSError, json.JSONDecodeError, AttributeError):
    value = None
if value is not None:
    print(value, end="")
PY
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
  else
    shasum -a 256 "${path}" | awk '{ print $1 }'
  fi
}

print_notary_diagnostics() {
  local submit_stderr="$1"
  local log_json="$2"
  local log_stderr="$3"

  if [[ -s "${submit_stderr}" ]]; then
    sed -n '1,40p' "${submit_stderr}" >&2 || true
  fi
  if [[ -s "${log_json}" ]]; then
    python3 - "${log_json}" <<'PY' >&2 || true
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as source:
        payload = json.load(source)
except (OSError, json.JSONDecodeError):
    raise SystemExit(0)
issues = payload.get("issues") if isinstance(payload, dict) else None
if isinstance(issues, list):
    for issue in issues[:20]:
        if isinstance(issue, dict):
            print(": ".join(str(issue[key]) for key in ("severity", "path", "message") if issue.get(key)))
PY
  elif [[ -s "${log_stderr}" ]]; then
    sed -n '1,40p' "${log_stderr}" >&2 || true
  fi
}

platform="${1:-}"
kind="${2:-}"
artifact="${3:-}"
evidence_dir="${4:-target/public-cli-artifacts}"
if [[ -z "${platform}" || -z "${kind}" || -z "${artifact}" ]]; then
  usage
  exit 2
fi
case "${platform}" in
  macos-arm64|macos-x64) ;;
  *) usage; exit 2 ;;
esac
case "${kind}" in
  cli)
    evidence_prefix="ctx-${platform}"
    ;;
  runtime)
    evidence_prefix="ctx-onnxruntime-${platform}"
    ;;
  *) usage; exit 2 ;;
esac
[[ -f "${artifact}" ]] || die "macOS release artifact not found: ${artifact}"
[[ "${CTX_MACOS_SIGNING_LAUNCHED:-0}" == "1" ]] || \
  die "macOS signer must be invoked through the trusted narrow launcher"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "${root_dir}/scripts/macos-release-publisher-policy.sh"
"${root_dir}/scripts/check-macos-signing-trusted-ref.sh" >/dev/null
if [[ "${CTX_TEST_ONLY_MACOS_HOST:-}" == "Darwin" ]]; then
  [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" ]] || \
    die "CTX_TEST_ONLY_MACOS_HOST is restricted to non-CI local contract tests"
elif [[ "$(uname -s)" != "Darwin" ]]; then
  die "macOS release signing requires a native Darwin host"
fi

for command_name in base64 codesign ditto find openssl python3 rcodesign stat xcrun; do
  require_command "${command_name}"
done

secret_dir="${CTX_MACOS_SIGNING_SECRET_DIR:-}"
[[ "${secret_dir}" == /* && -d "${secret_dir}" && ! -L "${secret_dir}" && -O "${secret_dir}" ]] || \
  die "trusted launcher did not provide an owned secret directory"
[[ "$(path_mode "${secret_dir}")" == "700" ]] || \
  die "trusted launcher secret directory must have mode 0700"
secret_names=(
  APPLE_CODESIGN_CERT_P12_B64
  APPLE_CODESIGN_CERT_PASSWORD
  NOTARY_ISSUER
  NOTARY_KEY_ID
  NOTARY_KEY_P8_B64
)
for secret_name in "${secret_names[@]}"; do
  secret_path="${secret_dir}/${secret_name}"
  [[ -f "${secret_path}" && ! -L "${secret_path}" && -O "${secret_path}" ]] || \
    die "trusted launcher secret file is invalid: ${secret_name}"
  [[ "$(path_mode "${secret_path}")" == "600" ]] || \
    die "trusted launcher secret file must have mode 0600: ${secret_name}"
  [[ -s "${secret_path}" ]] || die "trusted launcher secret file is empty: ${secret_name}"
done
[[ "$(find "${secret_dir}" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" == "5" ]] || \
  die "trusted launcher secret directory must contain exactly five files"
cert_b64_path="${secret_dir}/APPLE_CODESIGN_CERT_P12_B64"
cert_password_path="${secret_dir}/APPLE_CODESIGN_CERT_PASSWORD"
notary_issuer_path="${secret_dir}/NOTARY_ISSUER"
notary_key_id_path="${secret_dir}/NOTARY_KEY_ID"
notary_key_b64_path="${secret_dir}/NOTARY_KEY_P8_B64"
notary_issuer=""
notary_key_id=""
IFS= read -r notary_issuer <"${notary_issuer_path}" || [[ -n "${notary_issuer}" ]]
IFS= read -r notary_key_id <"${notary_key_id_path}" || [[ -n "${notary_key_id}" ]]

notary_timeout="${CTX_MACOS_NOTARY_TIMEOUT:-30m}"
[[ "${notary_timeout}" =~ ^[1-9][0-9]*[smh]$ ]] || \
  die "CTX_MACOS_NOTARY_TIMEOUT must be a positive integer followed by s, m, or h"

mkdir -p "${evidence_dir}"
evidence_dir="$(cd "${evidence_dir}" && pwd)"
artifact="$(cd "$(dirname "${artifact}")" && pwd)/$(basename "${artifact}")"
submit_json="${evidence_dir}/${evidence_prefix}.notary-submit.json"
submit_stderr="${evidence_dir}/${evidence_prefix}.notary-submit.stderr"
log_json="${evidence_dir}/${evidence_prefix}.notary-log.json"
log_stderr="${evidence_dir}/${evidence_prefix}.notary-log.stderr"
codesign_details="${evidence_dir}/${evidence_prefix}.codesign.txt"
evidence_json="${evidence_dir}/${evidence_prefix}.signing.json"
attestation_json="${evidence_dir}/${evidence_prefix}.attestation.json"
attestation_cms="${evidence_dir}/${evidence_prefix}.attestation.cms"
rm -f "${submit_json}" "${submit_stderr}" "${log_json}" "${log_stderr}" \
  "${codesign_details}" "${evidence_json}" \
  "${attestation_json}" "${attestation_cms}"

umask 077
secret_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signing.XXXXXX")"
cleanup() {
  rm -rf "${secret_root}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cert_path="${secret_root}/codesign-cert.p12"
cert_pem_path="${secret_root}/codesign-cert.pem"
cert_private_key_path="${secret_root}/codesign-cert.key"
notary_key_path="${secret_root}/AuthKey.p8"
notary_zip="${secret_root}/${evidence_prefix}.zip"

decode_b64_file APPLE_CODESIGN_CERT_P12_B64 "${cert_b64_path}" "${cert_path}"
extract_codesign_certificate "${cert_path}" "${cert_password_path}" "${cert_pem_path}"
extract_codesign_private_key \
  "${cert_path}" "${cert_password_path}" "${cert_private_key_path}"
openssl pkey -in "${cert_private_key_path}" -noout >/dev/null 2>&1 || \
  die "APPLE_CODESIGN_CERT_P12_B64 private key did not parse"
certificate_subject="$(openssl x509 \
  -in "${cert_pem_path}" -noout -subject -nameopt RFC2253 2>/dev/null || true)"
certificate_subject=",${certificate_subject#subject=},"
certificate_authority="$(subject_authority "${certificate_subject}")" || \
  die "APPLE_CODESIGN_CERT_P12_B64 is missing a Developer ID common name"
certificate_team_id="$(subject_organizational_unit "${certificate_subject}")" || \
  die "APPLE_CODESIGN_CERT_P12_B64 is missing an Apple Team ID"
[[ "${certificate_team_id}" =~ ^[A-Z0-9]{10}$ ]] || \
  die "APPLE_CODESIGN_CERT_P12_B64 has an invalid Apple Team ID"
authority_matches_team "${certificate_authority}" "${certificate_team_id}" || \
  die "APPLE_CODESIGN_CERT_P12_B64 authority and Team ID disagree"
ctx_macos_release_team_id_matches_policy "${certificate_team_id}" || \
  die "APPLE_CODESIGN_CERT_P12_B64 does not match the pinned project release publisher"
certificate_eku="$(openssl x509 \
  -in "${cert_pem_path}" -noout -ext extendedKeyUsage 2>/dev/null || true)"
grep -Eq '(^|[ ,])(Code Signing|1\.3\.6\.1\.5\.5\.7\.3\.3)(,|$)' \
  <<<"${certificate_eku}" || \
  die "APPLE_CODESIGN_CERT_P12_B64 certificate lacks the Code Signing EKU"
certificate_key_usage="$(openssl x509 \
  -in "${cert_pem_path}" -noout -ext keyUsage 2>/dev/null || true)"
[[ "${certificate_key_usage}" == *"X509v3 Key Usage: critical"* \
  && "${certificate_key_usage}" == *"Digital Signature"* ]] || \
  die "APPLE_CODESIGN_CERT_P12_B64 certificate lacks critical Digital Signature key usage"
certificate_profile="$(openssl x509 -in "${cert_pem_path}" -noout -text 2>/dev/null || true)"
[[ "${certificate_profile}" == *"1.2.840.113635.100.6.1.13: critical"* ]] || \
  die "APPLE_CODESIGN_CERT_P12_B64 certificate lacks Apple's critical Developer ID extension"
openssl verify -purpose any -partial_chain -no-CApath -no-CAstore -ignore_critical \
  -CAfile "${root_dir}/scripts/apple-developer-id-g2-ca.pem" \
  "${cert_pem_path}" >/dev/null 2>&1 || \
  die "APPLE_CODESIGN_CERT_P12_B64 does not chain exclusively to Apple's pinned Developer ID G2 CA"

decode_b64_file NOTARY_KEY_P8_B64 "${notary_key_b64_path}" "${notary_key_path}"
grep -Fq 'BEGIN PRIVATE KEY' "${notary_key_path}" || \
  die "NOTARY_KEY_P8_B64 did not decode to a PKCS#8 private key"
openssl pkey -in "${notary_key_path}" -noout >/dev/null 2>&1 || \
  die "NOTARY_KEY_P8_B64 did not decode to a valid private key"

if ! rcodesign sign \
  --for-notarization \
  --p12-file "${cert_path}" \
  --p12-password-file "${cert_password_path}" \
  "${artifact}"; then
  die "Developer ID signing failed for ${platform} ${kind}"
fi
codesign --verify --strict --verbose=4 "${artifact}" >/dev/null 2>&1 || \
  die "strict codesign verification failed for ${platform} ${kind}"
codesign -d --verbose=4 "${artifact}" >"${codesign_details}" 2>&1 || \
  die "could not inspect Developer ID signature for ${platform} ${kind}"
chmod 0644 "${codesign_details}"
grep -Fqx "Authority=${certificate_authority}" "${codesign_details}" || \
  die "signed ${platform} ${kind} does not have the pinned ctx Apple authority"
grep -Fqx "TeamIdentifier=${certificate_team_id}" "${codesign_details}" || \
  die "signed ${platform} ${kind} does not match the verified certificate Team ID"
codesign_details_have_runtime "${codesign_details}" || \
  die "signed ${platform} ${kind} is missing hardened runtime flags"
grep -Eq '^Timestamp=.+$' "${codesign_details}" || \
  die "signed ${platform} ${kind} is missing a secure timestamp"
signed_sha256="$(sha256_file "${artifact}")"

ditto -c -k --keepParent "${artifact}" "${notary_zip}" || \
  die "failed to create temporary notarization ZIP for ${platform} ${kind}"
set +e
xcrun notarytool submit "${notary_zip}" \
  --key "${notary_key_path}" \
  --key-id "${notary_key_id}" \
  --issuer "${notary_issuer}" \
  --wait \
  --timeout "${notary_timeout}" \
  --output-format json >"${submit_json}" 2>"${submit_stderr}"
submit_status=$?
set -e
chmod 0644 "${submit_json}" "${submit_stderr}" 2>/dev/null || true
notary_status="$(json_field "${submit_json}" status || true)"
submission_id="$(json_field "${submit_json}" id || true)"
if [[ "${submit_status}" -ne 0 || "${notary_status}" != "Accepted" ]]; then
  if [[ -n "${submission_id}" ]]; then
    xcrun notarytool log "${submission_id}" \
      --key "${notary_key_path}" \
      --key-id "${notary_key_id}" \
      --issuer "${notary_issuer}" \
      --output-format json >"${log_json}" 2>"${log_stderr}" || true
    chmod 0644 "${log_json}" "${log_stderr}" 2>/dev/null || true
  fi
  print_notary_diagnostics "${submit_stderr}" "${log_json}" "${log_stderr}"
  if [[ "${submit_status}" -eq 124 ]]; then
    die "Apple notarization timed out after ${notary_timeout} for ${platform} ${kind}"
  fi
  die "Apple notarization failed for ${platform} ${kind} with status ${notary_status:-unknown}"
fi

codesign --verify --strict --verbose=4 "${artifact}" >/dev/null 2>&1 || \
  die "post-notarization codesign verification failed for ${platform} ${kind}"
final_sha256="$(sha256_file "${artifact}")"
[[ "${final_sha256}" == "${signed_sha256}" ]] || \
  die "${platform} ${kind} mutated after Developer ID signing"

python3 "${root_dir}/scripts/macos-release-signing-evidence.py" write \
  --output "${evidence_json}" \
  --platform "${platform}" \
  --kind "${kind}" \
  --artifact "${artifact}" \
  --codesign-details "${codesign_details}" \
  --notary-submit "${submit_json}"
python3 "${root_dir}/scripts/macos-release-signing-evidence.py" create-attestation \
  --output "${attestation_json}" \
  --platform "${platform}" \
  --kind "${kind}" \
  --artifact "${artifact}" \
  --notary-submit "${submit_json}" \
  --source-commit "$(git -C "${root_dir}" rev-parse --verify HEAD)" \
  --codesign-authority "${certificate_authority}"
if ! openssl cms -sign \
  -binary \
  -in "${attestation_json}" \
  -signer "${cert_pem_path}" \
  -inkey "${cert_private_key_path}" \
  -outform DER \
  -out "${attestation_cms}" \
  -md sha256 \
  -noattr >/dev/null 2>&1; then
  die "failed to create Developer ID CMS attestation for ${platform} ${kind}"
fi
chmod 0644 "${attestation_json}" "${attestation_cms}"
"${root_dir}/scripts/verify-macos-release-attestation.sh" \
  "${platform}" "${kind}" "${artifact}" "${attestation_json}" "${attestation_cms}" \
  >/dev/null
printf 'signed and notarized %s %s sha256=%s evidence=%s\n' \
  "${platform}" "${kind}" "${final_sha256}" "${evidence_json}"
