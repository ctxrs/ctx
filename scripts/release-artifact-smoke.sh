#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/release-artifact-smoke.sh PLATFORM [RELEASE_DRY_RUN_DIR]

Installs or extracts the exact staged release artifact for PLATFORM into a
temporary bin directory, then runs:

  ctx --version
  ctx setup
  ctx import --provider codex --path tests/fixtures/provider-history/codex-sessions --json
  ctx search onboarding --json
  ctx doctor --json
  ctx validate --json

The script writes non-publishing runtime smoke evidence under CTX_ARTIFACT_DIR.
USAGE
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
  ' "${path}"
}

platform_spec() {
  case "$1" in
    linux-x64)
      printf '%s|%s|%s\n' "linux_x64" "x86_64-unknown-linux-gnu" ""
      ;;
    macos-arm64)
      printf '%s|%s|%s\n' "macos_arm64" "aarch64-apple-darwin" ""
      ;;
    macos-x64)
      printf '%s|%s|%s\n' "macos_x64" "x86_64-apple-darwin" ""
      ;;
    freebsd-x64)
      printf '%s|%s|%s\n' "freebsd_x64" "x86_64-unknown-freebsd" ""
      ;;
    *)
      printf 'unsupported release artifact smoke platform for Unix runner: %s\n' "$1" >&2
      return 2
      ;;
  esac
}

