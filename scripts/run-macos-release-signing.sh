#!/usr/bin/env bash
set -euo pipefail
case "$-" in
  *x*) set +x ;;
esac

INFISICAL_PROJECT_ID="590927ab-758e-41b0-9e15-4cf070e87cf4"
INFISICAL_ENVIRONMENT="prod"
INFISICAL_SECRET_PATH="/"

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/run-macos-release-signing.sh --preflight
  scripts/run-macos-release-signing.sh PLATFORM KIND ARTIFACT [EVIDENCE_DIR]
  scripts/run-macos-release-signing.sh --attest-runtime-archive PLATFORM ARCHIVE NESTED_DYLIB [EVIDENCE_DIR]

Runs a tool-only trusted release preflight, signs/notarizes one Mach-O using five
protected secret files, or authorizes a final runtime archive using only the
Developer ID P12 and password files. KIND is cli, helper, or runtime. The
helper kind accepts only the canonical ctx-pro executable and requires an
explicit CTX_MACOS_RELEASE_SOURCE_COMMIT.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required macOS signing tool: $1"
}

require_openssl3_exclusive_trust() {
  local version verify_help cms_help
  version="$(openssl version 2>/dev/null || true)"
  [[ "${version}" == OpenSSL\ 3.* ]] || \
    die "macOS signing requires OpenSSL 3"
  verify_help="$(openssl verify -help 2>&1 || true)"
  cms_help="$(openssl cms -help 2>&1 || true)"
  for flag in -no-CApath -no-CAstore -ignore_critical; do
    [[ "${verify_help}" == *"${flag}"* && "${cms_help}" == *"${flag}"* ]] || \
      die "selected OpenSSL 3 lacks required exclusive-trust flag ${flag}"
  done
}

mode=sign
case "${1:-}" in
  --preflight)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    mode=preflight
    ;;
  --attest-runtime-archive)
    [[ $# -ge 4 && $# -le 5 ]] || { usage; exit 2; }
    mode=archive_attestation
    platform="$2"
    artifact="$3"
    nested_artifact="$4"
    evidence_dir="${5:-target/public-cli-artifacts}"
    ;;
  *)
    platform="${1:-}"
    kind="${2:-}"
    artifact="${3:-}"
    evidence_dir="${4:-target/public-cli-artifacts}"
    [[ -n "${platform}" && -n "${kind}" && -n "${artifact}" && $# -le 4 ]] || {
      usage
      exit 2
    }
    ;;
esac

helper_source_commit=""
if [[ "${mode}" == "sign" ]]; then
  case "${platform}" in
    macos-arm64|macos-x64) ;;
    *) die "unsupported macOS signing platform: ${platform}" ;;
  esac
  case "${kind}" in
    cli|runtime) ;;
    helper)
      [[ "${artifact##*/}" == "ctx-pro-${platform}" ]] || \
        die "macOS helper artifact must be named ctx-pro-${platform}"
      [[ -f "${artifact}" && ! -L "${artifact}" && -x "${artifact}" ]] || \
        die "macOS helper artifact must be an executable regular non-symlink file"
      helper_source_commit="${CTX_MACOS_RELEASE_SOURCE_COMMIT:-}"
      [[ "${helper_source_commit}" =~ ^[0-9a-f]{40}$ \
        && ! "${helper_source_commit}" =~ ^0{40}$ ]] || \
        die "macOS helper signing requires an explicit non-placeholder 40-character CTX_MACOS_RELEASE_SOURCE_COMMIT"
      ;;
    *) die "unsupported macOS signing artifact kind: ${kind}" ;;
  esac
fi

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"${root_dir}/scripts/check-macos-signing-trusted-ref.sh" >/dev/null
host_system="$(uname -s)"
if [[ "${host_system}" != "Darwin" && "${host_system}" != "Linux" ]]; then
  die "macOS release signing requires Linux or Darwin"
fi

secret_source="${CTX_MACOS_SIGNING_SECRET_SOURCE:-}"
if [[ -z "${secret_source}" ]]; then
  secret_source=infisical
fi
case "${secret_source}" in
  infisical) ;;
  injected) ;;
  *) die "CTX_MACOS_SIGNING_SECRET_SOURCE must be infisical or injected" ;;
esac

for command_name in base64 find git openssl python3 stat; do
  require_command "${command_name}"
done
require_openssl3_exclusive_trust
if [[ "${mode}" == "sign" || "${mode}" == "preflight" ]]; then
  require_command rcodesign
  if [[ "${host_system}" == "Linux" ]]; then
    require_command zip
  fi
  if [[ "${host_system}" == "Darwin" ]]; then
    for command_name in codesign ditto xcode-select xcrun; do
      require_command "${command_name}"
    done
    xcode-select -p >/dev/null 2>&1 || die "xcode-select has no active developer directory"
    xcrun notarytool --version >/dev/null 2>&1 || die "xcrun notarytool is unavailable"
  fi
  rcodesign --version >/dev/null 2>&1 || die "rcodesign version check failed"
