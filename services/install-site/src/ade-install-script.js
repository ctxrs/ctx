export const ADE_FUNCTIONS_BASE = "https://api.ade.ctx.rs/functions/v1";
const DEFAULT_CHANNEL = "stable";

export function renderAdeInstallScript({
  functionsBase = ADE_FUNCTIONS_BASE,
  channel = DEFAULT_CHANNEL,
  downloadId = "",
  referrerDomain = "",
  utmSource = "",
  utmMedium = "",
  utmCampaign = "",
} = {}) {
  const normalizedBase = String(functionsBase).replace(/\/+$/, "");
  const normalizedChannel = String(channel);
  const normalizedDownloadId = String(downloadId).trim();
  const normalizedReferrerDomain = String(referrerDomain).trim();
  const normalizedUtmSource = String(utmSource).trim();
  const normalizedUtmMedium = String(utmMedium).trim();
  const normalizedUtmCampaign = String(utmCampaign).trim();
  return `#!/bin/sh
set -eu

log() {
  printf '%s\\n' "$*" >&2
}

fail() {
  log "error: $*"
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

need_cmd curl
need_cmd mktemp
need_cmd find
need_cmd awk
need_cmd uname
need_cmd mv

functions_base="\${CTX_FUNCTIONS_BASE:-${normalizedBase}}"
channel="\${CTX_CHANNEL:-${normalizedChannel}}"
download_id="\${CTX_DOWNLOAD_ID:-${normalizedDownloadId}}"
referrer_domain="\${CTX_INSTALL_REFERRER_DOMAIN:-${normalizedReferrerDomain}}"
utm_source="\${CTX_INSTALL_UTM_SOURCE:-${normalizedUtmSource}}"
utm_medium="\${CTX_INSTALL_UTM_MEDIUM:-${normalizedUtmMedium}}"
utm_campaign="\${CTX_INSTALL_UTM_CAMPAIGN:-${normalizedUtmCampaign}}"
manifest_url_base="\${functions_base%/}/releases/$channel/latest.json"
os="$(uname -s)"
arch="$(uname -m)"

tmp_dir="$(mktemp -d "\${TMPDIR:-/tmp}/ctx-install.XXXXXX")"
mount_dir="$tmp_dir/mount"
manifest_json="$tmp_dir/latest.json"
mounted=0
cleanup_stage_path=""
cleanup_backup_path=""

cleanup() {
  if [ "$mounted" = "1" ]; then
    if command -v hdiutil >/dev/null 2>&1; then
      hdiutil detach "$mount_dir" -quiet >/dev/null 2>&1 || true
    fi
  fi
  if [ -n "$cleanup_stage_path" ] && [ -e "$cleanup_stage_path" ]; then
    rm -rf "$cleanup_stage_path"
  fi
  if [ -n "$cleanup_backup_path" ] && [ -e "$cleanup_backup_path" ]; then
    rm -rf "$cleanup_backup_path"
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

unique_target_sibling_path() {
  target_path="$1"
  label="$2"
  candidate="$target_path.$label.$$"
  index=0
  while [ -e "$candidate" ]; do
    index=$((index + 1))
    candidate="$target_path.$label.$$.\${index}"
  done
  printf "%s\\n" "$candidate"
}

stage_path_for_target() {
  unique_target_sibling_path "$1" "ctx-stage"
}

backup_path_for_target() {
  unique_target_sibling_path "$1" "ctx-backup"
}

promote_staged_path() {
  staged_path="$1"
  target_path="$2"
  target_label="$3"
  backup_path=""

  cleanup_stage_path="$staged_path"
  cleanup_backup_path=""

  if [ -e "$target_path" ]; then
    backup_path="$(backup_path_for_target "$target_path")"
    mv "$target_path" "$backup_path" || fail "failed to move existing $target_label aside"
    cleanup_backup_path="$backup_path"
  fi

  if mv "$staged_path" "$target_path"; then
    cleanup_stage_path=""
    if [ -n "$backup_path" ] && [ -e "$backup_path" ]; then
      rm -rf "$backup_path" || fail "failed to remove previous $target_label backup"
    fi
    cleanup_backup_path=""
    return 0
  fi

  if [ -n "$backup_path" ] && [ -e "$backup_path" ]; then
    mv "$backup_path" "$target_path" || fail "failed to restore previous $target_label after upgrade failure"
    cleanup_backup_path=""
  fi

  fail "failed to install $target_label"
}

append_query_param() {
  input_url="$1"
  key="$2"
  value="$3"
  if [ -z "$value" ]; then
    printf "%s\\n" "$input_url"
    return
  fi
  separator="?"
  case "$input_url" in
    *\\?*) separator="&" ;;
  esac
  printf "%s%s%s=%s\\n" "$input_url" "$separator" "$key" "$value"
}

append_release_attribution() {
  input_url="$1"
  output_url="$(append_query_param "$input_url" "ctx_download_id" "$download_id")"
  output_url="$(append_query_param "$output_url" "referrer_domain" "$referrer_domain")"
  output_url="$(append_query_param "$output_url" "utm_source" "$utm_source")"
  output_url="$(append_query_param "$output_url" "utm_medium" "$utm_medium")"
  output_url="$(append_query_param "$output_url" "utm_campaign" "$utm_campaign")"
  printf "%s\\n" "$output_url"
}

first_open_start_path() {
  output_path="/"
  output_path="$(append_query_param "$output_path" "ctx_download_id" "$download_id")"
  printf "%s\\n" "$output_path"
}

extract_manifest_field_macos() {
  key="$1"
  plutil -extract "$key" raw -o - "$manifest_json" 2>/dev/null || true
}

extract_manifest_field_linux() {
  key="$1"
  PYTHONHOME= PYTHONPATH= python3 - "$manifest_json" "$key" <<'PY'
import json
import sys

manifest_path = sys.argv[1]
key_path = sys.argv[2]
parts = key_path.split(".")
with open(manifest_path, "r", encoding="utf-8") as f:
    data = json.load(f)
for part in parts:
    if isinstance(data, dict) and part in data:
        data = data[part]
    else:
        print("")
        raise SystemExit(0)
if isinstance(data, (str, int, float, bool)):
    print(data)
else:
    print("")
PY
}

resolve_download_url() {
  url_path="$1"
  case "$url_path" in
    http://*|https://*) resolved_url="$url_path" ;;
    /*) resolved_url="\${functions_base%/}$url_path" ;;
    *) resolved_url="\${functions_base%/}/$url_path" ;;
  esac
  append_release_attribution "$resolved_url"
}

sha256_of_file() {
  file_path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file_path" | awk '{print $1}'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file_path" | awk '{print $1}'
    return 0
  fi
  fail "missing required command: sha256sum (or shasum)"
}

verify_artifact_sha() {
  artifact_path="$1"
  expected_sha="$2"
  if [ -z "$expected_sha" ]; then
    fail "manifest missing sha256 for selected artifact"
  fi
  actual_sha="$(sha256_of_file "$artifact_path")"
  if [ "$actual_sha" != "$expected_sha" ]; then
    fail "sha256 mismatch (expected $expected_sha, got $actual_sha)"
  fi
  log "Verified artifact sha256"
}

launch_linux_appimage() {
  appimage_path="$1"
  start_path="$2"
  CTX_DESKTOP_START_PATH="$start_path" "$appimage_path" >/dev/null 2>&1 &
}

write_linux_launcher_script() {
  launcher_path="$1"
  appimage_path="$2"
  printf '%s\n%s\nexec "%s" "$@"\n' \
    '#!/bin/sh' \
    'export CTX_DESKTOP_START_PATH=/' \
    "$appimage_path" >"$launcher_path"
  chmod 0755 "$launcher_path"
}

linux_icon_path() {
  printf "%s\\n" "\${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps/ctx.png"
}

extract_linux_icon() {
  appimage_path="$1"
  output_icon_path="$2"
  extract_dir="$tmp_dir/appimage-icon"

  rm -rf "$extract_dir"
  mkdir -p "$extract_dir"
  if ! (
    cd "$extract_dir"
    "$appimage_path" --appimage-extract >/dev/null 2>&1
  ); then
    fail "failed to extract application icon from AppImage"
  fi

  icon_source=""
  if [ -f "$extract_dir/squashfs-root/usr/share/icons/hicolor/512x512/apps/ctx.png" ]; then
    icon_source="$extract_dir/squashfs-root/usr/share/icons/hicolor/512x512/apps/ctx.png"
  elif [ -f "$extract_dir/squashfs-root/.DirIcon" ]; then
    icon_source="$extract_dir/squashfs-root/.DirIcon"
  else
    icon_source="$(find "$extract_dir/squashfs-root/usr/share/icons" -type f -name '*.png' 2>/dev/null | head -n 1 || true)"
  fi

  if [ -z "$icon_source" ] || [ ! -f "$icon_source" ]; then
    fail "failed to locate application icon in AppImage contents"
  fi

  mkdir -p "\${output_icon_path%/*}"
  cp "$icon_source" "$output_icon_path"
  chmod 0644 "$output_icon_path"
}

promote_linux_icon() {
  source_icon_path="$1"
  icon_path="$(linux_icon_path)"
  staged_icon="$(stage_path_for_target "$icon_path")"

  mkdir -p "\${staged_icon%/*}"
  cp "$source_icon_path" "$staged_icon"
  chmod 0644 "$staged_icon"
  promote_staged_path "$staged_icon" "$icon_path" "ctx desktop icon"
  log "Installed app icon at $icon_path"
  printf "%s\\n" "$icon_path"
}

install_linux_desktop_entry() {
  launcher_path="$1"
  appimage_path="$2"
  icon_path="$3"
  desktop_entry_dir="\${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  desktop_entry_path="$desktop_entry_dir/ctx.desktop"

  mkdir -p "$desktop_entry_dir"
  cat >"$desktop_entry_path" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=ctx
Comment=ctx desktop app
TryExec=$appimage_path
Exec=$launcher_path
Icon=$icon_path
Terminal=false
Categories=Development;
StartupNotify=true
EOF
  chmod 0644 "$desktop_entry_path"

  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$desktop_entry_dir" >/dev/null 2>&1 || true
  fi

  log "Installed desktop entry at $desktop_entry_path"
}

install_macos() {
  local_arch="$1"
  need_cmd hdiutil
  need_cmd plutil
  need_cmd ditto
  need_cmd open

  case "$local_arch" in
    arm64) platform="macos-arm64" ;;
    x86_64) platform="macos-x64" ;;
    *) fail "unsupported macOS architecture: $local_arch" ;;
  esac

  url_path="$(extract_manifest_field_macos "platforms.$platform.desktop.url_path")"
  if [ -z "$url_path" ]; then
    url_path="$(extract_manifest_field_macos "platforms.$platform.dmg.url_path")"
  fi
  [ -n "$url_path" ] || fail "manifest does not contain a desktop dmg for $platform"

  expected_sha="$(extract_manifest_field_macos "platforms.$platform.desktop.sha256")"
  if [ -z "$expected_sha" ]; then
    expected_sha="$(extract_manifest_field_macos "platforms.$platform.dmg.sha256")"
  fi

  artifact_path="$tmp_dir/ctx-installer.dmg"
  download_url="$(resolve_download_url "$url_path")"
  log "Downloading $download_url"
  curl -fL --retry 3 --retry-delay 2 "$download_url" -o "$artifact_path"
  verify_artifact_sha "$artifact_path" "$expected_sha"

  mkdir -p "$mount_dir"
  log "Mounting DMG"
  hdiutil attach "$artifact_path" -nobrowse -readonly -mountpoint "$mount_dir" >/dev/null
  mounted=1

  app_src="$(find "$mount_dir" -maxdepth 2 -type d -name '*.app' | head -n 1)"
  [ -n "$app_src" ] || fail "no .app bundle found in downloaded DMG"
  app_name="$(basename "$app_src")"

  install_root="\${CTX_INSTALL_DIR:-/Applications}"
  if [ -z "\${CTX_INSTALL_DIR:-}" ]; then
    probe_file="$install_root/.ctx-write-probe-$$"
    if ! touch "$probe_file" 2>/dev/null; then
      install_root="$HOME/Applications"
      mkdir -p "$install_root"
    else
      rm -f "$probe_file"
    fi
  else
    mkdir -p "$install_root"
  fi

  target_app="$install_root/$app_name"
  staged_app="$(stage_path_for_target "$target_app")"
  ditto "$app_src" "$staged_app"
  promote_staged_path "$staged_app" "$target_app" "$app_name"

  log "Installed $app_name to $target_app"
  if [ "\${CTX_INSTALL_NO_OPEN:-0}" != "1" ]; then
    open "$target_app"
    log "Launched $app_name"
  fi
}

install_linux() {
  local_arch="$1"
  need_cmd python3
  need_cmd chmod
  need_cmd cp

  case "$local_arch" in
    x86_64|amd64) platform="linux-x64" ;;
    aarch64|arm64) platform="linux-arm64" ;;
    *) fail "unsupported Linux architecture: $local_arch" ;;
  esac

  url_path="$(extract_manifest_field_linux "platforms.$platform.appimage.url_path")"
  [ -n "$url_path" ] || fail "manifest does not contain an AppImage for $platform"

  expected_sha="$(extract_manifest_field_linux "platforms.$platform.appimage.sha256")"

  artifact_path="$tmp_dir/ctx-installer.AppImage"
  download_url="$(resolve_download_url "$url_path")"
  log "Downloading $download_url"
  curl -fL --retry 3 --retry-delay 2 "$download_url" -o "$artifact_path"
  verify_artifact_sha "$artifact_path" "$expected_sha"

  install_root="\${CTX_INSTALL_DIR:-$HOME/.local/share/ctx}"
  mkdir -p "$install_root"
  target_appimage="$install_root/ctx.AppImage"
  staged_appimage="$(stage_path_for_target "$target_appimage")"
  cp "$artifact_path" "$staged_appimage"
  chmod +x "$staged_appimage"
  cleanup_stage_path="$staged_appimage"

  icon_candidate="$tmp_dir/ctx-icon.png"
  extract_linux_icon "$staged_appimage" "$icon_candidate"
  promote_staged_path "$staged_appimage" "$target_appimage" "ctx desktop AppImage"
  icon_path="$(promote_linux_icon "$icon_candidate")"

  bin_dir="\${CTX_BIN_DIR:-$HOME/.local/bin}"
  mkdir -p "$bin_dir"
  launcher_path="$bin_dir/ctx-desktop"
  write_linux_launcher_script "$launcher_path" "$target_appimage"
  install_linux_desktop_entry "$launcher_path" "$target_appimage" "$icon_path"

  log "Installed ctx desktop AppImage to $target_appimage"
  log "Installed launcher script at $launcher_path"

  if [ "\${CTX_INSTALL_NO_OPEN:-0}" != "1" ]; then
    launch_linux_appimage "$target_appimage" "$(first_open_start_path)"
    log "Launched ctx desktop"
  fi
}

install_windows() {
  fail "windows support is coming soon!"
}

manifest_url="$(append_release_attribution "$manifest_url_base")"

log "Resolving latest release manifest from $manifest_url"
curl -fsSL "$manifest_url" -o "$manifest_json"

case "$os" in
  Darwin) install_macos "$arch" ;;
  Linux) install_linux "$arch" ;;
  MINGW*|MSYS*|CYGWIN*) install_windows ;;
  *) fail "unsupported operating system: $os" ;;
esac

log "Done."
`;
}
