export function renderHostedUninstallTransactionHelpers() {
  return `recovery_path_mode() {
  recovery_mode="$(stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1" 2>/dev/null)" || return 1
  case "$recovery_mode" in ""|*[!0-7]*) return 1 ;; esac
  [ "\${#recovery_mode}" -le 4 ] || return 1
  printf '%s' "$recovery_mode"
}

validate_recovery_private_file() {
  recovery_file="$1"
  [ -f "$recovery_file" ] && [ ! -L "$recovery_file" ] &&
    [ "$(path_owner_uid "$recovery_file" 2>/dev/null || true)" = "$(id -u)" ] &&
    [ "$(path_link_count "$recovery_file" 2>/dev/null || true)" = "1" ] ||
    fail "cannot authenticate interrupted uninstall file ownership"
  recovery_file_mode="$(recovery_path_mode "$recovery_file")" ||
    fail "cannot classify interrupted uninstall file permissions"
  [ "$((0$recovery_file_mode & 077))" = "0" ] ||
    fail "interrupted uninstall file is not owner-private"
}

reject_retired_recovery_delete_data() {
  [ "$data_choice" = "--delete-data" ] || return 0
  recovery_helper="\${install_path%/*}/.\${install_path##*/}.hosted-uninstall-helper"
  recovery_journal="\${install_path%/*}/.\${install_path##*/}.hosted-install-transaction.json"
  if [ ! -e "$recovery_journal" ] && [ ! -L "$recovery_journal" ] &&
     [ ! -e "$recovery_helper" ] && [ ! -L "$recovery_helper" ]; then
    return 0
  fi
  recovery_directory="\${install_path%/*}"
  [ -d "$recovery_directory" ] && [ ! -L "$recovery_directory" ] &&
    [ "$(cd -P "$recovery_directory" && pwd -P)" = "$recovery_directory" ] &&
    [ "$(path_owner_uid "$recovery_directory" 2>/dev/null || true)" = "$(id -u)" ] &&
    [ "$marker_path" = "$install_path.install.json" ] ||
    fail "interrupted uninstall requires the canonical owner-controlled install directory"
  recovery_directory_mode="$(recovery_path_mode "$recovery_directory")" ||
    fail "cannot classify interrupted uninstall directory permissions"
  # Match the native Unix install-directory policy: retain configured group
  # access, but reject other-account writes. State/executable copies are private.
  [ "$((0$recovery_directory_mode & 002))" = "0" ] ||
    fail "interrupted uninstall directory allows other accounts to write"
  validate_recovery_private_file "$recovery_journal"
  if [ ! -e "$recovery_helper" ] && [ ! -L "$recovery_helper" ] &&
     [ -f "$install_path" ] && [ -f "$marker_path" ]; then
    # Preparation may have recorded its journal before staging the helper.
    validate_managed_install
    load_installed_cli_version
    if installed_cli_is_unified; then
      fail "--delete-data was legacy derived-data cleanup and is retired in ctx 1.5. History and legacy data are preserved; rerun without --delete-data"
    fi
    return 0
  fi
  validate_recovery_private_file "$recovery_helper"
  [ "$(path_size_bytes "$recovery_journal")" -le 16777216 ] &&
    [ "$(json_scalar_field "$recovery_journal" schema_version)" = "1" ] &&
    [ "$(json_string_field "$recovery_journal" kind)" = "uninstall" ] &&
    [ "$(json_string_field "$recovery_journal" install_path)" = "$(json_escape "$install_path")" ] ||
    fail "cannot authenticate interrupted uninstall identity; rerun without --delete-data"
  recovery_digest="$(json_string_field "$recovery_journal" binary_sha256)" ||
    fail "interrupted uninstall is missing its executable identity"
  valid_sha256 "$recovery_digest" && [ -x "$recovery_helper" ] &&
    [ "$(path_size_bytes "$recovery_helper")" -le 536870912 ] &&
    [ "$(sha256_file "$recovery_helper" | tr 'A-F' 'a-f')" = "$recovery_digest" ] ||
    fail "interrupted uninstall helper differs from its recorded executable identity"
  load_cli_version "$recovery_helper"
  if installed_cli_is_unified; then
    fail "--delete-data was legacy derived-data cleanup and is retired in ctx 1.5. History and legacy data are preserved; rerun without --delete-data"
  fi
}

validate_hosted_uninstall_transaction_receipt() {
  transaction_receipt="$1"
  expected_status="$2"
  expected_helper="$install_path"
  expected_helper="\${expected_helper%/*}/.\${expected_helper##*/}.hosted-uninstall-helper"
  [ "$(json_scalar_field "$transaction_receipt" schema_version 2>/dev/null || true)" = "2" ] &&
    [ "$(json_string_field "$transaction_receipt" command 2>/dev/null || true)" = "hosted_uninstall_transaction" ] &&
    [ "$(json_string_field "$transaction_receipt" status 2>/dev/null || true)" = "$expected_status" ] &&
    [ "$(json_scalar_field "$transaction_receipt" daemon_admission_fenced 2>/dev/null || true)" = "true" ] &&
    [ "$(json_string_field "$transaction_receipt" install_path 2>/dev/null || true)" = "$(json_escape "$install_path")" ] &&
    [ "$(json_string_field "$transaction_receipt" helper_path 2>/dev/null || true)" = "$(json_escape "$expected_helper")" ] ||
    fail "ctx returned invalid hosted uninstall transaction proof"
  hosted_uninstall_helper="$expected_helper"
}

prepare_hosted_uninstall_transaction() {
  transaction_owner="$install_path"
  if [ ! -x "$transaction_owner" ]; then
    transaction_owner="\${install_path%/*}/.\${install_path##*/}.hosted-uninstall-helper"
  fi
  [ -f "$transaction_owner" ] && [ ! -L "$transaction_owner" ] && [ -x "$transaction_owner" ] ||
    fail "an interrupted hosted uninstall has no recorded executable helper"
  hosted_transaction_output="$(mktemp "\${TMPDIR:-/tmp}/ctx-hosted-uninstall.XXXXXX")" ||
    fail "could not create hosted uninstall transaction proof"
  if ! "$transaction_owner" upgrade \
    --hosted-transaction uninstall-prepare \
    --install-path "$install_path" \
    --attempt-id "$install_attempt_id" >"$hosted_transaction_output"; then
    fail "ctx could not prepare its crash-recoverable hosted uninstall transaction"
  fi
  validate_hosted_uninstall_transaction_receipt "$hosted_transaction_output" prepared
  rm -f "$hosted_transaction_output"
}

commit_hosted_uninstall_transaction() {
  hosted_transaction_output="$(mktemp "\${TMPDIR:-/tmp}/ctx-hosted-uninstall.XXXXXX")" ||
    fail "could not create hosted uninstall transaction proof"
  if ! "$hosted_uninstall_helper" upgrade \
    --hosted-transaction uninstall-arm \
    --install-path "$install_path" >"$hosted_transaction_output"; then
    fail "ctx could not arm its hosted uninstall transaction"
  fi
  validate_hosted_uninstall_transaction_receipt "$hosted_transaction_output" armed
  if ! "$hosted_uninstall_helper" upgrade \
    --hosted-transaction uninstall-commit \
    --install-path "$install_path" >"$hosted_transaction_output"; then
    fail "ctx could not commit its hosted uninstall transaction"
  fi
  validate_hosted_uninstall_transaction_receipt "$hosted_transaction_output" committed
  rm -f "$hosted_transaction_output"
  rm -f "$hosted_uninstall_helper"
}

recover_hosted_uninstall_transaction() {
  hosted_uninstall_helper="\${install_path%/*}/.\${install_path##*/}.hosted-uninstall-helper"
  [ -f "$hosted_uninstall_helper" ] && [ ! -L "$hosted_uninstall_helper" ] &&
    [ -x "$hosted_uninstall_helper" ] ||
    fail "an interrupted hosted uninstall has no recorded executable helper"
  hosted_transaction_output="$(mktemp "\${TMPDIR:-/tmp}/ctx-hosted-uninstall.XXXXXX")" ||
    fail "could not create hosted uninstall transaction proof"
  if ! "$hosted_uninstall_helper" upgrade \
    --hosted-transaction uninstall-commit \
    --install-path "$install_path" >"$hosted_transaction_output"; then
    fail "ctx could not recover its hosted uninstall transaction"
  fi
  validate_hosted_uninstall_transaction_receipt "$hosted_transaction_output" committed
  rm -f "$hosted_transaction_output" "$hosted_uninstall_helper"
}

try_recover_armed_hosted_uninstall_transaction() {
  hosted_uninstall_helper="\${install_path%/*}/.\${install_path##*/}.hosted-uninstall-helper"
  hosted_uninstall_journal="\${install_path%/*}/.\${install_path##*/}.hosted-install-transaction.json"
  [ -f "$hosted_uninstall_journal" ] && [ ! -L "$hosted_uninstall_journal" ] &&
    [ -f "$hosted_uninstall_helper" ] && [ ! -L "$hosted_uninstall_helper" ] &&
    [ -x "$hosted_uninstall_helper" ] || return 1
  hosted_transaction_output="$(mktemp "\${TMPDIR:-/tmp}/ctx-hosted-uninstall.XXXXXX")" ||
    fail "could not create hosted uninstall transaction proof"
  if ! "$hosted_uninstall_helper" upgrade \
    --hosted-transaction uninstall-commit \
    --install-path "$install_path" >"$hosted_transaction_output" 2>/dev/null; then
    rm -f "$hosted_transaction_output"
    return 1
  fi
  validate_hosted_uninstall_transaction_receipt "$hosted_transaction_output" committed
  rm -f "$hosted_transaction_output" "$hosted_uninstall_helper"
  return 0
}
`;
}