fi

if [[ "${mode}" == "preflight" ]]; then
  printf 'macOS signing preflight ok: trusted ref, %s tools, and exclusive OpenSSL 3 trust\n' "${host_system}"
  exit 0
fi

if [[ "${mode}" == "archive_attestation" ]]; then
  secret_names=(APPLE_CODESIGN_CERT_P12_B64 APPLE_CODESIGN_CERT_PASSWORD)
  worker_path="${root_dir}/scripts/attest-macos-runtime-release-archive.sh"
  test_worker_variable=CTX_TEST_ONLY_MACOS_ATTESTER_PATH
else
  secret_names=(
    APPLE_CODESIGN_CERT_P12_B64
    APPLE_CODESIGN_CERT_PASSWORD
    NOTARY_ISSUER
    NOTARY_KEY_ID
    NOTARY_KEY_P8_B64
  )
  worker_path="${root_dir}/scripts/sign-notarize-macos-release-artifact.sh"
  test_worker_variable=CTX_TEST_ONLY_MACOS_SIGNER_PATH
fi

if [[ "${secret_source}" == "infisical" ]]; then
  require_command infisical
  infisical --version >/dev/null 2>&1 || die "Infisical CLI version check failed"
fi

umask 077
secret_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signing-launcher.XXXXXX")"
chmod 00700 "${secret_root}"
cleanup() {
  rm -rf "${secret_root}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fetch_secret() {
  local name="$1"
  local output="${secret_root}/${name}"
  local diagnostic="${secret_root}/${name}.stderr"

  case "${secret_source}" in
    infisical)
      if ! infisical secrets get "${name}" \
        --plain \
        --projectId "${INFISICAL_PROJECT_ID}" \
        --env "${INFISICAL_ENVIRONMENT}" \
        --path "${INFISICAL_SECRET_PATH}" \
        >"${output}" 2>"${diagnostic}"; then
        die "Infisical lookup failed for required macOS signing value ${name}"
      fi
      # Infisical appends one transport LF; rcodesign reads this file byte-for-byte.
      if [[ "${name}" == "APPLE_CODESIGN_CERT_PASSWORD" ]] && \
        ! python3 - "${output}" <<'PY'
import sys

path = sys.argv[1]
with open(path, "rb") as stream:
    value = stream.read()
if not value.endswith(b"\n"):
    raise SystemExit(1)
with open(path, "wb") as stream:
    stream.write(value[:-1])
PY
      then
        die "Infisical returned malformed output for required macOS signing value ${name}"
      fi
      rm -f "${diagnostic}"
      ;;
    injected)
      [[ -n "${!name:-}" ]] || die "missing required injected macOS signing value ${name}"
      printf '%s' "${!name}" >"${output}"
      ;;
  esac
  chmod 0600 "${output}"
  [[ -s "${output}" ]] || die "required macOS signing value ${name} was empty"
}

for secret_name in "${secret_names[@]}"; do
  fetch_secret "${secret_name}"
done

test_worker_path="${!test_worker_variable:-}"
if [[ -n "${test_worker_path}" ]]; then
  [[ -z "${BUILDKITE:-}" && -z "${CI:-}" && -z "${GITHUB_ACTIONS:-}" \
    && "${test_worker_path}" == /* \
    && -x "${test_worker_path}" ]] || \
    die "${test_worker_variable} is restricted to non-CI local contract tests"
  worker_path="${test_worker_path}"
fi

minimal_env=(
  "PATH=${PATH}"
  "HOME=${HOME:-/var/empty}"
  "TMPDIR=${TMPDIR:-/tmp}"
  "LANG=${LANG:-C}"
  "LC_ALL=${LC_ALL:-C}"
  "CTX_MACOS_SIGNING_LAUNCHED=1"
  "CTX_MACOS_SIGNING_SECRET_DIR=${secret_root}"
)
if [[ "${mode}" == "sign" ]]; then
  minimal_env+=("CTX_MACOS_NOTARY_TIMEOUT=${CTX_MACOS_NOTARY_TIMEOUT:-30m}")
  if [[ "${kind}" == "helper" ]]; then
    minimal_env+=("CTX_MACOS_RELEASE_SOURCE_COMMIT=${helper_source_commit}")
  fi
fi
for operational_name in \
  BUILDKITE BUILDKITE_BRANCH BUILDKITE_COMMIT BUILDKITE_PULL_REQUEST \
  BUILDKITE_REPO BUILDKITE_TAG \
  DEVELOPER_DIR LOGNAME USER; do
  if [[ -n "${!operational_name:-}" ]]; then
    minimal_env+=("${operational_name}=${!operational_name}")
  fi
done

if [[ "${mode}" == "archive_attestation" ]]; then
  env -i "${minimal_env[@]}" \
    "${worker_path}" "${platform}" "${artifact}" "${nested_artifact}" "${evidence_dir}"
else
  env -i "${minimal_env[@]}" \
    "${worker_path}" "${platform}" "${kind}" "${artifact}" "${evidence_dir}"
fi
