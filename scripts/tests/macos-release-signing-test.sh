#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signing-test.XXXXXX")"
trap 'rm -rf "${test_root}"' EXIT
fake_bin="${test_root}/bin"
mkdir -p "${fake_bin}" "${test_root}/tmp"

fail() {
  printf 'macOS signing contract test failed: %s\n' "$*" >&2
  exit 1
}

cat >"${fake_bin}/openssl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  version)
    printf '%s\n' 'OpenSSL 3.3.0 fake'
    ;;
  pkcs12)
    output=""
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "-out" ]]; then output="$2"; break; fi
      shift
    done
    [[ -n "${output}" ]]
    printf '%s\n' 'fake certificate or key' >"${output}"
    ;;
  x509)
    if [[ " $* " == *' -fingerprint '* ]]; then
      printf '%s\n' 'sha256 Fingerprint=F1:6C:D3:C5:4C:7F:83:CE:A4:BF:1A:3E:6A:08:19:C8:AA:A8:E4:A1:52:8F:D1:44:71:5F:35:06:43:D2:DF:3A'
    elif [[ " $* " == *' -ext extendedKeyUsage '* ]]; then
      if [[ -e "${TMPDIR}/fake-wrong-eku" ]]; then
        printf '%s\n' 'X509v3 Extended Key Usage:' '    TLS Web Server Authentication'
      else
        printf '%s\n' 'X509v3 Extended Key Usage:' '    Code Signing'
      fi
    elif [[ " $* " == *' -ext keyUsage '* ]]; then
      if [[ -e "${TMPDIR}/fake-wrong-key-usage" ]]; then
        printf '%s\n' 'X509v3 Key Usage: critical' '    Key Encipherment'
      else
        printf '%s\n' 'X509v3 Key Usage: critical' '    Digital Signature'
      fi
    elif [[ " $* " == *' -text '* ]]; then
      if [[ ! -e "${TMPDIR}/fake-missing-apple-critical-extension" ]]; then
        printf '%s\n' '1.2.840.113635.100.6.1.13: critical'
      fi
    elif [[ -e "${TMPDIR}/fake-coherent-wrong-identity" ]]; then
      printf '%s\n' 'subject=CN=Developer ID Application: Other Corp (OTHERTEAM),OU=OTHERTEAM,O=Other Corp'
    else
      printf '%s\n' 'subject=CN=Developer ID Application: Profound Health Institute LLC (SJSNARH4TG),OU=SJSNARH4TG,O=Profound Health Institute LLC'
    fi
    ;;
  pkey) ;;
  verify)
    if [[ "${2:-}" == "-help" ]]; then
      printf '%s\n' '-no-CApath' \
        "$([[ ! -e "${TMPDIR}/fake-openssl-missing-exclusive-flags" ]] && printf '%s' '-no-CAstore')" \
        '-ignore_critical'
      exit 0
    fi
    [[ " $* " == *' -no-CApath '* && " $* " == *' -no-CAstore '* \
      && " $* " == *' -ignore_critical '* ]]
    [[ ! -e "${TMPDIR}/fake-host-trust-only-certificate" ]]
    ;;
  cms)
    operation="${2:-}"
    if [[ "${operation}" == "-help" ]]; then
      printf '%s\n' '-no-CApath' \
        "$([[ ! -e "${TMPDIR}/fake-openssl-missing-exclusive-flags" ]] && printf '%s' '-no-CAstore')" \
        '-ignore_critical'
      exit 0
    fi
    original_args="$*"
    input=""
    output=""
    content=""
    signer_output=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -in) input="$2"; shift 2 ;;
        -out) output="$2"; shift 2 ;;
        -content) content="$2"; shift 2 ;;
        -signer) signer_output="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    case "${operation}" in
      -sign)
        sha256sum "${input}" | awk '{print $1}' >"${output}"
        printf '%s\n' attest >>"${TMPDIR}/tool-order.log"
        ;;
      -verify)
        [[ " ${original_args} " == *' -no-CApath '* \
          && " ${original_args} " == *' -no-CAstore '* \
          && " ${original_args} " == *' -ignore_critical '* ]]
        [[ ! -e "${TMPDIR}/fake-host-trust-only-certificate" ]]
        [[ "$(cat "${input}")" == "$(sha256sum "${content}" | awk '{print $1}')" ]] || exit 1
        printf '%s\n' \
          '-----BEGIN CERTIFICATE-----' \
          'fake signer certificate' \
          '-----END CERTIFICATE-----' >"${signer_output}"
        ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
SH

cat >"${fake_bin}/rcodesign" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then printf '%s\n' 'rcodesign 0.test'; exit 0; fi
[[ " $* " == *' --for-notarization '* ]]
original_args="$*"
p12=""
password=""
artifact="${!#}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --p12-file) p12="$2"; shift 2 ;;
    --p12-password-file) password="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "${original_args}" >"${TMPDIR}/rcodesign-argv.txt"
[[ "$(stat -c '%a' "${p12}" 2>/dev/null || stat -f '%Lp' "${p12}")" == "600" ]]
[[ "$(stat -c '%a' "${password}" 2>/dev/null || stat -f '%Lp' "${password}")" == "600" ]]
env | LC_ALL=C sort >"${TMPDIR}/signer-environment.txt"
printf '%s\n' "${CTX_MACOS_SIGNING_SECRET_DIR:?}" >"${TMPDIR}/last-signing-secret-dir.txt"
printf '%s\n' sign >>"${TMPDIR}/tool-order.log"
[[ ! -e "${TMPDIR}/fake-sign-failure" ]] || exit 17
printf '%s\n' '# FAKE_DEVELOPER_ID_SIGNATURE' >>"${artifact}"
SH

cat >"${fake_bin}/codesign" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
artifact="${!#}"
grep -Fq 'FAKE_DEVELOPER_ID_SIGNATURE' "${artifact}" || exit 1
if [[ "${1:-}" == "-d" ]]; then
  if [[ -e "${TMPDIR}/fake-coherent-wrong-identity" ]]; then
    authority='Developer ID Application: Other Corp (OTHERTEAM)'
    team='OTHERTEAM'
  else
    authority='Developer ID Application: Profound Health Institute LLC (SJSNARH4TG)'
    team='SJSNARH4TG'
  fi
  if [[ -e "${TMPDIR}/fake-missing-runtime" ]]; then
    code_directory_flags='flags=0x0(none)'
  else
    code_directory_flags='flags=0x10000(runtime)'
  fi
  cat >&2 <<DETAILS
Executable=fake
Identifier=rs.ctx.test
Authority=${authority}
TeamIdentifier=${team}
Timestamp=Jul 12, 2026 at 12:00:00 PM
CodeDirectory v=20500 size=47580 ${code_directory_flags} hashes=1475+7 location=embedded
DETAILS
fi
SH

