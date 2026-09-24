export function renderCliInstallShellPublication() {
  return `
verify_installed_target_identity() {
  target_marker="$install_path.install.json"
  [ -f "$install_path" ] && [ ! -L "$install_path" ] && [ -x "$install_path" ] ||
    fail "managed ctx upgrade did not retain a regular executable"
  [ -f "$target_marker" ] && [ ! -L "$target_marker" ] ||
    fail "managed ctx upgrade did not retain its regular install marker"
  [ "$(path_link_count "$install_path" 2>/dev/null || true)" = "1" ] &&
    [ "$(path_link_count "$target_marker" 2>/dev/null || true)" = "1" ] ||
    fail "managed ctx upgrade produced a hard-linked install identity"
  [ "$(sha256_file "$install_path" | tr 'A-F' 'a-f')" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "managed ctx upgrade did not publish the signed executable"
  [ "$(ownership_json_number_field "$target_marker" schema_version 2>/dev/null || true)" = "1" ] &&
    [ "$(ownership_json_string_field "$target_marker" manager 2>/dev/null || true)" = "ctx-hosted-installer" ] &&
    [ "$(ownership_json_string_field "$target_marker" install_path 2>/dev/null || true)" = "$(json_escape "$install_path")" ] &&
    [ "$(ownership_json_string_field "$target_marker" platform 2>/dev/null || true)" = "$platform" ] &&
    [ "$(ownership_json_string_field "$target_marker" version 2>/dev/null || true)" = "$version" ] &&
    [ "$(ownership_json_string_field "$target_marker" sha256 2>/dev/null | tr 'A-F' 'a-f' || true)" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "managed ctx upgrade returned a mismatched install marker"
  if [ "$release_phase" = "final" ] && [ -n "$pair_envelope_artifact" ]; then
    [ "$(json_top_level_boolean_field "$target_marker" managed_pair 2>/dev/null || true)" = "true" ] ||
      fail "managed ctx did not complete the signed Core/companion pair"
  fi
}

run_managed_core_upgrade() {
  # Clear progress in the parent shell before the isolated handoff can fail.
  stop_install_animation
  (
  if [ "$release_phase" = "bridge" ]; then
    CTX_SEARCH_SEMANTIC=0
    export CTX_SEARCH_SEMANTIC
  fi
  upgrade_metadata_uri="$(absolute_path_to_file_uri "$metadata_file")"
  upgrade_signature_uri="$(absolute_path_to_file_uri "$metadata_signature_file")"
  managed_upgrade_output="$tmp_dir/managed-upgrade.json"
  managed_upgrade_error="$tmp_dir/managed-upgrade.err"
  if CTX_RELEASE_METADATA_URL="$upgrade_metadata_uri" \
     CTX_RELEASE_METADATA_SIGNATURE_URL="$upgrade_signature_uri" \
     "$install_path" upgrade --channel "$channel" --format=json \
       >"$managed_upgrade_output" 2>"$managed_upgrade_error"; then
    :
  else
    managed_upgrade_status="$?"
    relay_bounded_child_stderr "$managed_upgrade_error"
    fail "installed ctx could not complete its managed lifecycle handoff (status $managed_upgrade_status); resolve the reported error before retrying"
  fi
  managed_upgrade_size="$(path_size_bytes "$managed_upgrade_output")" ||
    fail "could not determine managed ctx upgrade receipt size"
  [ "$managed_upgrade_size" -ge 2 ] && [ "$managed_upgrade_size" -le 65536 ] ||
    fail "managed ctx upgrade returned an invalid receipt; rerun this installer to finish any retained attempt"
  validate_managed_upgrade_result "$managed_upgrade_output" ||
    fail "managed ctx upgrade did not return typed lifecycle proof; rerun this installer to finish any retained attempt"
  verify_installed_target_identity
  )
}

publish_fresh_or_legacy_binary() {
  hosted_action="\${1:-install}"
  chmod 0700 "$artifact_path" || fail "could not prepare the verified ctx candidate"
  hosted_transaction_output="$tmp_dir/hosted-install-transaction.json"
  if ! "$artifact_path" upgrade \
    --hosted-transaction "$hosted_action" \
    --install-path "$install_path" \
    --attempt-id "$install_attempt_id" \
    --marker-source "$marker_tmp_path" \
    --ownership-source "$integration_manifest_tmp" \
    --binary-sha256 "$actual_checksum" \
    >"$hosted_transaction_output"; then
    fail "ctx could not complete its crash-recoverable hosted install transaction"
  fi
  [ "$(ownership_json_number_field "$hosted_transaction_output" schema_version 2>/dev/null || true)" = "1" ] &&
    [ "$(ownership_json_string_field "$hosted_transaction_output" command 2>/dev/null || true)" = "hosted_install_transaction" ] &&
    [ "$(ownership_json_string_field "$hosted_transaction_output" status 2>/dev/null || true)" = "committed" ] &&
    [ "$(ownership_json_string_field "$hosted_transaction_output" install_path 2>/dev/null || true)" = "$(json_escape "$install_path")" ] &&
    [ "$(ownership_json_string_field "$hosted_transaction_output" binary_sha256 2>/dev/null | tr 'A-F' 'a-f' || true)" = "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "ctx returned invalid hosted install transaction proof"
}

platform="\${CTX_PLATFORM:-}"
if [ -z "$platform" ]; then
  host_os="$(uname -s 2>/dev/null || printf unknown)"
  if [ "$host_os" = "FreeBSD" ]; then
    fail "FreeBSD has no prebuilt ctx binary; build ctx from source: https://github.com/ctxrs/ctx/blob/main/docs/unmanaged-installs.md#source-builds"
  fi
  platform="$(detect_platform)" || fail "cannot detect this host platform; set CTX_PLATFORM"
fi`;
}
