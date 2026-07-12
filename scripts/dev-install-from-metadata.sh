#!/usr/bin/env bash
set -euo pipefail

expected_onnxruntime_version="1.27.0"

usage() {
  cat <<'USAGE'
usage: scripts/dev-install-from-metadata.sh --metadata PATH_OR_URL [--artifact-dir DIR] [--platform PLATFORM] [--bin-dir DIR] [--runtime-dir DIR] [--no-runtime] [--no-modify-path] [--no-setup] [--no-skill] [--skill-agent AGENT] [--all-skill-agents] [--no-man]

Development/CI installer for explicit ctx release metadata.

For normal user installs, use:
  curl -fsSL https://ctx.rs/install | sh

This script is for release testing, CI smoke tests, and local artifact
validation. It installs the ctx binary from explicit metadata with pinned
SHA-256 checksums, installs the bundled ctx agent skill, then runs ctx setup to
index discovered local history. It is not the production hosted installer.

The production hosted installer is https://cli.ctx.rs/install and verifies
detached metadata signatures before trusting artifact URLs or checksums.

Options:
  --metadata PATH_OR_URL  Required. Local metadata file or HTTPS URL.
  --artifact-dir DIR      Read checksum-pinned artifacts from this local
                         directory instead of downloading them. Development
                         and native release smoke use only.
  --platform PLATFORM    linux-x64, linux-aarch64, macos-arm64, macos-x64,
                         or freebsd-x64.
                         Defaults to the current host when it can be detected.
  --bin-dir DIR          Install directory. Defaults to
                         ${CTX_BIN_DIR:-$HOME/.local/bin}.
  --runtime-dir DIR      ONNX Runtime sidecar install directory. Defaults to
                         ${CTX_RUNTIME_DIR:-$HOME/.ctx/runtime}.
  --no-runtime           Do not install optional ONNX Runtime sidecar metadata.
  --no-modify-path       Do not update shell startup files when the install
                         directory is not on PATH.
  --no-setup             Install only; do not install the skill or run ctx setup
                         unless a skill flag is also passed.
  --no-skill             Do not install the bundled ctx agent skill.
  --skill-agent AGENT    Install the skill into a specific agent skill dir.
                         Repeat for multiple agents.
  --all-skill-agents     Install the skill into all supported agent skill dirs.
  --no-man               Do not install generated man pages.
  --man-dir DIR          Man page directory. Defaults to $HOME/.local/share/man/man1.
  --dry-run              Validate metadata and print the planned install.
  -h, --help             Show this help.

Local launch pattern:
  bash scripts/dev-install-from-metadata.sh --metadata ./ctx-release-metadata.env
USAGE
}

fail() {
  printf 'dev-install-from-metadata.sh: %s\n' "$*" >&2
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
    Linux:aarch64|Linux:arm64)
      printf 'linux-aarch64'
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

fetch_artifact() {
  local url="$1"
  local name="$2"
  local dest="$3"
  local source

  if [[ -z "${artifact_dir}" ]]; then
    download_file "${url}" "${dest}"
    return
  fi
  source="${artifact_dir%/}/${name}"
  [[ -f "${source}" && ! -L "${source}" ]] || \
    fail "local artifact is missing or not a regular non-symlink file: ${source}"
  cp "${source}" "${dest}"
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

metadata_value_optional() {
  local file="$1"
  local key="$2"

  metadata_value "${file}" "${key}" 2>/dev/null || true
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

lowercase() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

json_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

shell_double_quote_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/`/\\`/g; s/\$/\\$/g'
}

runtime_manifest_escape() {
  json_escape "$1"
}

path_contains_dir() {
  local dir="${1%/}"
  local entry old_ifs path_entries

  old_ifs="${IFS}"
  IFS=:
  read -r -a path_entries <<< "${PATH:-}"
  IFS="${old_ifs}"
  for entry in "${path_entries[@]}"; do
    if [[ "${entry%/}" == "${dir}" ]]; then
      return 0
    fi
  done
  return 1
}

profile_has_path_line() {
  local profile="$1"
  local needle="$2"

  awk -v needle="${needle}" '
    $0 ~ /^[[:space:]]*#/ { next }
    index($0, needle) && ($0 ~ /PATH/ || $0 ~ /fish_user_paths/ || $0 ~ /fish_add_path/) {
      found = 1
      exit
    }
    END { exit found ? 0 : 1 }
  ' "${profile}"
}

path_setup_profile() {
  local shell_name="$1"

  test -n "${HOME:-}" || return 1

  case "${shell_name}" in
    fish)
      printf '%s/.config/fish/config.fish\n' "${HOME}"
      ;;
    zsh)
      printf '%s/.zshrc\n' "${ZDOTDIR:-${HOME}}"
      ;;
    bash)
      if [[ -f "${HOME}/.bashrc" ]]; then
        printf '%s/.bashrc\n' "${HOME}"
      elif [[ -f "${HOME}/.bash_profile" ]]; then
        printf '%s/.bash_profile\n' "${HOME}"
      elif [[ -f "${HOME}/.profile" ]]; then
        printf '%s/.profile\n' "${HOME}"
      elif [[ "${platform}" == macos-* ]]; then
        printf '%s/.bash_profile\n' "${HOME}"
      else
        printf '%s/.bashrc\n' "${HOME}"
      fi
      ;;
    *)
      printf '%s/.profile\n' "${HOME}"
      ;;
  esac
}