cat >"${fake_bin}/ditto" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
source="${@: -2:1}"
output="${@: -1}"
printf '%s\n' zip >>"${TMPDIR}/tool-order.log"
cp "${source}" "${output}"
if [[ -e "${TMPDIR}/fake-mutate-after-sign" ]]; then
  printf '%s\n' mutation >>"${source}"
fi
SH

cat >"${fake_bin}/xcrun" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "notarytool" ]]
operation="${2:-}"
case "${operation}" in
  --version)
    printf '%s\n' 'notarytool 1.test'
    ;;
  submit)
    printf '%s\n' "$*" >"${TMPDIR}/notarytool-argv.txt"
    env | LC_ALL=C sort >"${TMPDIR}/notarytool-environment.txt"
    [[ " $* " == *' --wait '* ]]
    [[ " $* " == *" --timeout ${CTX_MACOS_NOTARY_TIMEOUT:-30m} "* ]]
    printf '%s\n' notary-submit >>"${TMPDIR}/tool-order.log"
    result="accepted"
    [[ ! -s "${TMPDIR}/fake-notary-result" ]] || result="$(cat "${TMPDIR}/fake-notary-result")"
    case "${result}" in
      accepted)
        printf '%s\n' '{"id":"00000000-0000-0000-0000-000000000001","status":"Accepted"}'
        ;;
      rejected)
        printf '%s\n' '{"id":"00000000-0000-0000-0000-000000000002","status":"Invalid","statusSummary":"rejected"}'
        printf '%s\n' 'notary submission rejected' >&2
        exit 1
        ;;
      timeout)
        printf '%s\n' 'notary submission timed out' >&2
        exit 124
        ;;
      *) exit 2 ;;
    esac
    ;;
  log)
    printf '%s\n' '{"status":"Invalid","issues":[{"severity":"error","path":"ctx","message":"invalid signature"}]}'
    ;;
  *) exit 2 ;;
esac
SH

cat >"${fake_bin}/xcode-select" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "-p" ]]
printf '%s\n' '/Applications/Xcode.app/Contents/Developer'
SH

cat >"${fake_bin}/spctl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
touch "${TMPDIR}/spctl-was-invoked"
printf '%s\n' 'code is valid but does not seem to be an app' >&2
exit 3
SH

cat >"${fake_bin}/infisical" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf '%s\n' 'infisical 0.test'
  exit 0
fi
[[ "${1:-}" == "secrets" && "${2:-}" == "get" ]]
name="${3:-}"
printf '%s|%s\n' "${name}" "$*" >>"${TMPDIR}/infisical.log"
[[ ! -e "${TMPDIR}/fake-infisical-auth-failure" ]] || exit 1
if [[ -s "${TMPDIR}/fake-infisical-missing-name" \
  && "${name}" == "$(cat "${TMPDIR}/fake-infisical-missing-name")" ]]; then
  exit 1
fi
case "${name}" in
  APPLE_CODESIGN_CERT_P12_B64) printf '%s' 'cDEyLXNlY3JldC1zZW50aW5lbA==' ;;
  APPLE_CODESIGN_CERT_PASSWORD) printf '%s' 'password-secret-sentinel' ;;
  NOTARY_ISSUER) printf '%s' 'issuer-test-value' ;;
  NOTARY_KEY_ID) printf '%s' 'key-id-test-value' ;;
  NOTARY_KEY_P8_B64) printf '%s' 'LS0tLS1CRUdJTiBQUklWQVRFIEtFWS0tLS0tCnA4LXNlY3JldC1zZW50aW5lbAotLS0tLUVORCBQUklWQVRFIEtFWS0tLS0tCg==' ;;
  *) exit 3 ;;
esac
SH

cat >"${fake_bin}/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case " $* " in
  *' rev-parse --verify HEAD '*|*' rev-parse HEAD '*)
    printf '%040d\n' 1
    ;;
  *' rev-parse --verify refs/remotes/origin/main '*)
    printf '%040d\n' 1
    ;;
  *' diff '*)
    ;;
  *)
    printf 'unexpected fake git invocation: %s\n' "$*" >&2
    exit 2
    ;;
esac
SH

cat >"${fake_bin}/uname" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' Darwin
SH

cat >"${fake_bin}/stat" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "-f" && "${2:-}" == "%Lp" ]]; then
  exec /usr/bin/stat -c '%a' "$3"
