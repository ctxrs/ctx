import { CLI_INSTALL_SHELL_PRIOR_INTEGRATION } from "./cli-install-shell-prior-integration.js";

export const CLI_INSTALL_SHELL_VALIDATION = `json_scalar_field() {
  json_file="$1"
  json_field="$2"
  awk -v field="$json_field" '
    $0 ~ "^  \\\"" field "\\\"[[:space:]]*:" {
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/[[:space:]]*,?[[:space:]]*$/, "", value)
      gsub(/^"|"$/, "", value)
      print value
      exit
    }
  ' "$json_file"
}

json_document_is_well_formed() {
  json_file="$1"
  json_document_size="$(LC_ALL=C wc -c < "$json_file" 2>/dev/null)" || return 1
  json_document_size="\${json_document_size##* }"
  case "$json_document_size" in
    ""|*[!0-9]*) return 1 ;;
  esac
  [ "$json_document_size" -le 1048576 ] || return 1
  LC_ALL=C awk '
    BEGIN {
      quote = sprintf("%c", 34)
      backslash = sprintf("%c", 92)
      delete_character = sprintf("%c", 127)
      tab = sprintf("%c", 9)
      newline = sprintf("%c", 10)
      carriage_return = sprintf("%c", 13)
      for (byte = 0; byte <= 255; byte++) {
        byte_value[sprintf("%c", byte)] = byte
      }
    }
    function skip_whitespace(    character) {
      while (position <= document_length) {
        character = substr(document, position, 1)
        if (character != " " && character != tab &&
            character != newline && character != carriage_return) return
        position++
      }
    }
    function consume_utf8_scalar(    first, second, third, fourth) {
      first = byte_value[substr(document, position, 1)]
      if (first >= 194 && first <= 223) {
        if (position + 1 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        if (second < 128 || second > 191) return 0
        position += 2
        return 1
      }
      if (first == 224) {
        if (position + 2 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        third = byte_value[substr(document, position + 2, 1)]
        if (second < 160 || second > 191 ||
            third < 128 || third > 191) return 0
        position += 3
        return 1
      }
      if ((first >= 225 && first <= 236) ||
          (first >= 238 && first <= 239)) {
        if (position + 2 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        third = byte_value[substr(document, position + 2, 1)]
        if (second < 128 || second > 191 ||
            third < 128 || third > 191) return 0
        position += 3
        return 1
      }
      if (first == 237) {
        if (position + 2 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        third = byte_value[substr(document, position + 2, 1)]
        if (second < 128 || second > 159 ||
            third < 128 || third > 191) return 0
        position += 3
        return 1
      }
      if (first == 240) {
        if (position + 3 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        third = byte_value[substr(document, position + 2, 1)]
        fourth = byte_value[substr(document, position + 3, 1)]
        if (second < 144 || second > 191 ||
            third < 128 || third > 191 ||
            fourth < 128 || fourth > 191) return 0
        position += 4
        return 1
      }
      if (first >= 241 && first <= 243) {
        if (position + 3 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        third = byte_value[substr(document, position + 2, 1)]
        fourth = byte_value[substr(document, position + 3, 1)]
        if (second < 128 || second > 191 ||
            third < 128 || third > 191 ||
            fourth < 128 || fourth > 191) return 0
        position += 4
        return 1
      }
      if (first == 244) {
        if (position + 3 > document_length) return 0
        second = byte_value[substr(document, position + 1, 1)]
        third = byte_value[substr(document, position + 2, 1)]
        fourth = byte_value[substr(document, position + 3, 1)]
        if (second < 128 || second > 143 ||
            third < 128 || third > 191 ||
            fourth < 128 || fourth > 191) return 0
        position += 4
        return 1
      }
      return 0
    }
    function hex_value(character,    offset) {
      offset = index("0123456789abcdef", tolower(character))
      return offset == 0 ? -1 : offset - 1
    }
    function parse_hex_quad(start,    offset, digit, value) {
      value = 0
      for (offset = 0; offset < 4; offset++) {
        digit = hex_value(substr(document, start + offset, 1))
        if (digit < 0) return -1
        value = value * 16 + digit
      }
      return value
    }
    function parse_string(    character, escape, codepoint, low_surrogate, byte) {
      if (substr(document, position, 1) != quote) return 0
      position++
      while (position <= document_length) {
        character = substr(document, position, 1)
        if (character == quote) {
          position++
          return 1
        }
        if (character == backslash) {
          position++
          if (position > document_length) return 0
          escape = substr(document, position, 1)
          if (escape == "u") {
            codepoint = parse_hex_quad(position + 1)
            if (codepoint < 0) return 0
            if (codepoint >= 55296 && codepoint <= 56319) {
              if (substr(document, position + 5, 2) != backslash "u") return 0
              low_surrogate = parse_hex_quad(position + 7)
              if (low_surrogate < 56320 || low_surrogate > 57343) return 0
              position += 11
            } else {
              if (codepoint >= 56320 && codepoint <= 57343) return 0
              position += 5
            }
          } else if (escape == quote || escape == backslash ||
                     escape == "/" || escape == "b" || escape == "f" ||
                     escape == "n" || escape == "r" || escape == "t") {
            position++
          } else {
            return 0
          }
        } else {
          byte = byte_value[character]
          if (byte >= 128) {
            if (!consume_utf8_scalar()) return 0
          } else {
            if (character ~ /[[:cntrl:]]/ &&
                character != delete_character) return 0
            position++
          }
        }
      }
      return 0
    }
    function parse_number(    character) {
      if (substr(document, position, 1) == "-") position++
      character = substr(document, position, 1)
      if (character == "0") {
        position++
      } else if (character ~ /^[1-9]$/) {
        do {
          position++
          character = substr(document, position, 1)
        } while (character ~ /^[0-9]$/)
      } else {
        return 0
      }
      if (substr(document, position, 1) == ".") {
        position++
        if (substr(document, position, 1) !~ /^[0-9]$/) return 0
        while (substr(document, position, 1) ~ /^[0-9]$/) position++
      }
      character = substr(document, position, 1)
      if (character == "e" || character == "E") {
        position++
        character = substr(document, position, 1)
        if (character == "+" || character == "-") position++
        if (substr(document, position, 1) !~ /^[0-9]$/) return 0
        while (substr(document, position, 1) ~ /^[0-9]$/) position++
      }
      return 1
    }
    function parse_object(depth,    character) {
      position++
      skip_whitespace()
      if (substr(document, position, 1) == "}") {
        position++
        return 1
      }
      while (position <= document_length) {
        if (!parse_string()) return 0
        skip_whitespace()
        if (substr(document, position, 1) != ":") return 0
        position++
        if (!parse_value(depth + 1)) return 0
        skip_whitespace()
        character = substr(document, position, 1)
        if (character == "}") {
          position++
          return 1
        }
        if (character != ",") return 0
        position++
        skip_whitespace()
      }
      return 0
    }
    function parse_array(depth,    character) {
      position++
      skip_whitespace()
      if (substr(document, position, 1) == "]") {
        position++
        return 1
      }
      while (position <= document_length) {
        if (!parse_value(depth + 1)) return 0
        skip_whitespace()
        character = substr(document, position, 1)
        if (character == "]") {
          position++
          return 1
        }
        if (character != ",") return 0
        position++
        skip_whitespace()
      }
      return 0
    }
    function parse_value(depth,    character) {
      if (depth > 64) return 0
      skip_whitespace()
      character = substr(document, position, 1)
      if (character == "{") return parse_object(depth)
      if (character == "[") return parse_array(depth)
      if (character == quote) return parse_string()
      if (character == "-" || character ~ /^[0-9]$/) return parse_number()
      if (substr(document, position, 4) == "true") {
        position += 4
        return 1
      }
      if (substr(document, position, 5) == "false") {
        position += 5
        return 1
      }
      if (substr(document, position, 4) == "null") {
        position += 4
        return 1
      }
      return 0
    }
    {
      if (NR > 4096) too_many_records = 1
      if (!too_many_records) {
        if (NR > 1) document = document newline
        document = document $0
        if (length(document) > 1048576) too_large = 1
      }
    }
    END {
      if (too_large || too_many_records) exit 1
      document_length = length(document)
      position = 1
      if (!parse_value(0)) exit 1
      skip_whitespace()
      if (position != document_length + 1) exit 1
    }
  ' "$json_file"
}

json_top_level_boolean_field() {
  json_file="$1"
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
        }
      }
    }
    depth == 1 &&
      $0 ~ "^[[:space:]]*\\\"" field "\\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/[[:space:]]*,?[[:space:]]*$/, "", value)
      if (value != "true" && value != "false") invalid = 1
      result = value
    }
    {
      update_depth($0)
      if (depth < 0) invalid = 1
    }
    END {
      if (depth != 0 || in_string || count != 1 || invalid) exit 1
      print result
    }
  ' "$json_file"
}

json_top_level_string_field() {
  json_file="$1"
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
    depth == 1 &&
      $0 ~ "^[[:space:]]*\\\"" field "\\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*,?[[:space:]]*$/, "", value)
      if (value !~ /^[a-z][a-z0-9_]*$/) invalid = 1
      result = value
    }
    {
      update_depth($0)
      if (depth < 0) invalid = 1
    }
    END {
      if (depth != 0 || in_string || count != 1 || invalid) exit 1
      print result
    }
  ' "$json_file"
}

json_top_level_unsigned_integer_or_null_field() {
  json_file="$1"
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
        }
      }
    }
    depth == 1 &&
      $0 ~ "^[[:space:]]*\\\"" field "\\\"[[:space:]]*:" {
      count++
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/[[:space:]]*,?[[:space:]]*$/, "", value)
      if (value != "null" && value !~ /^(0|[1-9][0-9]*)$/) invalid = 1
      result = value
    }
    {
      update_depth($0)
      if (depth < 0) invalid = 1
    }
    END {
      if (depth != 0 || in_string || count > 1 || invalid) exit 1
      if (count == 0) {
        print "null"
      } else {
        print result
      }
    }
  ' "$json_file"
}

json_object_string_field() {
  json_file="$1"
  json_object="$2"
  json_field="$3"
  awk -v object="$json_object" -v field="$json_field" '
    BEGIN {
      quote = sprintf("%c", 34)
      backslash = sprintf("%c", 92)
    }
    function update_depth(text, i, character, escaped) {
      escaped = 0
      for (i = 1; i <= length(text); i++) {
        character = substr(text, i, 1)
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
        }
      }
    }
    !in_object && depth == 1 &&
      $0 ~ "^[[:space:]]*\\\"" object "\\\"[[:space:]]*:[[:space:]]*{" {
      object_count++
      update_depth($0)
      in_object = 1
      object_depth = depth
      next
    }
    in_object && depth == object_depth &&
      $0 ~ "^[[:space:]]*\\\"" field "\\\"[[:space:]]*:" {
      field_count++
      value = $0
      sub(/^[^:]*:[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*,?[[:space:]]*$/, "", value)
      if (value !~ /^[a-z][a-z0-9_]*$/) invalid = 1
      result = value
    }
    {
      update_depth($0)
      if (depth < 0) invalid = 1
      if (in_object && depth < object_depth) {
        in_object = 0
      }
    }
    END {
      if (depth != 0 || in_string || object_count != 1 ||
          field_count != 1 || invalid) exit 1
      print result
    }
  ' "$json_file"
}

json_object_boolean_field() {
  json_file="$1"
  json_object="$2"
  json_field="$3"
  awk -v object="$json_object" -v field="$json_field" '
    BEGIN {
      quote = sprintf("%c", 34)
      backslash = sprintf("%c", 92)
    }
    function update_depth(text, i, character, escaped) {
      escaped = 0
      for (i = 1; i <= length(text); i++) {
        character = substr(text, i, 1)
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
        }
      }
    }
    !in_object && depth == 1 &&
      $0 ~ "^[[:space:]]*\\\"" object "\\\"[[:space:]]*:[[:space:]]*{" {
      object_count++
      update_depth($0)
      in_object = 1
      object_depth = depth
      next
    }
    in_object && depth == object_depth &&
      $0 ~ "^[[:space:]]*\\\"" field "\\\"[[:space:]]*:" {
      field_count++
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/[[:space:]]*,?[[:space:]]*$/, "", value)
      if (value != "true" && value != "false") invalid = 1
      result = value
    }
    {
      update_depth($0)
      if (depth < 0) invalid = 1
      if (in_object && depth < object_depth) {
        in_object = 0
      }
    }
    END {
      if (depth != 0 || in_string || object_count != 1 ||
          field_count != 1 || invalid) exit 1
      print result
    }
  ' "$json_file"
}

json_object_string_or_null_field() {
  json_file="$1"
  json_object="$2"
  json_field="$3"
  json_required="\${4:-1}"
  LC_ALL=C awk -v object="$json_object" -v field="$json_field" \
    -v required="$json_required" '
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
    !in_object && depth == 1 &&
      $0 ~ "^[[:space:]]*\\\"" object "\\\"[[:space:]]*:[[:space:]]*{" {
      object_count++
      update_depth($0)
      in_object = 1
      object_depth = depth
      next
    }
    in_object && depth == object_depth &&
      $0 ~ "^[[:space:]]*\\\"" field "\\\"[[:space:]]*:" {
      field_count++
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/[[:space:]]*,?[[:space:]]*$/, "", value)
      if (value == "null") {
        result = ""
      } else if (value ~ /^"[ -~]*"$/ && index(value, backslash) == 0) {
        sub(/^"/, "", value)
        sub(/"$/, "", value)
        result = value
      } else {
        invalid = 1
      }
    }
    {
      update_depth($0)
      if (depth < 0) invalid = 1
      if (in_object && depth < object_depth) in_object = 0
    }
    END {
      if (depth != 0 || in_string || object_count != 1 ||
          field_count > 1 || (required == 1 && field_count != 1) || invalid) exit 1
      print result
    }
  ' "$json_file"
}

is_unsigned_integer() {
  case "$1" in
    ""|*[!0-9]*) return 1 ;;
    *) return 0 ;;
  esac
}

format_count() {
  awk -v value="$1" '
    BEGIN {
      formatted = value
      suffix = ""
      while (length(formatted) > 3) {
        suffix = "," substr(formatted, length(formatted) - 2) suffix
        formatted = substr(formatted, 1, length(formatted) - 3)
      }
      print formatted suffix
    }
  '
}

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
  path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{ print $1 }'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{ print $1 }'
    return 0
  fi
  if command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$path"
    return 0
  fi
  fail "sha256sum, shasum, or sha256 is required"
}

json_escape() {
  printf '%s' "$1" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g'
}

valid_sha256() {
  digest_value="$1"
  [ "\${#digest_value}" -eq 64 ] || return 1
  case "$digest_value" in
    *[!0-9a-f]*) return 1 ;;
  esac
}

ownership_json_string_field() {
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

ownership_json_field_count() {
  json_path="$1"
  json_field="$2"
  awk -v field="$json_field" '
    $0 ~ "^  \\\"" field "\\\"[[:space:]]*:" {
      count++
    }
    END {
      print count + 0
    }
  ' "$json_path"
}

ownership_json_number_field() {
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

valid_prior_owned_path() {
  candidate="$1"
  case "$candidate" in
    /*) ;;
    *) return 1 ;;
  esac
  case "/$candidate/" in
    */../*|*/./*) return 1 ;;
  esac
  carriage_return="$(printf '\\r')"
  case "$candidate" in
    *"$carriage_return"*) return 1 ;;
  esac
  case "$candidate" in
    *'
'*|*'	'*) return 1 ;;
  esac
}

valid_prior_profile_path() {
  candidate="$1"
  valid_prior_owned_path "$candidate" || return 1
  case "$candidate" in
    "$HOME/.bashrc"|"$HOME/.bash_profile"|"$HOME/.profile"|\
    "\${ZDOTDIR:-$HOME}/.zshrc"|"$HOME/.config/fish/config.fish") return 0 ;;
    *) return 1 ;;
  esac
}

prior_regular_file_matches() {
  candidate="$1"
  expected_digest="$2"
  [ -f "$candidate" ] && [ ! -L "$candidate" ] ||
    return 1
  [ "$(path_owner_uid "$candidate" 2>/dev/null || true)" = "$current_uid" ] ||
    return 1
  [ "$(path_link_count "$candidate" 2>/dev/null || true)" = "1" ] ||
    return 1
  actual_digest="$(sha256_file "$candidate" 2>/dev/null | tr 'A-F' 'a-f' || true)"
  [ "$actual_digest" = "$expected_digest" ]
}

prior_profile_block_matches() {
  candidate="$1"
  expected_digest="$2"
  valid_prior_profile_path "$candidate" ||
    return 1
  [ -f "$candidate" ] && [ ! -L "$candidate" ] ||
    return 1
  [ "$(path_owner_uid "$candidate" 2>/dev/null || true)" = "$current_uid" ] ||
    return 1
  [ "$(path_link_count "$candidate" 2>/dev/null || true)" = "1" ] ||
    return 1
  profile_extract="$tmp_dir/prior-profile-block"
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
  ' "$candidate" >"$profile_extract"; then
    return 1
  fi
  [ "$(sha256_file "$profile_extract" | tr 'A-F' 'a-f')" = "$expected_digest" ]
}

prior_skill_matches() {
  candidate="$1"
  expected_digest="$2"
  valid_prior_owned_path "$candidate" || return 1
  case "$candidate" in
    */skills/ctx-agent-history-search) ;;
    *) return 1 ;;
  esac
  skill_body="$candidate/SKILL.md"
  skill_marker="$candidate/.ctx-skill.json"
  [ -d "$candidate" ] && [ ! -L "$candidate" ] ||
    return 1
  [ -f "$skill_body" ] && [ ! -L "$skill_body" ] ||
    return 1
  [ -f "$skill_marker" ] && [ ! -L "$skill_marker" ] ||
    return 1
  for skill_owned_file in "$skill_body" "$skill_marker"; do
    [ "$(path_owner_uid "$skill_owned_file" 2>/dev/null || true)" = "$current_uid" ] ||
      return 1
    [ "$(path_link_count "$skill_owned_file" 2>/dev/null || true)" = "1" ] ||
      return 1
  done
  skill_schema="$(json_scalar_field "$skill_marker" schema_version 2>/dev/null || true)"
  skill_installer="$(json_scalar_field "$skill_marker" installer 2>/dev/null || true)"
  skill_name="$(json_scalar_field "$skill_marker" skill_name 2>/dev/null || true)"
  skill_hash="$(json_scalar_field "$skill_marker" skill_hash 2>/dev/null || true)"
  skill_actual_sha256="$(sha256_file "$skill_body" 2>/dev/null | tr 'A-F' 'a-f' || true)"
  [ "$skill_schema" = "1" ] &&
    [ "$skill_installer" = "ctx-cli" ] &&
    [ "$skill_name" = "ctx-agent-history-search" ] &&
    [ "$skill_hash" = "sha256:$skill_actual_sha256" ] ||
    return 1
  skill_ownership_copy="$tmp_dir/prior-skill-ownership"
  cat "$skill_body" "$skill_marker" >"$skill_ownership_copy"
  [ "$(sha256_file "$skill_ownership_copy" | tr 'A-F' 'a-f')" = "$expected_digest" ]
}

prior_record_matches() {
  prior_kind="$1"
  prior_digest="$2"
  prior_target="$3"
  case "$prior_kind" in
    man)
      valid_prior_owned_path "$prior_target" || return 1
      prior_name="\${prior_target##*/}"
      case "$prior_name" in ctx*.1) ;; *) return 1 ;; esac
      [ "$(canonical_file_path "$prior_target" 2>/dev/null || true)" = "$prior_target" ] ||
        return 1
      prior_regular_file_matches "$prior_target" "$prior_digest"
      ;;
    profile-file)
      valid_prior_profile_path "$prior_target" &&
        prior_regular_file_matches "$prior_target" "$prior_digest"
      ;;
    profile-block)
      prior_profile_block_matches "$prior_target" "$prior_digest"
      ;;
    skill)
      prior_skill_matches "$prior_target" "$prior_digest"
      ;;
    *) return 1 ;;
  esac
}

${CLI_INSTALL_SHELL_PRIOR_INTEGRATION}

`;
