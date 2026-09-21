export const CLI_INSTALL_SHELL_INTEGRATIONS = `skill_agent_list() {
  joined=
  old_ifs="$IFS"
  IFS='
'
  for agent in $skill_agents; do
    if [ -n "$joined" ]; then
      joined="$joined,$agent"
    else
      joined="$agent"
    fi
  done
  IFS="$old_ifs"
  printf '%s' "$joined"
}

run_skill_install() {
  set -- integrations install skills
  if [ "$all_skill_agents" = "1" ]; then
    set -- "$@" --all-agents
  elif [ -n "$skill_agents" ]; then
    old_ifs="$IFS"
    IFS='
'
    for agent in $skill_agents; do
      set -- "$@" --agent "$agent"
    done
    IFS="$old_ifs"
  fi
  set -- "$@" --format=json
  "$install_path" "$@" >"$tmp_dir/skill-install.out" 2>&1
}

record_owned_integration() {
  integration_kind="$1"
  integration_digest="$(printf '%s' "$2" | tr 'A-F' 'a-f')"
  integration_target="$3"
  case "$integration_kind" in
    man|profile-file|profile-block|skill) ;;
    *) fail "invalid installer integration ownership kind" ;;
  esac
  case "$integration_digest" in
    *[!0-9a-f]*) fail "invalid installer integration ownership digest" ;;
  esac
  [ "\${#integration_digest}" -eq 64 ] ||
    fail "invalid installer integration ownership digest"
  case "$integration_target" in
    /*) ;;
    *) fail "installer integration ownership path must be absolute" ;;
  esac
  case "$integration_target" in
    *'
'*|*'	'*) fail "installer integration ownership path contains a forbidden character" ;;
  esac
  integration_merge_tmp="$tmp_dir/integration-records-merge"
  : >"$integration_merge_tmp"
  tab="$(printf '\\t')"
  while IFS="$tab" read -r existing_kind existing_digest existing_target existing_extra; do
    [ -n "$existing_kind$existing_digest$existing_target$existing_extra" ] || continue
    [ -z "$existing_extra" ] ||
      fail "installer integration ownership contains an ambiguous record"
    if [ "$existing_target" = "$integration_target" ]; then
      continue
    fi
    printf '%s\\t%s\\t%s\\n' "$existing_kind" "$existing_digest" "$existing_target" >>"$integration_merge_tmp"
  done <"$integration_records_tmp"
  printf '%s\\t%s\\t%s\\n' "$integration_kind" "$integration_digest" "$integration_target" >>"$integration_merge_tmp"
  mv "$integration_merge_tmp" "$integration_records_tmp"
}

owned_integration_matches() {
  expected_kind="$1"
  expected_digest="$2"
  expected_target="$3"
  tab="$(printf '\\t')"
  while IFS="$tab" read -r existing_kind existing_digest existing_target existing_extra; do
    [ -z "$existing_extra" ] || return 1
    if [ "$existing_kind" = "$expected_kind" ] &&
       [ "$existing_digest" = "$expected_digest" ] &&
       [ "$existing_target" = "$expected_target" ]; then
      return 0
    fi
  done <"$integration_records_tmp"
  return 1
}

stage_integration_ownership() {
  integration_path="$install_path.install-integrations"
  integration_records_sha256="$(sha256_file "$integration_records_tmp" | tr 'A-F' 'a-f')"
  {
    printf '%s\\n' "CTX_INSTALL_INTEGRATIONS_V1"
    printf '%s\\t%s\\n' "records_sha256" "$integration_records_sha256"
    cat "$integration_records_tmp"
  } >"$integration_manifest_tmp"
  integration_sha256="$(sha256_file "$integration_manifest_tmp" | tr 'A-F' 'a-f')"
}

integration_generation_path() {
  generation_digest="$1"
  valid_sha256 "$generation_digest" || return 1
  printf '%s.%s\\n' "$install_path.install-integrations" "$generation_digest"
}

valid_integration_sidecar_file() {
  sidecar_path="$1"
  sidecar_digest="$2"
  [ -f "$sidecar_path" ] && [ ! -L "$sidecar_path" ] || return 1
  [ "$(path_owner_uid "$sidecar_path" 2>/dev/null || true)" = "$(id -u)" ] || return 1
  [ "$(path_link_count "$sidecar_path" 2>/dev/null || true)" = "1" ] || return 1
  integration_sidecar_size="$(path_size_bytes "$sidecar_path" 2>/dev/null || true)"
  case "$integration_sidecar_size" in ""|*[!0-9]*) return 1 ;; esac
  [ "$integration_sidecar_size" -le 1048576 ] || return 1
  [ "$(sha256_file "$sidecar_path" 2>/dev/null | tr 'A-F' 'a-f' || true)" = "$sidecar_digest" ]
}

install_integration_sidecar_atomically() {
  sidecar_source="$1"
  sidecar_destination="$2"
  sidecar_digest="$3"
  integration_sidecar_tmp_path="$(mktemp "$sidecar_destination.tmp.XXXXXX")" ||
    fail "could not stage managed integration ownership"
  if ! install -m 0600 "$sidecar_source" "$integration_sidecar_tmp_path" ||
     ! valid_integration_sidecar_file "$integration_sidecar_tmp_path" "$sidecar_digest"; then
    rm -f "$integration_sidecar_tmp_path"
    integration_sidecar_tmp_path=
    fail "could not stage managed integration ownership"
  fi
  if ! mv -f "$integration_sidecar_tmp_path" "$sidecar_destination"; then
    rm -f "$integration_sidecar_tmp_path"
    integration_sidecar_tmp_path=
    fail "could not publish managed integration ownership"
  fi
  integration_sidecar_tmp_path=
}

ensure_integration_generation() {
  generation_source="$1"
  generation_path="$2"
  generation_digest="$3"
  if [ -e "$generation_path" ] || [ -L "$generation_path" ]; then
    valid_integration_sidecar_file "$generation_path" "$generation_digest" ||
      fail "managed integration ownership generation is invalid"
    return 0
  fi
  install_integration_sidecar_atomically "$generation_source" "$generation_path" "$generation_digest"
}

recover_interrupted_integration_publication() {
  recovery_marker="$1"
  recovery_path="$2"
  integration_recovered_generation_path=
  integration_recovered_generation_digest=
  [ -f "$recovery_marker" ] && [ ! -L "$recovery_marker" ] &&
    [ -f "$recovery_path" ] && [ ! -L "$recovery_path" ] || return 0
  recovery_actual_digest="$(sha256_file "$recovery_path" 2>/dev/null | tr 'A-F' 'a-f' || true)"
  valid_sha256 "$recovery_actual_digest" || return 0
  recovery_actual_generation="$(integration_generation_path "$recovery_actual_digest")" || return 0
  valid_integration_sidecar_file "$recovery_path" "$recovery_actual_digest" &&
    valid_integration_sidecar_file "$recovery_actual_generation" "$recovery_actual_digest" || return 0

  recovery_path_fields="$(ownership_json_field_count "$recovery_marker" integrations_path 2>/dev/null || true)"
  recovery_digest_fields="$(ownership_json_field_count "$recovery_marker" integrations_sha256 2>/dev/null || true)"
  if [ "$recovery_path_fields:$recovery_digest_fields" = "0:0" ]; then
    rm -f "$recovery_path" || fail "could not recover interrupted managed integration publication"
    integration_recovered_generation_path="$recovery_actual_generation"
    integration_recovered_generation_digest="$recovery_actual_digest"
    return 0
  fi
  [ "$recovery_path_fields:$recovery_digest_fields" = "1:1" ] || return 0
  recovery_marker_path="$(ownership_json_string_field "$recovery_marker" integrations_path 2>/dev/null || true)"
  recovery_marker_digest="$(ownership_json_string_field "$recovery_marker" integrations_sha256 2>/dev/null | tr 'A-F' 'a-f' || true)"
  [ "$recovery_marker_path" = "$(json_escape "$recovery_path")" ] &&
    valid_sha256 "$recovery_marker_digest" || return 0
  [ "$recovery_actual_digest" != "$recovery_marker_digest" ] || return 0
  recovery_marker_generation="$(integration_generation_path "$recovery_marker_digest")" || return 0
  valid_integration_sidecar_file "$recovery_marker_generation" "$recovery_marker_digest" || return 0
  install_integration_sidecar_atomically \
    "$recovery_marker_generation" "$recovery_path" "$recovery_marker_digest"
  integration_recovered_generation_path="$recovery_actual_generation"
  integration_recovered_generation_digest="$recovery_actual_digest"
}

publish_integration_ownership() {
  stage_integration_ownership
  integration_published_generation_path="$(integration_generation_path "$integration_sha256")" ||
    fail "could not derive managed integration ownership generation"
  integration_published_generation_digest="$integration_sha256"
  ensure_integration_generation \
    "$integration_manifest_tmp" "$integration_published_generation_path" "$integration_sha256"

  integration_previous_generation_path=
  integration_previous_generation_digest=
  publication_marker="$install_path.install.json"
  if [ -f "$publication_marker" ] && [ ! -L "$publication_marker" ]; then
    publication_marker_path="$(ownership_json_string_field "$publication_marker" integrations_path 2>/dev/null || true)"
    publication_marker_digest="$(ownership_json_string_field "$publication_marker" integrations_sha256 2>/dev/null | tr 'A-F' 'a-f' || true)"
    if [ "$publication_marker_path" = "$(json_escape "$integration_path")" ] &&
       valid_sha256 "$publication_marker_digest" &&
       valid_integration_sidecar_file "$integration_path" "$publication_marker_digest"; then
      integration_previous_generation_path="$(integration_generation_path "$publication_marker_digest")" ||
        fail "could not derive prior managed integration ownership generation"
      integration_previous_generation_digest="$publication_marker_digest"
      ensure_integration_generation \
        "$integration_path" "$integration_previous_generation_path" "$publication_marker_digest"
    fi
  fi
  if [ -e "$integration_path" ] || [ -L "$integration_path" ]; then
    [ -n "$integration_previous_generation_path" ] ||
      fail "managed integration ownership destination is not bound to its marker"
  fi
  install_integration_sidecar_atomically \
    "$integration_published_generation_path" "$integration_path" "$integration_sha256"
}

cleanup_published_integration_generations() {
  for cleanup_generation_role in previous published recovered; do
    case "$cleanup_generation_role" in
      previous)
        cleanup_generation_path="\${integration_previous_generation_path:-}"
        cleanup_generation_digest="\${integration_previous_generation_digest:-}"
        ;;
      published)
        cleanup_generation_path="\${integration_published_generation_path:-}"
        cleanup_generation_digest="\${integration_published_generation_digest:-}"
        ;;
      recovered)
        cleanup_generation_path="\${integration_recovered_generation_path:-}"
        cleanup_generation_digest="\${integration_recovered_generation_digest:-}"
        ;;
    esac
    [ -n "$cleanup_generation_path" ] &&
      valid_integration_sidecar_file "$cleanup_generation_path" "$cleanup_generation_digest" || continue
    rm -f "$cleanup_generation_path" 2>/dev/null || :
  done
}

json_result_paths() {
  awk '
    BEGIN {
      quote = sprintf("%c", 34)
      backslash = sprintf("%c", 92)
    }
    {
      input = input $0 "\\n"
    }
    function parse_string(start, i, character, escaped) {
      if (substr(input, start, 1) != quote) return 0
      parsed_value = ""
      escaped = 0
      for (i = start + 1; i <= length(input); i++) {
        character = substr(input, i, 1)
        if (escaped) {
          if (character == quote || character == backslash || character == "/") {
            parsed_value = parsed_value character
          } else if (character == "b") {
            parsed_value = parsed_value sprintf("%c", 8)
          } else if (character == "f") {
            parsed_value = parsed_value sprintf("%c", 12)
          } else if (character == "n") {
            return 0
          } else if (character == "r") {
            return 0
          } else if (character == "t") {
            return 0
          } else {
            return 0
          }
          escaped = 0
        } else if (character == backslash) {
          escaped = 1
        } else if (character == quote) {
          parsed_end = i
          return 1
        } else {
          parsed_value = parsed_value character
        }
      }
      return 0
    }
    END {
      i = 1
      while (i <= length(input)) {
        if (substr(input, i, 1) != quote) {
          i++
          continue
        }
        if (!parse_string(i)) exit 1
        key = parsed_value
        i = parsed_end + 1
        j = i
        while (substr(input, j, 1) ~ /[[:space:]]/) j++
        if (key != "path" || substr(input, j, 1) != ":") continue
        j++
        while (substr(input, j, 1) ~ /[[:space:]]/) j++
        if (!parse_string(j)) exit 1
        print parsed_value
        i = parsed_end + 1
      }
    }
  ' "$1"
}

record_installed_skills() {
  skill_paths_file="$tmp_dir/skill-paths"
  if ! json_result_paths "$tmp_dir/skill-install.out" >"$skill_paths_file"; then
    return 1
  fi
  while IFS= read -r skill_path; do
    [ -n "$skill_path" ] || continue
    case "$skill_path" in
      /*/skills/ctx-agent-history-search) ;;
      *) continue ;;
    esac
    skill_body="$skill_path/SKILL.md"
    skill_marker="$skill_path/.ctx-skill.json"
    [ -d "$skill_path" ] && [ ! -L "$skill_path" ] ||
      continue
    [ -f "$skill_body" ] && [ ! -L "$skill_body" ] ||
      continue
    [ -f "$skill_marker" ] && [ ! -L "$skill_marker" ] ||
      continue
    skill_schema="$(json_scalar_field "$skill_marker" schema_version 2>/dev/null || true)"
    skill_installer="$(json_scalar_field "$skill_marker" installer 2>/dev/null || true)"
    skill_name="$(json_scalar_field "$skill_marker" skill_name 2>/dev/null || true)"
    skill_hash="$(json_scalar_field "$skill_marker" skill_hash 2>/dev/null || true)"
    skill_actual_sha256="$(sha256_file "$skill_body" | tr 'A-F' 'a-f')"
    if [ "$skill_schema" = "1" ] &&
       [ "$skill_installer" = "ctx-cli" ] &&
       [ "$skill_name" = "ctx-agent-history-search" ] &&
       [ "$skill_hash" = "sha256:$skill_actual_sha256" ]; then
      skill_ownership_copy="$tmp_dir/skill-ownership"
      cat "$skill_body" "$skill_marker" >"$skill_ownership_copy"
      record_owned_integration skill "$(sha256_file "$skill_ownership_copy")" "$skill_path"
    fi
  done <"$skill_paths_file"
}

`;