profile_contains_path_setup() {
  local profile="$1"
  local dir="${2%/}"
  local dir_escaped home_prefix rel

  [[ -f "${profile}" ]] || return 1
  profile_has_path_line "${profile}" "${dir}" && return 0
  dir_escaped="$(shell_double_quote_escape "${dir}")"
  profile_has_path_line "${profile}" "${dir_escaped}" && return 0

  if [[ -n "${HOME:-}" ]]; then
    home_prefix="${HOME%/}/"
    if [[ "${dir}" == "${home_prefix}"* ]]; then
      rel="${dir#"${home_prefix}"}"
      profile_has_path_line "${profile}" "\$HOME/${rel}" && return 0
      profile_has_path_line "${profile}" "~/${rel}" && return 0
    fi
  fi

  return 1
}

path_setup_snippet() {
  local shell_name="$1"
  local dir_escaped
  dir_escaped="$(shell_double_quote_escape "$2")"

  case "${shell_name}" in
    fish)
      cat <<EOF

# ctx installer PATH setup
if not contains -- "${dir_escaped}" \$PATH
    set -gx PATH "${dir_escaped}" \$PATH
end
EOF
      ;;
    *)
      cat <<EOF

# ctx installer PATH setup
case ":\${PATH}:" in
  *":${dir_escaped}:"*) ;;
  *) export PATH="${dir_escaped}:\${PATH}" ;;
esac
EOF
      ;;
  esac
}

print_current_path_command() {
  local shell_name="$1"
  local dir_escaped
  dir_escaped="$(shell_double_quote_escape "$2")"

  case "${shell_name}" in
    fish)
      printf '  set -gx PATH "%s" $PATH\n' "${dir_escaped}"
      ;;
    *)
      printf '  export PATH="%s:${PATH}"\n' "${dir_escaped}"
      ;;
  esac
}

configure_path_if_needed() {
  local dir="${bin_dir%/}"
  local shell_name profile profile_dir

  if path_contains_dir "${dir}"; then
    return 0
  fi

  shell_name="${SHELL:-}"
  shell_name="${shell_name##*/}"
  if [[ -z "${shell_name}" ]]; then
    shell_name="sh"
  fi

  if (( ! modify_path )); then
    printf '\n%s is not on PATH; shell startup file update skipped.\n' "${dir}"
    printf 'For this shell session, run:\n'
    print_current_path_command "${shell_name}" "${dir}"
    return 0
  fi

  if [[ -n "${GITHUB_PATH:-}" ]]; then
    printf '%s\n' "${dir}" >> "${GITHUB_PATH}"
    printf '\nAdded %s to GITHUB_PATH for later GitHub Actions steps.\n' "${dir}"
    return 0
  fi

  if [[ "${CI:-}" == "1" || "${CI:-}" == "true" ]]; then
    printf '\n%s is not on PATH; CI detected, not editing shell startup files.\n' "${dir}"
    printf 'For this shell session, run:\n'
    print_current_path_command "${shell_name}" "${dir}"
    return 0
  fi

  if ! profile="$(path_setup_profile "${shell_name}")"; then
    printf '\n%s is not on PATH; HOME is unavailable, so no shell startup file was updated.\n' "${dir}"
    printf 'For this shell session, run:\n'
    print_current_path_command "${shell_name}" "${dir}"
    return 0
  fi

  printf '\n'
  if profile_contains_path_setup "${profile}" "${dir}"; then
    printf 'Found existing PATH setup for %s in %s.\n' "${dir}" "${profile}"
  else
    profile_dir="$(dirname "${profile}")"
    if mkdir -p "${profile_dir}" && path_setup_snippet "${shell_name}" "${dir}" >> "${profile}"; then
      printf 'Added ctx PATH setup to %s.\n' "${profile}"
    else
      printf 'warning: could not update shell startup file %s\n' "${profile}" >&2
    fi
  fi

  printf '%s is not on the current PATH; restart your shell or run:\n' "${dir}"
  print_current_path_command "${shell_name}" "${dir}"
  printf 'Then verify with:\n'
  printf '  ctx status\n'
}

