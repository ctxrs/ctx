#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: scripts/install.sh --metadata PATH_OR_URL [--platform PLATFORM] [--bin-dir DIR] [--no-setup]

Installs the ctx binary from explicit release metadata with pinned SHA-256
checksums, then runs ctx setup to index discovered local history. The installer
never evaluates remote scripts or metadata as shell.

Options:
  --metadata PATH_OR_URL  Required. Local metadata file or HTTPS URL.
  --platform PLATFORM    linux-x64, macos-arm64, macos-x64, or freebsd-x64.
                         Defaults to the current host when it can be detected.
  --bin-dir DIR          Install directory. Defaults to $HOME/.local/bin.
  --no-setup             Install only; do not run ctx setup after install.
  --dry-run              Validate metadata and print the planned install.
  -h, --help             Show this help.

Local launch pattern:
  curl -fsSLO https://example.invalid/ctx/install.sh
  bash install.sh --metadata ./ctx-release-metadata.env
USAGE
}

fail() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

detect_platform() {
  local os arch

  os="$(uname -s 2>/dev/null || printf unknown)"
  arch="$(uname -m 2>/dev/null || printf unknown)"

  case "${os}:${arch}" in
    Linux:x86_64|Linux:amd64)
      printf 'linux-x64'
      ;;
    Darwin:arm64|Darwin:aarch64)
      printf 'macos-arm64'
      ;;
    Darwin:x86_64|Darwin:amd64)
      printf 'macos-x64'
      ;;
    FreeBSD:x86_64|FreeBSD:amd64)
      printf 'freebsd-x64'
      ;;
    *)
      return 1
      ;;
  esac
}

download_file() {
  local url="$1"
  local dest="$2"

  case "${url}" in
    https://*)
      ;;
    *)
      fail "refusing non-HTTPS download URL: ${url}"
      ;;
  esac

  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --connect-timeout 20 "${url}" -o "${dest}"
    return 0
  fi

  if command -v wget >/dev/null 2>&1; then
    wget --https-only -q -O "${dest}" "${url}"
    return 0
  fi

  fail "curl or wget is required for HTTPS downloads"
}

read_metadata_source() {
  local source="$1"
  local dest="$2"

  case "${source}" in
    https://*)
      download_file "${source}" "${dest}"
      ;;
    http://*)
      fail "refusing insecure metadata URL: ${source}"
      ;;
    *)
      test -f "${source}" || fail "metadata file not found: ${source}"
      cp "${source}" "${dest}"
      ;;
  esac
}

metadata_value() {
  local file="$1"
  local key="$2"
  local line value

  line="$(awk -F= -v key="${key}" '
    $0 ~ /^[[:space:]]*#/ { next }
    $1 == key { print; found = 1; exit }
    END { if (!found) exit 1 }
  ' "${file}")" || return 1
  value="${line#*=}"
  value="${value%$'\r'}"
  printf '%s\n' "${value}"
}

validate_safe_value() {
  local name="$1"
  local value="$2"

  case "${value}" in
    *$'\n'*|*$'\r'*|*".."*|*"/"*|*"\\"*)
      fail "unsafe ${name}: ${value}"
      ;;
  esac
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

  fail "sha256sum, shasum, or sha256 is required"
}

metadata_source=""
platform=""
bin_dir="${HOME:-}/.local/bin"
dry_run=0
run_setup=1

while (($# > 0)); do
  case "$1" in
    --metadata)
      shift
      metadata_source="${1:-}"
      ;;
    --platform)
      shift
      platform="${1:-}"
      ;;
    --bin-dir)
      shift
      bin_dir="${1:-}"
      ;;
    --no-setup)
      run_setup=0
      ;;
    --dry-run)
      dry_run=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
  shift || true
done

test -n "${metadata_source}" || fail "--metadata is required"
test -n "${bin_dir}" || fail "--bin-dir cannot be empty"

if [[ -z "${platform}" ]]; then
  platform="$(detect_platform)" || fail "cannot detect this host platform; pass --platform"
fi

case "${platform}" in
  linux-x64|macos-arm64|macos-x64|freebsd-x64)
    ;;
  *)
    fail "unsupported platform: ${platform}"
    ;;
esac

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-install.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT
metadata_file="${tmp_dir}/metadata.env"
read_metadata_source "${metadata_source}" "${metadata_file}"

schema_version="$(metadata_value "${metadata_file}" CTX_RELEASE_SCHEMA_VERSION)" || fail "metadata missing CTX_RELEASE_SCHEMA_VERSION"
version="$(metadata_value "${metadata_file}" CTX_RELEASE_VERSION)" || fail "metadata missing CTX_RELEASE_VERSION"
base_url="$(metadata_value "${metadata_file}" CTX_RELEASE_BASE_URL)" || fail "metadata missing CTX_RELEASE_BASE_URL"
platform_key="${platform//-/_}"
artifact="$(metadata_value "${metadata_file}" "CTX_RELEASE_ARTIFACT_${platform_key}")" || fail "metadata missing artifact for ${platform}"
checksum="$(metadata_value "${metadata_file}" "CTX_RELEASE_SHA256_${platform_key}")" || fail "metadata missing checksum for ${platform}"

[[ "${schema_version}" == "1" ]] || fail "unsupported metadata schema: ${schema_version}"
[[ "${base_url}" == https://* ]] || fail "metadata base URL must be HTTPS"
[[ "${checksum}" =~ ^[0-9a-fA-F]{64}$ ]] || fail "checksum for ${platform} is not a SHA-256 hex digest"
[[ "${checksum}" != "0000000000000000000000000000000000000000000000000000000000000000" ]] || fail "checksum for ${platform} is a placeholder"
validate_safe_value "artifact name" "${artifact}"

artifact_url="${base_url%/}/${artifact}"
download_path="${tmp_dir}/${artifact}"
install_name="ctx"
case "${artifact}" in
  *.exe)
    install_name="ctx.exe"
    ;;
esac
install_path="${bin_dir%/}/${install_name}"

printf 'ctx install plan: version=%s platform=%s artifact=%s bin=%s\n' \
  "${version}" "${platform}" "${artifact}" "${install_path}"

if ((dry_run)); then
  exit 0
fi

download_file "${artifact_url}" "${download_path}"
actual_checksum="$(sha256_file "${download_path}")"
if [[ "${actual_checksum,,}" != "${checksum,,}" ]]; then
  fail "checksum mismatch for ${artifact}: expected ${checksum}, got ${actual_checksum}"
fi

mkdir -p "${bin_dir}"
install -m 0755 "${download_path}" "${install_path}"
printf 'installed ctx to %s\n' "${install_path}"

if [[ "${CTX_INSTALL_NO_SETUP:-0}" == "1" ]]; then
  run_setup=0
fi

if ((run_setup)); then
  setup_progress="${CTX_SETUP_PROGRESS:-auto}"
  printf 'running ctx setup to index local history (pass --no-setup or set CTX_INSTALL_NO_SETUP=1 to skip next time)\n'
  "${install_path}" setup --progress "${setup_progress}"
else
  printf 'setup skipped; run %s setup to index local history\n' "${install_path}"
fi
