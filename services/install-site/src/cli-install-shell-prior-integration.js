export const CLI_INSTALL_SHELL_PRIOR_INTEGRATION = `load_previous_integration_ownership() {
  previous_marker="$install_path.install.json"
  previous_integration_fixed="$install_path.install-integrations"
  previous_integration="$previous_integration_fixed"
  if [ -f "$previous_marker" ] && [ ! -L "$previous_marker" ]; then
    previous_path_fields="$(ownership_json_field_count "$previous_marker" integrations_path 2>/dev/null || true)"
    previous_digest_fields="$(ownership_json_field_count "$previous_marker" integrations_sha256 2>/dev/null || true)"
    case "$previous_path_fields:$previous_digest_fields" in
      0:0) ;;
      1:1)
        previous_marker_digest="$(ownership_json_string_field "$previous_marker" integrations_sha256 2>/dev/null | tr 'A-F' 'a-f' || true)"
        valid_sha256 "$previous_marker_digest" ||
          fail "prior managed integration ownership is not bound to its marker"
        previous_marker_path="$(ownership_json_string_field "$previous_marker" integrations_path 2>/dev/null || true)"
        previous_generation="$previous_integration_fixed.$previous_marker_digest"
        if [ "$previous_marker_path" = "$(json_escape "$previous_integration_fixed")" ]; then
          previous_integration="$previous_integration_fixed"
        elif [ "$previous_marker_path" = "$(json_escape "$previous_generation")" ]; then
          previous_integration="$previous_generation"
        else
          fail "prior managed integration ownership is not bound to its marker"
        fi
        ;;
      *) fail "prior managed integration ownership is not bound to its marker" ;;
    esac
  fi
  if [ "$previous_integration" = "$previous_integration_fixed" ]; then
    recover_interrupted_integration_publication "$previous_marker" "$previous_integration"
  fi
  if [ ! -e "$previous_integration" ] && [ ! -L "$previous_integration" ]; then
    if [ -f "$previous_marker" ] && [ ! -L "$previous_marker" ]; then
      missing_integration_path_fields="$(ownership_json_field_count "$previous_marker" integrations_path 2>/dev/null || true)"
      missing_integration_digest_fields="$(ownership_json_field_count "$previous_marker" integrations_sha256 2>/dev/null || true)"
      if [ "$missing_integration_path_fields" != "0" ] ||
         [ "$missing_integration_digest_fields" != "0" ]; then
        fail "prior managed integration ownership is absent"
      fi
      load_previous_man_page_receipt "$previous_marker"
    fi
    return 0
  fi

  [ -f "$install_path" ] && [ ! -L "$install_path" ] && [ -x "$install_path" ] &&
    [ -f "$previous_marker" ] && [ ! -L "$previous_marker" ] &&
    [ -f "$previous_integration" ] && [ ! -L "$previous_integration" ] ||
    fail "cannot replace invalid prior managed integration ownership"
  current_uid="$(id -u)"
  for previous_owned_path in "$install_path" "$previous_marker" "$previous_integration"; do
    [ "$(path_owner_uid "$previous_owned_path")" = "$current_uid" ] ||
      fail "prior managed install ownership is not owned by the current user"
    [ "$(path_link_count "$previous_owned_path")" = "1" ] ||
      fail "prior managed install ownership must not be hard-linked"
  done
  [ "$(path_size_bytes "$previous_marker")" -le 65536 ] &&
    [ "$(path_size_bytes "$previous_integration")" -le 1048576 ] ||
    fail "prior managed install ownership exceeds its size limit"

  [ "$(ownership_json_number_field "$previous_marker" schema_version 2>/dev/null || true)" = "1" ] ||
    fail "prior managed install marker has an invalid schema"
  [ "$(ownership_json_string_field "$previous_marker" manager 2>/dev/null || true)" = "ctx-hosted-installer" ] ||
    fail "prior managed install marker has an invalid manager"
  [ "$(ownership_json_string_field "$previous_marker" install_path 2>/dev/null || true)" = "$(json_escape "$install_path")" ] ||
    fail "prior managed install marker does not own the install path"
  [ "$(ownership_json_string_field "$previous_marker" platform 2>/dev/null || true)" = "$platform" ] ||
    fail "prior managed install marker platform does not match"
  previous_binary_digest="$(ownership_json_string_field "$previous_marker" sha256 2>/dev/null | tr 'A-F' 'a-f' || true)"
  previous_binary_actual="$(sha256_file "$install_path" | tr 'A-F' 'a-f')"
  valid_sha256 "$previous_binary_digest" &&
    { [ "$previous_binary_actual" = "$previous_binary_digest" ] ||
      { [ -n "$pair_envelope_artifact" ] &&
        [ "$previous_binary_actual" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ]; }; } ||
    fail "prior managed binary differs from its install marker"
  previous_version="$(ownership_json_string_field "$previous_marker" version 2>/dev/null || true)"
  case "$previous_version" in
    ""|*[!0-9A-Za-z.+-]*) fail "prior managed install marker contains an invalid version" ;;
  esac
  previous_integration_field="$(ownership_json_string_field "$previous_marker" integrations_path 2>/dev/null || true)"
  [ "$previous_integration_field" = "$(json_escape "$previous_integration")" ] ||
    fail "prior managed integration ownership is not bound to its marker"
  previous_integration_digest="$(ownership_json_string_field "$previous_marker" integrations_sha256 2>/dev/null | tr 'A-F' 'a-f' || true)"
  valid_sha256 "$previous_integration_digest" &&
    [ "$(sha256_file "$previous_integration" | tr 'A-F' 'a-f')" = "$previous_integration_digest" ] ||
    fail "prior managed integration ownership differs from its marker"
  [ "$(sed -n '1p' "$previous_integration")" = "CTX_INSTALL_INTEGRATIONS_V1" ] ||
    fail "prior managed integration ownership has an invalid schema"

  prior_records_header="$(sed -n '2p' "$previous_integration")"
  tab="$(printf '\\t')"
  IFS="$tab" read -r prior_header_kind prior_records_digest prior_header_extra <<EOF
$prior_records_header
EOF
  [ "$prior_header_kind" = "records_sha256" ] &&
    valid_sha256 "$prior_records_digest" &&
    [ -z "$prior_header_extra" ] ||
    fail "prior managed integration ownership has an invalid records digest"
  prior_records_copy="$tmp_dir/prior-integration-records"
  awk 'NR > 2 { print }' "$previous_integration" >"$prior_records_copy"
  [ "$(sha256_file "$prior_records_copy" | tr 'A-F' 'a-f')" = "$prior_records_digest" ] ||
    fail "prior managed integration ownership records digest does not match"
  awk 'length($0) == 0 { exit 1 }' "$prior_records_copy" ||
    fail "prior managed integration ownership contains a noncanonical blank record"

  prior_seen_targets="$tmp_dir/prior-integration-targets"
  : >"$prior_seen_targets"
  while IFS="$tab" read -r prior_kind prior_digest prior_target prior_extra; do
    [ -n "$prior_kind$prior_digest$prior_target$prior_extra" ] || continue
    [ -z "$prior_extra" ] && valid_sha256 "$prior_digest" &&
      valid_prior_owned_path "$prior_target" ||
      fail "prior managed integration ownership contains an invalid record"
    case "$prior_kind" in
      man)
        prior_name="\${prior_target##*/}"
        case "$prior_name" in
          ctx*.1) ;;
          *) fail "prior managed integration ownership contains an unsafe man-page path" ;;
        esac
        ;;
      profile-file|profile-block)
        valid_prior_profile_path "$prior_target" ||
          fail "prior managed integration ownership contains an unsafe profile path"
        ;;
      skill)
        case "$prior_target" in
          */skills/ctx-agent-history-search) ;;
          *) fail "prior managed integration ownership contains an unsafe skill path" ;;
        esac
        ;;
      *) fail "prior managed integration ownership contains an unknown record type" ;;
    esac
    prior_duplicate=0
    while IFS= read -r prior_seen_target; do
      if [ "$prior_seen_target" = "$prior_target" ]; then
        prior_duplicate=1
        break
      fi
    done <"$prior_seen_targets"
    [ "$prior_duplicate" = "0" ] ||
      fail "prior managed integration ownership contains duplicate targets"
    printf '%s\\n' "$prior_target" >>"$prior_seen_targets"
    if prior_record_matches "$prior_kind" "$prior_digest" "$prior_target"; then
      printf '%s\\t%s\\t%s\\n' "$prior_kind" "$prior_digest" "$prior_target" >>"$integration_records_tmp"
    fi
  done <"$prior_records_copy"
  integration_path="$previous_integration"
  integration_sha256="$previous_integration_digest"
  load_previous_man_page_receipt "$previous_marker"
}`;
