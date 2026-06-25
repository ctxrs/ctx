#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
if [[ -f "${script_dir}/ci-common.sh" ]]; then
  source "${script_dir}/ci-common.sh"
else
  source "${script_dir}/scripts/ci-common.sh"
fi

certificate_failures=0
completion_evidence_root="${CTX_COMPLETION_EVIDENCE_ROOT:-.}"
completion_evidence_root_explicit=0
completion_certificate_mode="${CTX_COMPLETION_CERTIFICATE_MODE:-release-evidence}"
completion_certificate_mode_explicit=0

if [[ -n "${CTX_COMPLETION_EVIDENCE_ROOT:-}" ]]; then
  completion_evidence_root_explicit=1
fi

usage() {
  cat <<'USAGE'
usage: scripts/release-completion-certificate.sh [OPTIONS]

Verifies non-publishing release evidence and writes completion certificate
artifacts. The default mode validates real evidence only.

Options:
  --mode=release-evidence       Validate real, non-publishing release evidence.
  --mode=contract-self-test     Validate explicit self-test fixture evidence.
  --contract-self-test          Generate and validate contract fixture evidence.
  --evidence-root PATH          Evidence tree root. Defaults to the repo root.
  --artifact-dir PATH           Completion certificate output directory.
  -h, --help                    Show this help.

Contract self-test evidence never satisfies public release approval. It must
carry self_test_fixture=true and evidence_class=contract_fixture markers.
USAGE
}

completion_contract_mode() {
  [[ "${completion_certificate_mode}" == "contract-self-test" ]]
}

completion_certificate_evidence_class() {
  if completion_contract_mode; then
    printf 'contract_fixture'
  else
    printf 'release_artifact_evidence'
  fi
}

completion_certificate_self_test_json() {
  if completion_contract_mode; then
    printf 'true'
  else
    printf 'false'
  fi
}

completion_certificate_evidence_verified_json() {
  if completion_contract_mode; then
    printf 'false'
  else
    printf 'true'
  fi
}

completion_certificate_scaffold_verified_json() {
  if completion_contract_mode; then
    printf 'true'
  else
    printf 'false'
  fi
}

completion_certificate_verification_scope() {
  if completion_contract_mode; then
    printf 'non-publishing CI scaffolding and blocker evidence only'
  else
    printf 'non-publishing real artifact evidence; publication remains blocked by external release blockers'
  fi
}

fail_certificate() {
  certificate_failures=$(( certificate_failures + 1 ))
  printf 'completion certificate evidence failure: %s\n' "$*" >&2
}

require_file() {
  local path="$1"
  local full_path="${completion_evidence_root}/${path}"

  if [[ ! -s "${full_path}" ]]; then
    fail_certificate "required evidence is missing or empty: ${path}"
  fi
}

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return 0
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
    return 0
  fi

  if command -v sha256 >/dev/null 2>&1; then
    sha256 -q "${path}"
    return 0
  fi

  printf 'sha256sum, shasum, or sha256 is required\n' >&2
  return 1
}

require_json_parser() {
  if ! command -v jq >/dev/null 2>&1 && ! command -v python3 >/dev/null 2>&1; then
    fail_certificate "jq or python3 is required to verify release manifest evidence"
    return 1
  fi
}

manifest_value() {
  local path="$1"
  local query="$2"
  local full_path="${completion_evidence_root}/${path}"

  if command -v jq >/dev/null 2>&1; then
    jq -r "(${query}) as \$value | if \$value == null then empty else \$value end" "${full_path}" 2>/dev/null || true
    return 0
  fi

  python3 - "${full_path}" "${query}" <<'PY' 2>/dev/null || true
import json
import re
import sys

path, query = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)

if query == '.artifacts | if type == "array" then length else -1 end':
    artifacts = data.get("artifacts")
    print(len(artifacts) if isinstance(artifacts, list) else -1)
    raise SystemExit(0)

value = data
for part in query.lstrip(".").split("."):
    match = re.fullmatch(r"([A-Za-z0-9_]+)(?:\[(\d+)\])?", part)
    if not match:
        raise SystemExit(0)
    key, index = match.group(1), match.group(2)
    if not isinstance(value, dict) or key not in value:
        raise SystemExit(0)
    value = value[key]
    if index is not None:
        if not isinstance(value, list):
            raise SystemExit(0)
        idx = int(index)
        if idx >= len(value):
            raise SystemExit(0)
        value = value[idx]

if value is None:
    raise SystemExit(0)
if isinstance(value, bool):
    print("true" if value else "false")
else:
    print(value)
PY
}

require_contains() {
  local path="$1"
  local text="$2"
  local description="$3"
  local full_path="${completion_evidence_root}/${path}"

  require_file "${path}"
  if [[ -f "${full_path}" ]] && ! grep -F -q -- "${text}" "${full_path}"; then
    fail_certificate "${description}: ${path} missing ${text}"
  fi
}

require_not_contains() {
  local path="$1"
  local text="$2"
  local description="$3"
  local full_path="${completion_evidence_root}/${path}"

  require_file "${path}"
  if [[ -f "${full_path}" ]] && grep -F -q -- "${text}" "${full_path}"; then
    fail_certificate "${description}: ${path} must not contain ${text}"
  fi
}

require_env_key() {
  local path="$1"
  local key="$2"
  local expected="$3"
  local actual

  actual="$(env_value "${path}" "${key}")"
  if [[ "${actual}" != "${expected}" ]]; then
    fail_certificate "${path} must set ${key}=${expected}"
  fi
}

require_env_sha256() {
  local path="$1"
  local key="$2"
  local actual

  actual="$(env_value "${path}" "${key}")"
  if [[ ! "${actual}" =~ ^[0-9a-f]{64}$ ]]; then
    fail_certificate "${path} must set ${key} to a real lowercase SHA-256 checksum"
  fi
}

require_env_present() {
  local path="$1"
  local key="$2"
  local description="$3"
  local actual

  actual="$(env_value "${path}" "${key}")"
  if [[ -z "${actual}" ]]; then
    fail_certificate "${description}: ${path} must set ${key}"
  fi
}