to_repo_relative() {
  local path="$1"
  local parent full prefix

  case "${path}" in
    /*)
      parent="$(cd "$(dirname "${path}")" && pwd)"
      full="${parent}/$(basename "${path}")"
      prefix="${CTX_REPO_ROOT%/}/"
      if [[ "${full}" == "${prefix}"* ]]; then
        printf '%s\n' "${full#"${prefix}"}"
      else
        printf '%s\n' "${path}"
      fi
      ;;
    *)
      printf '%s\n' "${path#./}"
      ;;
  esac
}

find_extracted_ctx() {
  local extract_dir="$1"
  local suffix="$2"
  local candidate

  candidate="$(find "${extract_dir}" -type f -name "ctx${suffix}" | sort | head -n1)"
  [[ -n "${candidate}" ]] || return 1
  printf '%s\n' "${candidate}"
}

install_staged_artifact() {
  local artifact_path="$1"
  local bin_dir="$2"
  local suffix="$3"
  local extract_dir installed

  mkdir -p "${bin_dir}"
  case "${artifact_path}" in
    *.tar.gz|*.tgz)
      extract_dir="${bin_dir}/extract"
      mkdir -p "${extract_dir}"
      tar -xzf "${artifact_path}" -C "${extract_dir}"
      installed="$(find_extracted_ctx "${extract_dir}" "${suffix}")"
      chmod 0755 "${installed}" 2>/dev/null || true
      printf '%s|%s\n' "tar" "${installed}"
      ;;
    *.tar.xz)
      extract_dir="${bin_dir}/extract"
      mkdir -p "${extract_dir}"
      tar -xJf "${artifact_path}" -C "${extract_dir}"
      installed="$(find_extracted_ctx "${extract_dir}" "${suffix}")"
      chmod 0755 "${installed}" 2>/dev/null || true
      printf '%s|%s\n' "tar" "${installed}"
      ;;
    *.zip)
      command -v unzip >/dev/null 2>&1 || {
        printf 'unzip is required to smoke test zip artifact: %s\n' "${artifact_path}" >&2
        return 1
      }
      extract_dir="${bin_dir}/extract"
      mkdir -p "${extract_dir}"
      unzip -q "${artifact_path}" -d "${extract_dir}"
      installed="$(find_extracted_ctx "${extract_dir}" "${suffix}")"
      chmod 0755 "${installed}" 2>/dev/null || true
      printf '%s|%s\n' "zip" "${installed}"
      ;;
    *)
      installed="${bin_dir}/ctx${suffix}"
      cp "${artifact_path}" "${installed}"
      chmod 0755 "${installed}" 2>/dev/null || true
      printf '%s|%s\n' "direct-binary-copy" "${installed}"
      ;;
  esac
}

run_ctx_smoke_step() {
  local name="$1"
  local ctx_bin="$2"
  local home_dir="$3"
  local data_root="$4"
  local stdout="$5"
  local stderr="$6"
  shift 6

  env \
    -u OPENAI_API_KEY \
    -u ANTHROPIC_API_KEY \
    -u GEMINI_API_KEY \
    -u GOOGLE_API_KEY \
    -u AZURE_OPENAI_API_KEY \
    HOME="${home_dir}" \
    CTX_DATA_ROOT="${data_root}" \
    "${ctx_bin}" "$@" >"${stdout}" 2>"${stderr}" || {
      printf 'release artifact smoke command failed (%s): %s %s\n' "${name}" "${ctx_bin}" "$*" >&2
      printf 'stdout: %s\nstderr: %s\n' "${stdout}" "${stderr}" >&2
      return 1
    }
}

write_artifact_smoke_evidence() {
  local platform="$1"
  local platform_key="$2"
  local target_triple="$3"
  local release_dir="$4"
  local artifact="$5"
  local artifact_path="$6"
  local artifact_checksum="$7"
  local artifact_bytes="$8"
  local metadata="$9"
  local manifest="${10}"
  local install_method="${11}"
  local installed_bin="${12}"
  local fixture="${13}"
  local command_dir="${14}"
  local version_output="${15}"
  local out_dir="${16}"
  local smoke_json smoke_md generated_at commit branch host_triple release_dir_rel artifact_rel metadata_rel manifest_rel command_dir_rel

  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  host_triple="$(ctx_detect_host_triple 2>/dev/null || true)"
  release_dir_rel="$(to_repo_relative "${release_dir}")"
  artifact_rel="$(to_repo_relative "${artifact_path}")"
  metadata_rel="$(to_repo_relative "${metadata}")"
  manifest_rel="$(to_repo_relative "${manifest}")"
  command_dir_rel="$(to_repo_relative "${command_dir}")"
  smoke_json="${out_dir}/artifact-smoke.json"
  smoke_md="${out_dir}/artifact-smoke.md"

  cat > "${smoke_json}" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_release_artifact_smoke",
  "mode": "release-artifact-smoke",
  "status": "passed",
  "publishing": false,
  "platform": "$(ctx_json_escape "${platform}")",
  "platform_key": "$(ctx_json_escape "${platform_key}")",
  "target_triple": "$(ctx_json_escape "${target_triple}")",
  "host_triple": "$(ctx_json_escape "${host_triple}")",
  "release_dry_run_dir": "$(ctx_json_escape "${release_dir_rel}")",
  "release_manifest": "$(ctx_json_escape "${manifest_rel}")",
  "release_metadata": "$(ctx_json_escape "${metadata_rel}")",
  "release_artifact": "$(ctx_json_escape "${artifact_rel}")",
  "release_artifact_name": "$(ctx_json_escape "${artifact}")",
  "release_artifact_sha256": "$(ctx_json_escape "${artifact_checksum}")",
  "release_artifact_bytes": ${artifact_bytes},
  "install_method": "$(ctx_json_escape "${install_method}")",
  "installed_artifact_runtime": true,
  "fixture": "$(ctx_json_escape "${fixture}")",
  "command_output_dir": "$(ctx_json_escape "${command_dir_rel}")",
  "version_output": "$(ctx_json_escape "${version_output}")",
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

  cat > "${smoke_md}" <<EOF
# ctx Release Artifact Smoke

- Publishing: false
- Platform: \`${platform}\`
- Target triple: \`${target_triple}\`
- Release artifact: \`${artifact_rel}\`
- SHA-256: \`${artifact_checksum}\`
- Install method: \`${install_method}\`
- Fixture: \`${fixture}\`
- Commands: \`ctx --version\`, \`ctx setup\`, \`ctx import\`, \`ctx search\`, \`ctx doctor\`, \`ctx validate\`
- Status: passed
EOF

  printf 'release artifact smoke: %s\n' "${smoke_json}"
  printf 'release artifact smoke notes: %s\n' "${smoke_md}"
}

run_release_artifact_smoke() {
  local platform="$1"
  local release_dir="$2"
  local spec platform_key target_triple suffix metadata manifest version artifact checksum artifact_path actual_checksum artifact_bytes
  local temp_root bin_dir install_result install_method installed_bin home_dir data_root fixture command_dir version_output

  spec="$(platform_spec "${platform}")"
  IFS='|' read -r platform_key target_triple suffix <<<"${spec}"

  ctx_require_host_triple "${CTX_EXPECT_HOST_TRIPLE:-${target_triple}}"

  metadata="${release_dir}/ctx-release-metadata.env"
  manifest="${release_dir}/manifest.json"
  [[ -s "${metadata}" ]] || {
    printf 'release artifact smoke metadata is missing: %s\n' "${metadata}" >&2
    return 1
  }
  [[ -s "${manifest}" ]] || {
    printf 'release artifact smoke manifest is missing: %s\n' "${manifest}" >&2
    return 1
  }

  version="$(env_value "${metadata}" CTX_RELEASE_VERSION)"
  artifact="$(env_value "${metadata}" "CTX_RELEASE_ARTIFACT_${platform_key}")"
  checksum="$(env_value "${metadata}" "CTX_RELEASE_SHA256_${platform_key}")"
  artifact_path="${release_dir}/${artifact}"
  [[ -s "${artifact_path}" ]] || {
    printf 'release artifact smoke artifact is missing or empty: %s\n' "${artifact_path}" >&2
    return 1
  }
  actual_checksum="$(sha256_file "${artifact_path}")"
  if [[ "${actual_checksum}" != "${checksum}" ]]; then
    printf 'release artifact smoke checksum mismatch for %s: metadata %s, file %s\n' \
      "${artifact_path}" "${checksum}" "${actual_checksum}" >&2
    return 1
  fi
  artifact_bytes="$(wc -c < "${artifact_path}" | tr -d '[:space:]')"

  temp_root="$(mktemp -d "${TMPDIR}/ctx-release-artifact-smoke.${platform}.XXXXXX")"
  bin_dir="${temp_root}/bin"
  home_dir="${temp_root}/home"
  data_root="${temp_root}/data-root"
  command_dir="${CTX_ARTIFACT_DIR}/commands"
  fixture="tests/fixtures/provider-history/codex-sessions"
  mkdir -p "${home_dir}" "${data_root}" "${command_dir}"

  install_result="$(install_staged_artifact "${artifact_path}" "${bin_dir}" "${suffix}")"
  IFS='|' read -r install_method installed_bin <<<"${install_result}"
  [[ -x "${installed_bin}" || -f "${installed_bin}" ]] || {
    printf 'installed ctx artifact is missing: %s\n' "${installed_bin}" >&2
    return 1
  }

  run_ctx_smoke_step "version" "${installed_bin}" "${home_dir}" "${data_root}" \
    "${command_dir}/version.stdout" "${command_dir}/version.stderr" --version
  version_output="$(tr -d '\r' < "${command_dir}/version.stdout" | head -n1)"
  if [[ "${version_output}" != *"${version}"* ]]; then
    printf 'ctx --version output does not contain release version %s: %s\n' "${version}" "${version_output}" >&2
    return 1
  fi

  run_ctx_smoke_step "setup" "${installed_bin}" "${home_dir}" "${data_root}" \
    "${command_dir}/setup.stdout" "${command_dir}/setup.stderr" setup
  run_ctx_smoke_step "import" "${installed_bin}" "${home_dir}" "${data_root}" \
    "${command_dir}/import.stdout" "${command_dir}/import.stderr" import --provider codex --path "${fixture}" --json
  run_ctx_smoke_step "search" "${installed_bin}" "${home_dir}" "${data_root}" \
    "${command_dir}/search.stdout" "${command_dir}/search.stderr" search onboarding --json
  run_ctx_smoke_step "doctor" "${installed_bin}" "${home_dir}" "${data_root}" \
    "${command_dir}/doctor.stdout" "${command_dir}/doctor.stderr" doctor --json
  run_ctx_smoke_step "validate" "${installed_bin}" "${home_dir}" "${data_root}" \
    "${command_dir}/validate.stdout" "${command_dir}/validate.stderr" validate --json

  write_artifact_smoke_evidence \
    "${platform}" \
    "${platform_key}" \
    "${target_triple}" \
    "${release_dir}" \
    "${artifact}" \
    "${artifact_path}" \
    "${checksum}" \
    "${artifact_bytes}" \
    "${metadata}" \
    "${manifest}" \
    "${install_method}" \
    "${installed_bin}" \
    "${fixture}" \
    "${command_dir}" \
    "${version_output}" \
    "${CTX_ARTIFACT_DIR}"
}

main() {
  local platform="${1:-${CTX_RELEASE_PLATFORM:-}}"
  local release_dir

  case "${platform}" in
    -h|--help|help|"")
      usage
      return 0
      ;;
  esac

  cd "${CTX_REPO_ROOT}"
  ctx_init_resource_env
  release_dir="${2:-${CTX_RELEASE_DRY_RUN_DIR:-artifacts/buildkite/release-dry-run/${platform}}}"
  CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-artifacts/buildkite/release-artifact-smoke/${platform}}"
  mkdir -p "${CTX_ARTIFACT_DIR}"
  ctx_timing_init
  trap ctx_timing_finish EXIT
  ctx_print_resource_env

  ctx_run_timed "release-artifact-smoke-${platform}" run_release_artifact_smoke "${platform}" "${release_dir}"
}

main "$@"