export function renderHostedUninstallVersionHelpers() {
  return `load_cli_version() {
  version_output="$("$1" --version 2>/dev/null)" ||
    fail "unable to determine the installed ctx version; reinstall ctx and try again"
  case "$version_output" in
    "ctx "*) installed_cli_version_value="\${version_output#ctx }" ;;
    *) fail "the installed ctx binary returned an invalid version" ;;
  esac
  version_core="\${installed_cli_version_value%%[-+]*}"
  case "$version_core" in
    *.*.*) ;;
    *) fail "the installed ctx binary returned an invalid version" ;;
  esac
  version_major="\${version_core%%.*}"
  version_remainder="\${version_core#*.}"
  version_minor="\${version_remainder%%.*}"
  version_patch="\${version_remainder#*.}"
  for version_component in "$version_major" "$version_minor" "$version_patch"; do
    case "$version_component" in
      ""|*[!0-9]*) fail "the installed ctx binary returned an invalid version" ;;
    esac
  done
}

load_installed_cli_version() {
  load_cli_version "$install_path"
  [ "$installed_cli_version_value" = "$marker_version" ] ||
    fail "installed ctx version differs from its managed install marker"
}

installed_cli_is_unified() {
  [ "$version_major" -gt 1 ] || { [ "$version_major" -eq 1 ] && [ "$version_minor" -ge 5 ]; }
}
`;
}