write_install_marker() {
  local marker_path="$1"
  local metadata_url="$2"
  local source_commit="$3"
  local published_at="$4"
  local installed_at
  installed_at="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

  cat > "${marker_path}.$$" <<EOF
{
  "schema_version": 1,
  "manager": "ctx-explicit-metadata-installer",
  "metadata_trust": "explicit-unsigned",
  "install_path": "$(json_escape "${install_path}")",
  "platform": "$(json_escape "${platform}")",
  "channel": "$(json_escape "${channel}")",
  "version": "$(json_escape "${version}")",
  "sha256": "$(json_escape "${actual_checksum}")",
  "metadata_url": "$(json_escape "${metadata_url}")",
  "artifact_url": "$(json_escape "${artifact_url}")",
  "source_commit": "$(json_escape "${source_commit}")",
  "published_at": "$(json_escape "${published_at}")",
  "installed_at": "$(json_escape "${installed_at}")"
}
EOF
  mv "${marker_path}.$$" "${marker_path}"
}

extract_runtime_asset() {
  local archive="$1"
  local dest="$2"
  local name="$3"
  local runtime_platform="$4"
  local runtime_version="$5"

  [[ "${name}" == *.tar.gz ]] || fail "unsupported ONNX Runtime archive format for ${runtime_platform}: ${name}"
  command -v python3 >/dev/null 2>&1 || fail "python3 is required to safely install ONNX Runtime"
  mkdir -p "${dest}"
  python3 -I - "${archive}" "${dest}" "${runtime_platform}" "${runtime_version}" <<'PY'
import os
import posixpath
import shutil
import sys
import tarfile

archive, destination, platform, version = sys.argv[1:]
library = "libonnxruntime.dylib" if platform.startswith("macos-") else "libonnxruntime.so"
expected_files = {
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "GIT_COMMIT_ID",
    f"lib/{library}",
}
expected_entries = expected_files | {"lib"}
members = {}
total_size = 0
with tarfile.open(archive, "r:gz") as bundle:
    for member in bundle.getmembers():
        raw = member.name
        canonical = posixpath.normpath(raw.rstrip("/"))
        expected_raw = canonical
        if (
            not raw
            or "\\" in raw
            or raw.startswith("/")
            or canonical in ("", ".", "..")
            or canonical.startswith("../")
            or raw != expected_raw
        ):
            raise SystemExit(f"unsafe or non-canonical runtime archive path: {raw!r}")
        if canonical in members:
            raise SystemExit(f"duplicate runtime archive entry: {canonical}")
        if canonical not in expected_entries:
            raise SystemExit(f"unexpected runtime archive entry: {canonical}")
        if member.mode & 0o7000:
            raise SystemExit(f"unsafe permission bits on runtime archive entry: {canonical}")
        if canonical == "lib":
            if not member.isdir():
                raise SystemExit("runtime lib entry is not a directory")
        elif not member.isfile():
            raise SystemExit(f"runtime archive entry is not a regular file: {canonical}")
        total_size += member.size
        if total_size > 1024 * 1024 * 1024:
            raise SystemExit("runtime archive expands beyond the 1 GiB safety limit")
        members[canonical] = member
    if set(members) != expected_entries:
        missing = sorted(expected_entries - set(members))
        raise SystemExit("runtime archive entries do not exactly match the expected layout; missing: " + ", ".join(missing))

    version_file = bundle.extractfile(members["VERSION_NUMBER"])
    if version_file is None or version_file.read() != (version + "\n").encode():
        raise SystemExit(f"runtime VERSION_NUMBER is not exactly {version}")

    os.makedirs(os.path.join(destination, "lib"), mode=0o755, exist_ok=True)
    for entry_name in sorted(expected_files):
        source = bundle.extractfile(members[entry_name])
        if source is None:
            raise SystemExit(f"could not read runtime archive entry: {entry_name}")
        target = os.path.join(destination, *entry_name.split("/"))
        with source, open(target, "wb") as output:
            shutil.copyfileobj(source, output)
        os.chmod(target, 0o755 if entry_name.startswith("lib/") else 0o644)
PY
}

