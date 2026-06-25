#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/release-r2-staging-readback-proof.sh [--contract-fixture] [RELEASE_CANDIDATE_DIR]

Validates the release-candidate R2 staging shape and writes upload/readback
evidence. By default this script does not upload. Real R2 upload/readback is
available only with explicit manager approval:

  CTX_RELEASE_R2_UPLOAD_READBACK=1 CTX_RELEASE_R2_MANAGER_APPROVED=1 \
    scripts/release-r2-staging-readback-proof.sh artifacts/buildkite/release-candidate

The real mode stages objects with Wrangler, reads each object back, verifies
SHA-256 checksums, and still performs no ctx.rs/install cutover.
USAGE
}

readback_failures=0

fail_readback() {
  readback_failures=$(( readback_failures + 1 ))
  printf 'R2 staging readback failure: %s\n' "$*" >&2
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

env_value() {
  local path="$1"
  local key="$2"

  awk -F= -v key="${key}" '
    $0 ~ /^[[:space:]]*#/ { next }
    $1 == key {
      value = substr($0, length(key) + 2)
      sub(/\r$/, "", value)
      print value
      found = 1
      exit
    }
    END { if (!found) exit 1 }
  ' "${path}" 2>/dev/null || true
}

require_file() {
  local path="$1"

  if [[ ! -s "${path}" ]]; then
    fail_readback "required release-candidate file is missing or empty: ${path}"
  fi
}

require_contains() {
  local path="$1"
  local text="$2"
  local description="$3"

  require_file "${path}"
  if [[ -f "${path}" ]] && ! grep -F -q -- "${text}" "${path}"; then
    fail_readback "${description}: ${path} missing ${text}"
  fi
}

write_contract_fixture() {
  local out_dir="$1"
  local json markdown generated_at commit branch

  mkdir -p "${out_dir}"
  json="${out_dir}/r2-staging-readback.json"
  markdown="${out_dir}/r2-staging-readback.md"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_r2_staging_readback",
  "evidence_class": "contract_fixture",
  "self_test_fixture": true,
  "mode": "r2-staging-readback",
  "status": "blocked_manual_required",
  "publishing": false,
  "upload_performed": false,
  "readback_performed": false,
  "no_ctx_rs_cutover": true,
  "required_before_public_release": true,
  "manual_lane": true,
  "manager_approval_required": true,
  "validated_upload_object_count": 9,
  "validated_readback_object_count": 0,
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<'EOF'
# R2 Staging Readback Contract Fixture

- Evidence class: contract_fixture
- Self-test fixture: true
- Publishing: false
- Upload performed: false
- Readback performed: false
- Blocker: real R2 upload/readback requires approved credentials and an explicit manager-run command.
EOF

  printf 'R2 staging readback fixture: %s\n' "${json}"
  printf 'R2 staging readback notes fixture: %s\n' "${markdown}"
}

candidate_objects_tsv() {
  local candidate_dir="$1"
  local manifest="${candidate_dir}/release-candidate-manifest.json"
  local metadata="${candidate_dir}/ctx-release-metadata.env"
  local checksums="${candidate_dir}/checksums.sha256"
  local prefix metadata_sha checksums_sha manifest_sha

  prefix="$(env_value "${metadata}" CTX_RELEASE_R2_PREFIX)"
  metadata_sha="$(sha256_file "${metadata}")" || return 1
  checksums_sha="$(sha256_file "${checksums}")" || return 1
  manifest_sha="$(sha256_file "${manifest}")" || return 1
  if ! command -v jq >/dev/null 2>&1; then
    printf 'jq is required for real R2 object readback evidence\n' >&2
    return 1
  fi

  if ! jq -r '
    .artifacts[] | [.r2_object_key, .source_path, .sha256] | @tsv
  ' "${manifest}"; then
    return 1
  fi
  if ! jq -r '
    .installers[] | [.r2_object_key, .source_path, .sha256] | @tsv
  ' "${manifest}"; then
    return 1
  fi
  printf '%s\t%s\t%s\n' \
    "${prefix}/ctx-release-metadata.env" \
    "${metadata}" \
    "${metadata_sha}"
  printf '%s\t%s\t%s\n' \
    "${prefix}/checksums.sha256" \
    "${checksums}" \
    "${checksums_sha}"
  printf '%s\t%s\t%s\n' \
    "${prefix}/release-candidate-manifest.json" \
    "${manifest}" \
    "${manifest_sha}"
}

validate_candidate_shape() {
  local candidate_dir="$1"
  local metadata manifest checksums plan commands bucket prefix public_base_url upload_command_count artifact_lines installer_lines

  metadata="${candidate_dir}/ctx-release-metadata.env"
  manifest="${candidate_dir}/release-candidate-manifest.json"
  checksums="${candidate_dir}/checksums.sha256"
  plan="${candidate_dir}/r2-upload-plan.md"
  commands="${candidate_dir}/r2-upload-commands.sh"

  require_file "${metadata}"
  require_file "${manifest}"
  require_file "${checksums}"
  require_file "${plan}"
  require_file "${commands}"
  require_contains "${manifest}" '"publishing": false' "R2 readback manifest records non-publishing status"
  require_contains "${manifest}" '"upload_performed": false' "R2 readback manifest records no upload by metadata generation"
  require_contains "${plan}" "Cleanup staged objects" "R2 readback plan records cleanup"
  require_contains "${commands}" "wrangler r2 object put" "R2 readback commands record staging puts"

  bucket="$(env_value "${metadata}" CTX_RELEASE_R2_BUCKET)"
  prefix="$(env_value "${metadata}" CTX_RELEASE_R2_PREFIX)"
  public_base_url="$(env_value "${metadata}" CTX_RELEASE_BASE_URL)"
  if [[ -z "${bucket}" ]]; then
    fail_readback "R2 bucket is missing from release metadata"
  fi
  if [[ "${prefix}" != ctx/releases/release-candidate/v0.1.0/* ]]; then
    fail_readback "R2 prefix must use ctx/releases/release-candidate/v0.1.0/<commit>: ${prefix:-<missing>}"
  fi
  if [[ "${public_base_url}" != https://*/ctx/releases/release-candidate/v0.1.0/* ]]; then
    fail_readback "public base URL must be an HTTPS ctx/releases candidate URL: ${public_base_url:-<missing>}"
  fi
  if [[ "${prefix}" == *work-record* || "${prefix}" == *work-recorder* || "${public_base_url}" == *work-record* || "${public_base_url}" == *work-recorder* ]]; then
    fail_readback "R2 staging metadata must not use stale public product naming"
  fi
  if awk '!/^[[:space:]]*#/ && /ctx\.rs\/install|ctx\.rs\/.*install/ { found = 1 } END { exit found ? 0 : 1 }' "${commands}"; then
    fail_readback "release-candidate upload commands must not include a ctx.rs/install cutover"
  fi

  upload_command_count="$(grep -c 'wrangler r2 object put' "${commands}" || true)"
  artifact_lines="$(grep -c '^CTX_RELEASE_ARTIFACT_' "${metadata}" || true)"
  installer_lines="$(grep -c '^CTX_RELEASE_INSTALLER_' "${metadata}" || true)"
  if [[ "${upload_command_count}" != "$(( artifact_lines + installer_lines + 3 ))" ]]; then
    fail_readback "R2 command file object count does not match metadata-derived object count"
  fi
  if (( readback_failures > 0 )); then
    return 1
  fi
}

run_real_upload_readback() {
  local candidate_dir="$1"
  local bucket="$2"
  local object_tsv="$3"
  local readback_dir="$4"
  local object_key source_path expected_sha readback_path actual_sha upload_count=0 readback_count=0

  if [[ "${CTX_RELEASE_R2_MANAGER_APPROVED:-0}" != "1" ]]; then
    printf 'CTX_RELEASE_R2_UPLOAD_READBACK=1 requires CTX_RELEASE_R2_MANAGER_APPROVED=1\n' >&2
    return 1
  fi
  if ! command -v wrangler >/dev/null 2>&1; then
    printf 'wrangler is required for real R2 upload/readback proof\n' >&2
    return 1
  fi

  mkdir -p "${readback_dir}"
  while IFS=$'\t' read -r object_key source_path expected_sha; do
    if [[ -z "${object_key}" || -z "${source_path}" || -z "${expected_sha}" ]]; then
      printf 'invalid R2 object row in %s\n' "${object_tsv}" >&2
      return 1
    fi
    if [[ "${source_path}" != /* ]]; then
      source_path="${CTX_REPO_ROOT}/${source_path}"
    fi
    if [[ ! -s "${source_path}" ]]; then
      printf 'R2 source object is missing or empty: %s\n' "${source_path}" >&2
      return 1
    fi
    if ! wrangler r2 object put "${bucket}/${object_key}" --file "${source_path}"; then
      return 1
    fi
    upload_count=$(( upload_count + 1 ))

    readback_path="${readback_dir}/${object_key//\//_}"
    if ! wrangler r2 object get "${bucket}/${object_key}" --file "${readback_path}"; then
      return 1
    fi
    actual_sha="$(sha256_file "${readback_path}")" || return 1
    if [[ "${actual_sha}" != "${expected_sha}" ]]; then
      printf 'R2 readback checksum mismatch for %s: expected %s got %s\n' "${object_key}" "${expected_sha}" "${actual_sha}" >&2
      return 1
    fi
    readback_count=$(( readback_count + 1 ))
  done < "${object_tsv}"

  printf '%s %s\n' "${upload_count}" "${readback_count}" > "${candidate_dir}/.r2-readback-counts"
}

write_r2_staging_readback() {
  local candidate_dir="$1"
  local out_dir="$2"
  local metadata json markdown object_tsv bucket prefix public_base_url generated_at commit branch
  local object_count upload_count readback_count status upload_performed readback_performed manual_lane manager_approval_required
  local readback_dir

  if ! validate_candidate_shape "${candidate_dir}"; then
    return 1
  fi
  metadata="${candidate_dir}/ctx-release-metadata.env"
  bucket="$(env_value "${metadata}" CTX_RELEASE_R2_BUCKET)"
  prefix="$(env_value "${metadata}" CTX_RELEASE_R2_PREFIX)"
  public_base_url="$(env_value "${metadata}" CTX_RELEASE_BASE_URL)"

  mkdir -p "${out_dir}"
  json="${out_dir}/r2-staging-readback.json"
  markdown="${out_dir}/r2-staging-readback.md"
  object_tsv="${out_dir}/r2-staging-objects.tsv"
  if ! candidate_objects_tsv "${candidate_dir}" > "${object_tsv}"; then
    return 1
  fi
  if ! awk -F '\t' '
    NF != 3 || $1 == "" || $2 == "" || $3 !~ /^[0-9a-f]{64}$/ { bad = 1 }
    END { exit bad ? 1 : 0 }
  ' "${object_tsv}"; then
    printf 'R2 object manifest contains an invalid object key, source path, or SHA-256\n' >&2
    return 1
  fi
  object_count="$(wc -l < "${object_tsv}" | tr -d '[:space:]')"

  status="blocked_manual_required"
  upload_performed=false
  readback_performed=false
  upload_count=0
  readback_count=0
  manual_lane=true
  manager_approval_required=true

  if [[ "${CTX_RELEASE_R2_UPLOAD_READBACK:-0}" == "1" ]]; then
    readback_dir="${out_dir}/readback"
    if ! run_real_upload_readback "${candidate_dir}" "${bucket}" "${object_tsv}" "${readback_dir}"; then
      return 1
    fi
    if ! read -r upload_count readback_count < "${candidate_dir}/.r2-readback-counts"; then
      printf 'R2 upload/readback did not write object counts\n' >&2
      return 1
    fi
    rm -f "${candidate_dir}/.r2-readback-counts"
    if [[ "${upload_count}" != "${object_count}" || "${readback_count}" != "${object_count}" ]]; then
      printf 'R2 upload/readback count mismatch: planned %s uploaded %s read back %s\n' \
        "${object_count}" "${upload_count}" "${readback_count}" >&2
      return 1
    fi
    status="passed"
    upload_performed=true
    readback_performed=true
    manual_lane=false
    manager_approval_required=false
  fi

  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_r2_staging_readback",
  "mode": "r2-staging-readback",
  "status": "${status}",
  "publishing": false,
  "upload_performed": ${upload_performed},
  "readback_performed": ${readback_performed},
  "no_ctx_rs_cutover": true,
  "required_before_public_release": true,
  "manual_lane": ${manual_lane},
  "manager_approval_required": ${manager_approval_required},
  "bucket": "$(ctx_json_escape "${bucket}")",
  "prefix": "$(ctx_json_escape "${prefix}")",
  "public_base_url": "$(ctx_json_escape "${public_base_url}")",
  "object_manifest": "r2-staging-objects.tsv",
  "validated_upload_object_count": ${object_count},
  "validated_readback_object_count": ${readback_count},
  "actual_upload_object_count": ${upload_count},
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${markdown}" <<EOF
# R2 Staging Readback

- Publishing: false
- Upload performed: \`${upload_performed}\`
- Readback performed: \`${readback_performed}\`
- Bucket: \`${bucket}\`
- Prefix: \`${prefix}\`
- Planned objects: \`${object_count}\`
- Readback-verified objects: \`${readback_count}\`
- Cutover status: no \`ctx.rs/install\` redirect or site publication is performed.
- Blocker: real R2 upload/readback requires approved credentials and an explicit manager-run command when status is \`blocked_manual_required\`.
EOF

  printf 'R2 staging readback: %s\n' "${json}"
  printf 'R2 staging object manifest: %s\n' "${object_tsv}"
  printf 'R2 staging readback notes: %s\n' "${markdown}"
}

main() {
  local mode="collect"
  local candidate_dir

  case "${1:-}" in
    -h|--help|help)
      usage
      return 0
      ;;
    --contract-fixture)
      mode="contract-fixture"
      shift
      ;;
  esac

  cd "${CTX_REPO_ROOT}"
  CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/r2-staging-readback}"
  ctx_timing_init
  trap ctx_timing_finish EXIT

  if [[ "${mode}" == "contract-fixture" ]]; then
    ctx_run_timed "release-r2-staging-readback-contract-fixture" write_contract_fixture "${CTX_ARTIFACT_DIR}"
  else
    candidate_dir="${1:-artifacts/buildkite/release-candidate}"
    ctx_run_timed "release-r2-staging-readback" write_r2_staging_readback "${candidate_dir}" "${CTX_ARTIFACT_DIR}"
  fi
}

main "$@"
