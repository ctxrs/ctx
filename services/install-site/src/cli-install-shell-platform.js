export function renderCliInstallShellPlatform({ normalizedMetadataPublicKeyPem }) {
  return `detect_platform() {
  os="$(uname -s 2>/dev/null || printf unknown)"
  arch="$(uname -m 2>/dev/null || printf unknown)"
  case "$os:$arch" in
    Linux:x86_64|Linux:amd64) printf 'linux-x64' ;;
    Linux:aarch64|Linux:arm64) printf 'linux-aarch64' ;;
    Darwin:arm64|Darwin:aarch64) printf 'macos-arm64' ;;
    Darwin:x86_64|Darwin:amd64) printf 'macos-x64' ;;
    *) return 1 ;;
  esac
}

write_bounded_file() {
  # Apple's system sh uses Bash 3.2's fixed 1024-byte file-limit units even
  # in POSIX mode. Supported Linux system sh implementations use 512 bytes.
  case "$(uname -s)" in
    Darwin) file_limit_unit=1024 ;;
    Linux) file_limit_unit=512 ;;
    *) fail "cannot determine file-limit units for this host" ;;
  esac
  file_limit_blocks=$((($1 + file_limit_unit - 1) / file_limit_unit))
  shift
  # Keep the limit in the prescribed system shell, independent of caller mode.
  /bin/sh -c 'ulimit -c 0 || exit 1; ulimit -f "$1" || exit 1; shift; exec "$@"' \\
    ctx-bounded "$file_limit_blocks" "$@"
}

download_file() {
  url="$1"
  dest="$2"
  case "$url" in
    https://*) ;;
    *) fail "refusing non-HTTPS download URL: $url" ;;
  esac
  # curl may finish one last attempt after retry-max-time; it is not a total
  # operation deadline. Each attempt, including redirects/body, has max-time.
  write_bounded_file "\${3:-268435456}" curl --proto '=https' --tlsv1.2 -fsSL \\
    --retry 3 --connect-timeout 20 --max-time "\${4:-3600}" \\
    --retry-max-time "\${4:-3600}" "$url" -o "$dest"
}

download_release_artifact() {
  raw_url="$1"
  raw_dest="$2"
  gzip_url="$raw_url.gz"
  gzip_dest="$raw_dest.gz"
  artifact_compression="identity"

  if command -v gzip >/dev/null 2>&1; then
    # Official single-member gzip producers: G(C)=C+ceil(C/8)+ceil(C/64)+32.
    # C=256MiB, G(C)=292MiB+32; round up to the system shell's file-limit unit.
    if download_file "$gzip_url" "$gzip_dest" 306184224 3600 2>/dev/null; then
      if ! write_bounded_file 268435456 gzip -dc "$gzip_dest" >"$raw_dest"; then
        fail "could not decompress release artifact: $gzip_url"
      fi
      artifact_compression="gzip"
      return 0
    fi
    rm -f "$gzip_dest"
  fi

  download_file "$raw_url" "$raw_dest"
}

write_metadata_public_key() {
  cat >"$1" <<'CTX_METADATA_PUBLIC_KEY'
${normalizedMetadataPublicKeyPem}
CTX_METADATA_PUBLIC_KEY
}

verify_release_metadata_signature() {
  metadata_path="$1"
  signature_path="$2"
  public_key_path="$3"
  raw_signature_path="$tmp_dir/metadata.sig.raw"

  if ! openssl enc -A -d -base64 -in "$signature_path" -out "$raw_signature_path" 2>/dev/null; then
    fail "metadata signature is not base64-encoded RSA-SHA256 bytes"
  fi
  [ -s "$raw_signature_path" ] || fail "metadata signature is empty"
  if ! openssl dgst -sha256 -verify "$public_key_path" -signature "$raw_signature_path" "$metadata_path" >/dev/null 2>&1; then
    fail "metadata signature verification failed"
  fi
}

metadata_value() {
  file="$1"
  key="$2"
  awk -F= -v key="$key" '
    $0 ~ /^[[:space:]]*#/ { next }
    $1 == key { print substr($0, length(key) + 2); found = 1; exit }
    END { if (!found) exit 1 }
  ' "$file"
}

metadata_value_optional() {
  file="$1"
  key="$2"
  metadata_value "$file" "$key" 2>/dev/null || true
}

load_persisted_config_controls() {
  persisted_semantic_enabled=0
  persisted_daemon_disabled=0
  if [ -n "\${CTX_DATA_ROOT:-}" ]; then
    persisted_config_file="\${CTX_DATA_ROOT%/}/config.toml"
  else
    [ -n "\${HOME:-}" ] || return 0
    persisted_config_file="\${HOME%/}/.ctx/config.toml"
  fi
  [ -f "$persisted_config_file" ] || return 0

  if persisted_config_values="$(LC_ALL=C awk -v config_path="$persisted_config_file" '
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
        if (value == before) {
          break
        }
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
        if (value == before) {
          break
        }
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
        } else if ((first >= 225 && first <= 236) || (first >= 238 && first <= 239)) {
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
    function strip_comment(line, position, char, in_single, in_double, escaped) {
      in_single = 0
      in_double = 0
      escaped = 0
      for (position = 1; position <= length(line); position += 1) {
        char = substr(line, position, 1)
        if (in_double) {
          if (escaped) {
            escaped = 0
          } else if (char == "\\\\") {
            escaped = 1
          } else if (char == "\\"") {
            in_double = 0
          }
          continue
        }
        if (in_single) {
          if (char == sprintf("%c", 39)) {
            in_single = 0
          }
          continue
        }
        if (char == "#") {
          return substr(line, 1, position - 1)
        }
        if (char == "\\"") {
          in_double = 1
        } else if (char == sprintf("%c", 39)) {
          in_single = 1
        }
      }
      return line
    }
    function reject(message) {
      print message
      invalid = 1
      exit 2
    }
    function quoted_string(value, first, last) {
      if (length(value) < 2) {
        return 0
      }
      first = substr(value, 1, 1)
      last = substr(value, length(value), 1)
      return (first == "\\"" && last == "\\"") ||
        (first == sprintf("%c", 39) && last == sprintf("%c", 39))
    }
    # Only validate values consumed by installer preflight. Core owns the full schema.
    function validate_value(full_key, value, line_number, string_value) {
      if (full_key == "daemon.enabled" ||
          full_key == "search.semantic") {
        if (value != "true" && value != "false") {
          reject(full_key " at line " line_number " must be a boolean")
        }
        return
      }
      if (full_key == "indexing.mode") {
        if (!quoted_string(value)) {
          reject(full_key " at line " line_number " must be a quoted string")
        }
        string_value = tolower(substr(value, 2, length(value) - 2))
        if (string_value != "auto" && string_value != "automatic" && string_value != "manual") {
          reject(full_key " at line " line_number " must be either \"auto\" or \"manual\"")
        }
        return
      }
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
      semantic_enabled = 0
      legacy_daemon_disabled = 0
      indexing_mode_set = 0
      indexing_disabled = 0
    }
    {
      if (!valid_utf8($0)) {
        reject("persisted config is not valid UTF-8: " config_path)
      }
      line = trim(strip_comment($0))
      if (line == "") {
        next
      }
      if (substr(line, 1, 1) == "[") {
        if (substr(line, length(line), 1) != "]") {
          reject("invalid config section header at line " NR ": " line)
        }
        section = trim(substr(line, 2, length(line) - 2))
        if (section == "") {
          reject("empty config section header at line " NR)
        }
        next
      }
      equals = index(line, "=")
      if (equals == 0) {
        reject("invalid config line " NR ": expected \`[section]\` or \`key = value\`")
      }
      key = trim(substr(line, 1, equals - 1))
      if (key == "") {
        reject("empty config key at line " NR)
      }
      value = trim(substr(line, equals + 1))
      full_key = section == "" ? key : section "." key
      if (full_key in first_line) {
        reject(sprintf("duplicate config key \`%s\` at line %d; first set at line %d", full_key, NR, first_line[full_key]))
      }
      first_line[full_key] = NR
      validate_value(full_key, value, NR)
      if (full_key == "search.semantic") {
        semantic_enabled = value == "true"
      } else if (full_key == "daemon.enabled") {
        legacy_daemon_disabled = value == "false"
      } else if (full_key == "indexing.mode") {
        indexing_mode_set = 1
        indexing_value = tolower(substr(value, 2, length(value) - 2))
        indexing_disabled = indexing_value == "manual"
      }
    }
    END {
      if (!invalid) {
        daemon_disabled = indexing_mode_set ? indexing_disabled : legacy_daemon_disabled
        printf "%d %d\\n", semantic_enabled, daemon_disabled
      }
    }
  ' "$persisted_config_file")"; then
    set -- $persisted_config_values
    [ "$#" -eq 2 ] || fail "could not parse persisted config controls"
    persisted_semantic_enabled="$1"
    persisted_daemon_disabled="$2"
  else
    [ -n "$persisted_config_values" ] ||
      persisted_config_values="could not read persisted config: $persisted_config_file"
    fail "$persisted_config_values"
  fi
}

canonical_daemon_disabled() {
  [ "\${CTX_DAEMON_ENABLED+x}" = "x" ] || return 1
  daemon_control_value="$CTX_DAEMON_ENABLED"
  while :; do
    case "$daemon_control_value" in
      [[:space:]]*) daemon_control_value="\${daemon_control_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$daemon_control_value" in
      *[[:space:]]) daemon_control_value="\${daemon_control_value%?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$daemon_control_value" in
      '"'* ) daemon_control_value="\${daemon_control_value#?}" ;;
      *) break ;;
    esac
  done
  while :; do
    case "$daemon_control_value" in
      *'"') daemon_control_value="\${daemon_control_value%?}" ;;
      *) break ;;
    esac
  done
  case "$daemon_control_value" in
    0|[Ff][Aa][Ll][Ss][Ee]|[Nn][Oo]|[Oo][Ff][Ff]) return 0 ;;
    *) return 1 ;;
  esac
}

absolute_path_to_file_uri() {
  file_path="$1"
  case "$file_path" in
    /*) ;;
    *) fail "cannot create file URI from non-absolute path: $file_path" ;;
  esac
  printf 'file://'
  printf '%s' "$file_path" | LC_ALL=C od -A n -v -t x1 | awk '
    {
      for (i = 1; i <= NF; i++) {
        if ($i == "2f") {
          printf "/"
        } else {
          printf "%%%s", toupper($i)
        }
      }
    }
    END { printf "\\n" }
  '
}

validate_safe_value() {
  name="$1"
  value="$2"
  case "$value" in
    *'
'*|*'..'*|*'/'*|*'\\'*) fail "unsafe $name: $value" ;;
  esac
}

profile_has_path_line() {
  profile="$1"
  needle="$2"
  [ -f "$profile" ] || return 1
  awk -v needle="$needle" '
    $0 ~ /^[[:space:]]*#/ { next }
    index($0, needle) && ($0 ~ /PATH/ || $0 ~ /fish_user_paths/ || $0 ~ /fish_add_path/) {
      found = 1
      exit
    }
    END { exit found ? 0 : 1 }
  ' "$profile"
}

shell_double_quote_escape() {
  printf '%s' "$1" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g; s/\`/\\\\\`/g; s/\\$/\\\\$/g'
}

path_setup_profile() {
  shell_name="$1"
  test -n "\${HOME:-}" || return 1

  case "$shell_name" in
    fish)
      printf '%s/.config/fish/config.fish\\n' "$HOME"
      ;;
    zsh)
      printf '%s/.zshrc\\n' "\${ZDOTDIR:-$HOME}"
      ;;
    bash)
      if [ -f "$HOME/.bashrc" ]; then
        printf '%s/.bashrc\\n' "$HOME"
      elif [ -f "$HOME/.bash_profile" ]; then
        printf '%s/.bash_profile\\n' "$HOME"
      elif [ -f "$HOME/.profile" ]; then
        printf '%s/.profile\\n' "$HOME"
      else
        case "$platform" in
          macos-*) printf '%s/.bash_profile\\n' "$HOME" ;;
          *) printf '%s/.bashrc\\n' "$HOME" ;;
        esac
      fi
      ;;
    *)
      printf '%s/.profile\\n' "$HOME"
      ;;
  esac
}

profile_contains_path_setup() {
  profile="$1"
  dir="\${2%/}"
  profile_has_path_line "$profile" "$dir" && return 0
  dir_escaped="$(shell_double_quote_escape "$dir")"
  profile_has_path_line "$profile" "$dir_escaped" && return 0

  if [ -n "\${HOME:-}" ]; then
    home_prefix="\${HOME%/}/"
    case "$dir" in
      "$home_prefix"*)
        rel="\${dir#"$home_prefix"}"
        profile_has_path_line "$profile" "\\$HOME/$rel" && return 0
        profile_has_path_line "$profile" "~/$rel" && return 0
        ;;
    esac
  fi

  return 1
}

path_setup_snippet() {
  shell_name="$1"
  dir_escaped="$(shell_double_quote_escape "$2")"

  case "$shell_name" in
    fish)
      cat <<EOF
# >>> ctx installer PATH setup >>>
if test "\\$PATH[1]" != "$dir_escaped"
    set -gx PATH "$dir_escaped" \\$PATH
end
# <<< ctx installer PATH setup <<<
EOF
      ;;
    *)
      cat <<EOF
# >>> ctx installer PATH setup >>>
case "\\\${PATH}:" in
  "$dir_escaped:"*) ;;
  *) export PATH="$dir_escaped:\\\${PATH}" ;;
esac
# <<< ctx installer PATH setup <<<
EOF
      ;;
  esac
}

bare_ctx_resolves_to_install() {
  resolved_ctx="$(command -v ctx 2>/dev/null || true)"
  [ -n "$resolved_ctx" ] || return 1
  resolved_ctx="$(canonical_file_path "$resolved_ctx" 2>/dev/null || true)"
  [ -n "$resolved_ctx" ] && [ "$resolved_ctx" = "$install_path" ]
}

configure_path_if_needed() {
  dir="\${bin_dir%/}"
  path_result=
  path_export_command=
  path_profile_persisted=0
  path_profile_semantics=0
  if bare_ctx_resolves_to_install; then
    return 0
  fi

  dir_escaped="$(shell_double_quote_escape "$dir")"
  path_export_command="export PATH=\\"$dir_escaped:\\$PATH\\""
  path_display_dir="$dir"
  if [ -n "\${HOME:-}" ] && [ "$dir" = "\${HOME%/}/.local/bin" ]; then
    path_display_dir='\$HOME/.local/bin'
    path_export_command='export PATH="\$HOME/.local/bin:\$PATH"'
  fi
  shell_name="\${SHELL:-}"
  shell_name="\${shell_name##*/}"
  [ -n "$shell_name" ] || shell_name="sh"

  if [ "$modify_path" != "1" ]; then
    path_result=1
    return 0
  fi

  if [ -n "\${GITHUB_PATH:-}" ]; then
    printf '%s\\n' "$dir" >>"$GITHUB_PATH" || true
    path_result=1
    return 0
  fi

  if [ "\${CI:-}" = "1" ] || [ "\${CI:-}" = "true" ]; then
    path_result=1
    return 0
  fi

  if ! profile="$(path_setup_profile "$shell_name")"; then
    path_result=1
    return 0
  fi

  if profile_contains_path_setup "$profile" "$dir"; then
    path_profile_semantics=1
    path_result=1
  else
    profile_dir="$(dirname "$profile")"
    profile_existed=0
    if [ -e "$profile" ] || [ -L "$profile" ]; then
      profile_existed=1
    fi
    profile_snippet="$tmp_dir/path-profile-snippet"
    path_setup_snippet "$shell_name" "$dir" >"$profile_snippet"
    if [ "$profile_existed" = "1" ] &&
       { [ ! -f "$profile" ] ||
         [ -L "$profile" ] ||
         [ "$(path_owner_uid "$profile" 2>/dev/null || true)" != "$(id -u)" ] ||
         [ "$(stat -c '%h' "$profile" 2>/dev/null || stat -f '%l' "$profile" 2>/dev/null || true)" != "1" ]; }; then
      path_result=1
    elif mkdir -p "$profile_dir" &&
         { [ "$profile_existed" != "1" ] ||
           [ ! -s "$profile" ] ||
           [ -z "$(tail -c 1 "$profile" 2>/dev/null)" ] ||
           printf '\\n' >>"$profile"; } &&
         cat "$profile_snippet" >>"$profile"; then
      if [ "$profile_existed" = "1" ]; then
        record_owned_integration profile-block "$(sha256_file "$profile_snippet")" "$profile"
      else
        record_owned_integration profile-file "$(sha256_file "$profile")" "$profile"
      fi
      path_profile_persisted=1
      path_result=1
    else
      path_result=1
    fi
  fi
}

`;
}