require_env_https() {
  local path="$1"
  local key="$2"
  local actual

  actual="$(env_value "${path}" "${key}")"
  if [[ "${actual}" != https://* ]]; then
    fail_certificate "${path} must set ${key} to an HTTPS URL"
  fi
}

env_value() {
  local path="$1"
  local key="$2"
  local full_path="${completion_evidence_root}/${path}"

  require_file "${path}"
  awk -F= -v key="${key}" '
    $1 == key {
      value = substr($0, length(key) + 2)
      sub(/\r$/, "", value)
      print value
      found = 1
      exit
    }
    END { if (!found) exit 1 }
  ' "${full_path}" 2>/dev/null || true
}

require_manifest_value() {
  local path="$1"
  local query="$2"
  local expected="$3"
  local description="$4"
  local actual

  actual="$(manifest_value "${path}" "${query}")"
  if [[ "${actual}" != "${expected}" ]]; then
    fail_certificate "${description}: ${path} expected ${expected}, got ${actual:-<missing>}"
  fi
}

require_manifest_current_head() {
  local path="$1"
  local description="$2"
  local expected_commit actual_commit

  expected_commit="${CTX_EXPECTED_GIT_COMMIT:-${BUILDKITE_COMMIT:-}}"
  if [[ -z "${expected_commit}" ]]; then
    expected_commit="$(git rev-parse HEAD 2>/dev/null || true)"
  fi
  if [[ -z "${expected_commit}" ]]; then
    fail_certificate "${description}: could not determine current git commit for ${path}; set CTX_EXPECTED_GIT_COMMIT or BUILDKITE_COMMIT"
    return 0
  fi
  actual_commit="$(manifest_value "${path}" ".git_commit")"
  if [[ "${actual_commit}" != "${expected_commit}" ]]; then
    fail_certificate "${description}: ${path} git_commit must match current HEAD ${expected_commit}, got ${actual_commit:-<missing>}"
  fi
}

require_manifest_positive_int() {
  local path="$1"
  local query="$2"
  local description="$3"
  local actual

  actual="$(manifest_value "${path}" "${query}")"
  if [[ ! "${actual}" =~ ^[1-9][0-9]*$ ]]; then
    fail_certificate "${description}: ${path} expected a positive integer, got ${actual:-<missing>}"
  fi
}

default_freebsd_exception_path() {
  printf '%s\n' "artifacts/buildkite/release-exceptions/freebsd-x64/freebsd-x64-exception.json"
}

is_contract_evidence_class() {
  [[ "${1:-}" == "contract" || "${1:-}" == "contract_fixture" ]]
}

require_not_contract_fixture() {
  local path="$1"
  local description="$2"
  local self_test_fixture evidence_class

  self_test_fixture="$(manifest_value "${path}" ".self_test_fixture")"
  evidence_class="$(manifest_value "${path}" ".evidence_class")"
  if [[ "${self_test_fixture}" == "true" ]] || is_contract_evidence_class "${evidence_class}"; then
    fail_certificate "${description}: ${path} is contract self-test evidence and cannot satisfy real completion evidence"
  fi
}

require_contract_fixture_boundary() {
  local path="$1"
  local description="$2"
  local self_test_fixture evidence_class

  if completion_contract_mode; then
    self_test_fixture="$(manifest_value "${path}" ".self_test_fixture")"
    evidence_class="$(manifest_value "${path}" ".evidence_class")"
    if [[ -n "${self_test_fixture}" && "${self_test_fixture}" != "true" ]]; then
      fail_certificate "${description}: ${path} self_test_fixture must be true in contract self-test mode"
    fi
    if [[ -n "${evidence_class}" ]] && ! is_contract_evidence_class "${evidence_class}"; then
      fail_certificate "${description}: ${path} evidence_class must be contract or contract_fixture in contract self-test mode"
    fi
  else
    require_not_contract_fixture "${path}" "${description}"
  fi
}

require_summary_status() {
  local path="$1"
  local expected_mode="$2"

  require_file "${path}"
  require_json_parser || return 0
  require_manifest_value "${path}" ".schema_version" "1" "${expected_mode} summary records schema version"
  require_manifest_value "${path}" ".mode" "${expected_mode}" "${expected_mode} summary records mode"
  require_manifest_value "${path}" ".status" "passed" "${expected_mode} summary records passing status"
  require_manifest_value "${path}" ".publishing" "false" "${expected_mode} summary records non-publishing status"
  require_contract_fixture_boundary "${path}" "${expected_mode} summary"
}

validate_manager_release_exception() {
  local path="$1"
  local reason

  require_file "${path}"
  require_json_parser || return 0
  require_manifest_value "${path}" ".schema_version" "1" "FreeBSD release exception records schema version"
  require_manifest_value "${path}" ".kind" "release_target_exception" "FreeBSD release exception records kind"
  require_manifest_value "${path}" ".platform" "freebsd-x64" "FreeBSD release exception records platform"
  require_manifest_value "${path}" ".target_triple" "x86_64-unknown-freebsd" "FreeBSD release exception records target triple"
  require_manifest_value "${path}" ".manager_approved" "true" "FreeBSD release exception records manager approval"
  require_manifest_current_head "${path}" "FreeBSD release exception records current head"
  require_contract_fixture_boundary "${path}" "FreeBSD release exception"
  reason="$(manifest_value "${path}" ".reason")"
  if [[ -z "${reason}" ]]; then
    fail_certificate "FreeBSD release exception must include a non-empty reason"
  fi
}

validate_release_dry_run() {
  local platform="$1"
  local target_triple="$2"
  local manifest="$3"
  local metadata="$4"
  local platform_key manifest_dir checksum_file artifact_path artifact_name artifact_full_path artifact_checksum artifact_bytes artifact_count metadata_artifact metadata_checksum checksum_entry file_checksum file_bytes

  platform_key="${platform//-/_}"
  require_file "${manifest}"
  require_json_parser || return 0

  require_manifest_value "${manifest}" ".schema_version" "1" "${platform} manifest records schema version"
  require_manifest_value "${manifest}" ".dry_run" "true" "${platform} manifest records dry-run"
  require_manifest_value "${manifest}" ".upload" "false" "${platform} manifest records non-uploading release"
  require_manifest_value "${manifest}" ".package" "ctx" "${platform} manifest records package"
  require_manifest_value "${manifest}" ".platform" "${platform}" "${platform} manifest records platform"
  require_manifest_value "${manifest}" ".target_triple" "${target_triple}" "${platform} manifest records target triple"
  require_manifest_current_head "${manifest}" "${platform} manifest records current head"
  require_contract_fixture_boundary "${manifest}" "${platform} manifest"

  artifact_count="$(manifest_value "${manifest}" '.artifacts | if type == "array" then length else -1 end')"
  if [[ "${artifact_count}" != "1" ]]; then
    fail_certificate "${platform} manifest must record exactly one release artifact"
    return 0
  fi

  artifact_path="$(manifest_value "${manifest}" '.artifacts[0].path')"
  artifact_checksum="$(manifest_value "${manifest}" '.artifacts[0].sha256')"
  artifact_bytes="$(manifest_value "${manifest}" '.artifacts[0].bytes')"
  artifact_name="$(basename "${artifact_path}")"
  manifest_dir="$(dirname "${manifest}")"
  checksum_file="${manifest_dir}/checksums.sha256"

  if [[ -z "${artifact_path}" || "${artifact_path}" = /* || "${artifact_path}" == *..* ]]; then
    fail_certificate "${platform} manifest must record a safe relative artifact path"
  else
    require_file "${artifact_path}"
  fi
  if [[ "${artifact_name}" != ctx-* ]]; then
    fail_certificate "${platform} manifest artifact name must start with ctx-: ${artifact_name:-<missing>}"
  fi
  if [[ ! "${artifact_checksum}" =~ ^[0-9a-f]{64}$ ]]; then
    fail_certificate "${platform} manifest artifact checksum must be a lowercase SHA-256"
  fi
  if [[ ! "${artifact_bytes}" =~ ^[1-9][0-9]*$ ]]; then
    fail_certificate "${platform} manifest artifact bytes must be a positive integer"
  fi

  require_file "${checksum_file}"
  checksum_entry="$(awk -v name="${artifact_name}" '
    {
      checksum = $1
      artifact = $2
      sub(/\r$/, "", checksum)
      sub(/\r$/, "", artifact)
      if (artifact == name) {
        print checksum
        found = 1
        exit
      }
    }
    END { if (!found) exit 1 }
  ' "${completion_evidence_root}/${checksum_file}" 2>/dev/null || true)"
  if [[ "${checksum_entry}" != "${artifact_checksum}" ]]; then
    fail_certificate "${platform} checksums.sha256 must match manifest checksum for ${artifact_name}"
  fi

  require_env_key "${metadata}" "CTX_RELEASE_SCHEMA_VERSION" "1"
  require_env_key "${metadata}" "CTX_RELEASE_CHANNEL" "dry-run"
  require_env_sha256 "${metadata}" "CTX_RELEASE_SHA256_${platform_key}"
  metadata_artifact="$(env_value "${metadata}" "CTX_RELEASE_ARTIFACT_${platform_key}")"
  metadata_checksum="$(env_value "${metadata}" "CTX_RELEASE_SHA256_${platform_key}")"
  if [[ "${metadata_artifact}" != "${artifact_name}" ]]; then
    fail_certificate "${platform} metadata artifact ${metadata_artifact:-<missing>} must equal manifest artifact name ${artifact_name:-<missing>}"
  fi
  if [[ "${metadata_checksum}" != "${artifact_checksum}" ]]; then
    fail_certificate "${platform} metadata checksum must equal manifest artifact checksum"
  fi

  artifact_full_path="${completion_evidence_root}/${artifact_path}"
  if [[ -f "${artifact_full_path}" ]]; then
    file_checksum="$(sha256_file "${artifact_full_path}")"
    if [[ "${file_checksum}" != "${artifact_checksum}" ]]; then
      fail_certificate "${platform} artifact file checksum must equal manifest checksum"
    fi
    file_bytes="$(wc -c < "${artifact_full_path}" | tr -d '[:space:]')"
    if [[ "${file_bytes}" != "${artifact_bytes}" ]]; then
      fail_certificate "${platform} artifact file size must equal manifest bytes"
    fi
  fi
}

validate_release_artifact_smoke() {
  local platform="$1"
  local target_triple="$2"
  local manifest="$3"
  local metadata="$4"
  local platform_key smoke artifact_path artifact_checksum artifact_bytes metadata_artifact expected_artifact_path status_field

  platform_key="${platform//-/_}"
  smoke="artifacts/buildkite/release-artifact-smoke/${platform}/artifact-smoke.json"
  require_file "${smoke}"
  require_json_parser || return 0

  require_manifest_value "${smoke}" ".schema_version" "1" "${platform} artifact smoke records schema version"
  require_manifest_value "${smoke}" ".kind" "ctx_release_artifact_smoke" "${platform} artifact smoke records kind"
  require_manifest_value "${smoke}" ".mode" "release-artifact-smoke" "${platform} artifact smoke records mode"
  require_manifest_value "${smoke}" ".status" "passed" "${platform} artifact smoke records passing status"
  require_manifest_value "${smoke}" ".publishing" "false" "${platform} artifact smoke records non-publishing status"
  require_manifest_value "${smoke}" ".platform" "${platform}" "${platform} artifact smoke records platform"
  require_manifest_value "${smoke}" ".platform_key" "${platform_key}" "${platform} artifact smoke records platform key"
  require_manifest_value "${smoke}" ".target_triple" "${target_triple}" "${platform} artifact smoke records target triple"
  require_manifest_value "${smoke}" ".release_manifest" "${manifest}" "${platform} artifact smoke references release manifest"
  require_manifest_value "${smoke}" ".release_metadata" "${metadata}" "${platform} artifact smoke references release metadata"
  require_manifest_value "${smoke}" ".installed_artifact_runtime" "true" "${platform} artifact smoke ran installed artifact runtime"
  require_manifest_value "${smoke}" ".fixture" "tests/fixtures/provider-history/codex-sessions" "${platform} artifact smoke records fixture"
  require_manifest_current_head "${smoke}" "${platform} artifact smoke records current head"
  require_contract_fixture_boundary "${smoke}" "${platform} artifact smoke"

  artifact_path="$(manifest_value "${manifest}" ".artifacts[0].path")"
  artifact_checksum="$(manifest_value "${manifest}" ".artifacts[0].sha256")"
  artifact_bytes="$(manifest_value "${manifest}" ".artifacts[0].bytes")"
  metadata_artifact="$(env_value "${metadata}" "CTX_RELEASE_ARTIFACT_${platform_key}")"
  expected_artifact_path="artifacts/buildkite/release-dry-run/${platform}/${metadata_artifact}"
  if [[ "${artifact_path}" != "${expected_artifact_path}" ]]; then
    fail_certificate "${platform} manifest artifact path must match metadata artifact: expected ${expected_artifact_path}, got ${artifact_path:-<missing>}"
  fi
  require_manifest_value "${smoke}" ".release_artifact" "${artifact_path}" "${platform} artifact smoke references exact staged artifact"
  require_manifest_value "${smoke}" ".release_artifact_name" "${metadata_artifact}" "${platform} artifact smoke records artifact name"
  require_manifest_value "${smoke}" ".release_artifact_sha256" "${artifact_checksum}" "${platform} artifact smoke records artifact checksum"
  require_manifest_value "${smoke}" ".release_artifact_bytes" "${artifact_bytes}" "${platform} artifact smoke records artifact bytes"

  for status_field in version setup import search doctor validate; do
    require_manifest_value "${smoke}" ".${status_field}_status" "passed" "${platform} artifact smoke records ${status_field} command"
  done
}

validate_freebsd_release_target() {
  local manifest="artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json"
  local metadata="artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env"
  local blocker="artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json"
  local exception

  if completion_contract_mode; then
    require_file "${blocker}"
    require_manifest_value "${blocker}" ".schema_version" "1" "FreeBSD blocker records schema version"
    require_manifest_value "${blocker}" ".kind" "release_platform_blocker" "FreeBSD blocker records kind"
    require_manifest_value "${blocker}" ".platform" "freebsd-x64" "FreeBSD blocker records platform"
    require_manifest_value "${blocker}" ".target_triple" "x86_64-unknown-freebsd" "FreeBSD blocker records target triple"
    require_manifest_value "${blocker}" ".publishing" "false" "FreeBSD blocker records non-publishing status"
    require_manifest_current_head "${blocker}" "FreeBSD blocker records current head"
    require_contract_fixture_boundary "${blocker}" "FreeBSD blocker"
    return 0
  fi

  if [[ -s "${completion_evidence_root}/${manifest}" || -s "${completion_evidence_root}/${metadata}" ]]; then
    validate_release_dry_run \
      "freebsd-x64" \
      "x86_64-unknown-freebsd" \
      "${manifest}" \
      "${metadata}"
    validate_release_artifact_smoke \
      "freebsd-x64" \
      "x86_64-unknown-freebsd" \
      "${manifest}" \
      "${metadata}"
    return 0
  fi

  exception="$(default_freebsd_exception_path)"
  validate_manager_release_exception "${exception}"
}

completion_freebsd_status() {
  local manifest="artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json"
  local metadata="artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env"

  if completion_contract_mode; then
    printf 'contract_blocker_fixture_verified'
    return 0
  fi
  if [[ -s "${completion_evidence_root}/${manifest}" && -s "${completion_evidence_root}/${metadata}" ]]; then
    printf 'native_release_artifact_smoke_verified'
    return 0
  fi
  printf 'manager_exception_verified'
}

completion_freebsd_manager_exception_required_json() {
  if [[ "$(completion_freebsd_status)" == "native_release_artifact_smoke_verified" ]]; then
    printf 'false'
  else
    printf 'true'
  fi
}

validate_release_candidate_metadata() {
  local manifest="artifacts/buildkite/release-candidate/release-candidate-manifest.json"
  local metadata="artifacts/buildkite/release-candidate/ctx-release-metadata.env"
  local checksums="artifacts/buildkite/release-candidate/checksums.sha256"
  local plan="artifacts/buildkite/release-candidate/r2-upload-plan.md"
  local commands="artifacts/buildkite/release-candidate/r2-upload-commands.sh"
  local platform platform_key artifact checksum checksum_entry artifact_path actual_checksum r2_prefix public_base_url release_exception_path

  require_file "${manifest}"
  require_file "${metadata}"
  require_file "${checksums}"
  require_file "${plan}"
  require_file "${commands}"
  require_json_parser || return 0

  require_manifest_value "${manifest}" ".schema_version" "1" "release candidate manifest records schema version"
  require_manifest_value "${manifest}" ".kind" "ctx_release_candidate" "release candidate manifest records kind"
  require_manifest_value "${manifest}" ".release_candidate_status" "staging_plan_only" "release candidate manifest records staging-plan-only status"
  require_manifest_value "${manifest}" ".launch_ready" "false" "release candidate manifest records launch-blocked status"
  require_manifest_value "${manifest}" ".publishing" "false" "release candidate manifest records non-publishing status"
  require_manifest_value "${manifest}" ".package" "ctx" "release candidate manifest records package"
  require_manifest_value "${manifest}" ".version" "0.1.0" "release candidate manifest records 0.1.0"
  require_manifest_value "${manifest}" ".channel" "release-candidate" "release candidate manifest records channel"
  require_manifest_current_head "${manifest}" "release candidate manifest records current head"
  require_contract_fixture_boundary "${manifest}" "release candidate manifest"
  require_manifest_value "${manifest}" ".r2.bucket" "$(env_value "${metadata}" CTX_RELEASE_R2_BUCKET)" "release candidate manifest records R2 bucket"
  require_manifest_value "${manifest}" ".r2.prefix" "$(env_value "${metadata}" CTX_RELEASE_R2_PREFIX)" "release candidate manifest records R2 prefix"
  require_manifest_value "${manifest}" ".r2.upload_performed" "false" "release candidate manifest records R2 upload not performed"

  require_env_key "${metadata}" "CTX_RELEASE_SCHEMA_VERSION" "1"
  require_env_key "${metadata}" "CTX_RELEASE_VERSION" "0.1.0"
  require_env_key "${metadata}" "CTX_RELEASE_CHANNEL" "release-candidate"
  require_env_https "${metadata}" "CTX_RELEASE_BASE_URL"
  require_env_present "${metadata}" "CTX_RELEASE_R2_BUCKET" "release candidate metadata records R2 bucket"
  require_env_present "${metadata}" "CTX_RELEASE_R2_PREFIX" "release candidate metadata records R2 prefix"
  r2_prefix="$(env_value "${metadata}" CTX_RELEASE_R2_PREFIX)"
  public_base_url="$(env_value "${metadata}" CTX_RELEASE_BASE_URL)"
  if [[ "${r2_prefix}" != ctx/releases/* ]]; then
    fail_certificate "release candidate R2 prefix must use ctx/releases public artifact layout"
  fi
  if [[ "${r2_prefix}" == *work-recorder* || "${r2_prefix}" == *work-record* ]]; then
    fail_certificate "release candidate R2 prefix must not brand public artifact paths around work-record"
  fi
  if [[ "${public_base_url}" != */ctx/releases/* ]]; then
    fail_certificate "release candidate public base URL must use ctx/releases artifact layout"
  fi
  if [[ "${public_base_url}" == *work-recorder* || "${public_base_url}" == *work-record* ]]; then
    fail_certificate "release candidate public base URL must not brand public artifact URLs around work-record"
  fi

  while IFS='|' read -r platform platform_key; do
    artifact="$(env_value "${metadata}" "CTX_RELEASE_ARTIFACT_${platform_key}")"
    checksum="$(env_value "${metadata}" "CTX_RELEASE_SHA256_${platform_key}")"
    require_env_sha256 "${metadata}" "CTX_RELEASE_SHA256_${platform_key}"
    if [[ -z "${artifact}" || "${artifact}" = /* || "${artifact}" == *..* || "${artifact}" == */* || "${artifact}" == *\\* ]]; then
      fail_certificate "release candidate metadata artifact for ${platform} must be a safe file name"
      continue
    fi

    checksum_entry="$(awk -v name="${artifact}" '
      {
        checksum = $1
        artifact_name = $2
        sub(/\r$/, "", checksum)
        sub(/\r$/, "", artifact_name)
        if (artifact_name == name) {
          print checksum
          found = 1
          exit
        }
      }
      END { if (!found) exit 1 }
    ' "${completion_evidence_root}/${checksums}" 2>/dev/null || true)"
    if [[ "${checksum_entry}" != "${checksum}" ]]; then
      fail_certificate "release candidate checksums.sha256 must match metadata checksum for ${artifact}"
    fi

    artifact_path="artifacts/buildkite/release-dry-run/${platform}/${artifact}"
    require_file "${artifact_path}"
    if [[ -f "${completion_evidence_root}/${artifact_path}" ]]; then
      actual_checksum="$(sha256_file "${completion_evidence_root}/${artifact_path}")"
      if [[ "${actual_checksum}" != "${checksum}" ]]; then
        fail_certificate "release candidate metadata checksum must match artifact file for ${artifact}"
      fi
    fi
    require_env_present "${metadata}" "CTX_RELEASE_R2_OBJECT_${platform_key}" "release candidate metadata records R2 object for ${platform}"
  done <<'EOF'
linux-x64|linux_x64
macos-arm64|macos_arm64
macos-x64|macos_x64
windows-x64|windows_x64
EOF

  artifact="$(env_value "${metadata}" "CTX_RELEASE_ARTIFACT_freebsd_x64")"
  if [[ -n "${artifact}" ]]; then
    platform="freebsd-x64"
    platform_key="freebsd_x64"
    checksum="$(env_value "${metadata}" "CTX_RELEASE_SHA256_${platform_key}")"
    require_env_sha256 "${metadata}" "CTX_RELEASE_SHA256_${platform_key}"
    if [[ "${artifact}" = /* || "${artifact}" == *..* || "${artifact}" == */* || "${artifact}" == *\\* ]]; then
      fail_certificate "release candidate metadata artifact for ${platform} must be a safe file name"
    else
      checksum_entry="$(awk -v name="${artifact}" '
        {
          checksum = $1
          artifact_name = $2
          sub(/\r$/, "", checksum)
          sub(/\r$/, "", artifact_name)
          if (artifact_name == name) {
            print checksum
            found = 1
            exit
          }
        }
        END { if (!found) exit 1 }
      ' "${completion_evidence_root}/${checksums}" 2>/dev/null || true)"
      if [[ "${checksum_entry}" != "${checksum}" ]]; then
        fail_certificate "release candidate checksums.sha256 must match metadata checksum for ${artifact}"
      fi
      artifact_path="artifacts/buildkite/release-dry-run/${platform}/${artifact}"
      require_file "${artifact_path}"
      if [[ -f "${completion_evidence_root}/${artifact_path}" ]]; then
        actual_checksum="$(sha256_file "${completion_evidence_root}/${artifact_path}")"
        if [[ "${actual_checksum}" != "${checksum}" ]]; then
          fail_certificate "release candidate metadata checksum must match artifact file for ${artifact}"
        fi
      fi
    fi
    require_env_present "${metadata}" "CTX_RELEASE_R2_OBJECT_${platform_key}" "release candidate metadata records R2 object for ${platform}"
    require_manifest_value "${manifest}" ".freebsd_x64.status" "artifact_proof" "release candidate manifest records FreeBSD artifact proof"
  elif completion_contract_mode; then
    require_contains "${metadata}" "CTX_RELEASE_BLOCKER_FREEBSD_X64=" "release candidate metadata records FreeBSD blocker"
    require_manifest_value "${manifest}" ".freebsd_x64.status" "blocked" "release candidate manifest records FreeBSD blocker status"
  else
    release_exception_path="$(env_value "${metadata}" "CTX_RELEASE_EXCEPTION_FREEBSD_X64")"
    release_exception_path="${release_exception_path:-$(default_freebsd_exception_path)}"
    validate_manager_release_exception "${release_exception_path}"
    require_manifest_value "${manifest}" ".freebsd_x64.status" "manager_exception" "release candidate manifest records FreeBSD manager exception"
  fi

  require_contains "${plan}" "Cleanup staged objects" "release candidate R2 plan records cleanup"
  require_contains "${commands}" "wrangler r2 object put" "release candidate R2 command file records staging commands"
  require_contains "${commands}" 'scripts/install.sh' "release candidate R2 command file stages Bash installer"
  require_contains "${commands}" 'scripts/install.ps1' "release candidate R2 command file stages PowerShell installer"
  require_contains "${manifest}" '"installers"' "release candidate manifest records installers"
}

validate_dependency_advisory_license_audit() {
  local manifest="artifacts/buildkite/supply-chain/dependency-advisory-license-audit.json"
  local status advisory_status license_status

  require_file "${manifest}"
  require_json_parser || return 0
  require_manifest_value "${manifest}" ".schema_version" "1" "dependency advisory/license audit records schema version"
  require_manifest_value "${manifest}" ".kind" "ctx_dependency_advisory_license_audit" "dependency advisory/license audit records kind"
  require_manifest_value "${manifest}" ".mode" "dependency-advisory-license-audit" "dependency advisory/license audit records mode"
  require_manifest_value "${manifest}" ".publishing" "false" "dependency advisory/license audit records non-publishing status"
  require_manifest_value "${manifest}" ".package" "ctx" "dependency advisory/license audit records package"
  require_manifest_value "${manifest}" ".version" "0.1.0" "dependency advisory/license audit records version"
  require_manifest_value "${manifest}" ".required_before_public_release" "true" "dependency advisory/license audit blocks public release until proof exists"
  require_manifest_current_head "${manifest}" "dependency advisory/license audit records current head"
  require_contract_fixture_boundary "${manifest}" "dependency advisory/license audit"
  require_manifest_positive_int "${manifest}" ".cargo_metadata.packages_checked" "dependency advisory/license audit records checked package count"
  require_manifest_value "${manifest}" ".license_audit.status" "passed" "dependency license audit records passing license inventory"
  require_manifest_value "${manifest}" ".license_audit.missing_license_count" "0" "dependency license audit records no missing license metadata"

  status="$(manifest_value "${manifest}" ".status")"
  case "${status}" in
    passed|blocked_manual_required)
      ;;
    *)
      fail_certificate "dependency advisory/license audit status must be passed or blocked_manual_required, got ${status:-<missing>}"
      ;;
  esac

  advisory_status="$(manifest_value "${manifest}" ".advisory_audit.status")"
  case "${advisory_status}" in
    passed|provided_manual_evidence|blocked_manual_required)
      ;;
    *)
      fail_certificate "dependency advisory audit status must be passed, provided_manual_evidence, or blocked_manual_required, got ${advisory_status:-<missing>}"
      ;;
  esac
  if [[ "${advisory_status}" != "passed" ]]; then
    require_manifest_value "${manifest}" ".advisory_audit.manual_lane" "true" "dependency advisory audit records manual lane when tool proof is absent"
    require_manifest_value "${manifest}" ".advisory_audit.manager_approval_required" "true" "dependency advisory audit requires manager approval when tool proof is absent"
  fi

  license_status="$(manifest_value "${manifest}" ".license_audit.status")"
  if [[ "${license_status}" != "passed" ]]; then
    fail_certificate "dependency license audit must pass before completion evidence can be written"
  fi
}

