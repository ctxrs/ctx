#!/usr/bin/env bash
set -euo pipefail
case "$-" in
  *x*) set +x ;;
esac

EXPECTED_TEAM_ID="SJSNARH4TG"

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

subject_authority() {
  local subject="$1"
  [[ "${subject}" == *",CN="* ]] || return 1
  subject="${subject#*,CN=}"
  printf '%s\n' "${subject%%,*}"
}

path_mode() {
  if [[ "$(uname -s)" == "Darwin" ]]; then
    stat -f '%Lp' "$1"
  else
    stat -c '%a' "$1"
  fi
}

decode_b64_file() {
  local input="$1"
  local output="$2"
  rm -f "${output}"
  if base64 --decode <"${input}" >"${output}" 2>/dev/null \
    || base64 -d <"${input}" >"${output}" 2>/dev/null \
    || base64 -D <"${input}" >"${output}" 2>/dev/null; then
    chmod 0600 "${output}"
    [[ -s "${output}" ]] || die "decoded Developer ID P12 was empty"
    return
  fi
  rm -f "${output}"
  die "failed to decode Developer ID P12"
}

extract_p12_part() {
  local p12_path="$1"
  local password_path="$2"
  local output="$3"
  shift 3
  rm -f "${output}"
  if openssl pkcs12 -in "${p12_path}" -passin "file:${password_path}" \
    "$@" -out "${output}" >/dev/null 2>&1 \
    || openssl pkcs12 -legacy -in "${p12_path}" -passin "file:${password_path}" \
      "$@" -out "${output}" >/dev/null 2>&1; then
    chmod 0600 "${output}"
    return
  fi
  rm -f "${output}"
  die "Developer ID P12 could not be opened with its password file"
}

[[ $# -ge 3 && $# -le 4 ]] || {
  printf 'usage: %s PLATFORM ARCHIVE NESTED_DYLIB [EVIDENCE_DIR]\n' "$0" >&2
  exit 2
}
platform="$1"
archive="$2"
nested_artifact="$3"
evidence_dir="${4:-target/public-cli-artifacts}"
case "${platform}" in macos-arm64|macos-x64) ;; *) die "unsupported macOS platform" ;; esac
[[ -f "${archive}" ]] || die "final runtime archive missing: ${archive}"
[[ -f "${nested_artifact}" ]] || die "final runtime dylib missing: ${nested_artifact}"
[[ "$(basename "${nested_artifact}")" == "libonnxruntime.dylib" ]] || \
  die "final runtime attestation requires libonnxruntime.dylib"