install_runtime_asset() {
  local artifact_name="$1"
  local checksum_value="$2"
  local runtime_version="$3"
  local artifact_url runtime_download actual_runtime_checksum runtime_parent runtime_path tmp_runtime_path manifest_path installed_at

  validate_safe_value "ONNX Runtime artifact name" "${artifact_name}"
  [[ "${checksum_value}" =~ ^[0-9a-fA-F]{64}$ ]] || fail "checksum for ONNX Runtime ${platform} is not a SHA-256 hex digest"
  [[ "${checksum_value}" != "0000000000000000000000000000000000000000000000000000000000000000" ]] || fail "checksum for ONNX Runtime ${platform} is a placeholder"
  test -n "${runtime_dir}" || fail "--runtime-dir cannot be empty when ONNX Runtime metadata is present"

  artifact_url="${base_url%/}/${artifact_name}"
  runtime_download="${tmp_dir}/${artifact_name}"
  fetch_artifact "${artifact_url}" "${artifact_name}" "${runtime_download}"
  actual_runtime_checksum="$(sha256_file "${runtime_download}")"
  if [[ "$(lowercase "${actual_runtime_checksum}")" != "$(lowercase "${checksum_value}")" ]]; then
    fail "checksum mismatch for ${artifact_name}: expected ${checksum_value}, got ${actual_runtime_checksum}"
  fi

  runtime_parent="${runtime_dir%/}/onnxruntime/${runtime_version}"
  runtime_path="${runtime_parent}/${platform}"
  tmp_runtime_path="${runtime_path}.tmp.$$"
  rm -rf "${tmp_runtime_path}"
  mkdir -p "${runtime_parent}"
  extract_runtime_asset "${runtime_download}" "${tmp_runtime_path}" "${artifact_name}" "${platform}" "${runtime_version}"
  rm -rf "${runtime_path}"
  mv "${tmp_runtime_path}" "${runtime_path}"

  manifest_path="${runtime_path}/ctx-runtime-install.json"
  installed_at="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  cat > "${manifest_path}.tmp" <<EOF
{
  "schema_version": 1,
  "manager": "ctx-explicit-metadata-installer",
  "metadata_trust": "explicit-unsigned",
  "runtime": "onnxruntime",
  "platform": "$(runtime_manifest_escape "${platform}")",
  "version": "$(runtime_manifest_escape "${runtime_version}")",
  "sha256": "$(runtime_manifest_escape "${actual_runtime_checksum}")",
  "artifact_url": "$(runtime_manifest_escape "${artifact_url}")",
  "installed_at": "$(runtime_manifest_escape "${installed_at}")"
}
EOF
  mv "${manifest_path}.tmp" "${manifest_path}"
  printf 'Installed ONNX Runtime sidecar: %s\n' "${runtime_path}"
}

metadata_source=""
artifact_dir=""
platform=""
bin_dir="${CTX_BIN_DIR:-${HOME:-}/.local/bin}"
runtime_dir="${CTX_RUNTIME_DIR:-${HOME:-}/.ctx/runtime}"
man_dir="${CTX_MAN_DIR:-${HOME:-}/.local/share/man/man1}"
dry_run=0
modify_path=1
run_setup=1
run_skill=1
install_runtime=1
no_skill_requested=0
explicit_skill_request=0
all_skill_agents=0
skill_agents=()
install_man=1

