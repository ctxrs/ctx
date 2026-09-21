import {
  generateInstallAttemptId,
  normalizeEmbeddedInstallAttemptId,
} from "./install-attempt-id.js";
import {
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
} from "./install-stage-contract.js";
import {
  renderHostedUninstallTransactionHelpers,
  renderHostedUninstallVersionHelpers,
} from "./uninstall-script-transaction.js";

const DEFAULT_FUNCTIONS_BASE = "https://cli.ctx.rs/functions/v1";

export function renderUninstallScript({
  functionsBase = DEFAULT_FUNCTIONS_BASE,
  installAttemptId = generateInstallAttemptId(),
} = {}) {
  const normalizedBase = String(functionsBase).replace(/\/+$/, "");
  const normalizedInstallAttemptId = normalizeEmbeddedInstallAttemptId(installAttemptId);
  return `#!/bin/sh
set -eu

log() {
  printf '%s\\n' "$*" >&2
}

fail() {
  log "error: $*"
  exit 1
}

legacy_control_truthy() {
  legacy_value="$1"
  while :; do
    case "$legacy_value" in
      [[:space:]]*) legacy_value="\${legacy_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$legacy_value" in
      *[[:space:]]) legacy_value="\${legacy_value%?}" ;;
      *) break ;;
    esac
  done
  case "$legacy_value" in
    ""|0|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff]) return 1 ;;
    *) return 0 ;;
  esac
}

canonical_analytics_disabled() {
  analytics_value="\${CTX_ANALYTICS_ENABLED-}"
  while :; do
    case "$analytics_value" in
      [[:space:]]*) analytics_value="\${analytics_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$analytics_value" in
      *[[:space:]]) analytics_value="\${analytics_value%?}" ;;
      *) break ;;
    esac
  done
  case "$analytics_value" in
    0|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff]) return 0 ;;
    *) return 1 ;;
  esac
}

deprecated_control_warning=
note_deprecated_control() {
  deprecated_mapping="$1 -> $2"
  if [ -n "$deprecated_control_warning" ]; then
    deprecated_control_warning="$deprecated_control_warning; $deprecated_mapping"
  else
    deprecated_control_warning="$deprecated_mapping"
  fi
}

apply_deprecated_analytics_controls() {
  if [ "\${CTX_ANALYTICS_OFF+x}" = x ]; then
    note_deprecated_control CTX_ANALYTICS_OFF CTX_ANALYTICS_ENABLED=false
    if legacy_control_truthy "$CTX_ANALYTICS_OFF"; then
      CTX_ANALYTICS_ENABLED=false
      export CTX_ANALYTICS_ENABLED
    fi
  fi
  if [ "\${CTX_DISABLE_ANALYTICS+x}" = x ]; then
    note_deprecated_control CTX_DISABLE_ANALYTICS CTX_ANALYTICS_ENABLED=false
    if legacy_control_truthy "$CTX_DISABLE_ANALYTICS"; then
      CTX_ANALYTICS_ENABLED=false
      export CTX_ANALYTICS_ENABLED
    fi
  fi
  if [ "\${CTX_INSTALL_DIAGNOSTICS_OFF+x}" = x ]; then
    note_deprecated_control CTX_INSTALL_DIAGNOSTICS_OFF CTX_ANALYTICS_ENABLED=false
    if legacy_control_truthy "$CTX_INSTALL_DIAGNOSTICS_OFF"; then
      CTX_ANALYTICS_ENABLED=false
      export CTX_ANALYTICS_ENABLED
    fi
  fi
  unset CTX_ANALYTICS_OFF CTX_DISABLE_ANALYTICS CTX_INSTALL_DIAGNOSTICS_OFF
  if [ -n "$deprecated_control_warning" ]; then
    log "warning: deprecated environment variables detected: $deprecated_control_warning. Update your environment to use the replacements."
  fi
}

valid_install_attempt_id() {
  attempt_id_value="$1"
  case "$attempt_id_value" in
    ia_*) ;;
    *) return 1 ;;
  esac
  case "$attempt_id_value" in
    *[!A-Za-z0-9_-]*) return 1 ;;
  esac
  attempt_id_length="\${#attempt_id_value}"
  [ "$attempt_id_length" -ge 11 ] && [ "$attempt_id_length" -le 131 ]
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

need_cmd awk
need_cmd id
need_cmd mktemp
need_cmd rm
need_cmd stat
need_cmd uname

functions_base="${normalizedBase}"
install_attempt_id="${normalizedInstallAttemptId}"
if [ -n "\${CTX_INSTALL_ATTEMPT_ID-}" ] && valid_install_attempt_id "$CTX_INSTALL_ATTEMPT_ID"; then
  install_attempt_id="$CTX_INSTALL_ATTEMPT_ID"
fi
unset CTX_INSTALL_ATTEMPT_ID
apply_deprecated_analytics_controls

data_choice=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --delete-data)
      [ -z "$data_choice" ] || fail "choose exactly one of --delete-data or --keep-data"
      data_choice="--delete-data"
      ;;
    --keep-data)
      [ -z "$data_choice" ] || fail "choose exactly one of --delete-data or --keep-data"
      data_choice="--keep-data"
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
  shift
done

os="$(uname -s)"
machine_arch="$(uname -m)"
case "$os" in
  Darwin) telemetry_platform="macos" ;;
  Linux) telemetry_platform="linux" ;;
  MINGW*|MSYS*|CYGWIN*) fail "Windows requires the hosted PowerShell uninstaller at https://ctx.rs/uninstall.ps1" ;;
  *) fail "unsupported operating system: $os" ;;
esac
case "$machine_arch" in
  x86_64|amd64|AMD64) telemetry_arch="x64" ;;
  aarch64|arm64|ARM64) telemetry_arch="arm64" ;;
  *) fail "unsupported architecture: $machine_arch" ;;
esac
home_dir="$HOME"
bin_dir="\${CTX_BIN_DIR:-\${HOME:-}/.local/bin}"
install_path="\${CTX_UNINSTALL_INSTALL_PATH:-$bin_dir/ctx}"
marker_path="\${CTX_UNINSTALL_MARKER_PATH:-$install_path.install.json}"
man_dir="\${CTX_MAN_DIR:-\${HOME:-}/.local/share/man/man1}"
data_dir="\${CTX_DATA_ROOT:-$home_dir/.ctx}"
daemon_teardown_output=

install_stage_delivery_enabled=1
report_install_stage() {
  report_stage="$1"
  report_status="$2"
  [ "$install_stage_delivery_enabled" = "1" ] || return 0
  canonical_analytics_disabled && return 0
  command -v curl >/dev/null 2>&1 || return 0
  case "$functions_base" in
    https://*) ;;
    *) return 0 ;;
  esac
  payload='{"event_name":"${INSTALL_STAGE_EVENT_NAME}","event_version":${INSTALL_STAGE_EVENT_VERSION},"install_attempt_id":"'"$install_attempt_id"'","stage":"'"$report_stage"'","status":"'"$report_status"'","platform":"'"$telemetry_platform"'","arch":"'"$telemetry_arch"'","script_family":"posix"}'
  if ! curl -fsS --connect-timeout 1 --max-time 1 -H "content-type: application/json" -X POST --data "$payload" "\${functions_base%/}/install-attempt" >/dev/null 2>&1; then
    install_stage_delivery_enabled=0
  fi
  return 0
}

uninstall_terminal_reported=0
report_install_stage "uninstall" "started"
report_uninstall_failure() {
  status="$?"
  trap - EXIT
  if [ -n "$daemon_teardown_output" ]; then
    rm -f "$daemon_teardown_output"
  fi
  if [ "$status" -ne 0 ] && [ "$uninstall_terminal_reported" = "0" ]; then
    report_install_stage "uninstall" "failed"
  fi
  exit "$status"
}
trap report_uninstall_failure EXIT

path_owner_uid() {
  stat -c '%u' "$1" 2>/dev/null || stat -f '%u' "$1" 2>/dev/null
}

path_link_count() {
  stat -c '%h' "$1" 2>/dev/null || stat -f '%l' "$1" 2>/dev/null
}

path_size_bytes() {
  stat -c '%s' "$1" 2>/dev/null || stat -f '%z' "$1" 2>/dev/null
}

sha256_file() {
  digest_path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$digest_path" | awk '{ print $1 }'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$digest_path" | awk '{ print $1 }'
    return 0
  fi
  if command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$digest_path"
    return 0
  fi
  fail "sha256sum, shasum, or sha256 is required"
}

valid_sha256() {
  digest_value="$1"
  [ "\${#digest_value}" -eq 64 ] || return 1
  case "$digest_value" in
    *[!0-9a-f]*) return 1 ;;
  esac
}

json_escape() {
  printf '%s' "$1" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g'
}

json_string_field() {
  json_path="$1"
  json_field="$2"
  awk -v field="$json_field" '
    $0 ~ "^  \\\"" field "\\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*,?[[:space:]]*$/, "", value)
      result = value
    }
    END {
      if (count != 1) exit 1
      print result
    }
  ' "$json_path"
}

json_optional_string_field() {
  json_path="$1"
  json_field="$2"
  awk -v field="$json_field" '
    $0 ~ "^  \\\"" field "\\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*,?[[:space:]]*$/, "", value)
      result = value
    }
    END {
      if (count > 1) exit 1
      if (count == 1) print result
    }
  ' "$json_path"
}

json_scalar_field() {
  json_path="$1"
  json_field="$2"
  awk -v field="$json_field" '
    $0 ~ "^  \\\"" field "\\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/[[:space:]]*,?[[:space:]]*$/, "", value)
      result = value
    }
    END {
      if (count != 1) exit 1
      print result
    }
  ' "$json_path"
}

validate_core_daemon_teardown_result() {
  daemon_json_path="$1"
  expected_requested_root="$(json_escape "$data_dir")"
  awk -v expected_requested_root="$expected_requested_root" '
    function trim(value) {
      sub(/^[[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      return value
    }
    BEGIN {
      expected["schema_version"] = "1"
      expected["command"] = "\\"daemon_prepare_uninstall\\""
      expected["ok"] = "true"
      expected["scope"] = "\\"installation\\""
      expected["installation_quiescent"] = "true"
      expected["daemon_enabled"] = "false"
      expected["daemon_running"] = "false"
      expected["owner_lock_released"] = "true"
      expected["endpoint_released"] = "true"
      expected["supervisor_removed"] = "true"
      expected["coordination_state_removed"] = "true"
      expected["binary_retained"] = "true"
      expected["retry_safe"] = "true"
      expected["local_only"] = "true"
      expected_count = 18
    }
    {
      line = trim($0)
      if (line == "") next
      if (in_roots) {
        if (line == "]" || line == "],") {
          in_roots = 0
          roots_closed = 1
          next
        }
        value = line
        sub(/,[[:space:]]*$/, "", value)
        if (value !~ /^".*"$/) {
          invalid = 1
          next
        }
        roots_count++
        if (value == "\\"" expected_requested_root "\\"") requested_root_seen = 1
        if (value == canonical_root) canonical_root_seen = 1
        next
      }
      if (line == "{") {
        if (opened || closed) invalid = 1
        opened = 1
        next
      }
      if (line == "}") {
        if (!opened || closed) invalid = 1
        closed = 1
        next
      }
      if (!opened || closed || line !~ /^"[A-Za-z_]+"[[:space:]]*:/) {
        invalid = 1
        next
      }
      key = line
      sub(/^"/, "", key)
      sub(/".*$/, "", key)
      value = line
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/,[[:space:]]*$/, "", value)
      value = trim(value)
      if (seen[key]) {
        invalid = 1
        next
      }
      seen[key] = 1
      seen_count++
      if (key in expected) {
        if (value != expected[key]) invalid = 1
      } else if (key == "requested_data_root") {
        if (value != "\\"" expected_requested_root "\\"") invalid = 1
      } else if (key == "canonical_data_root") {
        if (value !~ /^".+"$/) {
          invalid = 1
        } else {
          canonical_root = value
        }
      } else if (key == "quiesced_roots") {
        if (value != "[") {
          invalid = 1
        } else {
          in_roots = 1
        }
      } else if (key == "quiesced_root_count") {
        if (value !~ /^(0|[1-9][0-9]*)$/) {
          invalid = 1
        } else {
          declared_roots_count = value + 0
        }
      } else {
        invalid = 1
      }
    }
    END {
      if (invalid || in_roots || !roots_closed || !opened || !closed ||
          seen_count != expected_count || roots_count < 1 ||
          declared_roots_count != roots_count ||
          !requested_root_seen || !canonical_root_seen) exit 1
      for (key in expected) {
        if (!seen[key]) exit 1
      }
    }
  ' "$daemon_json_path"
}

canonical_file_path() {
  requested_path="$1"
  case "$requested_path" in
    /*) ;;
    *) return 1 ;;
  esac
  requested_name="\${requested_path##*/}"
  requested_parent="\${requested_path%/*}"
  [ -n "$requested_name" ] && [ "$requested_parent" != "$requested_path" ] || return 1
  resolved_parent="$(cd -P "$requested_parent" 2>/dev/null && pwd -P)" || return 1
  printf '%s/%s\\n' "\${resolved_parent%/}" "$requested_name"
}

validate_managed_install() {
  [ -f "$install_path" ] && [ ! -L "$install_path" ] && [ -x "$install_path" ] ||
    fail "the installed ctx executable is absent, replaced, or not a regular executable"
  [ -f "$marker_path" ] && [ ! -L "$marker_path" ] ||
    fail "a regular managed install marker is required; package-manager and unmanaged binaries must use their own uninstall flow"

  canonical_install_path="$(canonical_file_path "$install_path")" ||
    fail "could not resolve the installed ctx path"
  [ "$canonical_install_path" = "$install_path" ] ||
    fail "the requested ctx executable path is not canonical"
  expected_marker_path="$canonical_install_path.install.json"
  [ "$marker_path" = "$expected_marker_path" ] ||
    fail "managed install marker must be the canonical adjacent marker"

  current_uid="$(id -u)"
  [ "$(path_owner_uid "$install_path")" = "$current_uid" ] ||
    fail "the installed ctx executable is not owned by the current user"
  [ "$(path_owner_uid "$marker_path")" = "$current_uid" ] ||
    fail "the managed install marker is not owned by the current user"
  [ "$(path_link_count "$install_path")" = "1" ] ||
    fail "the installed ctx executable must not be hard-linked"
  [ "$(path_link_count "$marker_path")" = "1" ] ||
    fail "the managed install marker must not be hard-linked"
  marker_size="$(path_size_bytes "$marker_path")" ||
    fail "could not determine managed install marker size"
  [ "$marker_size" -le 65536 ] ||
    fail "managed install marker exceeds the size limit"

  marker_schema="$(json_scalar_field "$marker_path" schema_version)" ||
    fail "managed install marker has an invalid schema"
  [ "$marker_schema" = "1" ] || fail "managed install marker has an unsupported schema"
  marker_manager="$(json_string_field "$marker_path" manager)" ||
    fail "managed install marker is missing its manager"
  [ "$marker_manager" = "ctx-hosted-installer" ] ||
    fail "managed install marker does not identify the ctx hosted installer"
  marker_install_path="$(json_string_field "$marker_path" install_path)" ||
    fail "managed install marker is missing its install path"
  [ "$marker_install_path" = "$(json_escape "$canonical_install_path")" ] ||
    fail "managed install marker does not own the requested executable"

  case "$telemetry_platform-$telemetry_arch" in
    linux-x64) expected_platform="linux-x64" ;;
    linux-arm64) expected_platform="linux-aarch64" ;;
    macos-x64) expected_platform="macos-x64" ;;
    macos-arm64) expected_platform="macos-arm64" ;;
    *) fail "unsupported uninstall platform" ;;
  esac
  marker_platform="$(json_string_field "$marker_path" platform)" ||
    fail "managed install marker is missing its platform"
  [ "$marker_platform" = "$expected_platform" ] ||
    fail "managed install marker platform does not match this host"

  marker_version="$(json_string_field "$marker_path" version)" ||
    fail "managed install marker is missing its version"
  case "$marker_version" in
    ""|*[!0-9A-Za-z.+-]*) fail "managed install marker contains an invalid version" ;;
  esac
  marker_sha256="$(json_string_field "$marker_path" sha256 2>/dev/null | tr 'A-F' 'a-f')" ||
    fail "managed install marker is missing its SHA-256 identity"
  valid_sha256 "$marker_sha256" ||
    fail "managed install marker contains an invalid SHA-256 identity"
  installed_sha256="$(sha256_file "$install_path" | tr 'A-F' 'a-f')" ||
    fail "could not hash the installed ctx executable"
  [ "$installed_sha256" = "$marker_sha256" ] ||
    fail "installed ctx executable differs from its managed install marker"

  integration_path="$(json_optional_string_field "$marker_path" integrations_path)" ||
    fail "managed install marker contains duplicate integration ownership fields"
  integration_sha256="$(json_optional_string_field "$marker_path" integrations_sha256 2>/dev/null | tr 'A-F' 'a-f')" ||
    fail "managed install marker contains duplicate integration ownership fields"
  if { [ -n "$integration_path" ] && [ -z "$integration_sha256" ]; } ||
     { [ -z "$integration_path" ] && [ -n "$integration_sha256" ]; }; then
    fail "managed install marker contains an incomplete integration identity"
  fi
  integration_outer_digest_required=0
  if [ -n "$integration_path" ]; then
    [ -n "$integration_path" ] && valid_sha256 "$integration_sha256" ||
      fail "managed install marker contains an incomplete integration identity"
    integration_fixed_path="$canonical_install_path.install-integrations"
    integration_generation_path="$integration_fixed_path.$integration_sha256"
    if [ "$integration_path" = "$(json_escape "$integration_fixed_path")" ]; then
      integration_path="$integration_fixed_path"
    elif [ "$integration_path" = "$(json_escape "$integration_generation_path")" ]; then
      integration_path="$integration_generation_path"
    else
      fail "managed integration ownership path is not canonical and adjacent"
    fi
    integration_outer_digest_required=1
  else
    integration_path="$canonical_install_path.install-integrations"
    if [ ! -e "$integration_path" ] && [ ! -L "$integration_path" ]; then
      integration_path=
    fi
  fi
  if [ -n "$integration_path" ]; then
    [ -f "$integration_path" ] && [ ! -L "$integration_path" ] ||
      fail "managed integration ownership file is absent or replaced"
    [ "$(path_owner_uid "$integration_path")" = "$current_uid" ] ||
      fail "managed integration ownership file is not owned by the current user"
    [ "$(path_link_count "$integration_path")" = "1" ] ||
      fail "managed integration ownership file must not be hard-linked"
    integration_size="$(path_size_bytes "$integration_path")" ||
      fail "could not determine managed integration ownership size"
    [ "$integration_size" -le 1048576 ] ||
      fail "managed integration ownership exceeds the size limit"
    if [ "$integration_outer_digest_required" = "1" ]; then
      actual_integration_sha256="$(sha256_file "$integration_path" | tr 'A-F' 'a-f')" ||
        fail "could not hash managed integration ownership"
      [ "$actual_integration_sha256" = "$integration_sha256" ] ||
        fail "managed integration ownership differs from its marker"
    fi
    [ "$(sed -n '1p' "$integration_path")" = "CTX_INSTALL_INTEGRATIONS_V1" ] ||
      fail "managed integration ownership has an invalid schema"
    validate_integration_records_digest
    validate_integration_ownership_records
  fi
}

remove_verified_file() {
  owned_path="$1"
  owned_sha256="$2"
  owned_label="$3"
  if [ ! -e "$owned_path" ] && [ ! -L "$owned_path" ]; then
    return 0
  fi
  if [ ! -f "$owned_path" ] || [ -L "$owned_path" ] ||
     [ "$(path_owner_uid "$owned_path" 2>/dev/null || true)" != "$current_uid" ] ||
     [ "$(path_link_count "$owned_path" 2>/dev/null || true)" != "1" ]; then
    log "Preserved modified or replaced $owned_label: $owned_path"
    return 0
  fi
  actual_owned_sha256="$(sha256_file "$owned_path" | tr 'A-F' 'a-f')" || {
    log "Preserved unverifiable $owned_label: $owned_path"
    return 0
  }
  if [ "$actual_owned_sha256" != "$owned_sha256" ]; then
    log "Preserved modified $owned_label: $owned_path"
    return 0
  fi
  rm -f "$owned_path"
  log "Removed $owned_path"
}

valid_owned_path() {
  owned_path="$1"
  case "$owned_path" in
    /*) ;;
    *) return 1 ;;
  esac
  case "/$owned_path/" in
    */../*|*/./*) return 1 ;;
  esac
  case "$owned_path" in
    *'
'*|*'	'*) return 1 ;;
  esac
}

valid_man_path() {
  candidate="$1"
  valid_owned_path "$candidate" || return 1
  candidate_name="\${candidate##*/}"
  case "$candidate_name" in
    ctx*.1) ;;
    *) return 1 ;;
  esac
  canonical_candidate="$(canonical_file_path "$candidate")" || return 1
  [ "$canonical_candidate" = "$candidate" ]
}

valid_profile_path() {
  candidate="$1"
  valid_owned_path "$candidate" || return 1
  case "$candidate" in
    "$HOME/.bashrc"|"$HOME/.bash_profile"|"$HOME/.profile"|\
    "\${ZDOTDIR:-$HOME}/.zshrc"|"$HOME/.config/fish/config.fish") return 0 ;;
    *) return 1 ;;
  esac
}

valid_skill_path() {
  candidate="$1"
  valid_owned_path "$candidate" || return 1
  case "$candidate" in
    */skills/ctx-agent-history-search) return 0 ;;
    *) return 1 ;;
  esac
}

validate_integration_records_digest() {
  records_kind=
  records_digest=
  records_extra=
  tab="$(printf '\\t')"
  second_line="$(sed -n '2p' "$integration_path")"
  IFS="$tab" read -r records_kind records_digest records_extra <<EOF
$second_line
EOF
  [ "$records_kind" = "records_sha256" ] && valid_sha256 "$records_digest" &&
    [ -z "$records_extra" ] ||
    fail "managed integration ownership has an invalid records digest"
  integration_records_copy="$(mktemp "\${TMPDIR:-/tmp}/ctx-uninstall-integrations.XXXXXX")" ||
    fail "could not create an integration verification file"
  awk 'NR > 2 { print }' "$integration_path" >"$integration_records_copy"
  actual_records_digest="$(sha256_file "$integration_records_copy" | tr 'A-F' 'a-f')" || {
    rm -f "$integration_records_copy"
    fail "could not hash managed integration records"
  }
  rm -f "$integration_records_copy"
  [ "$actual_records_digest" = "$records_digest" ] ||
    fail "managed integration ownership records digest does not match"
}

validate_integration_ownership_records() {
  tab="$(printf '\\t')"
  {
    IFS= read -r integration_header || fail "managed integration ownership is empty"
    [ "$integration_header" = "CTX_INSTALL_INTEGRATIONS_V1" ] ||
      fail "managed integration ownership has an invalid schema"
    IFS= read -r integration_records_header ||
      fail "managed integration ownership is missing its records digest"
    while IFS="$tab" read -r integration_kind integration_digest integration_target integration_extra; do
      [ -n "$integration_kind$integration_digest$integration_target$integration_extra" ] || continue
      [ -z "$integration_extra" ] && valid_sha256 "$integration_digest" &&
        valid_owned_path "$integration_target" ||
        fail "managed integration ownership contains an invalid record"
      case "$integration_kind" in
        man)
          valid_man_path "$integration_target" ||
            fail "managed integration ownership contains an unsafe man-page path"
          ;;
        profile-file|profile-block)
          valid_profile_path "$integration_target" ||
            fail "managed integration ownership contains an unsafe profile path"
          ;;
        skill)
          valid_skill_path "$integration_target" ||
            fail "managed integration ownership contains an unsafe skill path"
          ;;
        *) fail "managed integration ownership contains an unknown record type" ;;
      esac
    done
  } <"$integration_path"
}

remove_owned_profile_block() {
  profile_path="$1"
  block_sha256="$2"
  if [ ! -e "$profile_path" ] && [ ! -L "$profile_path" ]; then
    return 0
  fi
  if ! valid_profile_path "$profile_path" ||
     [ ! -f "$profile_path" ] || [ -L "$profile_path" ] ||
     [ "$(path_owner_uid "$profile_path" 2>/dev/null || true)" != "$current_uid" ] ||
     [ "$(path_link_count "$profile_path" 2>/dev/null || true)" != "1" ]; then
    log "Preserved modified or replaced installer PATH profile: $profile_path"
    return 0
  fi
  profile_extract="$(mktemp "\${TMPDIR:-/tmp}/ctx-uninstall-profile.XXXXXX")" ||
    fail "could not create a profile verification file"
  profile_rewrite="$(mktemp "\${TMPDIR:-/tmp}/ctx-uninstall-profile.XXXXXX")" ||
    fail "could not create a profile rewrite file"
  if ! awk '
    /^# >>> ctx installer PATH setup >>>$/ {
      blocks++
      copying = 1
    }
    copying { print }
    /^# <<< ctx installer PATH setup <<<$/{ copying = 0 }
    END {
      if (blocks != 1 || copying) exit 1
    }
  ' "$profile_path" >"$profile_extract"; then
    rm -f "$profile_extract" "$profile_rewrite"
    log "Preserved modified installer PATH profile block: $profile_path"
    return 0
  fi
  extracted_sha256="$(sha256_file "$profile_extract" | tr 'A-F' 'a-f')" || {
    rm -f "$profile_extract" "$profile_rewrite"
    log "Preserved unverifiable installer PATH profile block: $profile_path"
    return 0
  }
  if [ "$extracted_sha256" != "$block_sha256" ]; then
    rm -f "$profile_extract" "$profile_rewrite"
    log "Preserved modified installer PATH profile block: $profile_path"
    return 0
  fi
  awk '
    /^# >>> ctx installer PATH setup >>>$/ { removing = 1; next }
    /^# <<< ctx installer PATH setup <<<$/{ removing = 0; next }
    !removing { print }
  ' "$profile_path" >"$profile_rewrite"
  cat "$profile_rewrite" >"$profile_path"
  rm -f "$profile_extract" "$profile_rewrite"
  log "Removed installer PATH profile block from $profile_path"
}

remove_owned_skill() {
  skill_path="$1"
  skill_sha256="$2"
  if [ ! -e "$skill_path" ] && [ ! -L "$skill_path" ]; then
    return 0
  fi
  skill_body="$skill_path/SKILL.md"
  skill_marker="$skill_path/.ctx-skill.json"
  if ! valid_skill_path "$skill_path" ||
     [ ! -d "$skill_path" ] || [ -L "$skill_path" ] ||
     [ ! -f "$skill_body" ] || [ -L "$skill_body" ] ||
     [ ! -f "$skill_marker" ] || [ -L "$skill_marker" ] ||
     [ "$(path_owner_uid "$skill_body" 2>/dev/null || true)" != "$current_uid" ] ||
     [ "$(path_owner_uid "$skill_marker" 2>/dev/null || true)" != "$current_uid" ] ||
     [ "$(path_link_count "$skill_body" 2>/dev/null || true)" != "1" ] ||
     [ "$(path_link_count "$skill_marker" 2>/dev/null || true)" != "1" ]; then
    log "Preserved modified or unowned ctx skill: $skill_path"
    return 0
  fi
  skill_schema="$(json_scalar_field "$skill_marker" schema_version 2>/dev/null || true)"
  skill_installer="$(json_string_field "$skill_marker" installer 2>/dev/null || true)"
  skill_name="$(json_string_field "$skill_marker" skill_name 2>/dev/null || true)"
  skill_marker_hash="$(json_string_field "$skill_marker" skill_hash 2>/dev/null || true)"
  actual_skill_sha256="$(sha256_file "$skill_body" 2>/dev/null | tr 'A-F' 'a-f' || true)"
  skill_ownership_copy="$(mktemp "\${TMPDIR:-/tmp}/ctx-uninstall-skill.XXXXXX")" ||
    fail "could not create a skill verification file"
  cat "$skill_body" "$skill_marker" >"$skill_ownership_copy"
  actual_skill_ownership_sha256="$(sha256_file "$skill_ownership_copy" 2>/dev/null | tr 'A-F' 'a-f' || true)"
  rm -f "$skill_ownership_copy"
  if [ "$skill_schema" != "1" ] ||
     [ "$skill_installer" != "ctx-cli" ] ||
     [ "$skill_name" != "ctx-agent-history-search" ] ||
     [ "$skill_marker_hash" != "sha256:$actual_skill_sha256" ] ||
     [ "$actual_skill_ownership_sha256" != "$skill_sha256" ]; then
    log "Preserved modified or unowned ctx skill: $skill_path"
    return 0
  fi
  rm -f "$skill_body" "$skill_marker"
  rmdir "$skill_path" 2>/dev/null || :
  log "Removed installer-owned ctx skill from $skill_path"
}

remove_owned_integrations() {
  [ -n "\${integration_path:-}" ] || return 0
  tab="$(printf '\\t')"
  {
    IFS= read -r integration_header || fail "managed integration ownership is empty"
    [ "$integration_header" = "CTX_INSTALL_INTEGRATIONS_V1" ] ||
      fail "managed integration ownership has an invalid schema"
    IFS= read -r integration_records_header ||
      fail "managed integration ownership is missing its records digest"
    while IFS="$tab" read -r integration_kind integration_digest integration_target integration_extra; do
      [ -n "$integration_kind$integration_digest$integration_target$integration_extra" ] || continue
      [ -z "$integration_extra" ] && valid_sha256 "$integration_digest" &&
        valid_owned_path "$integration_target" ||
        fail "managed integration ownership contains an invalid record"
      case "$integration_kind" in
        man)
          valid_man_path "$integration_target" ||
            fail "managed integration ownership contains an unsafe man-page path"
          remove_verified_file "$integration_target" "$integration_digest" "ctx man page"
          ;;
        profile-file)
          valid_profile_path "$integration_target" ||
            fail "managed integration ownership contains an unsafe profile path"
          remove_verified_file "$integration_target" "$integration_digest" "installer PATH profile"
          ;;
        profile-block)
          remove_owned_profile_block "$integration_target" "$integration_digest"
          ;;
        skill)
          remove_owned_skill "$integration_target" "$integration_digest"
          ;;
        *) fail "managed integration ownership contains an unknown record type" ;;
      esac
    done
  } <"$integration_path"
}

resolve_tty_path() {
  for fd in 0 1 2; do
    if [ -t "$fd" ]; then
      tty_path="$(tty <&"$fd" 2>/dev/null || true)"
      if [ -n "$tty_path" ]; then
        printf '%s\\n' "$tty_path"
        return 0
      fi
    fi
  done
  return 1
}

${renderHostedUninstallVersionHelpers()}
installed_cli_predates_managed_lifecycle() {
  if [ "$version_major" -eq 0 ] && [ "$version_minor" -le 25 ]; then
    return 0
  fi
  return 1
}

prepare_core_daemon_uninstall() {
  if installed_cli_predates_managed_lifecycle; then
    log "Installed ctx $installed_cli_version_value predates the persistent Core daemon; no Core daemon teardown is required."
    return 0
  fi

  daemon_teardown_output="$(mktemp "\${TMPDIR:-/tmp}/ctx-daemon-uninstall.XXXXXX")" ||
    fail "could not create a Core daemon teardown verification file"
  if "$install_path" --data-root "$data_dir" daemon disable \
      --prepare-uninstall --format=json >"$daemon_teardown_output"; then
    :
  else
    daemon_status="$?"
    rm -f "$daemon_teardown_output"
    daemon_teardown_output=
    fail "Core daemon teardown failed with status $daemon_status; ctx remains installed"
  fi
  daemon_result_size="$(path_size_bytes "$daemon_teardown_output")" ||
    fail "could not determine Core daemon teardown result size"
  if [ "$daemon_result_size" -lt 2 ] || [ "$daemon_result_size" -gt 65536 ]; then
    rm -f "$daemon_teardown_output"
    daemon_teardown_output=
    fail "Core daemon teardown returned an invalid result; ctx remains installed"
  fi
  if ! validate_core_daemon_teardown_result "$daemon_teardown_output"; then
    rm -f "$daemon_teardown_output"
    daemon_teardown_output=
    fail "Core daemon teardown did not prove complete cleanup; ctx remains installed"
  fi
  rm -f "$daemon_teardown_output"
  daemon_teardown_output=
}

installed_cli_has_pro_lifecycle() {
  if installed_cli_predates_managed_lifecycle; then
    return 1
  fi
  "$install_path" pro uninstall --help >/dev/null 2>&1 ||
    fail "installed ctx $installed_cli_version_value does not expose the required Pro uninstall capability; reinstall ctx and try again"
  return 0
}

uninstall_pro_with_native_cli() {
  if installed_cli_is_unified; then
    return 0
  fi
  if [ ! -x "$install_path" ]; then
    fail "the installed ctx binary is required to uninstall safely; reinstall ctx and try again"
  fi
  if ! installed_cli_has_pro_lifecycle; then
    log "Installed ctx $installed_cli_version_value predates Local Pro; no Pro lifecycle cleanup is required."
    return 0
  fi

  case "$data_choice" in
    --delete-data)
      "$install_path" --data-root "$data_dir" pro uninstall --delete-data
      ;;
    --keep-data)
      "$install_path" --data-root "$data_dir" pro uninstall --keep-data
      ;;
    "")
      tty_path="$(resolve_tty_path || true)"
      if [ -z "$tty_path" ]; then
        fail "noninteractive uninstall requires --delete-data or --keep-data"
      fi
      exec 3<> "$tty_path" || fail "unable to open the interactive terminal"
      "$install_path" --data-root "$data_dir" pro uninstall <&3 >&3 2>&3
      exec 3>&-
      ;;
  esac
}

${renderHostedUninstallTransactionHelpers()}
uninstall_cli() {
  remove_owned_integrations
  if [ "$legacy_uninstall" = "1" ] && [ -n "\${integration_path:-}" ]; then
    rm -f "$integration_path"
    log "Removed $integration_path"
  fi
}

# This read-only preflight classifies the recorded executable before any
# recovery commit or helper cleanup. The native owner still validates the full
# journal and owns every transaction mutation.
reject_retired_recovery_delete_data

if { [ ! -e "$install_path" ] && [ ! -L "$install_path" ]; } &&
   { [ ! -e "$marker_path" ] && [ ! -L "$marker_path" ]; }; then
  completed_helper="\${install_path%/*}/.\${install_path##*/}.hosted-uninstall-helper"
  completed_journal="\${install_path%/*}/.\${install_path##*/}.hosted-install-transaction.json"
  if [ -f "$completed_journal" ] && [ ! -L "$completed_journal" ]; then
    recover_hosted_uninstall_transaction
  elif [ -f "$completed_helper" ] && [ ! -L "$completed_helper" ] &&
     [ "$(path_owner_uid "$completed_helper" 2>/dev/null || true)" = "$(id -u)" ] &&
     [ "$(path_link_count "$completed_helper" 2>/dev/null || true)" = "1" ]; then
    rm -f "$completed_helper"
  fi
  log "ctx is already uninstalled."
  report_install_stage "uninstall" "completed"
  uninstall_terminal_reported=1
  exit 0
fi

if { [ ! -e "$install_path" ] && [ ! -L "$install_path" ]; } &&
   [ -f "$marker_path" ] && [ ! -L "$marker_path" ]; then
  recover_hosted_uninstall_transaction
  log "ctx uninstall recovery complete. Local ctx history was preserved."
  report_install_stage "uninstall" "completed"
  uninstall_terminal_reported=1
  exit 0
fi

if try_recover_armed_hosted_uninstall_transaction; then
  log "ctx uninstall recovery complete. Local ctx history was preserved."
  report_install_stage "uninstall" "completed"
  uninstall_terminal_reported=1
  exit 0
fi

validate_managed_install
load_installed_cli_version
if installed_cli_is_unified && [ "$data_choice" = "--delete-data" ]; then
  fail "--delete-data was legacy derived-data cleanup and is retired in ctx 1.5. History and legacy data are preserved; rerun without --delete-data"
fi
legacy_uninstall=0
if installed_cli_predates_managed_lifecycle; then
  legacy_uninstall=1
else
  prepare_hosted_uninstall_transaction
fi
prepare_core_daemon_uninstall
validate_managed_install
uninstall_pro_with_native_cli
validate_managed_install
uninstall_cli
if [ "$legacy_uninstall" = "1" ]; then
  rm -f "$install_path" "$marker_path"
else
  commit_hosted_uninstall_transaction
fi

log "ctx uninstall complete. Local ctx history was preserved."
report_install_stage "uninstall" "completed"
uninstall_terminal_reported=1
`;
}