validate_sbom_provenance_signature_evidence() {
  local manifest="artifacts/buildkite/supply-chain/sbom-provenance-signature.json"
  local status field field_status

  require_file "${manifest}"
  require_json_parser || return 0
  require_manifest_value "${manifest}" ".schema_version" "1" "SBOM/provenance/signature evidence records schema version"
  require_manifest_value "${manifest}" ".kind" "ctx_sbom_provenance_signature_evidence" "SBOM/provenance/signature evidence records kind"
  require_manifest_value "${manifest}" ".mode" "sbom-provenance-signature" "SBOM/provenance/signature evidence records mode"
  require_manifest_value "${manifest}" ".publishing" "false" "SBOM/provenance/signature evidence records non-publishing status"
  require_manifest_value "${manifest}" ".package" "ctx" "SBOM/provenance/signature evidence records package"
  require_manifest_value "${manifest}" ".version" "0.1.0" "SBOM/provenance/signature evidence records version"
  require_manifest_value "${manifest}" ".required_before_public_release" "true" "SBOM/provenance/signature evidence blocks public release until proof exists"
  require_manifest_current_head "${manifest}" "SBOM/provenance/signature evidence records current head"
  require_contract_fixture_boundary "${manifest}" "SBOM/provenance/signature evidence"

  status="$(manifest_value "${manifest}" ".status")"
  case "${status}" in
    passed|blocked_manual_required)
      ;;
    *)
      fail_certificate "SBOM/provenance/signature evidence status must be passed or blocked_manual_required, got ${status:-<missing>}"
      ;;
  esac

  for field in sbom provenance signature notarization; do
    field_status="$(manifest_value "${manifest}" ".${field}.status")"
    case "${field_status}" in
      provided_manual_evidence|blocked_manual_required)
        ;;
      *)
        fail_certificate "${field} evidence status must be provided_manual_evidence or blocked_manual_required, got ${field_status:-<missing>}"
        continue
        ;;
    esac
    require_manifest_value "${manifest}" ".${field}.required_before_public_release" "true" "${field} evidence is required before public release"
    if [[ "${field_status}" == "blocked_manual_required" ]]; then
      require_manifest_value "${manifest}" ".${field}.manual_lane" "true" "${field} evidence records manual lane when external proof is absent"
      require_manifest_value "${manifest}" ".${field}.manager_approval_required" "true" "${field} evidence requires manager approval when external proof is absent"
    fi
  done
}