while (($# > 0)); do
  case "$1" in
    --metadata)
      shift
      metadata_source="${1:-}"
      ;;
    --artifact-dir)
      shift
      artifact_dir="${1:-}"
      ;;
    --platform)
      shift
      platform="${1:-}"
      ;;
    --bin-dir)
      shift
      bin_dir="${1:-}"
      ;;
    --runtime-dir)
      shift
      runtime_dir="${1:-}"
      ;;
    --no-runtime)
      install_runtime=0
      ;;
    --no-modify-path)
      modify_path=0
      ;;
    --no-setup)
      run_setup=0
      ;;
    --no-skill)
      run_skill=0
      no_skill_requested=1
      ;;
    --skill-agent)
      shift
      test -n "${1:-}" || fail "--skill-agent requires a value"
      skill_agents+=("$1")
      explicit_skill_request=1
      ;;
    --all-skill-agents)
      all_skill_agents=1
      explicit_skill_request=1
      ;;
    --no-man)
      install_man=0
      ;;
    --man-dir)
      shift
      man_dir="${1:-}"
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
test -n "${man_dir}" || fail "--man-dir cannot be empty"
if [[ -n "${artifact_dir}" ]]; then
  [[ -d "${artifact_dir}" ]] || fail "--artifact-dir is not a directory: ${artifact_dir}"
  artifact_dir="$(cd "${artifact_dir}" && pwd -P)"
fi

if [[ -z "${platform}" ]]; then
  platform="$(detect_platform)" || fail "cannot detect this host platform; pass --platform"
fi

case "${platform}" in
  linux-x64|linux-aarch64|macos-arm64|macos-x64|freebsd-x64)
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
runtime_artifact="$(metadata_value_optional "${metadata_file}" "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${platform_key}")"
runtime_checksum="$(metadata_value_optional "${metadata_file}" "CTX_RELEASE_ONNXRUNTIME_SHA256_${platform_key}")"
runtime_version="$(metadata_value_optional "${metadata_file}" CTX_RELEASE_ONNXRUNTIME_VERSION)"
channel="$(metadata_value_optional "${metadata_file}" CTX_RELEASE_CHANNEL)"
source_commit="$(metadata_value_optional "${metadata_file}" CTX_RELEASE_SOURCE_COMMIT)"
published_at="$(metadata_value_optional "${metadata_file}" CTX_RELEASE_PUBLISHED_AT)"
if [[ -z "${channel}" ]]; then
  channel="stable"
