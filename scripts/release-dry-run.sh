#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

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

  printf 'sha256sum or shasum is required\n' >&2
  return 1
}

build_host_release() {
  local cargo_locked_args=()

  ctx_require_host_triple "${CTX_EXPECT_HOST_TRIPLE:-}" || return $?
  ctx_ensure_rust_toolchain

  if [[ "${CTX_CARGO_LOCKED:-1}" != "0" && -f Cargo.lock ]]; then
    cargo_locked_args+=(--locked)
  fi

  cargo build --workspace --release --bins "${cargo_locked_args[@]}"
}

write_manifest() {
  local out_dir="$1"
  local version platform platform_key target_triple expected_host_triple host_triple commit branch suffix source_bin artifact artifact_rel checksum bytes manifest checksum_file metadata_file generated_at

  version="$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' crates/ctx-cli/Cargo.toml)"
  host_triple="$(ctx_detect_host_triple)"
  expected_host_triple="${CTX_EXPECT_HOST_TRIPLE:-}"
  target_triple="${CTX_RELEASE_TARGET_TRIPLE:-${host_triple}}"
  platform="${CTX_RELEASE_PLATFORM:-host-${host_triple}}"
  platform_key="${platform//-/_}"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  suffix="$(ctx_host_exe_suffix)"
  source_bin="target/release/ctx${suffix}"

  if [[ ! -f "${source_bin}" ]]; then
    printf 'expected host binary missing: %s\n' "${source_bin}" >&2
    return 1
  fi

  artifact="ctx-${version}-${target_triple}${suffix}"
  artifact_rel="${out_dir}/${artifact}"
  cp "${source_bin}" "${artifact_rel}"
  chmod 0755 "${artifact_rel}" 2>/dev/null || true

  checksum="$(sha256_file "${artifact_rel}")"
  bytes="$(wc -c < "${artifact_rel}" | tr -d '[:space:]')"
  generated_at="$(date +%s)"
  manifest="${out_dir}/manifest.json"
  checksum_file="${out_dir}/checksums.sha256"
  metadata_file="${out_dir}/ctx-release-metadata.env"

  printf '%s  %s\n' "${checksum}" "${artifact}" > "${checksum_file}"
  cat > "${metadata_file}" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=${version}
CTX_RELEASE_CHANNEL=dry-run
CTX_RELEASE_BASE_URL=https://github.com/ctxrs/ctx/releases/download/v${version}
CTX_RELEASE_ARTIFACT_${platform_key}=${artifact}
CTX_RELEASE_SHA256_${platform_key}=${checksum}
EOF

  cat > "${manifest}" <<EOF
{
  "schema_version": 1,
  "dry_run": true,
  "upload": false,
  "package": "ctx",
  "version": "$(ctx_json_escape "${version}")",
  "platform": "$(ctx_json_escape "${platform}")",
  "target_triple": "$(ctx_json_escape "${target_triple}")",
  "host_triple": "$(ctx_json_escape "${host_triple}")",
  "expected_host_triple": "$(ctx_json_escape "${expected_host_triple}")",
  "git_commit": "$(ctx_json_escape "${commit}")",
  "git_branch": "$(ctx_json_escape "${branch}")",
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

  printf 'release dry-run manifest: %s\n' "${manifest}"
  printf 'release dry-run checksums: %s\n' "${checksum_file}"
  printf 'release dry-run install metadata: %s\n' "${metadata_file}"
}

cd "${CTX_REPO_ROOT}"
ctx_init_resource_env
CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-target/ctx-artifacts/release-dry-run}"
mkdir -p "${CTX_ARTIFACT_DIR}"
ctx_timing_init
trap ctx_timing_finish EXIT
ctx_print_resource_env

ctx_run_timed "release-dry-run-build-host" build_host_release
ctx_run_timed "release-dry-run-manifest" write_manifest "${CTX_ARTIFACT_DIR}"