validate_r2_staging_readback_evidence() {
  local manifest="artifacts/buildkite/r2-staging-readback/r2-staging-readback.json"
  local status

  require_file "${manifest}"
  require_json_parser || return 0
  require_manifest_value "${manifest}" ".schema_version" "1" "R2 staging readback records schema version"
  require_manifest_value "${manifest}" ".kind" "ctx_r2_staging_readback" "R2 staging readback records kind"
  require_manifest_value "${manifest}" ".mode" "r2-staging-readback" "R2 staging readback records mode"
  require_manifest_value "${manifest}" ".publishing" "false" "R2 staging readback records non-publishing status"
  require_manifest_value "${manifest}" ".no_ctx_rs_cutover" "true" "R2 staging readback records no ctx.rs cutover"
  require_manifest_value "${manifest}" ".required_before_public_release" "true" "R2 staging readback blocks public release until proof exists"
  require_manifest_current_head "${manifest}" "R2 staging readback records current head"
  require_contract_fixture_boundary "${manifest}" "R2 staging readback"
  require_manifest_positive_int "${manifest}" ".validated_upload_object_count" "R2 staging readback records planned object count"

  status="$(manifest_value "${manifest}" ".status")"
  case "${status}" in
    passed)
      require_manifest_value "${manifest}" ".upload_performed" "true" "R2 staging readback records real upload"
      require_manifest_value "${manifest}" ".readback_performed" "true" "R2 staging readback records real readback"
      require_manifest_positive_int "${manifest}" ".validated_readback_object_count" "R2 staging readback records readback object count"
      ;;
    blocked_manual_required)
      require_manifest_value "${manifest}" ".upload_performed" "false" "R2 staging readback must not claim upload when blocked"
      require_manifest_value "${manifest}" ".readback_performed" "false" "R2 staging readback must not claim readback when blocked"
      require_manifest_value "${manifest}" ".manual_lane" "true" "R2 staging readback records manual lane when credentials are absent"
      require_manifest_value "${manifest}" ".manager_approval_required" "true" "R2 staging readback requires manager approval when credentials are absent"
      ;;
    *)
      fail_certificate "R2 staging readback status must be passed or blocked_manual_required, got ${status:-<missing>}"
      ;;
  esac
}

validate_provider_live_e2e_lanes() {
  local manifest="artifacts/buildkite/provider-live-e2e-lanes/provider-live-e2e-lanes.json"
  local notes="artifacts/buildkite/provider-live-e2e-lanes/provider-live-e2e-lanes.md"

  require_file "${manifest}"
  require_file "${notes}"
  require_json_parser || return 0

  require_manifest_value "${manifest}" ".schema_version" "1" "provider live E2E lanes record schema version"
  require_manifest_value "${manifest}" ".kind" "provider_live_e2e_lane_definitions" "provider live E2E lanes record kind"
  require_manifest_value "${manifest}" ".publishing" "false" "provider live E2E lanes record non-publishing status"
  require_manifest_value "${manifest}" ".default_enabled" "false" "provider live E2E lanes are disabled by default"
  require_manifest_current_head "${manifest}" "provider live E2E lanes record current head"
  require_contract_fixture_boundary "${manifest}" "provider live E2E lanes"
  require_contains "${notes}" "CTX_LIVE_PROVIDER_E2E=1" "provider live E2E notes record global opt-in"
  require_contains "${notes}" "Codex" "provider live E2E notes include Codex"
  require_contains "${notes}" "Claude Code" "provider live E2E notes include Claude Code"
  require_contains "${notes}" "Gemini CLI" "provider live E2E notes include Gemini CLI"
  require_contains "${notes}" "OpenRouter" "provider live E2E notes include OpenRouter generated harness"
}

validate_release_docs() {
  require_contains "docs/release-install.md" "No public release install command is published" "release install docs keep public installer command absent"
  require_contains "docs/release-install.md" "source build path" "release install docs document source build status"
  require_contains "docs/release-install.md" "freebsd-x64" "release install docs include FreeBSD as a required release target"
  require_contains "docs/release-install.md" "manager-approved release exception" "release install docs require explicit target exceptions"
  require_contains "docs/release-install.md" "packaged artifact runtime smoke" "release install docs require packaged artifact smoke"
  require_contains "docs/release-install.md" "Dependency advisory/license evidence" "release install docs require dependency advisory/license approval"
  require_contains "docs/release-install.md" "R2 staging upload/readback" "release install docs require R2 upload/readback approval"
  require_not_contains "docs/release-install.md" "ctx.rs/install" "release install docs must not expose public installer endpoint"
  require_not_contains "docs/release-install.md" "curl -fsSL" "release install docs must not expose curl installer command"

  require_contains "docs/release-supply-chain.md" "Contract fixture evidence" "release supply-chain docs distinguish contract fixtures"
  require_contains "docs/release-supply-chain.md" "Host artifact dry-run" "release supply-chain docs distinguish host dry-run"
  require_contains "docs/release-supply-chain.md" "Multi-platform artifact proof" "release supply-chain docs distinguish platform proof"
  require_contains "docs/release-supply-chain.md" "packaged artifact runtime smoke" "release supply-chain docs include artifact runtime smoke"
  require_contains "docs/release-supply-chain.md" "freebsd-x64" "release supply-chain docs include FreeBSD platform proof"
  require_contains "docs/release-supply-chain.md" "first-class release target" "release supply-chain docs keep FreeBSD in scope"
  require_contains "docs/release-supply-chain.md" "manager-approved release exception" "release supply-chain docs require explicit target exceptions"
  require_contains "docs/release-supply-chain.md" "Signing, notarization, SBOM, and provenance" "release supply-chain docs list external supply-chain blockers"
  require_contains "docs/release-supply-chain.md" "Dependency advisory/license evidence" "release supply-chain docs describe dependency advisory/license evidence"
  require_contains "docs/release-supply-chain.md" "blocked_manual_required" "release supply-chain docs require truthful blocked manual lanes"

  require_contains "docs/release-r2-layout.md" "R2 staging layout" "release R2 docs describe staging layout"
  require_contains "docs/release-r2-layout.md" "ctx/releases/release-candidate/" "release R2 docs record candidate prefix"
  require_contains "docs/release-r2-layout.md" "freebsd-x64" "release R2 docs include FreeBSD artifact staging"
  require_contains "docs/release-r2-layout.md" "release-artifact-smoke" "release R2 docs reference artifact smoke evidence"
  require_contains "docs/release-r2-layout.md" "No installer endpoint cutover" "release R2 docs keep public installer cutover blocked"
  require_contains "docs/release-r2-layout.md" "Upload/readback proof" "release R2 docs describe upload/readback proof"
  require_contains "docs/release-r2-layout.md" "CTX_RELEASE_R2_MANAGER_APPROVED=1" "release R2 docs require manager-approved upload/readback"

  require_contains "docs/freebsd-release-worker.md" "freebsd-x64" "FreeBSD release worker docs record queue label"
  require_contains "docs/freebsd-release-worker.md" "x86_64-unknown-freebsd" "FreeBSD release worker docs record target triple"
  require_contains "docs/freebsd-release-worker.md" "release-artifact-smoke" "FreeBSD release worker docs include artifact smoke evidence"
  require_contains "docs/freebsd-release-worker.md" "first-class release target" "FreeBSD release worker docs keep FreeBSD in scope"
  require_contains "docs/freebsd-release-worker.md" "manager-approved" "FreeBSD release worker docs require manager approval if missing"
  require_contains "docs/freebsd-release-worker.md" "exception" "FreeBSD release worker docs require explicit exception if missing"
}

completion_release_platforms() {
  cat <<'EOF'
linux-x64|linux_x64|x86_64-unknown-linux-gnu|
macos-arm64|macos_arm64|aarch64-apple-darwin|
macos-x64|macos_x64|x86_64-apple-darwin|
windows-x64|windows_x64|x86_64-pc-windows-gnu|.exe
EOF
}