fi
[[ "${schema_version}" == "1" ]] || fail "unsupported metadata schema: ${schema_version}"
[[ "${base_url}" == https://* ]] || fail "metadata base URL must be HTTPS"
[[ "${checksum}" =~ ^[0-9a-fA-F]{64}$ ]] || fail "checksum for ${platform} is not a SHA-256 hex digest"
[[ "${checksum}" != "0000000000000000000000000000000000000000000000000000000000000000" ]] || fail "checksum for ${platform} is a placeholder"
validate_safe_value "artifact name" "${artifact}"
if [[ -n "${runtime_artifact}" || -n "${runtime_checksum}" ]]; then
  [[ -n "${runtime_artifact}" ]] || fail "metadata missing ONNX Runtime artifact for ${platform}"
  [[ -n "${runtime_checksum}" ]] || fail "metadata missing ONNX Runtime checksum for ${platform}"
  [[ -n "${runtime_version}" ]] || fail "metadata missing CTX_RELEASE_ONNXRUNTIME_VERSION"
  [[ "${runtime_version}" == "${expected_onnxruntime_version}" ]] || \
    fail "unsupported ONNX Runtime version ${runtime_version}; expected ${expected_onnxruntime_version}"
  validate_safe_value "ONNX Runtime artifact name" "${runtime_artifact}"
fi

artifact_url="${base_url%/}/${artifact}"
download_path="${tmp_dir}/${artifact}"
install_name="ctx"
case "${artifact}" in
  *.exe)
    install_name="ctx.exe"
    ;;
esac
install_path="${bin_dir%/}/${install_name}"

if [[ "${CTX_INSTALL_NO_MAN:-0}" == "1" ]]; then
  install_man=0
fi

if [[ "${CTX_INSTALL_NO_MODIFY_PATH:-0}" == "1" ]]; then
  modify_path=0
fi

if [[ "${CTX_INSTALL_NO_SETUP:-0}" == "1" ]]; then
  run_setup=0
fi

if [[ "${CTX_INSTALL_NO_RUNTIME:-0}" == "1" ]]; then
  install_runtime=0
fi

if [[ "${CTX_INSTALL_ALL_SKILL_AGENTS:-0}" == "1" ]]; then
  all_skill_agents=1
  explicit_skill_request=1
fi

if [[ -n "${CTX_INSTALL_SKILL_AGENTS:-}" ]]; then
  explicit_skill_request=1
  IFS=',' read -r -a env_skill_agents <<< "${CTX_INSTALL_SKILL_AGENTS}"
  if ((${#env_skill_agents[@]} > 0)); then
    for agent in "${env_skill_agents[@]}"; do
      agent="${agent//[[:space:]]/}"
      if [[ -n "${agent}" ]]; then
        skill_agents+=("${agent}")
      fi
    done
  fi
fi

if [[ "${CTX_INSTALL_NO_SKILL:-0}" == "1" ]]; then
  run_skill=0
  no_skill_requested=1
fi

if ((no_skill_requested && explicit_skill_request)); then
  fail "cannot combine --no-skill or CTX_INSTALL_NO_SKILL=1 with skill agent options"
fi

if ((all_skill_agents && ${#skill_agents[@]} > 0)); then
  fail "cannot combine --all-skill-agents with --skill-agent or CTX_INSTALL_SKILL_AGENTS"
fi

if ((! run_setup && ! explicit_skill_request)); then
  run_skill=0
fi

if ((dry_run)); then
  printf 'Dry run: would install ctx %s (%s)\n' "${version}" "${platform}"
else
  printf 'Installing ctx %s (%s)\n' "${version}" "${platform}"
fi
printf '  binary: %s\n' "${install_path}"
if ((install_runtime)) && [[ -n "${runtime_artifact}" ]]; then
  printf '  onnxruntime: %s\n' "${runtime_dir%/}/onnxruntime/${runtime_version}/${platform}"
elif [[ -n "${runtime_artifact}" ]]; then
  printf '  onnxruntime: skipped\n'
else
  printf '  onnxruntime: not present in metadata\n'
fi
if ((run_skill)); then
  if ((all_skill_agents)); then
    printf '  skill: all supported agents\n'
  elif ((${#skill_agents[@]} > 0)); then
    skill_agent_list="$(IFS=,; printf '%s' "${skill_agents[*]}")"
    printf '  skill: %s\n' "${skill_agent_list}"
  else
    printf '  skill: universal + detected agent folders\n'
  fi
else
  printf '  skill: skipped\n'
fi
if ((run_setup)); then
  printf '  history: index discovered sessions\n'
else
  printf '  history: skipped\n'
fi

if ((dry_run)); then
  exit 0
fi

fetch_artifact "${artifact_url}" "${artifact}" "${download_path}"
actual_checksum="$(sha256_file "${download_path}")"
if [[ "$(lowercase "${actual_checksum}")" != "$(lowercase "${checksum}")" ]]; then
  fail "checksum mismatch for ${artifact}: expected ${checksum}, got ${actual_checksum}"
fi

mkdir -p "${bin_dir}"
install -m 0755 "${download_path}" "${install_path}"

write_install_marker "${install_path}.install.json" "${metadata_source}" "${source_commit}" "${published_at}"

if ((install_runtime)) && [[ -n "${runtime_artifact}" ]]; then
  install_runtime_asset "${runtime_artifact}" "${runtime_checksum}" "${runtime_version}"
fi

if ((install_man)); then
  mkdir -p "${man_dir}"
  if "${install_path}" docs man --out "${man_dir}" >/dev/null; then
    :
  else
    printf 'warning: failed to install ctx man pages to %s\n' "${man_dir}" >&2
  fi
fi
printf '\nInstalled ctx binary.\n'

if ((run_skill)); then
  skill_args=(integrations install skills)
  if ((all_skill_agents)); then
    skill_args+=(--all-agents)
  elif ((${#skill_agents[@]} > 0)); then
    for agent in "${skill_agents[@]}"; do
      skill_args+=(--agent "${agent}")
    done
  fi
  printf '\n'
  if ! "${install_path}" "${skill_args[@]}"; then
    printf 'warning: ctx integrations install skills failed after install; run %s integrations install skills to retry\n' "${install_path}" >&2
  fi
else
  printf '\nAgent skill skipped. Run %s integrations install skills to install it later.\n' "${install_path}"
fi

if ((run_setup)); then
  setup_progress="${CTX_SETUP_PROGRESS:-auto}"
  printf '\nIndexing local agent history...\n'
  setup_status=0
  if "${install_path}" setup --progress "${setup_progress}"; then
    :
  else
    setup_status=$?
    printf 'warning: ctx setup failed after install; run %s setup --progress %s to retry\n' "${install_path}" "${setup_progress}" >&2
  fi
else
  setup_status=0
  printf '\nSetup skipped. Run %s setup to index local history.\n' "${install_path}"
fi

configure_path_if_needed

if ((setup_status != 0)); then
  exit "${setup_status}"
fi