[[ "${CTX_MACOS_SIGNING_LAUNCHED:-0}" == "1" ]] || \
  die "runtime archive attester must be invoked through the trusted narrow launcher"

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"${root_dir}/scripts/check-macos-signing-trusted-ref.sh" >/dev/null
if [[ "${CTX_TEST_ONLY_MACOS_HOST:-}" == "Darwin" ]]; then
  [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" ]] || \
    die "CTX_TEST_ONLY_MACOS_HOST is restricted to non-CI local contract tests"
elif [[ "$(uname -s)" != "Darwin" ]]; then
  die "runtime archive attestation requires a native Darwin host"
fi

secret_dir="${CTX_MACOS_SIGNING_SECRET_DIR:-}"
[[ "${secret_dir}" == /* && -d "${secret_dir}" && ! -L "${secret_dir}" && -O "${secret_dir}" ]] || \
  die "trusted launcher did not provide an owned secret directory"
[[ "$(path_mode "${secret_dir}")" == "700" ]] || \
  die "trusted launcher secret directory must have mode 0700"
for secret_name in APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD; do
  secret_path="${secret_dir}/${secret_name}"
  [[ -f "${secret_path}" && ! -L "${secret_path}" && -O "${secret_path}" ]] || \
    die "trusted launcher secret file is invalid: ${secret_name}"
  [[ "$(path_mode "${secret_path}")" == "600" && -s "${secret_path}" ]] || \
    die "trusted launcher secret file must be nonempty mode 0600: ${secret_name}"
done
[[ "$(find "${secret_dir}" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" == "2" ]] || \
  die "runtime archive attestation requires exactly two secret files"

mkdir -p "${evidence_dir}"
evidence_dir="$(cd "${evidence_dir}" && pwd)"
archive="$(cd "$(dirname "${archive}")" && pwd)/$(basename "${archive}")"
nested_artifact="$(cd "$(dirname "${nested_artifact}")" && pwd)/libonnxruntime.dylib"
statement="${evidence_dir}/ctx-onnxruntime-${platform}.release-attestation.json"
cms="${evidence_dir}/ctx-onnxruntime-${platform}.release-attestation.cms"
notary_submit="${evidence_dir}/ctx-onnxruntime-${platform}.notary-submit.json"
[[ -s "${notary_submit}" ]] || die "accepted runtime notarization response is missing"

umask 077
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-runtime-attestation.XXXXXX")"
complete=0
cleanup() {
  rm -rf "${work_dir}" >/dev/null 2>&1 || true
  if [[ "${complete}" != "1" ]]; then
    rm -f "${statement}" "${cms}"
  fi
}
trap cleanup EXIT
p12_path="${work_dir}/codesign.p12"
cert_path="${work_dir}/codesign.pem"
key_path="${work_dir}/codesign.key"
decode_b64_file "${secret_dir}/APPLE_CODESIGN_CERT_P12_B64" "${p12_path}"
extract_p12_part "${p12_path}" "${secret_dir}/APPLE_CODESIGN_CERT_PASSWORD" \
  "${cert_path}" -clcerts -nokeys
extract_p12_part "${p12_path}" "${secret_dir}/APPLE_CODESIGN_CERT_PASSWORD" \
  "${key_path}" -nocerts -nodes
openssl pkey -in "${key_path}" -noout >/dev/null 2>&1 || \
  die "Developer ID P12 private key did not parse"

subject="$(openssl x509 -in "${cert_path}" -noout -subject -nameopt RFC2253 2>/dev/null || true)"
subject=",${subject#subject=},"
certificate_authority="$(subject_authority "${subject}")" || \
  die "runtime archive attester certificate is missing a Developer ID common name"
authority_matches_expected_team "${certificate_authority}" || \
  die "runtime archive attester is not the pinned ctx Developer ID identity"
[[ "${subject}" == *",OU=${EXPECTED_TEAM_ID},"* ]] || \
  die "runtime archive attester does not have the pinned ctx Apple Team ID"
eku="$(openssl x509 -in "${cert_path}" -noout -ext extendedKeyUsage 2>/dev/null || true)"
grep -Eq '(^|[ ,])(Code Signing|1\.3\.6\.1\.5\.5\.7\.3\.3)(,|$)' <<<"${eku}" || \
  die "runtime archive attester certificate lacks the Code Signing EKU"
key_usage="$(openssl x509 -in "${cert_path}" -noout -ext keyUsage 2>/dev/null || true)"
[[ "${key_usage}" == *"X509v3 Key Usage: critical"* \
  && "${key_usage}" == *"Digital Signature"* ]] || \
  die "runtime archive attester certificate lacks critical Digital Signature key usage"
certificate_profile="$(openssl x509 -in "${cert_path}" -noout -text 2>/dev/null || true)"
[[ "${certificate_profile}" == *"1.2.840.113635.100.6.1.13: critical"* ]] || \
  die "runtime archive attester certificate lacks Apple's critical Developer ID extension"
openssl verify -purpose any -partial_chain -no-CApath -no-CAstore -ignore_critical \
  -CAfile "${root_dir}/scripts/apple-developer-id-g2-ca.pem" \
  "${cert_path}" >/dev/null 2>&1 || \
  die "runtime archive attester does not chain exclusively to the pinned Apple G2 CA"

python3 "${root_dir}/scripts/macos-release-signing-evidence.py" \
  create-runtime-archive-attestation \
  --output "${statement}" \
  --platform "${platform}" \
  --archive "${archive}" \
  --nested-artifact "${nested_artifact}" \
  --notary-submit "${notary_submit}" \
  --source-commit "$(git -C "${root_dir}" rev-parse --verify HEAD)" \
  --codesign-authority "${certificate_authority}"
openssl cms -sign -binary \
  -in "${statement}" \
  -signer "${cert_path}" \
  -inkey "${key_path}" \
  -outform DER \
  -out "${cms}" \
  -md sha256 \
  -noattr >/dev/null 2>&1 || die "failed to sign final runtime archive attestation"
chmod 0644 "${statement}" "${cms}"
"${root_dir}/scripts/verify-macos-release-attestation.sh" \
  --runtime-archive "${platform}" "${archive}" "${nested_artifact}" "${statement}" "${cms}" \
  >/dev/null
complete=1
printf 'authorized final %s runtime archive: %s\n' "${platform}" "${archive}"