write_contract_release_dry_run_platform() {
  local root="$1"
  local platform="$2"
  local platform_key="$3"
  local target_triple="$4"
  local suffix="$5"
  local artifact artifact_rel artifact_full checksum bytes generated_at commit branch platform_dir

  artifact="ctx-0.1.0-${target_triple}${suffix}"
  artifact_rel="artifacts/buildkite/release-dry-run/${platform}/${artifact}"
  artifact_full="${root}/${artifact_rel}"
  platform_dir="$(dirname "${artifact_full}")"
  mkdir -p "${platform_dir}"
  printf 'ctx completion certificate contract fixture for %s\n' "${platform}" > "${artifact_full}"
  chmod 0755 "${artifact_full}" 2>/dev/null || true

  checksum="$(sha256_file "${artifact_full}")"
  bytes="$(wc -c < "${artifact_full}" | tr -d '[:space:]')"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${platform_dir}/ctx-release-metadata.env" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=0.1.0
CTX_RELEASE_CHANNEL=dry-run
CTX_RELEASE_EVIDENCE_CLASS=contract_fixture
CTX_RELEASE_SELF_TEST_FIXTURE=true
CTX_RELEASE_BASE_URL=https://example.invalid/ctx/releases/release-candidate/v0.1.0/${commit}
CTX_RELEASE_ARTIFACT_${platform_key}=${artifact}
CTX_RELEASE_SHA256_${platform_key}=${checksum}
EOF

  cat > "${platform_dir}/checksums.sha256" <<EOF
${checksum}  ${artifact}
EOF

  cat > "${platform_dir}/manifest.json" <<EOF
{
  "schema_version": 1,
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "dry_run": true,
  "upload": false,
  "publishing": false,
  "package": "ctx",
  "version": "0.1.0",
  "platform": "$(ctx_json_escape "${platform}")",
  "target_triple": "$(ctx_json_escape "${target_triple}")",
  "host_triple": "$(ctx_json_escape "${target_triple}")",
  "expected_host_triple": "$(ctx_json_escape "${target_triple}")",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "buildkite": {
    "build_url": "$(ctx_json_escape "${BUILDKITE_BUILD_URL:-local}")",
    "build_id": "$(ctx_json_escape "${BUILDKITE_BUILD_ID:-}")",
    "job_id": "$(ctx_json_escape "${BUILDKITE_JOB_ID:-}")"
  },
  "generated_at_unix_s": ${generated_at},
  "artifacts": [
    {
      "path": "$(ctx_json_escape "${artifact_rel}")",
      "sha256": "$(ctx_json_escape "${checksum}")",
      "bytes": ${bytes}
    }
  ]
}
EOF
}

write_contract_release_dry_runs() {
  local root="$1"
  local platform platform_key target_triple suffix

  while IFS='|' read -r platform platform_key target_triple suffix; do
    write_contract_release_dry_run_platform "${root}" "${platform}" "${platform_key}" "${target_triple}" "${suffix}"
  done < <(completion_release_platforms)
}

