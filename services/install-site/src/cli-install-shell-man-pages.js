export const CLI_INSTALL_SHELL_MAN_PAGES = `man_page_receipt_directory() {
  case "$man_dir" in
    /*) printf '%s' "$man_dir" ;;
    *) printf '%s/%s' "$(pwd -P)" "$man_dir" ;;
  esac
}

owner_safe_man_directory() {
  candidate="$1"
  [ "$(path_owner_uid "$candidate" 2>/dev/null || true)" = "$(id -u)" ] || return 1
  candidate_mode="$(stat -c '%a' "$candidate" 2>/dev/null || stat -f '%Lp' "$candidate" 2>/dev/null)" || return 1
  case "$candidate_mode" in *[!0-7]*) return 1 ;; esac
  [ "\${#candidate_mode}" -ge 3 ] || return 1
  other_mode="\${candidate_mode#"\${candidate_mode%?}"}"
  group_modes="\${candidate_mode%?}"
  group_mode="\${group_modes#"\${group_modes%?}"}"
  case "$group_mode$other_mode" in *[2367]*) return 1 ;; esac
}

record_owned_man_page() {
  man_page_name="$1"
  man_page_digest="$(printf '%s' "$2" | tr 'A-F' 'a-f')"
  case "$man_page_name" in
    ctx*.1) ;;
    *) fail "invalid installer-owned man page name" ;;
  esac
  case "$man_page_name" in
    *[!A-Za-z0-9._-]*) fail "invalid installer-owned man page name" ;;
  esac
  case "$man_page_digest" in
    *[!0-9a-f]*) fail "invalid installer-owned man page digest" ;;
  esac
  [ "\${#man_page_digest}" -eq 64 ] ||
    fail "invalid installer-owned man page digest"
  printf '%s\\t%s\\n' "$man_page_name" "$man_page_digest" >>"$man_page_records_tmp"
  man_page_owned_count="$((man_page_owned_count + 1))"
}

build_installed_man_page_receipt() {
  man_page_directory="$(man_page_receipt_directory)"
  man_page_sorted_records="$tmp_dir/install-man-page-records-sorted"
  LC_ALL=C sort "$man_page_records_tmp" >"$man_page_sorted_records"
  man_page_files='['
  man_page_separator=
  tab="$(printf '\\t')"
  while IFS="$tab" read -r man_page_name man_page_digest man_page_extra; do
    [ -n "$man_page_name$man_page_digest$man_page_extra" ] || continue
    [ -z "$man_page_extra" ] || fail "ambiguous installer-owned man page record"
    man_page_files="$man_page_files$man_page_separator{\\"name\\":\\"$(json_escape "$man_page_name")\\",\\"sha256\\":\\"$man_page_digest\\"}"
    man_page_separator=,
  done <"$man_page_sorted_records"
  man_page_files="$man_page_files]"
  man_pages_json='{\"schema_version\":1,\"status\":\"installed\",\"directory\":\"'"$(json_escape "$man_page_directory")"'\",\"files\":'"$man_page_files"',\"binary_sha256\":\"'"$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')"'\"}'
  man_pages_receipt_present=1
}

initialize_man_page_receipt() {
  man_pages_json=null
  man_pages_receipt_present=0
  if [ "$install_man" != "1" ]; then
    man_pages_json='{\"schema_version\":1,\"status\":\"disabled\"}'
    man_pages_receipt_present=1
  fi
}

ownership_json_object_field() {
  json_path="$1"
  json_field="$2"
  awk -v field="$json_field" '
    BEGIN {
      quote = sprintf("%c", 34)
      backslash = sprintf("%c", 92)
    }
    function update_depth(text, i, character, escaped) {
      escaped = 0
      for (i = 1; i <= length(text); i++) {
        character = substr(text, i, 1)
        if (in_string) {
          if (escaped) escaped = 0
          else if (character == backslash) escaped = 1
          else if (character == quote) in_string = 0
        } else if (character == quote) in_string = 1
        else if (character == "{") depth++
        else if (character == "}") depth--
      }
    }
    collecting {
      result = result ORS $0
      update_depth($0)
      if (depth == 0) collecting = 0
      if (depth < 0) invalid = 1
      next
    }
    $0 ~ "^  \\"" field "\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      first = value
      sub(/^[[:space:]]*/, "", first)
      if (substr(first, 1, 1) != "{") invalid = 1
      result = value
      update_depth(value)
      if (depth > 0) collecting = 1
      if (depth < 0) invalid = 1
      next
    }
    END {
      if (count != 1 || collecting || depth != 0 || in_string || invalid) exit 1
      sub(/[[:space:]]*,[[:space:]]*$/, "", result)
      print result
    }
  ' "$json_path"
}

load_previous_man_page_receipt() {
  previous_man_page_marker="$1"
  previous_man_pages_count="$(ownership_json_field_count "$previous_man_page_marker" man_pages 2>/dev/null || true)"
  case "$previous_man_pages_count" in
    0) return 0 ;;
    1) ;;
    *) fail "prior managed install marker has ambiguous man-page ownership" ;;
  esac
  previous_man_pages_path="$tmp_dir/prior-man-pages.json"
  if ! ownership_json_object_field "$previous_man_page_marker" man_pages >"$previous_man_pages_path" ||
     ! json_document_is_well_formed "$previous_man_pages_path"; then
    fail "prior managed install marker has invalid man-page ownership"
  fi
  man_pages_json="$(cat "$previous_man_pages_path")"
  man_pages_receipt_present=1
}
`;

