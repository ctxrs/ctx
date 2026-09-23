import { renderCliInstallShellReleasePreparation, renderCliInstallShellReleaseSequence } from "./cli-install-shell-release.js";
import {
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
} from "./install-stage-contract.js";
import {
  renderCliInstallShellPublication,
} from "./cli-install-shell-publication.js";
import {
  renderCliInstallShellMarker,
} from "./cli-install-shell-marker.js";
import {
  renderCliInstallShellManPageInstallation,
} from "./cli-install-shell-man-pages.js";
import {
  renderCliInstallShellManagedPairApply,
  renderCliInstallShellManagedPairReconciliation,
  renderCliInstallShellManagedPairPublication,
  renderCliInstallShellManagedPairSetupExecution,
} from "./cli-install-shell-managed-pair.js";
export function renderCliInstallShellWorkflow({
  stagingDogfood,
  stagingDogfoodMarker,
}) {
  const corePublication = stagingDogfood
    ? `if [ "$managed_reinstall" = "1" ]; then
  [ "$(json_top_level_boolean_field "$previous_marker" staging_dogfood 2>/dev/null || true)" = "true" ] ||
    fail "staging dogfood reinstall requires an exact immutable staging marker"
  previous_channel="$(ownership_json_string_field "$previous_marker" channel 2>/dev/null || true)"
  [ "$previous_channel" = "$release_channel" ] &&
    [ "$previous_version" = "$version" ] &&
    [ "$previous_binary_digest" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "staging dogfood reinstall does not match the installed immutable target"
  managed_core_handoff=1
  verify_installed_target_identity
else`
    : `if [ "$pending_hosted_migration" = "1" ]; then
  managed_core_handoff=1
  stage_integration_ownership
  stage_install_marker "$tmp_dir/install-marker.XXXXXX"
  publish_fresh_or_legacy_binary migrate
  verify_installed_target_identity
  # The journal supplied the first attempt's authenticated sidecar. Restore
  # its records before later integration reconciliation uses our manifest.
  load_previous_integration_ownership
  stage_integration_ownership
elif [ "$managed_reinstall" = "1" ] &&
   ! previous_install_predates_persistent_daemon; then
  managed_core_handoff=1
  if [ "$install_man" != "1" ] && [ "$release_phase" != "bridge" ]; then
    # Core serializes the opt-out with runtime refresh and upgrade publication.
    disable_core_man_pages_before_upgrade
  fi
  if [ "$release_phase" = "final" ] &&
     [ "$(compare_release_versions "$previous_version" "1.6.3")" != "1" ] &&
     [ "$(path_size_bytes "$artifact_path")" -gt 134217728 ]; then
    # Released 1.6.3 cannot download this signed executable. The candidate
    # fences the installed daemon before its hosted replacement transaction.
    load_previous_man_page_receipt "$previous_marker"
    stage_integration_ownership
    stage_install_marker "$tmp_dir/install-marker.XXXXXX"
    publish_fresh_or_legacy_binary migrate
    verify_installed_target_identity
  else
    run_managed_core_upgrade
  fi
else
  if [ "$managed_reinstall" = "1" ] && [ "$preserve_core_man_pages" = "1" ]; then
    # Pre-daemon managed releases use the direct publication path, so there is
    # no Core marker receipt to preserve.
    preserve_core_man_pages=0
  fi`;
  const pairManagedRerun = stagingDogfood
    ? `    [ "$(json_top_level_boolean_field "$previous_marker" staging_dogfood 2>/dev/null || true)" = "true" ] ||
      fail "staging dogfood reinstall requires an exact immutable staging marker"
    previous_channel="$(ownership_json_string_field "$previous_marker" channel 2>/dev/null || true)"
    [ "$previous_channel" = "$release_channel" ] &&
      [ "$previous_version" = "$version" ] &&
      [ "$previous_binary_digest" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ] ||
      fail "staging dogfood reinstall does not match the installed immutable target"
    managed_core_handoff=1
    if [ "$install_man" != "1" ] && [ "$release_phase" != "bridge" ]; then
      disable_core_man_pages_before_upgrade
    fi
    verify_installed_target_identity`
    : `    if managed_pair_requires_candidate_apply; then
      stage_install_marker "$tmp_dir/install-marker.XXXXXX"
      apply_managed_pair_candidate "$marker_tmp_path" 1
      managed_core_handoff=1
    else
      managed_core_handoff=1
      if [ "$install_man" != "1" ] && [ "$release_phase" != "bridge" ]; then
        disable_core_man_pages_before_upgrade
      fi
      run_managed_core_upgrade
    fi`;
  return `install_stage_delivery_enabled=1
report_install_stage() {
  report_stage="$1"
  report_status="$2"
  [ "$install_stage_delivery_enabled" = "1" ] || return 0
  canonical_analytics_disabled && return 0
  command -v curl >/dev/null 2>&1 || return 0
  case "$install_telemetry_endpoint" in
    https://*) ;;
    *) return 0 ;;
  esac

  payload='{"event_name":"${INSTALL_STAGE_EVENT_NAME}","event_version":${INSTALL_STAGE_EVENT_VERSION},"install_attempt_id":"'"$(json_escape "$install_attempt_id")"'","stage":"'"$(json_escape "$report_stage")"'","status":"'"$(json_escape "$report_status")"'","platform":"'"$(json_escape "$telemetry_platform")"'","arch":"'"$(json_escape "$telemetry_arch")"'","script_family":"posix"}'
  if ! curl -fsS --connect-timeout 1 --max-time 1 -H "content-type: application/json" -X POST --data "$payload" "$install_telemetry_endpoint" >/dev/null 2>&1; then
    install_stage_delivery_enabled=0
  fi
  return 0
}

apply_deprecated_controls
load_persisted_config_controls

if [ "\${CTX_INSTALL_NO_SETUP:-0}" = "1" ]; then
  run_setup=0
fi

if [ "\${CTX_INSTALL_NO_DAEMON:-0}" = "1" ]; then
  setup_no_daemon=1
fi

case "\${CTX_INSTALL_SEMANTIC:-0}" in
  0|""|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff]) ;;
  1|[Tt][Rr][Uu][Ee]|[Yy][Ee][Ss]|[Oo][Nn]) semantic_enabled=1 ;;
  *) fail "CTX_INSTALL_SEMANTIC must be a canonical boolean" ;;
esac

semantic_search_control=unset
if [ "\${CTX_SEARCH_SEMANTIC+x}" = "x" ]; then
  if semantic_search_control_value="$(
    printf '%s' "$CTX_SEARCH_SEMANTIC" |
      LC_ALL=C awk '
          function trim(value, before, position, width) {
            while (1) {
              before = value
              sub(/^[[:space:]]+/, "", value)
              for (position = 1; position <= unicode_space_count; position += 1) {
                width = length(unicode_space[position])
                if (substr(value, 1, width) == unicode_space[position]) {
                  value = substr(value, width + 1)
                  break
                }
              }
              if (value == before) break
            }
            while (1) {
              before = value
              sub(/[[:space:]]+$/, "", value)
              for (position = 1; position <= unicode_space_count; position += 1) {
                width = length(unicode_space[position])
                if (substr(value, length(value) - width + 1) == unicode_space[position]) {
                  value = substr(value, 1, length(value) - width)
                  break
                }
              }
              if (value == before) break
            }
            return value
          }
          function continuation(byte) {
            return byte >= 128 && byte <= 191
          }
          function valid_utf8(value, position, value_length, first, second, third, fourth) {
            value_length = length(value)
            position = 1
            while (position <= value_length) {
              first = byte_value[substr(value, position, 1)]
              if (first <= 127) {
                position += 1
              } else if (first >= 194 && first <= 223) {
                if (position + 1 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                if (!continuation(second)) return 0
                position += 2
              } else if (first == 224) {
                if (position + 2 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                third = byte_value[substr(value, position + 2, 1)]
                if (second < 160 || second > 191 || !continuation(third)) return 0
                position += 3
              } else if ((first >= 225 && first <= 236) ||
                         (first >= 238 && first <= 239)) {
                if (position + 2 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                third = byte_value[substr(value, position + 2, 1)]
                if (!continuation(second) || !continuation(third)) return 0
                position += 3
              } else if (first == 237) {
                if (position + 2 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                third = byte_value[substr(value, position + 2, 1)]
                if (second < 128 || second > 159 || !continuation(third)) return 0
                position += 3
              } else if (first == 240) {
                if (position + 3 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                third = byte_value[substr(value, position + 2, 1)]
                fourth = byte_value[substr(value, position + 3, 1)]
                if (second < 144 || second > 191 ||
                    !continuation(third) || !continuation(fourth)) return 0
                position += 4
              } else if (first >= 241 && first <= 243) {
                if (position + 3 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                third = byte_value[substr(value, position + 2, 1)]
                fourth = byte_value[substr(value, position + 3, 1)]
                if (!continuation(second) ||
                    !continuation(third) || !continuation(fourth)) return 0
                position += 4
              } else if (first == 244) {
                if (position + 3 > value_length) return 0
                second = byte_value[substr(value, position + 1, 1)]
                third = byte_value[substr(value, position + 2, 1)]
                fourth = byte_value[substr(value, position + 3, 1)]
                if (second < 128 || second > 143 ||
                    !continuation(third) || !continuation(fourth)) return 0
                position += 4
              } else {
                return 0
              }
            }
            return 1
          }
          BEGIN {
            for (byte = 0; byte <= 255; byte += 1) {
              byte_value[sprintf("%c", byte)] = byte
            }
            unicode_space[++unicode_space_count] = sprintf("%c%c", 194, 133)
            unicode_space[++unicode_space_count] = sprintf("%c%c", 194, 160)
            unicode_space[++unicode_space_count] = sprintf("%c%c%c", 225, 154, 128)
            for (byte = 128; byte <= 138; byte += 1) {
              unicode_space[++unicode_space_count] = sprintf("%c%c%c", 226, 128, byte)
            }
            unicode_space[++unicode_space_count] = sprintf("%c%c%c", 226, 128, 168)
            unicode_space[++unicode_space_count] = sprintf("%c%c%c", 226, 128, 169)
            unicode_space[++unicode_space_count] = sprintf("%c%c%c", 226, 128, 175)
            unicode_space[++unicode_space_count] = sprintf("%c%c%c", 226, 129, 159)
            unicode_space[++unicode_space_count] = sprintf("%c%c%c", 227, 128, 128)
          }
          {
            if (!valid_utf8($0)) {
              invalid = 1
              exit 2
            }
            if (NR > 1) value = value "\\n"
            value = value $0
          }
          END {
            if (!invalid) printf "%s", trim(value)
          }
        '
  )"; then
    :
  else
    semantic_search_control_value=
  fi
  while :; do
    case "$semantic_search_control_value" in
      \\"*) semantic_search_control_value="\${semantic_search_control_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$semantic_search_control_value" in
      *\\") semantic_search_control_value="\${semantic_search_control_value%?}" ;;
      *) break ;;
    esac
  done
  case "$semantic_search_control_value" in
    "")
      ;;
    0|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff])
      semantic_search_control=false
      ;;
    1|[Tt][Rr][Uu][Ee]|[Yy][Ee][Ss]|[Oo][Nn])
      semantic_search_control=true
      semantic_enabled=1
      ;;
    *) fail "CTX_SEARCH_SEMANTIC must be a canonical boolean" ;;
  esac
fi

if [ "$semantic_enabled" != "1" ] &&
   [ "$semantic_search_control" != "false" ] &&
   [ "$persisted_semantic_enabled" = "1" ]; then
  semantic_enabled=1
fi

daemon_configuration_disabled=0
if canonical_daemon_disabled || [ "$persisted_daemon_disabled" = "1" ]; then
  daemon_configuration_disabled=1
fi
daemon_enabled=1
if [ "$setup_no_daemon" = "1" ] || [ "$daemon_configuration_disabled" = "1" ]; then
  daemon_enabled=0
fi

if [ "$semantic_enabled" = "1" ] && [ "$daemon_enabled" != "1" ]; then
  fail "Semantic installation requires an enabled daemon; remove installer no-daemon controls, clear daemon-disable environment controls, or set [daemon] enabled = true"
fi

${renderCliInstallShellMarker({ stagingDogfoodMarker })}validate_existing_managed_install() {
  previous_marker="$install_path.install.json"
  [ -f "$install_path" ] && [ ! -L "$install_path" ] && [ -x "$install_path" ] ||
    fail "an existing ctx install must be a regular executable owned by the hosted installer"
  [ -f "$previous_marker" ] && [ ! -L "$previous_marker" ] ||
    fail "an existing ctx install requires its regular hosted-install marker"
  current_uid="$(id -u)"
  for previous_owned_path in "$install_path" "$previous_marker"; do
    [ "$(path_owner_uid "$previous_owned_path" 2>/dev/null || true)" = "$current_uid" ] ||
      fail "prior managed install ownership is not owned by the current user"
    [ "$(path_link_count "$previous_owned_path" 2>/dev/null || true)" = "1" ] ||
      fail "prior managed install ownership must not be hard-linked"
  done
  previous_marker_size="$(path_size_bytes "$previous_marker")" ||
    fail "could not determine prior managed install marker size"
  [ "$previous_marker_size" -ge 2 ] && [ "$previous_marker_size" -le 65536 ] ||
    fail "prior managed install marker has an invalid size"
  [ "$(ownership_json_number_field "$previous_marker" schema_version 2>/dev/null || true)" = "1" ] ||
    fail "prior managed install marker has an invalid schema"
  [ "$(ownership_json_string_field "$previous_marker" manager 2>/dev/null || true)" = "ctx-hosted-installer" ] ||
    fail "prior managed install marker has an invalid manager"
  [ "$(ownership_json_string_field "$previous_marker" install_path 2>/dev/null || true)" = "$(json_escape "$install_path")" ] ||
    fail "prior managed install marker does not own the install path"
  [ "$(ownership_json_string_field "$previous_marker" platform 2>/dev/null || true)" = "$platform" ] ||
    fail "prior managed install marker platform does not match"
  previous_binary_digest="$(ownership_json_string_field "$previous_marker" sha256 2>/dev/null | tr 'A-F' 'a-f' || true)"; previous_binary_actual="$(sha256_file "$install_path" | tr 'A-F' 'a-f')"
  valid_sha256 "$previous_binary_digest" && { [ "$previous_binary_actual" = "$previous_binary_digest" ] || { [ -n "$pair_envelope_artifact" ] &&
      [ "$previous_binary_actual" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ]; }; } ||
    fail "prior managed binary differs from its install marker"
  previous_version="$(ownership_json_string_field "$previous_marker" version 2>/dev/null || true)"
  case "$previous_version" in
    ""|*[!0-9A-Za-z.+-]*) fail "prior managed install marker contains an invalid version" ;;
  esac
}

previous_install_predates_persistent_daemon() {
  previous_version_core="\${previous_version%%[-+]*}"
  case "$previous_version_core" in
    *.*.*) ;;
    *) return 1 ;;
  esac
  previous_version_major="\${previous_version_core%%.*}"
  previous_version_remainder="\${previous_version_core#*.}"
  previous_version_minor="\${previous_version_remainder%%.*}"
  case "$previous_version_major:$previous_version_minor" in
    *[!0-9:]*|:|*:) return 1 ;;
  esac
  [ "$previous_version_major" -eq 0 ] && [ "$previous_version_minor" -le 25 ]
}

managed_pair_requires_candidate_apply() {
  previous_install_predates_persistent_daemon && return 0
  candidate_digest="$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')"
  [ "$previous_binary_digest" != "$previous_binary_actual" ] &&
    [ "$previous_binary_actual" = "$candidate_digest" ]
}

validate_managed_upgrade_result() {
  upgrade_result_path="$1"
  expected_install_path="$(json_escape "$install_path")"
  awk \
    -v expected_version="$version" \
    -v expected_channel="$channel" \
    -v expected_platform="$platform" \
    -v expected_install_path="$expected_install_path" '
    function trim(value) {
      sub(/^[[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      return value
    }
    function update_depth(text, position, character, escaped) {
      escaped = 0
      for (position = 1; position <= length(text); position++) {
        character = substr(text, position, 1)
        if (in_string) {
          if (escaped) {
            escaped = 0
          } else if (character == backslash) {
            escaped = 1
          } else if (character == quote) {
            in_string = 0
          }
        } else if (character == quote) {
          in_string = 1
        } else if (character == "{") {
          depth++
        } else if (character == "}") {
          depth--
          if (depth < 0) invalid = 1
        }
      }
    }
    BEGIN {
      quote = sprintf("%c", 34)
      backslash = sprintf("%c", 92)
      required["schema_version"] = 1
      required["command"] = 1
      required["ok"] = 1
      required["status"] = 1
      required["message"] = 1
      required["current_version"] = 1
      required["latest_version"] = 1
      required["update_available"] = 1
      required["update_was_available"] = 1
      required["channel"] = 1
      required["platform"] = 1
      required["metadata_url"] = 1
      required["artifact_url"] = 1
      required["install_path"] = 1
      required["managed"] = 1
      required["applied"] = 1
      required["dry_run"] = 1
      required["warnings"] = 1
      required["upgrade_attempt_id"] = 1
      required_count = 19
    }
    {
      line = trim($0)
      before = depth
      if (before == 1 && line ~ /^"[A-Za-z_]+"[[:space:]]*:/) {
        key = line
        sub(/^"/, "", key)
        sub(/".*$/, "", key)
        value = line
        sub(/^[^:]*:[[:space:]]*/, "", value)
        sub(/,[[:space:]]*$/, "", value)
        value = trim(value)
        if (!(key in required) || seen[key]) {
          invalid = 1
        } else {
          seen[key] = 1
          seen_count++
          if (key == "schema_version" && value != "1") invalid = 1
          else if (key == "command" && value != quote "upgrade" quote) invalid = 1
          else if (key == "ok" && value != "true") invalid = 1
          else if (key == "status") {
            status = value
            if (status != quote "applied" quote &&
                status != quote "up_to_date" quote) invalid = 1
          } else if (key == "message" && value !~ /^".*"$/) invalid = 1
          else if (key == "current_version" &&
                   value != quote expected_version quote) invalid = 1
          else if (key == "latest_version" &&
                   value != quote expected_version quote) invalid = 1
          else if ((key == "update_available" || key == "update_was_available") &&
                   value != "true" && value != "false") invalid = 1
          else if (key == "channel" &&
                   value != quote expected_channel quote) invalid = 1
          else if (key == "platform" &&
                   value != quote expected_platform quote) invalid = 1
          else if ((key == "metadata_url" || key == "artifact_url") &&
                   value !~ /^".*"$/) invalid = 1
          else if (key == "install_path" &&
                   value != quote expected_install_path quote) invalid = 1
          else if (key == "managed" && value != "true") invalid = 1
          else if (key == "applied") applied = value
          else if (key == "dry_run" && value != "false") invalid = 1
          else if (key == "warnings" && value != "[]") invalid = 1
          else if (key == "upgrade_attempt_id" &&
                   value != "null" && value !~ /^"[^"]+"$/) invalid = 1
        }
      }
      update_depth(line)
    }
    END {
      if (in_string || depth != 0 || invalid || seen_count != required_count) exit 1
      for (key in required) {
        if (!seen[key]) exit 1
      }
      if (status == quote "applied" quote && applied != "true") exit 1
      if (status == quote "up_to_date" quote && applied != "false") exit 1
    }
  ' "$upgrade_result_path"
}
${renderCliInstallShellPublication()}

case "$platform" in
  linux-x64|linux-aarch64|macos-arm64|macos-x64) ;;
  *) fail "unsupported platform: $platform" ;;
esac

case "$platform" in
  linux-*) telemetry_platform="linux" ;;
  macos-*) telemetry_platform="macos" ;;
esac
case "$platform" in
  *-x64) telemetry_arch="x64" ;;
  *-aarch64|*-arm64) telemetry_arch="arm64" ;;
esac

report_install_stage "installer" "started"

tmp_dir="$(mktemp -d "\${TMPDIR:-/tmp}/ctx-cli-install.XXXXXX")"
tmp_dir="$(cd "$tmp_dir" && pwd -P)" || fail "could not resolve temporary directory"
integration_manifest_tmp="$tmp_dir/install-integrations"
integration_records_tmp="$tmp_dir/install-integration-records"
man_page_records_tmp="$tmp_dir/install-man-page-records"
marker_tmp_path=
binary_tmp_path=
integration_sidecar_tmp_path=
install_animation_pid=
: >"$integration_records_tmp"
: >"$man_page_records_tmp"
stop_install_animation() {
  if [ -n "$install_animation_pid" ]; then
    kill "$install_animation_pid" 2>/dev/null || true
    wait "$install_animation_pid" 2>/dev/null || true
    install_animation_pid=
    printf '\\rInstalling ctx %s...\\n' "$version" >&2
  fi
}

cleanup() {
  status="$?"
  trap - EXIT INT TERM
  stop_install_animation
  if [ "$status" -ne 0 ]; then
    report_install_stage "installer" "failed"
  fi
  if [ -n "$marker_tmp_path" ]; then
    rm -f "$marker_tmp_path"
  fi
  if [ -n "$binary_tmp_path" ]; then
    rm -f "$binary_tmp_path"
  fi
  if [ -n "$integration_sidecar_tmp_path" ]; then
    rm -f "$integration_sidecar_tmp_path"
  fi
  rm -rf "$tmp_dir"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "\${CTX_INSTALL_NO_MAN:-0}" = "1" ]; then
  install_man=0
fi

if [ "\${CTX_INSTALL_NO_MODIFY_PATH:-0}" = "1" ]; then
  modify_path=0
fi

if [ "\${CTX_INSTALL_ALL_SKILL_AGENTS:-0}" = "1" ]; then
  all_skill_agents=1
  explicit_skill_request=1
fi

if [ -n "\${CTX_INSTALL_SKILL_AGENTS:-}" ]; then
  explicit_skill_request=1
  old_ifs="$IFS"
  IFS=,
  for raw_agent in $CTX_INSTALL_SKILL_AGENTS; do
    agent="$(printf '%s' "$raw_agent" | tr -d '[:space:]')"
    if [ -n "$agent" ]; then
      append_skill_agent "$agent"
    fi
  done
  IFS="$old_ifs"
fi

if [ "\${CTX_INSTALL_NO_SKILL:-0}" = "1" ]; then
  run_skill=0
  no_skill_requested=1
fi

if [ "$no_skill_requested" = "1" ] && [ "$explicit_skill_request" = "1" ]; then
  fail "cannot combine --no-skill or CTX_INSTALL_NO_SKILL=1 with skill agent options"
fi

if [ "$all_skill_agents" = "1" ] && [ -n "$skill_agents" ]; then
  fail "cannot combine --all-skill-agents with --skill-agent or CTX_INSTALL_SKILL_AGENTS"
fi

start_install_animation() {
  if [ "$styled_output" != "1" ]; then
    log "Installing ctx $version..."
    return
  fi
  (
    dots=...
    while :; do
      printf '\\rInstalling ctx %s%s' "$version" "$dots" >&2
      case "$dots" in
        ...) dots='.  ' ;;
        '.  ') dots='.. ' ;;
        *) dots=... ;;
      esac
      sleep 0.1
    done
  ) &
  install_animation_pid="$!"
}

${renderCliInstallShellReleasePreparation()}
publish_release_phase() {
(umask 022; mkdir -p "$bin_dir")
[ ! -L "$secure_bin_dir" ] || fail "ctx install directory must not be a symlink"
bin_dir_owner="$(path_owner_uid "$secure_bin_dir")" ||
  fail "could not verify ctx install directory ownership"
[ "$bin_dir_owner" = "$(id -u)" ] ||
  fail "ctx install directory must be owned by the current user"
bin_dir="$(cd -P "$secure_bin_dir" && pwd -P)" ||
  fail "could not resolve the ctx install directory"
secure_bin_dir="$bin_dir"
install_path="$bin_dir/ctx"
managed_reinstall=0
managed_core_handoff=0
preserve_core_man_pages=0
man_pages_json=null
man_pages_receipt_present=0
if [ -L "$install_path.install.json" ] ||
   { [ -e "$install_path.install.json" ] && [ ! -f "$install_path.install.json" ]; }; then
  fail "managed install marker destination is not a regular file"
fi
${renderCliInstallShellManagedPairApply()}
try_resume_interrupted_managed_pair
if [ "$pending_hosted_migration" = "1" ]; then
  managed_reinstall=1
elif [ -e "$install_path" ] || [ -L "$install_path" ] ||
   [ -e "$install_path.install.json" ] || [ -L "$install_path.install.json" ]; then
  validate_existing_managed_install
  if [ "$stable_bridge" = "1" ]; then
    phase_order="$(compare_release_versions "$version" "$previous_version")" ||
      fail "invalid managed release version: $previous_version"
    [ "$phase_order" != "-1" ] || fail "refusing to downgrade the managed ctx installation"
  fi
  managed_reinstall=1
  if [ "$install_man" = "1" ]; then
    preserve_core_man_pages=1
  fi
fi
if [ "$pending_hosted_migration" != "1" ]; then
  load_previous_integration_ownership
fi
if [ "$managed_reinstall" != "1" ]; then
  initialize_man_page_receipt
fi
${renderCliInstallShellManagedPairPublication({ corePublication, pairManagedRerun })}
${renderCliInstallShellManagedPairReconciliation()}
if [ "$managed_core_handoff" != "1" ]; then
  verify_installed_target_identity
fi
report_install_stage "binary_install" "completed"
stop_install_animation

}

${renderCliInstallShellReleaseSequence({ stagingDogfood })}receipt_item "Installed and verified"

if [ "$semantic_enabled" = "1" ]; then
  repair_metadata_uri="$(absolute_path_to_file_uri "$metadata_file")"
  repair_metadata_signature_uri="$(absolute_path_to_file_uri "$metadata_signature_file")"
  if ! CTX_SEARCH_SEMANTIC=1 \
    CTX_RELEASE_METADATA_URL="$repair_metadata_uri" \
    CTX_RELEASE_METADATA_SIGNATURE_URL="$repair_metadata_signature_uri" \
    "$install_path" upgrade --channel "$channel" --format=json >"$tmp_dir/semantic-upgrade.out" 2>&1; then
    fail "ctx Semantic runtime repair failed"
  fi
fi

${renderCliInstallShellManPageInstallation()}

skill_install_failed=0
if [ "$run_skill" = "1" ]; then
  report_install_stage "skill_install" "started"
  if run_skill_install; then
    if record_installed_skills; then
      report_install_stage "skill_install" "completed"
    else
      report_install_stage "skill_install" "failed"
      skill_install_failed=1
    fi
  else
    report_install_stage "skill_install" "failed"
    skill_install_failed=1
  fi
else
  report_install_stage "skill_install" "skipped"
fi

setup_status=0
setup_verified=0
setup_initialized=
setup_mode=invalid
indexed_sessions=
indexed_items=
setup_wait_requested=0
if [ "$run_setup" = "1" ]; then
  setup_progress="\${CTX_SETUP_PROGRESS:-auto}"
  if [ "$setup_no_daemon" != "1" ]; then
    setup_wait_requested=1
  fi
  report_install_stage "setup" "started"
  if [ "$setup_progress" != "none" ] && [ -t 2 ]; then
    log ""
  fi
  ${renderCliInstallShellManagedPairSetupExecution()}
  if [ "$setup_status" = "0" ]; then
    if json_document_is_well_formed "$tmp_dir/setup-receipt.json"; then
      setup_schema_version="$(json_top_level_unsigned_integer_or_null_field "$tmp_dir/setup-receipt.json" schema_version)" ||
        setup_schema_version=
      setup_initialized="$(json_top_level_boolean_field "$tmp_dir/setup-receipt.json" initialized)" ||
        setup_initialized=
      setup_mode="$(json_top_level_string_field "$tmp_dir/setup-receipt.json" mode)" ||
        setup_mode=invalid
      indexed_sessions="$(json_top_level_unsigned_integer_or_null_field "$tmp_dir/setup-receipt.json" indexed_sessions)" ||
        indexed_sessions=
      indexed_items="$(json_top_level_unsigned_integer_or_null_field "$tmp_dir/setup-receipt.json" indexed_items)" ||
        indexed_items=
      if { [ "$setup_schema_version" = "2" ] || [ "$setup_schema_version" = "3" ]; } &&
         { [ "$setup_initialized" = "true" ] || [ "$setup_initialized" = "false" ]; } &&
         { [ "$indexed_sessions" = "null" ] || is_unsigned_integer "$indexed_sessions"; } &&
         { [ "$indexed_items" = "null" ] || is_unsigned_integer "$indexed_items"; }; then
        case "$setup_mode" in
          ready|pending|stale|unavailable) setup_verified=1 ;;
        esac
      fi
    fi

    if [ "$setup_verified" != "1" ]; then
      setup_status=1
    fi
  fi
  if [ "$setup_status" = "0" ]; then
    report_install_stage "setup" "completed"
  else
    report_install_stage "setup" "failed"
  fi
else
  report_install_stage "setup" "skipped"
fi

configure_path_if_needed
if [ "$managed_core_handoff" = "1" ]; then
  stage_integration_ownership
  if ! reconcile_managed_pair_integration; then
    receipt_warning "ctx installed, but integration ownership reconciliation is pending. Rerun this installer command to retry safely"
  fi
else
  publish_integration_ownership
  write_install_marker
  cleanup_published_integration_generations
fi

found_count=
found_unit=
if [ "$setup_verified" = "1" ] && [ "$setup_initialized" = "true" ]; then
  if is_unsigned_integer "$indexed_sessions" && [ "$indexed_sessions" != "0" ]; then
    found_count="$indexed_sessions"
    found_unit=sessions
  elif is_unsigned_integer "$indexed_items"; then
    found_count="$indexed_items"
    found_unit=records
  fi
fi
if [ "$setup_verified" = "1" ] && [ -n "$found_count" ]; then
  receipt_item "Found $(format_count "$found_count") $found_unit"
fi

indexing_continues=0
if [ "$setup_verified" = "1" ]; then
  case "$setup_mode" in
    ready) receipt_item "Index ready" ;;
    pending|stale)
      receipt_item "Indexing started"
      if [ "$daemon_enabled" = "1" ]; then indexing_continues=1; fi
      ;;
    unavailable)
      if [ "$daemon_configuration_disabled" = "1" ]; then
        receipt_item "Indexing deferred — daemon disabled"
      elif [ "$setup_no_daemon" = "1" ]; then
        receipt_item "Indexing deferred — daemon not started"
      fi
      ;;
  esac
fi

if [ "$run_setup" = "1" ] && [ "$setup_status" != "0" ]; then
  receipt_warning "Setup failed. Retry: ctx setup"
fi
if [ "$skill_install_failed" = "1" ]; then
  receipt_warning "Agent skill setup failed. Retry: ctx integrations install skills"
fi
if [ "$indexing_continues" = "1" ]; then
  log ""
  log "Indexing will continue in the background."
fi
if [ -n "$path_result" ]; then
  log ""
  log "To use the newly installed ctx in this shell, run:"
  log "  $path_export_command"
  if [ "$path_profile_persisted" = "1" ] || [ "$path_profile_semantics" = "1" ]; then
    log ""
    log "New terminal sessions will include it automatically."
  else
    log ""
    log "To add it for future terminal sessions, add $path_display_dir to your shell profile."
  fi
fi

if [ "$setup_verified" = "1" ]; then
  log ""
  log '  Search:    ctx search "test failure"'
  log "  Progress:  ctx index watch"
  log "  Status:    ctx status"
fi
if [ "$setup_status" != "0" ]; then
  exit "$setup_status"
fi
report_install_stage "installer" "completed"
`;
}