write_contract_release_artifact_smoke_platform() {
  local root="$1"
  local platform="$2"
  local platform_key="$3"
  local target_triple="$4"
  local suffix="$5"
  local artifact artifact_rel artifact_full checksum bytes out_dir json markdown generated_at commit branch command_dir

  artifact="ctx-0.1.0-${target_triple}${suffix}"
  artifact_rel="artifacts/buildkite/release-dry-run/${platform}/${artifact}"
  artifact_full="${root}/${artifact_rel}"
  checksum="$(sha256_file "${artifact_full}")"
  bytes="$(wc -c < "${artifact_full}" | tr -d '[:space:]')"
  out_dir="${root}/artifacts/buildkite/release-artifact-smoke/${platform}"
  command_dir="${out_dir}/commands"
  json="${out_dir}/artifact-smoke.json"
  markdown="${out_dir}/artifact-smoke.md"
  mkdir -p "${command_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  printf 'contract fixture command output for %s\n' "${platform}" > "${command_dir}/version.stdout"

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_release_artifact_smoke",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "mode": "release-artifact-smoke",
  "status": "passed",
  "publishing": false,
  "platform": "$(ctx_json_escape "${platform}")",
  "platform_key": "$(ctx_json_escape "${platform_key}")",
  "target_triple": "$(ctx_json_escape "${target_triple}")",
  "host_triple": "$(ctx_json_escape "${target_triple}")",
  "release_dry_run_dir": "artifacts/buildkite/release-dry-run/${platform}",
  "release_manifest": "artifacts/buildkite/release-dry-run/${platform}/manifest.json",
  "release_metadata": "artifacts/buildkite/release-dry-run/${platform}/ctx-release-metadata.env",
  "release_artifact": "$(ctx_json_escape "${artifact_rel}")",
  "release_artifact_name": "$(ctx_json_escape "${artifact}")",
  "release_artifact_sha256": "$(ctx_json_escape "${checksum}")",
  "release_artifact_bytes": ${bytes},
  "install_method": "contract-fixture",
  "installed_artifact_runtime": true,
  "fixture": "tests/fixtures/provider-history/codex-sessions",
  "command_output_dir": "artifacts/buildkite/release-artifact-smoke/${platform}/commands",
  "version_output": "ctx 0.1.0",
  "version_status": "passed",
  "setup_status": "passed",
  "import_status": "passed",
  "search_status": "passed",
  "doctor_status": "passed",
  "validate_status": "passed",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "buildkite": {
    "build_url": "$(ctx_json_escape "${BUILDKITE_BUILD_URL:-local}")",
    "build_id": "$(ctx_json_escape "${BUILDKITE_BUILD_ID:-}")",
    "job_id": "$(ctx_json_escape "${BUILDKITE_JOB_ID:-}")"
  },
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# ctx Release Artifact Smoke Contract Fixture

- Evidence class: contract_fixture
- Self-test fixture: true
- Publishing: false
- Platform: \`${platform}\`
- Target triple: \`${target_triple}\`
- Release artifact: \`${artifact_rel}\`
- Status: passed
EOF
}

write_contract_release_artifact_smokes() {
  local root="$1"
  local platform platform_key target_triple suffix

  while IFS='|' read -r platform platform_key target_triple suffix; do
    write_contract_release_artifact_smoke_platform "${root}" "${platform}" "${platform_key}" "${target_triple}" "${suffix}"
  done < <(completion_release_platforms)
}

write_contract_freebsd_blocker() {
  local root="$1"
  local out_dir="${root}/artifacts/buildkite/release-blockers/freebsd-x64"
  local generated_at commit branch

  mkdir -p "${out_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${out_dir}/freebsd-x64-blocker.json" <<EOF
{
  "schema_version": 1,
  "kind": "release_platform_blocker",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "platform": "freebsd-x64",
  "target_triple": "x86_64-unknown-freebsd",
  "required_release_target": true,
  "first_class_release_target": true,
  "native_proof_required": true,
  "manager_exception_required_for_public_release": true,
  "missing_runner_label": "queue=freebsd-x64",
  "artifact_status": "Contract fixture only; native FreeBSD release artifacts are not produced by this evidence tree.",
  "proof_path": "Provision queue=freebsd-x64, build ctx natively for x86_64-unknown-freebsd, and export manifest plus checksum evidence.",
  "publishing": false,
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${out_dir}/freebsd-x64-blocker.md" <<'EOF'
# FreeBSD x86_64 Contract Fixture Blocker

- Evidence class: contract_fixture
- Self-test fixture: true
- Platform: freebsd-x64
- Target triple: x86_64-unknown-freebsd
- Required release target: true
- Native proof required: true
- Manager-approved release exception required for public release without proof: true
- Publishing: false
EOF
}

write_contract_release_candidate() {
  local root="$1"
  local out_dir="${root}/artifacts/buildkite/release-candidate"
  local metadata checksums manifest plan commands generated_at commit branch bucket prefix public_base_url
  local platform platform_key target_triple suffix artifact artifact_path checksum bytes index
  local artifact_names=()
  local artifact_checksums=()
  local artifact_platform_keys=()
  local artifact_platforms=()
  local artifact_targets=()
  local artifact_bytes=()

  mkdir -p "${out_dir}"
  metadata="${out_dir}/ctx-release-metadata.env"
  checksums="${out_dir}/checksums.sha256"
  manifest="${out_dir}/release-candidate-manifest.json"
  plan="${out_dir}/r2-upload-plan.md"
  commands="${out_dir}/r2-upload-commands.sh"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  bucket="ctx-release-artifacts"
  prefix="ctx/releases/release-candidate/v0.1.0/${commit}"
  public_base_url="https://example.invalid/ctx/releases/release-candidate/v0.1.0/${commit}"

  while IFS='|' read -r platform platform_key target_triple suffix; do
    artifact="ctx-0.1.0-${target_triple}${suffix}"
    artifact_path="${root}/artifacts/buildkite/release-dry-run/${platform}/${artifact}"
    checksum="$(sha256_file "${artifact_path}")"
    bytes="$(wc -c < "${artifact_path}" | tr -d '[:space:]')"
    artifact_names+=("${artifact}")
    artifact_checksums+=("${checksum}")
    artifact_platform_keys+=("${platform_key}")
    artifact_platforms+=("${platform}")
    artifact_targets+=("${target_triple}")
    artifact_bytes+=("${bytes}")
  done < <(completion_release_platforms)

  {
    printf '# ctx release installer metadata, schema v1.\n'
    printf '# Contract fixture evidence for completion certificate self-test mode.\n'
    printf 'CTX_RELEASE_SCHEMA_VERSION=1\n'
    printf 'CTX_RELEASE_VERSION=0.1.0\n'
    printf 'CTX_RELEASE_CHANNEL=release-candidate\n'
    printf 'CTX_RELEASE_EVIDENCE_CLASS=contract_fixture\n'
    printf 'CTX_RELEASE_SELF_TEST_FIXTURE=true\n'
    printf 'CTX_RELEASE_BASE_URL=%s\n' "${public_base_url}"
    printf 'CTX_RELEASE_R2_BUCKET=%s\n' "${bucket}"
    printf 'CTX_RELEASE_R2_PREFIX=%s\n' "${prefix}"
    for index in "${!artifact_names[@]}"; do
      printf 'CTX_RELEASE_ARTIFACT_%s=%s\n' "${artifact_platform_keys[${index}]}" "${artifact_names[${index}]}"
      printf 'CTX_RELEASE_SHA256_%s=%s\n' "${artifact_platform_keys[${index}]}" "${artifact_checksums[${index}]}"
      printf 'CTX_RELEASE_R2_OBJECT_%s=%s/%s\n' "${artifact_platform_keys[${index}]}" "${prefix}" "${artifact_names[${index}]}"
    done
    printf 'CTX_RELEASE_INSTALLER_SH_R2_OBJECT=%s/install.sh\n' "${prefix}"
    printf 'CTX_RELEASE_INSTALLER_PS1_R2_OBJECT=%s/install.ps1\n' "${prefix}"
    printf 'CTX_RELEASE_BLOCKER_FREEBSD_X64=artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json\n'
  } > "${metadata}"

  : > "${checksums}"
  for index in "${!artifact_names[@]}"; do
    printf '%s  %s\n' "${artifact_checksums[${index}]}" "${artifact_names[${index}]}" >> "${checksums}"
  done

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "kind": "ctx_release_candidate",\n'
    printf '  "evidence_class": "contract_fixture",\n'
    printf '  "self_test_fixture": true,\n'
    printf '  "release_candidate_status": "staging_plan_only",\n'
    printf '  "launch_ready": false,\n'
    printf '  "publishing": false,\n'
    printf '  "package": "ctx",\n'
    printf '  "version": "0.1.0",\n'
    printf '  "channel": "release-candidate",\n'
    printf '  "git_commit": "%s",\n' "$(ctx_json_escape "${commit}")"
    printf '  "git_branch": "%s",\n' "$(ctx_json_escape "${branch}")"
    printf '  "buildkite": {\n'
    printf '    "build_url": "%s",\n' "$(ctx_json_escape "${BUILDKITE_BUILD_URL:-local}")"
    printf '    "build_id": "%s",\n' "$(ctx_json_escape "${BUILDKITE_BUILD_ID:-}")"
    printf '    "job_id": "%s"\n' "$(ctx_json_escape "${BUILDKITE_JOB_ID:-}")"
    printf '  },\n'
    printf '  "generated_at_unix_s": %s,\n' "${generated_at}"
    printf '  "r2": {\n'
    printf '    "bucket": "%s",\n' "$(ctx_json_escape "${bucket}")"
    printf '    "prefix": "%s",\n' "$(ctx_json_escape "${prefix}")"
    printf '    "public_base_url": "%s",\n' "$(ctx_json_escape "${public_base_url}")"
    printf '    "upload_performed": false\n'
    printf '  },\n'
    printf '  "artifacts": [\n'
    for index in "${!artifact_names[@]}"; do
      if (( index > 0 )); then
        printf ',\n'
      fi
      printf '    {\n'
      printf '      "platform": "%s",\n' "$(ctx_json_escape "${artifact_platforms[${index}]}")"
      printf '      "platform_key": "%s",\n' "$(ctx_json_escape "${artifact_platform_keys[${index}]}")"
      printf '      "target_triple": "%s",\n' "$(ctx_json_escape "${artifact_targets[${index}]}")"
      printf '      "name": "%s",\n' "$(ctx_json_escape "${artifact_names[${index}]}")"
      printf '      "sha256": "%s",\n' "$(ctx_json_escape "${artifact_checksums[${index}]}")"
      printf '      "bytes": %s,\n' "${artifact_bytes[${index}]}"
      printf '      "r2_object_key": "%s/%s"\n' "$(ctx_json_escape "${prefix}")" "$(ctx_json_escape "${artifact_names[${index}]}")"
      printf '    }'
    done
    printf '\n'
    printf '  ],\n'
    printf '  "installers": [\n'
    printf '    {"name": "install.sh", "source_path": "scripts/install.sh", "r2_object_key": "%s/install.sh"},\n' "$(ctx_json_escape "${prefix}")"
    printf '    {"name": "install.ps1", "source_path": "scripts/install.ps1", "r2_object_key": "%s/install.ps1"}\n' "$(ctx_json_escape "${prefix}")"
    printf '  ],\n'
    printf '  "freebsd_x64": {\n'
    printf '    "status": "blocked",\n'
    printf '    "required_release_target": true,\n'
    printf '    "first_class_release_target": true,\n'
    printf '    "native_proof_required": true,\n'
    printf '    "manager_exception_required_for_public_release": true,\n'
    printf '    "blocker_artifact": "artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json"\n'
    printf '  }\n'
    printf '}\n'
  } > "${manifest}"

  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n\n'
    printf '# Contract fixture staging commands only.\n'
    for index in "${!artifact_names[@]}"; do
      printf 'wrangler r2 object put "${CTX_RELEASE_R2_BUCKET}/${CTX_RELEASE_R2_PREFIX}/%s" --file "artifacts/buildkite/release-dry-run/%s/%s"\n' \
        "${artifact_names[${index}]}" \
        "${artifact_platforms[${index}]}" \
        "${artifact_names[${index}]}"
    done
    printf 'wrangler r2 object put "${CTX_RELEASE_R2_BUCKET}/${CTX_RELEASE_R2_PREFIX}/install.sh" --file "scripts/install.sh"\n'
    printf 'wrangler r2 object put "${CTX_RELEASE_R2_BUCKET}/${CTX_RELEASE_R2_PREFIX}/install.ps1" --file "scripts/install.ps1"\n'
    printf 'wrangler r2 object put "${CTX_RELEASE_R2_BUCKET}/${CTX_RELEASE_R2_PREFIX}/ctx-release-metadata.env" --file "artifacts/buildkite/release-candidate/ctx-release-metadata.env"\n'
    printf 'wrangler r2 object put "${CTX_RELEASE_R2_BUCKET}/${CTX_RELEASE_R2_PREFIX}/checksums.sha256" --file "artifacts/buildkite/release-candidate/checksums.sha256"\n'
    printf 'wrangler r2 object put "${CTX_RELEASE_R2_BUCKET}/${CTX_RELEASE_R2_PREFIX}/release-candidate-manifest.json" --file "artifacts/buildkite/release-candidate/release-candidate-manifest.json"\n'
  } > "${commands}"
  chmod +x "${commands}"

  {
    printf '# R2 Release Candidate Upload Plan\n\n'
    printf '%s\n' '- Evidence class: contract_fixture'
    printf '%s\n' '- Self-test fixture: true'
    printf '%s\n' '- Publishing: false'
    printf '%s `%s`\n\n' '- Prefix:' "${prefix}"
    printf 'Cleanup staged objects before retrying this fixture.\n'
    printf 'After upload, run installer dry-runs against the approved HTTPS base URL before any installer endpoint cutover.\n'
  } > "${plan}"
}

write_contract_r2_staging_smoke() {
  local root="$1"
  local out_dir="${root}/artifacts/buildkite/r2-staging-smoke"
  local generated_at commit branch

  mkdir -p "${out_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${out_dir}/r2-staging-smoke.json" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_r2_staging_smoke",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "mode": "r2-staging-smoke",
  "status": "passed",
  "publishing": false,
  "upload_performed": false,
  "no_ctx_rs_cutover": true,
  "validated_upload_object_count": 9,
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${out_dir}/r2-staging-smoke.md" <<'EOF'
# R2 Staging Smoke Contract Fixture

- Evidence class: contract_fixture
- Self-test fixture: true
- Publishing: false
- Blocker: R2 object upload and public HTTPS smoke require approved credentials and an explicit manager-run command; normal CI validates the plan only.
EOF
}

write_contract_supply_chain_r2_proofs() {
  local root="$1"

  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/supply-chain" \
    bash scripts/release-supply-chain-proof.sh --contract-fixture
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/r2-staging-readback" \
    bash scripts/release-r2-staging-readback-proof.sh --contract-fixture
}

write_contract_provider_live_lanes() {
  local root="$1"
  local out_dir="${root}/artifacts/buildkite/provider-live-e2e-lanes"
  local generated_at commit branch

  mkdir -p "${out_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${out_dir}/provider-live-e2e-lanes.json" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_lane_definitions",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "publishing": false,
  "default_enabled": false,
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at},
  "lanes": []
}
EOF

  cat > "${out_dir}/provider-live-e2e-lanes.md" <<'EOF'
# Provider Live E2E Lane Definitions

- Evidence class: contract_fixture
- Self-test fixture: true
- Publishing: false
- Global opt-in: `CTX_LIVE_PROVIDER_E2E=1`
- Providers listed by the release contract include Codex, Claude Code, Gemini CLI, and OpenRouter Generated Harness.
EOF
}

write_contract_summary() {
  local root="$1"
  local rel_dir="$2"
  local mode="$3"
  local out_dir="${root}/${rel_dir}"
  local generated_at commit branch

  mkdir -p "${out_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${out_dir}/${mode}.json" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_contract_fixture_summary",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "mode": "$(ctx_json_escape "${mode}")",
  "status": "passed",
  "publishing": false,
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF
}

write_contract_finished_artifacts() {
  local root="$1"

  write_contract_summary "${root}" "artifacts/buildkite/finished-product/product-decisions" "product-decisions"
  write_contract_summary "${root}" "artifacts/buildkite/finished-product/provider-fixtures" "provider-fixtures"
  write_contract_summary "${root}" "artifacts/buildkite/finished-product/rich-search-context" "rich-search-context"
  printf '{"schema_version":1,"evidence_class":"contract_fixture","self_test_fixture":true,"kind":"rich_context_fixture"}\n' \
    > "${root}/artifacts/buildkite/finished-product/rich-search-context/rich-context.json"
  write_contract_summary "${root}" "artifacts/buildkite/finished-product/search-mvp-package-audit" "search-mvp-package-audit"
  write_contract_summary "${root}" "artifacts/buildkite/finished-product/security-archive-fixtures" "security-archive-fixtures"
  printf '# Security Archive Fixture\n\n- Publishing: false\n- Evidence class: contract_fixture\n- Self-test fixture: true\n' \
    > "${root}/artifacts/buildkite/finished-product/security-archive-fixtures/security-archive-fixtures.md"
  write_contract_summary "${root}" "artifacts/buildkite/finished-product/jj-e2e-blocker-status" "jj-e2e-blocker-status"
  printf 'jj e2e blocker contract fixture\n' \
    > "${root}/artifacts/buildkite/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.txt"
  write_contract_summary "${root}" "artifacts/buildkite/finished-product/installer-dry-run-smoke" "installer-dry-run-smoke"
  printf 'ctx install plan contract fixture; publishing false\n' \
    > "${root}/artifacts/buildkite/finished-product/installer-dry-run-smoke/install-dry-run.txt"
}

write_contract_release_docs() {
  local root="$1"
  local path

  for path in \
    docs/release-install.md \
    docs/release-supply-chain.md \
    docs/release-r2-layout.md \
    docs/freebsd-release-worker.md; do
    if [[ ! -s "${CTX_REPO_ROOT}/${path}" ]]; then
      printf 'required release documentation is missing: %s\n' "${path}" >&2
      return 1
    fi
    mkdir -p "${root}/$(dirname "${path}")"
    cp "${CTX_REPO_ROOT}/${path}" "${root}/${path}"
  done
}

write_completion_contract_fixture() {
  local root="$1"

  rm -rf "${root}"
  mkdir -p "${root}/artifacts/buildkite/pipeline-contract"
  printf 'completion certificate contract fixture; publishing false\n' \
    > "${root}/artifacts/buildkite/pipeline-contract/pipeline-contract.txt"

  write_contract_release_dry_runs "${root}"
  write_contract_release_artifact_smokes "${root}"
  write_contract_freebsd_blocker "${root}"
  write_contract_release_candidate "${root}"
  write_contract_r2_staging_smoke "${root}"
  write_contract_supply_chain_r2_proofs "${root}"
  write_contract_finished_artifacts "${root}"
  write_contract_provider_live_lanes "${root}"
  write_contract_release_docs "${root}"

  printf 'completion certificate contract fixture root: %s\n' "${root}"
}

validate_evidence() {
  validate_release_dry_run \
    "linux-x64" \
    "x86_64-unknown-linux-gnu" \
    "artifacts/buildkite/release-dry-run/linux-x64/manifest.json" \
    "artifacts/buildkite/release-dry-run/linux-x64/ctx-release-metadata.env"
  validate_release_artifact_smoke \
    "linux-x64" \
    "x86_64-unknown-linux-gnu" \
    "artifacts/buildkite/release-dry-run/linux-x64/manifest.json" \
    "artifacts/buildkite/release-dry-run/linux-x64/ctx-release-metadata.env"
  validate_release_dry_run \
    "macos-arm64" \
    "aarch64-apple-darwin" \
    "artifacts/buildkite/release-dry-run/macos-arm64/manifest.json" \
    "artifacts/buildkite/release-dry-run/macos-arm64/ctx-release-metadata.env"
  validate_release_artifact_smoke \
    "macos-arm64" \
    "aarch64-apple-darwin" \
    "artifacts/buildkite/release-dry-run/macos-arm64/manifest.json" \
    "artifacts/buildkite/release-dry-run/macos-arm64/ctx-release-metadata.env"
  validate_release_dry_run \
    "macos-x64" \
    "x86_64-apple-darwin" \
    "artifacts/buildkite/release-dry-run/macos-x64/manifest.json" \
    "artifacts/buildkite/release-dry-run/macos-x64/ctx-release-metadata.env"
  validate_release_artifact_smoke \
    "macos-x64" \
    "x86_64-apple-darwin" \
    "artifacts/buildkite/release-dry-run/macos-x64/manifest.json" \
    "artifacts/buildkite/release-dry-run/macos-x64/ctx-release-metadata.env"
  validate_release_dry_run \
    "windows-x64" \
    "x86_64-pc-windows-gnu" \
    "artifacts/buildkite/release-dry-run/windows-x64/manifest.json" \
    "artifacts/buildkite/release-dry-run/windows-x64/ctx-release-metadata.env"
  validate_release_artifact_smoke \
    "windows-x64" \
    "x86_64-pc-windows-gnu" \
    "artifacts/buildkite/release-dry-run/windows-x64/manifest.json" \
    "artifacts/buildkite/release-dry-run/windows-x64/ctx-release-metadata.env"
  validate_freebsd_release_target

  require_file "artifacts/buildkite/pipeline-contract/pipeline-contract.txt"
  validate_release_candidate_metadata
  require_summary_status "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" "r2-staging-smoke"
  require_manifest_value "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" ".kind" "ctx_r2_staging_smoke" "R2 staging smoke records kind"
  require_manifest_value "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" ".upload_performed" "false" "R2 staging smoke records non-uploading CI posture"
  require_manifest_value "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" ".no_ctx_rs_cutover" "true" "R2 staging smoke records no ctx.rs cutover"
  if completion_contract_mode || [[ ! -s "${completion_evidence_root}/artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json" ]]; then
    require_manifest_value "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" ".validated_upload_object_count" "9" "R2 staging smoke validates upload object count"
  else
    require_manifest_value "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" ".validated_upload_object_count" "10" "R2 staging smoke validates upload object count"
  fi
  require_contains "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.md" "R2 object upload and public HTTPS smoke require approved credentials" "R2 staging smoke records upload blocker"
  validate_dependency_advisory_license_audit
  validate_sbom_provenance_signature_evidence
  validate_r2_staging_readback_evidence
  require_summary_status "artifacts/buildkite/finished-product/product-decisions/product-decisions.json" "product-decisions"
  require_summary_status "artifacts/buildkite/finished-product/provider-fixtures/provider-fixtures.json" "provider-fixtures"
  validate_provider_live_e2e_lanes
  require_summary_status "artifacts/buildkite/finished-product/rich-search-context/rich-search-context.json" "rich-search-context"
  require_file "artifacts/buildkite/finished-product/rich-search-context/rich-context.json"
  require_summary_status "artifacts/buildkite/finished-product/search-mvp-package-audit/search-mvp-package-audit.json" "search-mvp-package-audit"
  require_summary_status "artifacts/buildkite/finished-product/security-archive-fixtures/security-archive-fixtures.json" "security-archive-fixtures"
  require_contains "artifacts/buildkite/finished-product/security-archive-fixtures/security-archive-fixtures.md" "Publishing: false" "security archive fixture records non-publishing status"
  require_summary_status "artifacts/buildkite/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.json" "jj-e2e-blocker-status"
  require_file "artifacts/buildkite/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.txt"
  require_summary_status "artifacts/buildkite/finished-product/installer-dry-run-smoke/installer-dry-run-smoke.json" "installer-dry-run-smoke"
  require_contains "artifacts/buildkite/finished-product/installer-dry-run-smoke/install-dry-run.txt" "ctx install plan" "installer smoke records dry-run install plan"
  validate_release_docs

  if (( certificate_failures > 0 )); then
    return 1
  fi
}

write_certificate() {
  local out_dir="$1"
  local markdown json generated_at commit branch build_url build_id job_id buildkite_branch buildkite_commit evidence_class self_test_fixture evidence_verified scaffold_verified verification_scope
  local freebsd_status freebsd_manager_exception_required

  if ! validate_evidence; then
    return 1
  fi

  mkdir -p "${out_dir}"
  markdown="${out_dir}/ctx-completion-certificate.md"
  json="${out_dir}/ctx-completion-certificate.json"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  build_url="${BUILDKITE_BUILD_URL:-local}"
  build_id="${BUILDKITE_BUILD_ID:-}"
  job_id="${BUILDKITE_JOB_ID:-}"
  buildkite_branch="${BUILDKITE_BRANCH:-}"
  buildkite_commit="${BUILDKITE_COMMIT:-}"
  evidence_class="$(completion_certificate_evidence_class)"
  self_test_fixture="$(completion_certificate_self_test_json)"
  evidence_verified="$(completion_certificate_evidence_verified_json)"
  scaffold_verified="$(completion_certificate_scaffold_verified_json)"
  verification_scope="$(completion_certificate_verification_scope)"
  freebsd_status="$(completion_freebsd_status)"
  freebsd_manager_exception_required="$(completion_freebsd_manager_exception_required_json)"

  cat > "${markdown}" <<EOF
# ctx Completion Certificate

- Schema version: \`1\`
- Program: \`ctx-release-candidate\`
- Release candidate status: \`blocked-staging-plan-only\`
- Launch ready: \`false\`
- Release approval: \`false\`
- Evidence mode: \`${completion_certificate_mode}\`
- Evidence class: \`${evidence_class}\`
- Self-test fixture: \`${self_test_fixture}\`
- Evidence verification scope: \`${verification_scope}\`
- Repository: \`ctxrs/ctx\`
- Git commit: \`${commit}\`
- Git branch: \`${branch}\`
- Buildkite build: \`${build_url}\`
- Generated at Unix seconds: \`${generated_at}\`
- Publishing status: \`false\`

## Required Release Targets

- \`linux-x64\`: required production release proof, including packaged artifact runtime smoke
- \`macos-arm64\`: required production release proof, including packaged artifact runtime smoke
- \`macos-x64\`: required production release proof, including packaged artifact runtime smoke
- \`windows-x64\`: required production release proof, including packaged artifact runtime smoke
- \`freebsd-x64\`: required production release proof through a native \`freebsd-x64\` Buildkite lane, including packaged artifact runtime smoke; status \`${freebsd_status}\`

A production release requires proof for every target above, or an explicit
manager-approved release exception that names the missing target. When the
FreeBSD native manifest and metadata are present, no manager exception is
required for \`freebsd-x64\`. Contract blocker evidence remains a self-test
fixture only and does not make FreeBSD optional.

## Required Evidence

- Pipeline contract artifact: \`artifacts/buildkite/pipeline-contract/pipeline-contract.txt\`
- Linux x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/linux-x64/manifest.json\`
- Linux x64 release dry-run install metadata: \`artifacts/buildkite/release-dry-run/linux-x64/ctx-release-metadata.env\`
- Linux x64 packaged artifact smoke: \`artifacts/buildkite/release-artifact-smoke/linux-x64/artifact-smoke.json\`
- macOS arm64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/macos-arm64/manifest.json\`
- macOS arm64 release dry-run install metadata: \`artifacts/buildkite/release-dry-run/macos-arm64/ctx-release-metadata.env\`
- macOS arm64 packaged artifact smoke: \`artifacts/buildkite/release-artifact-smoke/macos-arm64/artifact-smoke.json\`
- macOS x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/macos-x64/manifest.json\`
- macOS x64 release dry-run install metadata: \`artifacts/buildkite/release-dry-run/macos-x64/ctx-release-metadata.env\`
- macOS x64 packaged artifact smoke: \`artifacts/buildkite/release-artifact-smoke/macos-x64/artifact-smoke.json\`
- Windows x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/windows-x64/manifest.json\`
- Windows x64 release dry-run install metadata: \`artifacts/buildkite/release-dry-run/windows-x64/ctx-release-metadata.env\`
- Windows x64 packaged artifact smoke: \`artifacts/buildkite/release-artifact-smoke/windows-x64/artifact-smoke.json\`
- FreeBSD x64 release dry-run manifest: \`artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json\`
- FreeBSD x64 release dry-run install metadata: \`artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env\`
- FreeBSD x64 packaged artifact smoke, only when native evidence is present: \`artifacts/buildkite/release-artifact-smoke/freebsd-x64/artifact-smoke.json\`
- FreeBSD x64 manager exception, only if native evidence is absent: \`artifacts/buildkite/release-exceptions/freebsd-x64/freebsd-x64-exception.json\`
- FreeBSD x64 contract blocker fixture, only in contract self-test mode: \`artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json\`
- Release candidate metadata: \`artifacts/buildkite/release-candidate/ctx-release-metadata.env\`
- Release candidate R2 upload plan: \`artifacts/buildkite/release-candidate/r2-upload-plan.md\`
- R2 staging smoke artifact: \`artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json\`
- Dependency advisory/license audit: \`artifacts/buildkite/supply-chain/dependency-advisory-license-audit.json\`
- SBOM, provenance, signature, and notarization evidence: \`artifacts/buildkite/supply-chain/sbom-provenance-signature.json\`
- R2 staging upload/readback proof: \`artifacts/buildkite/r2-staging-readback/r2-staging-readback.json\`
- Product decision regression artifact: \`artifacts/buildkite/finished-product/product-decisions/product-decisions.json\`
- Provider fixture import artifact: \`artifacts/buildkite/finished-product/provider-fixtures/provider-fixtures.json\`
- Provider live E2E lane definitions: \`artifacts/buildkite/provider-live-e2e-lanes/provider-live-e2e-lanes.json\`
- Rich search artifact: \`artifacts/buildkite/finished-product/rich-search-context/rich-context.json\`
- Search MVP package/content audit: \`artifacts/buildkite/finished-product/search-mvp-package-audit/search-mvp-package-audit.json\`
- Security/malicious archive fixture artifact: \`artifacts/buildkite/finished-product/security-archive-fixtures/security-archive-fixtures.md\`
- jj e2e blocker status artifact: \`artifacts/buildkite/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.txt\`
- Installer dry-run smoke artifact: \`artifacts/buildkite/finished-product/installer-dry-run-smoke/install-dry-run.txt\`
- Release install documentation: \`docs/release-install.md\`
- Release supply-chain documentation: \`docs/release-supply-chain.md\`
- Release R2 layout documentation: \`docs/release-r2-layout.md\`
- FreeBSD release worker notes: \`docs/freebsd-release-worker.md\`

## External Release Blockers

- This certificate is not a release approval and does not certify a real public RC until every blocker below is replaced by explicit PASS evidence.
- FreeBSD is a required first-class release target. Native \`freebsd-x64\` proof must be present before production release approval unless a manager-approved release exception explicitly names \`freebsd-x64\`.
- R2 object upload and public HTTPS installer smoke require approved credentials and an explicit manager-run command; normal CI validates the staging plan only.
- Dependency advisory proof, license inventory review, SBOM, provenance, signature, notarization, and R2 readback evidence must be present before any public release approval.
- Provider live E2E lanes are defined but remain opt-in; providers cannot be marked \`supported-live\` without real lane artifacts.
- Full jj e2e validation requires a runner image with \`jj\` installed; the CI lane records availability and blocker status without installing external tools.
- Production release publication requires final release metadata with non-placeholder SHA-256 checksums for every published artifact.
- Signing, notarization, SBOM publication, and provenance publication require configured external credentials and policy approval.
EOF

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_completion_certificate",
  "program": "ctx-release-candidate",
  "release_candidate_status": "blocked-staging-plan-only",
  "launch_ready": false,
  "release_approval": false,
  "public_release_approval": false,
  "evidence_mode": "$(ctx_json_escape "${completion_certificate_mode}")",
  "evidence_class": "$(ctx_json_escape "${evidence_class}")",
  "self_test_fixture": ${self_test_fixture},
  "evidence_verification_scope": "$(ctx_json_escape "${verification_scope}")",
  "repository": "ctxrs/ctx",
  "publishing": false,
  "release_target_policy": "Production release requires linux-x64, macos-arm64, macos-x64, windows-x64, and freebsd-x64 proof, or an explicit manager-approved release exception naming each missing target.",
  "required_release_targets": [
    {
      "platform": "linux-x64",
      "required": true,
      "status_in_this_certificate": "artifact_smoke_required",
      "artifact_smoke": "artifacts/buildkite/release-artifact-smoke/linux-x64/artifact-smoke.json"
    },
    {
      "platform": "macos-arm64",
      "required": true,
      "status_in_this_certificate": "artifact_smoke_required",
      "artifact_smoke": "artifacts/buildkite/release-artifact-smoke/macos-arm64/artifact-smoke.json"
    },
    {
      "platform": "macos-x64",
      "required": true,
      "status_in_this_certificate": "artifact_smoke_required",
      "artifact_smoke": "artifacts/buildkite/release-artifact-smoke/macos-x64/artifact-smoke.json"
    },
    {
      "platform": "windows-x64",
      "required": true,
      "status_in_this_certificate": "artifact_smoke_required",
      "artifact_smoke": "artifacts/buildkite/release-artifact-smoke/windows-x64/artifact-smoke.json"
    },
    {
      "platform": "freebsd-x64",
      "target_triple": "x86_64-unknown-freebsd",
      "required": true,
      "first_class_release_target": true,
      "required_native_buildkite_queue": "freebsd-x64",
      "status_in_this_certificate": "$(ctx_json_escape "${freebsd_status}")",
      "manager_exception_required_for_public_release_without_proof": ${freebsd_manager_exception_required},
      "manifest_artifact": "artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json",
      "metadata_artifact": "artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env",
      "artifact_smoke": "artifacts/buildkite/release-artifact-smoke/freebsd-x64/artifact-smoke.json",
      "blocker_artifact": "artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json"
    }
  ],
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "buildkite_build_url": "$(ctx_json_escape "${build_url}")",
  "buildkite": {
    "build_url": "$(ctx_json_escape "${build_url}")",
    "build_id": "$(ctx_json_escape "${build_id}")",
    "job_id": "$(ctx_json_escape "${job_id}")",
    "branch": "$(ctx_json_escape "${buildkite_branch}")",
    "commit": "$(ctx_json_escape "${buildkite_commit}")"
  },
  "generated_at_unix_s": ${generated_at},
  "required_evidence": {
    "pipeline_contract": "artifacts/buildkite/pipeline-contract/pipeline-contract.txt",
    "release_dry_run_linux_x64": "artifacts/buildkite/release-dry-run/linux-x64/manifest.json",
    "release_dry_run_linux_x64_metadata": "artifacts/buildkite/release-dry-run/linux-x64/ctx-release-metadata.env",
    "release_artifact_smoke_linux_x64": "artifacts/buildkite/release-artifact-smoke/linux-x64/artifact-smoke.json",
    "release_dry_run_macos_arm64": "artifacts/buildkite/release-dry-run/macos-arm64/manifest.json",
    "release_dry_run_macos_arm64_metadata": "artifacts/buildkite/release-dry-run/macos-arm64/ctx-release-metadata.env",
    "release_artifact_smoke_macos_arm64": "artifacts/buildkite/release-artifact-smoke/macos-arm64/artifact-smoke.json",
    "release_dry_run_macos_x64": "artifacts/buildkite/release-dry-run/macos-x64/manifest.json",
    "release_dry_run_macos_x64_metadata": "artifacts/buildkite/release-dry-run/macos-x64/ctx-release-metadata.env",
    "release_artifact_smoke_macos_x64": "artifacts/buildkite/release-artifact-smoke/macos-x64/artifact-smoke.json",
    "release_dry_run_windows_x64": "artifacts/buildkite/release-dry-run/windows-x64/manifest.json",
    "release_dry_run_windows_x64_metadata": "artifacts/buildkite/release-dry-run/windows-x64/ctx-release-metadata.env",
    "release_artifact_smoke_windows_x64": "artifacts/buildkite/release-artifact-smoke/windows-x64/artifact-smoke.json",
    "release_dry_run_freebsd_x64": "artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json",
    "release_dry_run_freebsd_x64_metadata": "artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env",
    "release_artifact_smoke_freebsd_x64": "artifacts/buildkite/release-artifact-smoke/freebsd-x64/artifact-smoke.json",
    "freebsd_x64_manager_exception": "artifacts/buildkite/release-exceptions/freebsd-x64/freebsd-x64-exception.json",
    "freebsd_x64_blocker": "artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json",
    "freebsd_x64_required_target_status": "artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json",
    "release_candidate_metadata": "artifacts/buildkite/release-candidate/ctx-release-metadata.env",
    "release_candidate_manifest": "artifacts/buildkite/release-candidate/release-candidate-manifest.json",
    "release_candidate_r2_upload_plan": "artifacts/buildkite/release-candidate/r2-upload-plan.md",
    "r2_staging_smoke": "artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json",
    "dependency_advisory_license_audit": "artifacts/buildkite/supply-chain/dependency-advisory-license-audit.json",
    "sbom_provenance_signature_evidence": "artifacts/buildkite/supply-chain/sbom-provenance-signature.json",
    "r2_staging_readback": "artifacts/buildkite/r2-staging-readback/r2-staging-readback.json",
    "product_decision_regressions": "artifacts/buildkite/finished-product/product-decisions/product-decisions.json",
    "provider_fixture_import": "artifacts/buildkite/finished-product/provider-fixtures/provider-fixtures.json",
    "provider_live_e2e_lane_definitions": "artifacts/buildkite/provider-live-e2e-lanes/provider-live-e2e-lanes.json",
    "rich_search_context": "artifacts/buildkite/finished-product/rich-search-context/rich-context.json",
    "search_mvp_package_audit": "artifacts/buildkite/finished-product/search-mvp-package-audit/search-mvp-package-audit.json",
    "security_archive_fixtures": "artifacts/buildkite/finished-product/security-archive-fixtures/security-archive-fixtures.md",
    "jj_e2e_blocker_status": "artifacts/buildkite/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.txt",
    "installer_dry_run_smoke": "artifacts/buildkite/finished-product/installer-dry-run-smoke/install-dry-run.txt",
    "release_install_docs": "docs/release-install.md",
    "release_supply_chain_docs": "docs/release-supply-chain.md",
    "release_r2_layout_docs": "docs/release-r2-layout.md",
    "freebsd_release_worker_notes": "docs/freebsd-release-worker.md"
  },
  "evidence_verified": ${evidence_verified},
  "evidence_scaffold_verified": ${scaffold_verified},
  "contract_self_test_verified": ${self_test_fixture},
  "external_release_blockers": [
    "This certificate is not a release approval and does not certify a real public RC until every blocker below is replaced by explicit PASS evidence.",
    "FreeBSD is a required first-class release target. Native freebsd-x64 proof must be present before production release approval unless a manager-approved release exception explicitly names freebsd-x64.",
    "R2 object upload and public HTTPS installer smoke require approved credentials and an explicit manager-run command; normal CI validates the staging plan only.",
    "Dependency advisory proof, license inventory review, SBOM, provenance, signature, notarization, and R2 readback evidence must be present before any public release approval.",
    "Provider live E2E lanes are defined but remain opt-in; providers cannot be marked supported-live without real lane artifacts.",
    "Full jj e2e validation requires a runner image with jj installed; the CI lane records availability and blocker status without installing external tools.",
    "Production release publication requires final release metadata with non-placeholder SHA-256 checksums for every published artifact.",
    "Signing, notarization, SBOM publication, and provenance publication require configured external credentials and policy approval."
  ]
}
EOF

  printf 'completion certificate: %s\n' "${markdown}"
  printf 'completion certificate json: %s\n' "${json}"
}

main() {
  local generate_contract_fixture=0

  while (( $# > 0 )); do
    case "$1" in
      --mode=*)
        completion_certificate_mode="${1#*=}"
        completion_certificate_mode_explicit=1
        ;;
      --mode)
        if (( $# < 2 )); then
          printf '%s\n' '--mode requires a value' >&2
          return 2
        fi
        shift
        completion_certificate_mode="$1"
        completion_certificate_mode_explicit=1
        ;;
      --contract-self-test|--self-test-contract)
        completion_certificate_mode="contract-self-test"
        completion_certificate_mode_explicit=1
        ;;
      --evidence-root=*)
        completion_evidence_root="${1#*=}"
        completion_evidence_root_explicit=1
        ;;
      --evidence-root)
        if (( $# < 2 )); then
          printf '%s\n' '--evidence-root requires a path' >&2
          return 2
        fi
        shift
        completion_evidence_root="$1"
        completion_evidence_root_explicit=1
        ;;
      --artifact-dir=*)
        CTX_ARTIFACT_DIR="${1#*=}"
        ;;
      --artifact-dir)
        if (( $# < 2 )); then
          printf '%s\n' '--artifact-dir requires a path' >&2
          return 2
        fi
        shift
        CTX_ARTIFACT_DIR="$1"
        ;;
      -h|--help|help)
        usage
        return 0
        ;;
      *)
        printf 'unknown completion certificate option: %s\n' "$1" >&2
        usage >&2
        return 2
        ;;
    esac
    shift
  done

  if [[ "${CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES:-0}" == "1" \
    && "${completion_certificate_mode}" == "release-evidence" \
    && "${completion_certificate_mode_explicit}" == "0" ]]; then
    completion_certificate_mode="contract-self-test"
  fi

  case "${completion_certificate_mode}" in
    release-evidence|contract-self-test)
      ;;
    *)
      printf 'unknown completion certificate mode: %s\n' "${completion_certificate_mode}" >&2
      usage >&2
      return 2
      ;;
  esac

  cd "${CTX_REPO_ROOT}"
  CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/completion-certificate}"
  if completion_contract_mode && (( completion_evidence_root_explicit == 0 )); then
    completion_evidence_root="${CTX_COMPLETION_CONTRACT_FIXTURE_ROOT:-${CTX_ARTIFACT_DIR}/contract-evidence}"
    generate_contract_fixture=1
  fi

  ctx_timing_init
  trap ctx_timing_finish EXIT

  if (( generate_contract_fixture == 1 )); then
    ctx_run_timed "completion-certificate-contract-fixture" write_completion_contract_fixture "${completion_evidence_root}"
  fi

  ctx_run_timed "release-completion-certificate" write_certificate "${CTX_ARTIFACT_DIR}"
}

main "$@"