export function renderCliInstallShellManPageInstallation() {
  return `man_install_failed=0
man_page_owned_count=0
record_man_install_failure() {
  man_install_failed=1
}

install_new_man_page() {
  generated_page="$1"
  generated_name="$2"
  (
    cd -P "$man_dir" || exit 1
    staged_page="$(mktemp .ctx-man-install.XXXXXX)" || exit 1
    trap 'rm -f "$staged_page"' EXIT
    trap 'exit 1' HUP INT TERM
    install -m 0644 "$generated_page" "$staged_page" || exit 1
    # A relative hard link is an atomic no-replace publication. The pinned
    # working directory prevents an ancestor rename/symlink swap redirect.
    ln "$staged_page" "$generated_name"
  )
}

replace_owned_man_page() {
  generated_page="$1"
  generated_name="$2"
  expected_digest="$3"
  (
    cd -P "$man_dir" || exit 1
    [ -f "$generated_name" ] && [ ! -L "$generated_name" ] || exit 1
    [ "$(path_owner_uid "$generated_name" 2>/dev/null || true)" = "$(id -u)" ] || exit 1
    [ "$(path_link_count "$generated_name" 2>/dev/null || true)" = "1" ] || exit 1
    [ "$(sha256_file "$generated_name" 2>/dev/null | tr 'A-F' 'a-f' || true)" = "$expected_digest" ] || exit 1
    staged_page="$(mktemp .ctx-man-install.XXXXXX)" || exit 1
    trap 'rm -f "$staged_page"' EXIT
    trap 'exit 1' HUP INT TERM
    install -m 0644 "$generated_page" "$staged_page" || exit 1
    mv -f "$staged_page" "$generated_name"
  )
}

if [ "$install_man" = "1" ] && [ "$preserve_core_man_pages" != "1" ]; then
  generated_man_dir="$tmp_dir/generated-man"
  mkdir -p "$generated_man_dir"
  if "$install_path" docs man --out "$generated_man_dir" >"$tmp_dir/man-install.out" 2>&1; then
    if ! mkdir -p "$man_dir"; then
      record_man_install_failure "$man_dir" "could not create directory"
    elif [ -L "$man_dir" ]; then
      record_man_install_failure "$man_dir" "destination is a symlink"
    else
      if ! man_dir="$(cd -P "$man_dir" && pwd -P)"; then
        record_man_install_failure "$man_dir" "could not resolve directory"
      elif ! owner_safe_man_directory "$man_dir"; then
        record_man_install_failure "$man_dir" "directory is not owner-safe"
      else
        generated_man_count=0
        for generated_man_path in "$generated_man_dir"/ctx*.1; do
          [ -f "$generated_man_path" ] && [ ! -L "$generated_man_path" ] || continue
          generated_man_count="$((generated_man_count + 1))"
          generated_man_name="\${generated_man_path##*/}"
          installed_man_path="$man_dir/$generated_man_name"
          generated_man_digest="$(sha256_file "$generated_man_path" | tr 'A-F' 'a-f')"
          if [ -e "$installed_man_path" ] || [ -L "$installed_man_path" ]; then
            if [ ! -f "$installed_man_path" ] || [ -L "$installed_man_path" ] ||
               [ "$(path_owner_uid "$installed_man_path" 2>/dev/null || true)" != "$(id -u)" ] ||
               [ "$(path_link_count "$installed_man_path" 2>/dev/null || true)" != "1" ]; then
              record_man_install_failure "$installed_man_path" "existing destination is not an owner-safe regular file"
              continue
            fi
            existing_man_digest="$(sha256_file "$installed_man_path" 2>/dev/null | tr 'A-F' 'a-f' || true)"
            if [ "$existing_man_digest" = "$generated_man_digest" ]; then
              if owned_integration_matches man "$existing_man_digest" "$installed_man_path"; then
                record_owned_man_page "$generated_man_name" "$existing_man_digest"
              else
                # Matching unmanaged content remains unowned and cannot seed a
                # future automatic replacement receipt.
                record_man_install_failure "$installed_man_path" "matching page is not installer-owned"
              fi
              continue
            else
              if owned_integration_matches man "$existing_man_digest" "$installed_man_path" &&
                 replace_owned_man_page "$generated_man_path" "$generated_man_name" "$existing_man_digest"; then
                installed_man_digest="$(sha256_file "$installed_man_path" | tr 'A-F' 'a-f')"
                record_owned_integration man "$installed_man_digest" "$installed_man_path"
                record_owned_man_page "$generated_man_name" "$installed_man_digest"
              else
                record_man_install_failure "$installed_man_path" "existing page differs and is not installer-owned"
              fi
              continue
            fi
          fi
          if install_new_man_page "$generated_man_path" "$generated_man_name"; then
            installed_man_digest="$(sha256_file "$installed_man_path" | tr 'A-F' 'a-f')"
            record_owned_integration man "$installed_man_digest" "$installed_man_path"
            record_owned_man_page "$generated_man_name" "$installed_man_digest"
          else
            record_man_install_failure "$installed_man_path" "could not install generated page"
          fi
        done
        if [ "$generated_man_count" = "0" ]; then
          record_man_install_failure "$man_dir" "no generated ctx*.1 pages"
        fi
      fi
    fi
  else
    record_man_install_failure "$man_dir" "ctx docs man failed"
  fi
fi

if [ "$preserve_core_man_pages" = "1" ]; then
  : # The new binary's first-start reconciler owns managed reruns.
elif [ "$managed_reinstall" = "1" ] || [ "$install_man" != "1" ]; then
  : # Receipts are created only by fresh installs.
elif [ "$man_install_failed" = "0" ] &&
     [ "\${generated_man_count:-0}" -gt 0 ] &&
     [ "$man_page_owned_count" -eq "$generated_man_count" ]; then
  build_installed_man_page_receipt
else
  man_pages_json=null
  man_pages_receipt_present=0
fi`;
}