fi
exec /usr/bin/stat "$@"
SH
chmod +x "${fake_bin}"/*

fake_signer="${test_root}/fake-signer"
cat >"${fake_signer}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"${TMPDIR}/fake-signer-argv.txt"
env | LC_ALL=C sort >"${TMPDIR}/fake-signer-environment.txt"
secret_dir="${CTX_MACOS_SIGNING_SECRET_DIR:?}"
[[ "$(stat -c '%a' "${secret_dir}")" == "700" ]]
[[ "$(find "${secret_dir}" -mindepth 1 -maxdepth 1 -type f | wc -l | tr -d ' ')" == "5" ]]
while IFS= read -r path; do
  [[ "$(stat -c '%a' "${path}")" == "600" ]]
done < <(find "${secret_dir}" -mindepth 1 -maxdepth 1 -type f)
SH
chmod +x "${fake_signer}"

fake_attester="${test_root}/fake-attester"
cat >"${fake_attester}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"${TMPDIR}/fake-attester-argv.txt"
env | LC_ALL=C sort >"${TMPDIR}/fake-attester-environment.txt"
secret_dir="${CTX_MACOS_SIGNING_SECRET_DIR:?}"
[[ "$(stat -c '%a' "${secret_dir}")" == "700" ]]
[[ "$(find "${secret_dir}" -mindepth 1 -maxdepth 1 -type f | wc -l | tr -d ' ')" == "2" ]]
for name in APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD; do
  [[ "$(stat -c '%a' "${secret_dir}/${name}")" == "600" ]]
done
SH
chmod +x "${fake_attester}"

unset BUILDKITE BUILDKITE_BRANCH BUILDKITE_COMMIT BUILDKITE_PULL_REQUEST
unset BUILDKITE_REPO BUILDKITE_TAG CI GITHUB_ACTIONS
export PATH="${fake_bin}:/usr/bin:/bin"
export TMPDIR="${test_root}/tmp"
export CTX_LOCAL_MACOS_SIGNING_LIVE_TEST=1
export CTX_TEST_ONLY_MACOS_HOST=Darwin
export CTX_MACOS_NOTARY_TIMEOUT=30m
export CTX_MACOS_SIGNING_SECRET_SOURCE=injected
export APPLE_CODESIGN_CERT_PASSWORD='password-secret-sentinel'
export APPLE_CODESIGN_CERT_P12_B64='cDEyLXNlY3JldC1zZW50aW5lbA=='
export NOTARY_ISSUER='issuer-test-value'
export NOTARY_KEY_ID='key-id-test-value'
export NOTARY_KEY_P8_B64='LS0tLS1CRUdJTiBQUklWQVRFIEtFWS0tLS0tCnA4LXNlY3JldC1zZW50aW5lbAotLS0tLUVORCBQUklWQVRFIEtFWS0tLS0tCg=='
export UNRELATED_SECRET='must-not-reach-signer'

launcher="${repo_root}/scripts/run-macos-release-signing.sh"
trust_gate="${repo_root}/scripts/check-macos-signing-trusted-ref.sh"
check_script="${repo_root}/scripts/check-macos-release-signing.sh"
attestation_check="${repo_root}/scripts/verify-macos-release-attestation.sh"
evidence_tool="${repo_root}/scripts/macos-release-signing-evidence.py"
execution_check="${repo_root}/scripts/verify-macos-signed-cli.sh"

new_artifact() {
  local name="$1"
  local path="${test_root}/${name}"
  cat >"${path}" <<'SH'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  printf '%s\n' 'ctx 0.25.0'
  exit 0
fi
exit 2
SH
  chmod 0755 "${path}"
  printf '%s\n' "${path}"
}

expect_failure() {
  local pattern="$1"
  local log="$2"
  shift 2
  if "$@" >"${log}" 2>&1; then
    fail "command unexpectedly succeeded: $*"
  fi
  grep -Fq "${pattern}" "${log}" || {
    sed -n '1,120p' "${log}" >&2
    fail "failure output did not contain: ${pattern}"
  }
}

trusted_infisical() {
  env \
    -u CTX_LOCAL_MACOS_SIGNING_LIVE_TEST \
    -u CTX_TEST_ONLY_MACOS_HOST \
    -u CI \
    -u GITHUB_ACTIONS \
    BUILDKITE=true \
    BUILDKITE_PULL_REQUEST=false \
    BUILDKITE_BRANCH=main \
    BUILDKITE_COMMIT=0000000000000000000000000000000000000001 \
    BUILDKITE_REPO=https://github.com/ctxrs/ctx.git \
    CTX_MACOS_SIGNING_SECRET_SOURCE=infisical \
    "$@"
}

"${trust_gate}" >/dev/null
expect_failure 'forbidden when CI is set' "${test_root}/local-ci-bypass.log" \
  env CI=1 "${trust_gate}"
expect_failure 'forbidden for Buildkite pull requests' "${test_root}/pr-gate.log" \
  env -u CTX_LOCAL_MACOS_SIGNING_LIVE_TEST \
    BUILDKITE=true BUILDKITE_PULL_REQUEST=42 BUILDKITE_BRANCH=main \
    BUILDKITE_REPO=https://github.com/ctxrs/ctx.git "${trust_gate}"
expect_failure 'restricted to the Buildkite main branch' "${test_root}/branch-gate.log" \
  env -u CTX_LOCAL_MACOS_SIGNING_LIVE_TEST \
    BUILDKITE=true BUILDKITE_PULL_REQUEST=false BUILDKITE_BRANCH=feature \
    BUILDKITE_REPO=https://github.com/ctxrs/ctx.git "${trust_gate}"
expect_failure 'BUILDKITE_COMMIT does not match' "${test_root}/commit-gate.log" \
  env -u CTX_LOCAL_MACOS_SIGNING_LIVE_TEST \
    BUILDKITE=true BUILDKITE_PULL_REQUEST=false BUILDKITE_BRANCH=main \
    BUILDKITE_COMMIT=0000000000000000000000000000000000000000 \
    BUILDKITE_REPO=https://github.com/ctxrs/ctx.git "${trust_gate}"

expect_failure 'categorically forbid Infisical' "${test_root}/local-infisical-source.log" \
  env CTX_MACOS_SIGNING_SECRET_SOURCE=infisical "${launcher}" --preflight
expect_failure 'forbid ambient Infisical authentication' "${test_root}/local-infisical-ambient.log" \
  env INFISICAL_TOKEN=ambient-auth-must-not-be-used "${launcher}" --preflight
grep -Fq 'ambient-auth-must-not-be-used' "${test_root}/local-infisical-ambient.log" \
  && fail "local Infisical rejection exposed ambient auth"

rm -f "${TMPDIR}/infisical.log"
"${launcher}" --preflight >"${test_root}/preflight.log" 2>&1
[[ ! -e "${TMPDIR}/infisical.log" ]] || fail "tool-only preflight accessed Infisical"
touch "${TMPDIR}/fake-openssl-missing-exclusive-flags"
expect_failure 'lacks required exclusive-trust flag -no-CAstore' \
  "${test_root}/openssl-exclusive-flags.log" "${launcher}" --preflight
rm -f "${TMPDIR}/fake-openssl-missing-exclusive-flags"

infisical_artifact="$(new_artifact infisical-cli)"
trusted_infisical "${launcher}" macos-arm64 cli "${infisical_artifact}" \
  "${test_root}/infisical-evidence" >/dev/null
[[ "$(wc -l < "${TMPDIR}/infisical.log" | tr -d ' ')" == "5" ]] || \
  fail "signing point did not fetch exactly five Infisical values"
while IFS='|' read -r name args; do
  case "${name}" in
    APPLE_CODESIGN_CERT_P12_B64|APPLE_CODESIGN_CERT_PASSWORD|NOTARY_ISSUER|NOTARY_KEY_ID|NOTARY_KEY_P8_B64) ;;
    *) fail "preflight fetched an unapproved Infisical value: ${name}" ;;
  esac
  [[ " ${args} " == *' --projectId 590927ab-758e-41b0-9e15-4cf070e87cf4 '* ]] || fail "missing pinned Infisical project"
  [[ " ${args} " == *' --env prod '* && " ${args} " == *' --path / '* ]] || fail "missing pinned Infisical env/path"
done <"${TMPDIR}/infisical.log"
for secret in password-secret-sentinel p12-secret-sentinel p8-secret-sentinel; do
  grep -Fq "${secret}" "${test_root}/preflight.log" && fail "preflight exposed ${secret}"
done

touch "${TMPDIR}/fake-infisical-auth-failure"
expect_failure 'Infisical lookup failed' "${test_root}/infisical-auth.log" \
  trusted_infisical "${launcher}" macos-arm64 cli \
    "$(new_artifact infisical-auth-failure)" "${test_root}/infisical-auth-evidence"
rm -f "${TMPDIR}/fake-infisical-auth-failure"
printf '%s' NOTARY_KEY_ID >"${TMPDIR}/fake-infisical-missing-name"
expect_failure 'NOTARY_KEY_ID' "${test_root}/infisical-missing.log" \
  trusted_infisical "${launcher}" macos-arm64 cli \
    "$(new_artifact infisical-missing)" "${test_root}/infisical-missing-evidence"
rm -f "${TMPDIR}/fake-infisical-missing-name"
mv "${fake_bin}/infisical" "${fake_bin}/infisical.off"
expect_failure 'missing required macOS signing tool: infisical' "${test_root}/infisical-tool.log" \
  trusted_infisical "${launcher}" macos-arm64 cli \
    "$(new_artifact infisical-tool)" "${test_root}/infisical-tool-evidence"
mv "${fake_bin}/infisical.off" "${fake_bin}/infisical"
mv "${fake_bin}/rcodesign" "${fake_bin}/rcodesign.off"
expect_failure 'missing required macOS signing tool: rcodesign' "${test_root}/required-tool.log" \
  "${launcher}" --preflight
mv "${fake_bin}/rcodesign.off" "${fake_bin}/rcodesign"

handoff_artifact="$(new_artifact handoff-cli)"
CTX_TEST_ONLY_MACOS_SIGNER_PATH="${fake_signer}" \
  "${launcher}" macos-arm64 cli "${handoff_artifact}" "${test_root}/handoff-evidence"
for handoff_file in "${TMPDIR}/fake-signer-argv.txt" "${TMPDIR}/fake-signer-environment.txt"; do
  for secret in \
    cDEyLXNlY3JldC1zZW50aW5lbA== \
    password-secret-sentinel \
    issuer-test-value \
    key-id-test-value \
    LS0tLS1CRUdJTiBQUklWQVRFIEtFWS0tLS0tCnA4LXNlY3JldC1zZW50aW5lbAotLS0tLUVORCBQUklWQVRFIEtFWS0tLS0tCg==; do
    grep -Fq "${secret}" "${handoff_file}" && fail "secret value reached fake signer argv/environment"
  done
  for secret_name in APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD NOTARY_ISSUER NOTARY_KEY_ID NOTARY_KEY_P8_B64; do
    grep -Fq "${secret_name}=" "${handoff_file}" && fail "secret variable reached fake signer environment"
  done
done

success_dir="${test_root}/success"
mkdir -p "${success_dir}"
success_artifact="$(new_artifact success-cli)"
rm -f "${TMPDIR}/tool-order.log"
"${launcher}" macos-arm64 cli "${success_artifact}" "${success_dir}" \
  >"${test_root}/success.log" 2>&1
if [[ -s "${TMPDIR}/last-signing-secret-dir.txt" ]]; then
  secret_dir="$(cat "${TMPDIR}/last-signing-secret-dir.txt")"
  [[ ! -e "${secret_dir}" ]] || fail "signing secret directory survived signing"
fi
"${execution_check}" macos-arm64 "${success_artifact}" 0.25.0 \
  "${success_dir}/ctx-macos-arm64.signing.json" >/dev/null
sha256sum "${success_artifact}" | awk '{print $1}' >"${success_artifact}.sha256"
"${check_script}" macos-arm64 cli "${success_artifact}" \
  "${success_dir}/ctx-macos-arm64.signing.json"
[[ "$(tr '\n' ' ' <"${TMPDIR}/tool-order.log")" == \
  'sign zip notary-submit attest ' ]] || fail "sign/notary/attestation ordering changed"
[[ ! -e "${TMPDIR}/spctl-was-invoked" ]] || \
  fail "spctl valid-but-not-app classification was treated as authoritative"

failed_execution_dir="${test_root}/failed-execution"
mkdir -p "${failed_execution_dir}"
failed_execution_artifact="$(new_artifact failed-execution-cli)"
sed -i 's/exit 0/exit 42/' "${failed_execution_artifact}"
"${launcher}" macos-arm64 cli "${failed_execution_artifact}" \
  "${failed_execution_dir}" >/dev/null
expect_failure 'exited with status 42' "${test_root}/failed-execution.log" \
  "${execution_check}" macos-arm64 "${failed_execution_artifact}" 0.25.0 \
    "${failed_execution_dir}/ctx-macos-arm64.signing.json"
grep -Fq 'UNRELATED_SECRET=' "${TMPDIR}/signer-environment.txt" && fail "unrelated secret reached signer"
grep -Fq 'INFISICAL_' "${TMPDIR}/signer-environment.txt" && fail "Infisical auth/config reached signer"
grep -Fq 'CTX_MACOS_SIGNING_SECRET_DIR=' "${TMPDIR}/signer-environment.txt" || \
  fail "signer did not receive the secure secret directory path"
for forbidden in APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD NOTARY_ISSUER NOTARY_KEY_ID NOTARY_KEY_P8_B64; do
  grep -Fq "${forbidden}=" "${TMPDIR}/signer-environment.txt" \
    && fail "${forbidden} value reached signer environment"
done
grep -Fq -- '--p12-password-file ' "${TMPDIR}/rcodesign-argv.txt" || \
  fail "rcodesign did not receive the P12 password as a file path"
grep -Fq -- '--key ' "${TMPDIR}/notarytool-argv.txt" || \
  fail "notarytool did not receive the P8 as a file path"
for secret in password-secret-sentinel p12-secret-sentinel p8-secret-sentinel; do
  grep -Fq "${secret}" "${TMPDIR}/signer-environment.txt" \
    "${TMPDIR}/rcodesign-argv.txt" "${TMPDIR}/notarytool-argv.txt" \
    "${TMPDIR}/notarytool-environment.txt" \
    && fail "secret value reached signer/tool argv or environment: ${secret}"
done

touch "${TMPDIR}/fake-missing-runtime"
expect_failure 'missing hardened runtime flags' "${test_root}/missing-runtime-signer.log" \
  "${launcher}" macos-arm64 cli "$(new_artifact missing-runtime-signer)" \
    "${test_root}/missing-runtime-signer-evidence"
expect_failure 'artifact is missing hardened runtime flags' \
  "${test_root}/missing-runtime-checker.log" \
  "${check_script}" macos-arm64 cli "${success_artifact}" \
    "${success_dir}/ctx-macos-arm64.signing.json"
rm -f "${TMPDIR}/fake-missing-runtime"

missing_runtime_details="${test_root}/missing-runtime.codesign.txt"
sed 's/flags=0x10000(runtime)/flags=0x0(none)/' \
  "${success_dir}/ctx-macos-arm64.codesign.txt" >"${missing_runtime_details}"
expect_failure 'runtime in CodeDirectory flags' "${test_root}/missing-runtime-evidence.log" \
  python3 "${evidence_tool}" write \
    --output "${test_root}/missing-runtime.signing.json" \
    --platform macos-arm64 --kind cli --artifact "${success_artifact}" \
    --codesign-details "${missing_runtime_details}" \
    --notary-submit "${success_dir}/ctx-macos-arm64.notary-submit.json"

touch "${TMPDIR}/fake-wrong-eku"
expect_failure 'certificate lacks the Code Signing EKU' "${test_root}/wrong-eku-signing.log" \
  "${launcher}" macos-arm64 cli "$(new_artifact wrong-eku)" "${test_root}/wrong-eku-evidence"
expect_failure 'signer certificate lacks the Code Signing EKU' "${test_root}/wrong-eku-cms.log" \
  "${attestation_check}" macos-arm64 cli "${success_artifact}" \
    "${success_dir}/ctx-macos-arm64.attestation.json" \
    "${success_dir}/ctx-macos-arm64.attestation.cms"
rm -f "${TMPDIR}/fake-wrong-eku"

touch "${TMPDIR}/fake-host-trust-only-certificate"
expect_failure 'chain exclusively' "${test_root}/host-store-leaf-bypass.log" \
  "${launcher}" macos-arm64 cli "$(new_artifact host-store-leaf)" \
    "${test_root}/host-store-leaf-evidence"
expect_failure 'CMS signature verification failed' "${test_root}/host-store-cms-bypass.log" \
  "${attestation_check}" macos-arm64 cli "${success_artifact}" \
    "${success_dir}/ctx-macos-arm64.attestation.json" \
    "${success_dir}/ctx-macos-arm64.attestation.cms"
rm -f "${TMPDIR}/fake-host-trust-only-certificate"

for missing in APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD NOTARY_ISSUER NOTARY_KEY_ID NOTARY_KEY_P8_B64; do
  artifact="$(new_artifact "missing-${missing}")"
  expect_failure "missing required injected macOS signing value ${missing}" "${test_root}/missing-${missing}.log" \
    env -u "${missing}" "${launcher}" macos-arm64 cli "${artifact}" "${test_root}/missing-evidence"
done

touch "${TMPDIR}/fake-coherent-wrong-identity"
artifact="$(new_artifact wrong-identity)"
expect_failure 'not the pinned ctx Developer ID identity' "${test_root}/wrong-identity.log" \
  "${launcher}" macos-arm64 cli "${artifact}" "${test_root}/wrong-identity-evidence"
rm -f "${TMPDIR}/fake-coherent-wrong-identity"

touch "${TMPDIR}/fake-sign-failure"
artifact="$(new_artifact sign-failure)"
expect_failure 'Developer ID signing failed' "${test_root}/sign-failure.log" \
  "${launcher}" macos-arm64 cli "${artifact}" "${test_root}/sign-failure-evidence"
rm -f "${TMPDIR}/fake-sign-failure"

printf '%s' rejected >"${TMPDIR}/fake-notary-result"
artifact="$(new_artifact rejected)"
expect_failure 'status Invalid' "${test_root}/rejected.log" \
  "${launcher}" macos-arm64 cli "${artifact}" "${test_root}/rejected-evidence"
[[ -s "${test_root}/rejected-evidence/ctx-macos-arm64.notary-log.json" ]] || fail "rejection log missing"
printf '%s' timeout >"${TMPDIR}/fake-notary-result"
rm -f "${TMPDIR}/tool-order.log"
artifact="$(new_artifact timeout)"
expect_failure 'timed out after 30m' "${test_root}/timeout.log" \
  "${launcher}" macos-arm64 cli "${artifact}" "${test_root}/timeout-evidence"
[[ -s "${test_root}/timeout-evidence/ctx-macos-arm64.notary-submit.stderr" ]] || fail "timeout stderr missing"
[[ "$(tr '\n' ' ' <"${TMPDIR}/tool-order.log")" == 'sign zip notary-submit ' ]] || \
  fail "timeout did not stop before post-notary verification/attestation"
rm -f "${TMPDIR}/fake-notary-result"

touch "${TMPDIR}/fake-mutate-after-sign"
artifact="$(new_artifact mutation)"
expect_failure 'mutated after Developer ID signing' "${test_root}/mutation.log" \
  "${launcher}" macos-arm64 cli "${artifact}" "${test_root}/mutation-evidence"
rm -f "${TMPDIR}/fake-mutate-after-sign"

ordering_dir="${test_root}/ordering"
mkdir -p "${ordering_dir}"
artifact="$(new_artifact ordering-cli)"
sha256sum "${artifact}" | awk '{print $1}' >"${artifact}.sha256"
"${launcher}" macos-x64 cli "${artifact}" "${ordering_dir}" >/dev/null
"${execution_check}" macos-x64 "${artifact}" 0.25.0 \
  "${ordering_dir}/ctx-macos-x64.signing.json" >/dev/null
expect_failure 'signed artifact checksum mismatch' "${test_root}/ordering.log" \
  "${check_script}" macos-x64 cli "${artifact}" "${ordering_dir}/ctx-macos-x64.signing.json"
sha256sum "${artifact}" | awk '{print $1}' >"${artifact}.sha256"
"${check_script}" macos-x64 cli "${artifact}" "${ordering_dir}/ctx-macos-x64.signing.json"

runtime_dir="${test_root}/runtime"
mkdir -p "${runtime_dir}/package/lib"
runtime="$(new_artifact libonnxruntime.dylib)"
"${launcher}" macos-x64 runtime "${runtime}" "${runtime_dir}" >/dev/null
cp "${runtime}" "${runtime_dir}/package/lib/libonnxruntime.dylib"
runtime_archive="${runtime_dir}/ctx-onnxruntime-macos-x64.tar.gz"
tar -czf "${runtime_archive}" -C "${runtime_dir}/package" lib/libonnxruntime.dylib
sha256sum "${runtime_archive}" | awk '{print $1}' >"${runtime_archive}.sha256"
runtime_evidence="${runtime_dir}/ctx-onnxruntime-macos-x64.signing.json"
python3 "${evidence_tool}" bind-archive \
  --evidence "${runtime_evidence}" --platform macos-x64 \
  --archive "${runtime_archive}" --checksum "${runtime_archive}.sha256" \
  --nested-artifact "${runtime}" --role release
"${check_script}" macos-x64 runtime "${runtime_archive}" "${runtime_evidence}"

CTX_TEST_ONLY_MACOS_ATTESTER_PATH="${fake_attester}" \
  "${launcher}" --attest-runtime-archive macos-x64 "${runtime_archive}" \
    "${runtime_dir}/package/lib/libonnxruntime.dylib" "${runtime_dir}"
for handoff_file in "${TMPDIR}/fake-attester-argv.txt" "${TMPDIR}/fake-attester-environment.txt"; do
  for secret in \
    cDEyLXNlY3JldC1zZW50aW5lbA== \
    password-secret-sentinel \
    issuer-test-value \
    key-id-test-value \
    LS0tLS1CRUdJTiBQUklWQVRFIEtFWS0tLS0tCnA4LXNlY3JldC1zZW50aW5lbAotLS0tLUVORCBQUklWQVRFIEtFWS0tLS0tCg==; do
    grep -Fq "${secret}" "${handoff_file}" \
      && fail "secret value reached final-archive attester argv/environment"
  done
  for secret_name in APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD NOTARY_ISSUER NOTARY_KEY_ID NOTARY_KEY_P8_B64; do
    grep -Fq "${secret_name}=" "${handoff_file}" \
      && fail "secret variable reached final-archive attester environment"
  done
done

rm -f "${TMPDIR}/infisical.log"
trusted_infisical "${launcher}" --attest-runtime-archive macos-x64 \
  "${runtime_archive}" "${runtime_dir}/package/lib/libonnxruntime.dylib" \
  "${runtime_dir}" >/dev/null
[[ "$(wc -l < "${TMPDIR}/infisical.log" | tr -d ' ')" == "2" ]] || \
  fail "final archive attestation did not fetch exactly two Infisical values"
cut -d '|' -f 1 "${TMPDIR}/infisical.log" \
  | cmp - <(printf '%s\n' APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD) \
  || fail "final archive attestation fetched non-P12 credentials"
release_statement="${runtime_dir}/ctx-onnxruntime-macos-x64.release-attestation.json"
release_cms="${runtime_dir}/ctx-onnxruntime-macos-x64.release-attestation.cms"
"${attestation_check}" --runtime-archive macos-x64 "${runtime_archive}" \
  "${runtime_dir}/package/lib/libonnxruntime.dylib" "${release_statement}" "${release_cms}"

cp "${runtime_archive}" "${runtime_archive}.authorized"
printf '%s\n' repackaged >>"${runtime_archive}"
expect_failure 'does not bind the exact release archive' "${test_root}/archive-repack.log" \
  "${attestation_check}" --runtime-archive macos-x64 "${runtime_archive}" \
    "${runtime_dir}/package/lib/libonnxruntime.dylib" "${release_statement}" "${release_cms}"
mv "${runtime_archive}.authorized" "${runtime_archive}"

for field in role provenance source_commit; do
  altered_statement="${runtime_dir}/altered-${field}.release-attestation.json"
  altered_cms="${runtime_dir}/altered-${field}.cms"
  cp "${runtime_dir}/ctx-onnxruntime-macos-x64.notary-submit.json" \
    "${runtime_dir}/altered-${field}.notary-submit.json"
  python3 - "${release_statement}" "${altered_statement}" "${field}" <<'PY'
import json
import sys
source, output, field = sys.argv[1:]
with open(source, encoding="utf-8") as stream:
    value = json.load(stream)
value[field] = {
    "role": "builder",
    "provenance": "non-native-repack",
    "source_commit": "f" * 40,
}[field]
with open(output, "w", encoding="utf-8") as stream:
    json.dump(value, stream, sort_keys=True, separators=(",", ":"))
    stream.write("\n")
PY
  openssl cms -sign -binary -in "${altered_statement}" \
    -signer ignored -inkey ignored -outform DER -out "${altered_cms}" -noattr
  expect_failure 'does not bind the exact release archive' \
    "${test_root}/archive-${field}.log" \
    "${attestation_check}" --runtime-archive macos-x64 "${runtime_archive}" \
      "${runtime_dir}/package/lib/libonnxruntime.dylib" "${altered_statement}" "${altered_cms}"
done

substitution_dir="${test_root}/substitution"
mkdir -p "${substitution_dir}"
substituted="${substitution_dir}/ctx-macos-arm64"
cp "${success_artifact}" "${substituted}"
cp "${success_dir}/ctx-macos-arm64."* "${substitution_dir}/"
printf '%s\n' 'substituted executable bytes' >>"${substituted}"
sha256sum "${substituted}" | awk '{print $1}' >"${substituted}.sha256"
python3 - "${substitution_dir}/ctx-macos-arm64.signing.json" "${substituted}" <<'PY'
import hashlib
import json
import sys
evidence_path, artifact = sys.argv[1:]
with open(evidence_path, encoding="utf-8") as source:
    evidence = json.load(source)
with open(artifact, "rb") as source:
    evidence["artifact_sha256"] = hashlib.sha256(source.read()).hexdigest()
with open(evidence_path, "w", encoding="utf-8") as output:
    json.dump(evidence, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
python3 "${evidence_tool}" create-attestation \
  --output "${substitution_dir}/ctx-macos-arm64.attestation.json" \
  --platform macos-arm64 --kind cli --artifact "${substituted}" \
  --notary-submit "${substitution_dir}/ctx-macos-arm64.notary-submit.json" \
  --source-commit "$(git -C "${repo_root}" rev-parse HEAD)"
expect_failure 'CMS signature verification failed' "${test_root}/substitution.log" \
  "${attestation_check}" macos-arm64 cli "${substituted}" \
  "${substitution_dir}/ctx-macos-arm64.attestation.json" \
  "${substitution_dir}/ctx-macos-arm64.attestation.cms"

touch "${TMPDIR}/fake-coherent-wrong-identity"
coherent_evidence="${test_root}/coherent-wrong.json"
cp "${success_dir}/ctx-macos-arm64.signing.json" "${coherent_evidence}"
python3 - "${coherent_evidence}" <<'PY'
import json
import sys
path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["codesign"]["authority"] = "Developer ID Application: Other Corp (OTHERTEAM)"
value["codesign"]["team_identifier"] = "OTHERTEAM"
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
expect_failure 'pinned ctx Apple authority' "${test_root}/coherent-wrong.log" \
  python3 "${evidence_tool}" verify-artifact \
    --evidence "${coherent_evidence}" --platform macos-arm64 --kind cli \
    --artifact "${success_artifact}" --checksum "${success_artifact}.sha256"
rm -f "${TMPDIR}/fake-coherent-wrong-identity"

printf '%s\n' 'unsigned nested dylib' >"${runtime_dir}/package/lib/libonnxruntime.dylib"
tar -czf "${runtime_archive}" -C "${runtime_dir}/package" lib/libonnxruntime.dylib
sha256sum "${runtime_archive}" | awk '{print $1}' >"${runtime_archive}.sha256"
python3 - "${runtime_evidence}" "${runtime_archive}" "${runtime_dir}/package/lib/libonnxruntime.dylib" <<'PY'
import hashlib
import json
import sys
evidence_path, archive, nested = sys.argv[1:]
with open(evidence_path, encoding="utf-8") as source:
    evidence = json.load(source)
with open(archive, "rb") as source:
    archive_sha = hashlib.sha256(source.read()).hexdigest()
with open(nested, "rb") as source:
    nested_sha = hashlib.sha256(source.read()).hexdigest()
evidence["artifact_sha256"] = nested_sha
evidence["packages"] = [{"archive_name": archive.rsplit("/", 1)[-1], "archive_sha256": archive_sha, "nested_artifact_sha256": nested_sha, "role": "release"}]
with open(evidence_path, "w", encoding="utf-8") as output:
    json.dump(evidence, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
expect_failure 'signed macOS attestation does not bind' "${test_root}/unsigned-nested.log" \
  "${check_script}" macos-x64 runtime "${runtime_archive}" "${runtime_evidence}"

if /usr/bin/python3 -c 'import cryptography' >/dev/null 2>&1 \
  && /usr/bin/openssl version | grep -Fq 'OpenSSL 3'; then
  decoy_root="${test_root}/real-signer-decoy"
  mkdir -p "${decoy_root}/scripts"
  cp "${attestation_check}" "${decoy_root}/scripts/verify-macos-release-attestation.sh"
  cp "${evidence_tool}" "${decoy_root}/scripts/macos-release-signing-evidence.py"
  /usr/bin/python3 - "${decoy_root}" <<'PY'
import datetime
import sys
import warnings
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID, ObjectIdentifier

root = Path(sys.argv[1])
now = datetime.datetime.now(datetime.timezone.utc)
warnings.filterwarnings("ignore", message="Attribute's length must be")


def key():
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def write_key(path, value):
    path.write_bytes(
        value.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.TraditionalOpenSSL,
            serialization.NoEncryption(),
        )
    )


ca_key = key()
ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "ctx test signer CA")])
ca_cert = (
    x509.CertificateBuilder()
    .subject_name(ca_name)
    .issuer_name(ca_name)
    .public_key(ca_key.public_key())
    .serial_number(x509.random_serial_number())
    .not_valid_before(now - datetime.timedelta(days=1))
    .not_valid_after(now + datetime.timedelta(days=1))
    .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
    .sign(ca_key, hashes.SHA256())
)


def leaf(name, common_name, team, eku=ExtendedKeyUsageOID.CODE_SIGNING, digital_signature=True):
    value_key = key()
    subject = x509.Name(
        [
            x509.NameAttribute(NameOID.ORGANIZATIONAL_UNIT_NAME, team),
            x509.NameAttribute(NameOID.COMMON_NAME, common_name, _validate=False),
        ]
    )
    value_cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(ca_name)
        .public_key(value_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .add_extension(x509.ExtendedKeyUsage([eku]), critical=False)
        .add_extension(
            x509.KeyUsage(
                digital_signature=digital_signature,
                content_commitment=False,
                key_encipherment=not digital_signature,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=False,
                crl_sign=False,
                encipher_only=None,
                decipher_only=None,
            ),
            critical=True,
        )
        .add_extension(
            x509.UnrecognizedExtension(
                ObjectIdentifier("1.2.840.113635.100.6.1.13"), b"\x05\x00"
            ),
            critical=True,
        )
        .sign(ca_key, hashes.SHA256())
    )
    write_key(root / f"{name}.key", value_key)
    (root / f"{name}.pem").write_bytes(value_cert.public_bytes(serialization.Encoding.PEM))


write_key(root / "ca.key", ca_key)
(root / "scripts" / "apple-developer-id-g2-ca.pem").write_bytes(
    ca_cert.public_bytes(serialization.Encoding.PEM)
)
leaf("wrong", "Developer ID Application: Wrong Signer (WRONGTEAM)", "WRONGTEAM")
leaf(
    "decoy",
    "Developer ID Application: Profound Health Institute LLC (SJSNARH4TG)",
    "SJSNARH4TG",
)
leaf(
    "wrong-eku",
    "Developer ID Application: Profound Health Institute LLC (SJSNARH4TG)",
    "SJSNARH4TG",
    ExtendedKeyUsageOID.SERVER_AUTH,
)
leaf(
    "wrong-key-usage",
    "Developer ID Application: Profound Health Institute LLC (SJSNARH4TG)",
    "SJSNARH4TG",
    digital_signature=False,
)
PY
  decoy_ca_fingerprint="$(/usr/bin/openssl x509 \
    -in "${decoy_root}/scripts/apple-developer-id-g2-ca.pem" \
    -noout -fingerprint -sha256 | sed 's/^.*Fingerprint=//')"
  if /usr/bin/openssl verify -purpose any -partial_chain -no-CApath -no-CAstore \
    -CAfile "${decoy_root}/scripts/apple-developer-id-g2-ca.pem" \
    "${decoy_root}/decoy.pem" >/dev/null 2>&1; then
    fail "real Apple-shaped certificate unexpectedly verified without -ignore_critical"
  fi
  /usr/bin/openssl verify -purpose any -partial_chain -no-CApath -no-CAstore \
    -ignore_critical \
    -CAfile "${decoy_root}/scripts/apple-developer-id-g2-ca.pem" \
    "${decoy_root}/decoy.pem" >/dev/null 2>&1 \
    || fail "real Apple-shaped certificate did not verify with -ignore_critical"
  /usr/bin/python3 - \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
    "${decoy_ca_fingerprint}" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
source = path.read_text(encoding="utf-8")
source = re.sub(
    r'^EXPECTED_CA_SHA256="[^"]+"$',
    f'EXPECTED_CA_SHA256="{sys.argv[2]}"',
    source,
    count=1,
    flags=re.MULTILINE,
)
path.write_text(source, encoding="utf-8")
PY
  chmod +x "${decoy_root}/scripts/verify-macos-release-attestation.sh"
  /usr/bin/git -C "${decoy_root}" init -q
  /usr/bin/git -C "${decoy_root}" config user.email test@ctx.rs
  /usr/bin/git -C "${decoy_root}" config user.name ctx-test
  /usr/bin/git -C "${decoy_root}" add scripts
  /usr/bin/git -C "${decoy_root}" commit -qm init
  decoy_commit="$(/usr/bin/git -C "${decoy_root}" rev-parse HEAD)"

  decoy_cli="${decoy_root}/ctx-macos-arm64"
  decoy_cli_statement="${decoy_root}/ctx-macos-arm64.attestation.json"
  decoy_cli_cms="${decoy_root}/ctx-macos-arm64.attestation.cms"
  decoy_cli_notary="${decoy_root}/ctx-macos-arm64.notary-submit.json"
  printf '%s\n' 'real decoy CMS executable' >"${decoy_cli}"
  printf '%s\n' '{"id":"real-decoy-cli","status":"Accepted"}' >"${decoy_cli_notary}"
  /usr/bin/python3 "${decoy_root}/scripts/macos-release-signing-evidence.py" \
    create-attestation --output "${decoy_cli_statement}" --platform macos-arm64 \
    --kind cli --artifact "${decoy_cli}" --notary-submit "${decoy_cli_notary}" \
    --source-commit "${decoy_commit}"
  decoy_valid_cms="${decoy_root}/ctx-macos-arm64.valid-attestation.cms"
  /usr/bin/openssl cms -sign -binary -in "${decoy_cli_statement}" \
    -signer "${decoy_root}/decoy.pem" -inkey "${decoy_root}/decoy.key" \
    -outform DER -out "${decoy_valid_cms}" -md sha256 -noattr >/dev/null 2>&1
  env PATH=/usr/bin:/bin \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
      macos-arm64 cli "${decoy_cli}" "${decoy_cli_statement}" "${decoy_valid_cms}" \
      >/dev/null
  env PATH=/usr/bin:/bin CTX_MACOS_RELEASE_SOURCE_COMMIT="${decoy_commit}" \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
      macos-arm64 cli "${decoy_cli}" "${decoy_cli_statement}" "${decoy_valid_cms}" \
      >/dev/null
  expect_failure 'source commit must be a non-placeholder' \
    "${test_root}/real-decoy-invalid-source-commit.log" \
    env PATH=/usr/bin:/bin CTX_MACOS_RELEASE_SOURCE_COMMIT=not-a-commit \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
      macos-arm64 cli "${decoy_cli}" "${decoy_cli_statement}" "${decoy_valid_cms}"
  expect_failure 'signed macOS attestation does not bind the exact pinned artifact' \
    "${test_root}/real-decoy-wrong-source-commit.log" \
    env PATH=/usr/bin:/bin \
    CTX_MACOS_RELEASE_SOURCE_COMMIT=1111111111111111111111111111111111111111 \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
      macos-arm64 cli "${decoy_cli}" "${decoy_cli_statement}" "${decoy_valid_cms}"
  /usr/bin/openssl cms -sign -binary -in "${decoy_cli_statement}" \
    -signer "${decoy_root}/wrong.pem" -inkey "${decoy_root}/wrong.key" \
    -certfile "${decoy_root}/decoy.pem" -outform DER -out "${decoy_cli_cms}" \
    -md sha256 -noattr >/dev/null 2>&1
  expect_failure 'actual signer does not have the pinned ctx Apple authority' \
    "${test_root}/real-decoy-cli.log" env PATH=/usr/bin:/bin \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
      macos-arm64 cli "${decoy_cli}" "${decoy_cli_statement}" "${decoy_cli_cms}"

  for profile in wrong-eku wrong-key-usage; do
    profile_cms="${decoy_root}/ctx-macos-arm64.${profile}.cms"
    /usr/bin/openssl cms -sign -binary -in "${decoy_cli_statement}" \
      -signer "${decoy_root}/${profile}.pem" \
      -inkey "${decoy_root}/${profile}.key" \
      -outform DER -out "${profile_cms}" -md sha256 -noattr >/dev/null 2>&1
    if [[ "${profile}" == "wrong-eku" ]]; then
      profile_failure='actual signer certificate lacks the Code Signing EKU'
    else
      profile_failure='actual signer lacks critical Digital Signature key usage'
    fi
    expect_failure "${profile_failure}" "${test_root}/real-${profile}.log" \
      env PATH=/usr/bin:/bin \
      "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
        macos-arm64 cli "${decoy_cli}" "${decoy_cli_statement}" "${profile_cms}"
  done

  decoy_archive="${decoy_root}/ctx-onnxruntime-macos-arm64.tar.gz"
  decoy_nested="${decoy_root}/libonnxruntime.dylib"
  decoy_archive_statement="${decoy_root}/ctx-onnxruntime-macos-arm64.release-attestation.json"
  decoy_archive_cms="${decoy_root}/ctx-onnxruntime-macos-arm64.release-attestation.cms"
  decoy_archive_notary="${decoy_root}/ctx-onnxruntime-macos-arm64.notary-submit.json"
  printf '%s\n' 'real decoy CMS archive' >"${decoy_archive}"
  printf '%s\n' 'real decoy CMS dylib' >"${decoy_nested}"
  printf '%s\n' '{"id":"real-decoy-runtime","status":"Accepted"}' >"${decoy_archive_notary}"
  /usr/bin/python3 "${decoy_root}/scripts/macos-release-signing-evidence.py" \
    create-runtime-archive-attestation --output "${decoy_archive_statement}" \
    --platform macos-arm64 --archive "${decoy_archive}" \
    --nested-artifact "${decoy_nested}" --notary-submit "${decoy_archive_notary}" \
    --source-commit "${decoy_commit}"
  /usr/bin/openssl cms -sign -binary -in "${decoy_archive_statement}" \
    -signer "${decoy_root}/wrong.pem" -inkey "${decoy_root}/wrong.key" \
    -certfile "${decoy_root}/decoy.pem" -outform DER -out "${decoy_archive_cms}" \
    -md sha256 -noattr >/dev/null 2>&1
  expect_failure 'actual signer does not have the pinned ctx Apple authority' \
    "${test_root}/real-decoy-archive.log" env PATH=/usr/bin:/bin \
    "${decoy_root}/scripts/verify-macos-release-attestation.sh" \
      --runtime-archive macos-arm64 "${decoy_archive}" "${decoy_nested}" \
      "${decoy_archive_statement}" "${decoy_archive_cms}"
fi

if [[ -x /usr/bin/openssl ]] && /usr/bin/openssl cms -help >/dev/null 2>&1; then
  forged_root="${test_root}/self-signed-forgery"
  mkdir -p "${forged_root}"
  forged_artifact="${forged_root}/ctx-macos-arm64"
  forged_statement="${forged_root}/ctx-macos-arm64.attestation.json"
  forged_cms="${forged_root}/ctx-macos-arm64.attestation.cms"
  forged_notary="${forged_root}/ctx-macos-arm64.notary-submit.json"
  printf '%s\n' 'self-signed coherent substitution' >"${forged_artifact}"
  printf '%s\n' '{"id":"forged","status":"Accepted"}' >"${forged_notary}"
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
    -subj '/CN=Developer ID Application: Other Corp (OTHERTEAM)/OU=OTHERTEAM' \
    -keyout "${forged_root}/key.pem" -out "${forged_root}/cert.pem" \
    >/dev/null 2>&1
  python3 "${evidence_tool}" create-attestation \
    --output "${forged_statement}" --platform macos-arm64 --kind cli \
    --artifact "${forged_artifact}" \
    --notary-submit "${forged_notary}" \
    --source-commit "$(git -C "${repo_root}" rev-parse HEAD)"
  /usr/bin/openssl cms -sign -binary -in "${forged_statement}" \
    -signer "${forged_root}/cert.pem" -inkey "${forged_root}/key.pem" \
    -outform DER -out "${forged_cms}" -md sha256 -noattr >/dev/null 2>&1
  expect_failure 'CMS signature verification failed' "${test_root}/self-signed-forgery.log" \
    env PATH=/usr/bin:/bin TMPDIR="${TMPDIR}" \
    "${attestation_check}" macos-arm64 cli "${forged_artifact}" \
    "${forged_statement}" "${forged_cms}"
fi

find "${test_root}/tmp" -maxdepth 1 \
  \( -name 'ctx-macos-signing-*' -o -name 'ctx-macos-runtime-attestation.*' \) \
  -print -quit \
  | grep -q . && fail "secret temporary directory was not removed"
for secret in password-secret-sentinel p12-secret-sentinel p8-secret-sentinel; do
  grep -R -Fq "${secret}" "${test_root}" \
    --exclude='infisical' \
    && fail "secret value appeared outside the scrubbed signer environment: ${secret}"
done

printf 'macOS release signing contract tests passed\n'
