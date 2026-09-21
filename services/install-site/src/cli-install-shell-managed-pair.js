import { managedPairApplyArguments, MANAGED_PAIR_APPLY_RECEIPT } from "./cli-install-managed-pair-contract.js";

export function renderCliInstallShellManagedPairPreparation() {
  return `is_sha256() {
  [ "\${#1}" = "64" ] || return 1
  case "$1" in *[!0-9a-fA-F]*) return 1 ;; esac
}

validate_pair_object_key() {
  object_label="$1"
  object_key="$2"
  object_algorithm="\${object_key%%/*}"
  object_rest="\${object_key#*/}"
  object_digest="\${object_rest%%/*}"
  object_name="\${object_rest#*/}"
  [ "$object_algorithm" = "sha256" ] &&
    [ "$object_rest" != "$object_key" ] &&
    [ "$object_name" != "$object_rest" ] &&
    [ "\${object_name#*/}" = "$object_name" ] &&
    is_sha256 "$object_digest" ||
    fail "$object_label object key is invalid"
  validate_safe_value "$object_label artifact name" "$object_name"
}

if [ -n "$pair_envelope_artifact" ]; then
  [ -n "$pair_core_object_key" ] || fail "metadata missing managed-pair Core object key"
  [ -n "$pair_core_checksum" ] || fail "metadata missing managed-pair Core checksum"
  [ -n "$pair_companion_object_key" ] || fail "metadata missing managed-pair companion object key"
  [ -n "$pair_companion_checksum" ] || fail "metadata missing managed-pair companion checksum"
  validate_safe_value "managed-pair envelope name" "$pair_envelope_artifact"
  validate_pair_object_key "managed-pair Core" "$pair_core_object_key"
  validate_pair_object_key "managed-pair companion" "$pair_companion_object_key"
  is_sha256 "$pair_core_checksum" || fail "managed-pair Core checksum is invalid"
  is_sha256 "$pair_companion_checksum" || fail "managed-pair companion checksum is invalid"
  pair_core_key_digest="\${pair_core_object_key#sha256/}"
  pair_core_key_digest="\${pair_core_key_digest%%/*}"
  pair_companion_key_digest="\${pair_companion_object_key#sha256/}"
  pair_companion_key_digest="\${pair_companion_key_digest%%/*}"
  [ "$(printf '%s' "$pair_core_key_digest" | tr 'A-F' 'a-f')" = "$(printf '%s' "$pair_core_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "managed-pair Core object key does not match its checksum"
  [ "$(printf '%s' "$pair_companion_key_digest" | tr 'A-F' 'a-f')" = "$(printf '%s' "$pair_companion_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "managed-pair companion object key does not match its checksum"
  artifact="\${pair_core_object_key##*/}"
  checksum="$pair_core_checksum"
  companion_artifact="\${pair_companion_object_key##*/}"
  download_file "\${base_url%/}/$pair_envelope_artifact" "$pair_envelope_path" 2097152 300
else
  [ -z "$pair_core_object_key$pair_core_checksum$pair_companion_object_key$pair_companion_checksum" ] ||
    fail "managed-pair component metadata is present without an envelope"
  [ -n "$artifact" ] || fail "metadata missing artifact for $platform"
  [ -n "$checksum" ] || fail "metadata missing checksum for $platform"
fi`;
}

export function renderCliInstallShellManagedPairTemporaryPaths() {
  return `companion_artifact_path="$phase_dir/ctx-pro"
pair_envelope_path="$phase_dir/managed-pair-envelope.json"`;
}

export function renderCliInstallShellManagedPairMetadata() {
  return `pair_envelope_artifact=
pair_core_object_key=
pair_core_checksum=
pair_companion_object_key=
pair_companion_checksum=
if [ "$(compare_release_versions "$version" "1.5.0")" = "-1" ]; then
pair_envelope_artifact="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_MANAGED_PAIR_ENVELOPE_$platform_key")"
pair_core_object_key="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_$platform_key")"
pair_core_checksum="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_$platform_key")"
pair_companion_object_key="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_$platform_key")"
pair_companion_checksum="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_$platform_key")"
fi`;
}

export function renderCliInstallShellManagedPairSetupExecution() {
  return `set -- setup --quiet --format json
if [ "$setup_wait_requested" = "1" ]; then
  set -- "$@" --wait
fi
if [ "$semantic_enabled" = "1" ]; then
  set -- "$@" --semantic
fi
set -- "$@" --progress "$setup_progress"
if [ "$setup_no_daemon" = "1" ]; then
  set -- "$@" --no-daemon
fi
run_hosted_setup() {
  CTX_HOSTED_INSTALLER_SETUP=1 "$install_path" "$@"
}
if [ "$setup_progress" = "none" ]; then
  run_hosted_setup "$@" \
    >"$tmp_dir/setup-receipt.json" 2>"$tmp_dir/setup.err" || setup_status="$?"
else
  run_hosted_setup "$@" \
    >"$tmp_dir/setup-receipt.json" || setup_status="$?"
fi`;
}

export function renderCliInstallShellManagedPairArtifactUrls() {
  return `if [ -n "$pair_envelope_artifact" ]; then
  artifact_url="\${base_url%/}/$pair_core_object_key"
  companion_artifact_url="\${base_url%/}/$pair_companion_object_key"
else
  artifact_url="\${base_url%/}/$artifact"
  companion_artifact_url=
fi`;
}

export function renderCliInstallShellManagedPairApply() {
  const applyArguments = managedPairApplyArguments({
    installRoot: '"$pair_install_root"',
    envelope: '"$pair_envelope_path"',
    core: '"$artifact_path"',
    companion: '"$companion_artifact_path"',
    marker: '"$pair_apply_marker"',
  }).join(" ");
  return `relay_bounded_child_stderr() {
  child_stderr_path="$1"
  [ -f "$child_stderr_path" ] && [ ! -L "$child_stderr_path" ] || return 0
  child_stderr_size="$(path_size_bytes "$child_stderr_path" 2>/dev/null || true)"
  case "$child_stderr_size" in ""|*[!0-9]*) return 0 ;; esac
  [ "$child_stderr_size" -gt 0 ] || return 0
  stop_install_animation
  log "ctx child output:"
  LC_ALL=C dd if="$child_stderr_path" bs=8192 count=1 2>/dev/null | awk '
    NR > 40 { next }
    {
      line = $0
      gsub(/[^ -~\t]/, "?", line)
      gsub(/[Bb][Ee][Aa][Rr][Ee][Rr][ \t]+[^ \t]+/, "Bearer <redacted>", line)
      gsub(/[Tt][Oo][Kk][Ee][Nn][ \t]*=[ \t]*[^ \t]+/, "token=<redacted-credential>", line)
      gsub(/[Ss][Ee][Cc][Rr][Ee][Tt][ \t]*=[ \t]*[^ \t]+/, "secret=<redacted-credential>", line)
      gsub(/[?&][Tt][Oo][Kk][Ee][Nn]=[^& \t]+/, "?<redacted-credential>", line)
      gsub(/[?&][Ss][Ee][Cc][Rr][Ee][Tt]=[^& \t]+/, "?<redacted-credential>", line)
      print line > "/dev/stderr"
    }
  ' || :
  [ "$child_stderr_size" -le 8192 ] || log "... ctx child output truncated ..."
}

apply_managed_pair_candidate() {
  pair_apply_marker="$1"
  pair_apply_required="$2"
  case "\${bin_dir##*/}" in
    bin) pair_install_root="\${bin_dir%/*}" ;;
    *) [ "$pair_apply_required" = "0" ] && return 1
       fail "managed-pair install directory must be <root>/bin" ;;
  esac
  case "$pair_install_root" in
    /*) ;;
    *) [ "$pair_apply_required" = "0" ] && return 1
       fail "managed-pair install root must be absolute" ;;
  esac
  chmod 0700 "$artifact_path" || fail "could not prepare the verified ctx candidate"
  pair_apply_receipt="$tmp_dir/managed-pair-apply.json"
  pair_apply_error="$tmp_dir/managed-pair-apply.err"
  pair_apply_status=0
  "$artifact_path" ${applyArguments} >"$pair_apply_receipt" 2>"$pair_apply_error" || pair_apply_status="$?"
  if [ "$pair_apply_status" != "0" ]; then
    [ "$pair_apply_required" = "0" ] && return 1
    relay_bounded_child_stderr "$pair_apply_error"
    fail "ctx managed-pair installation did not complete; resolve the error above before retrying (release $version, exit code $pair_apply_status)"
  fi
  managed_pair_success_receipt "$pair_apply_receipt" ${MANAGED_PAIR_APPLY_RECEIPT.command} || {
    [ "$pair_apply_required" = "0" ] && return 1
    relay_bounded_child_stderr "$pair_apply_error"
    fail "candidate Core returned invalid managed-pair apply proof (release $version); rerun this installer command to retry safely"
  }
  while IFS= read -r pair_warning; do
    receipt_warning "$pair_warning"
  done <"$tmp_dir/managed_pair_apply.warnings"
  return 0
}

managed_pair_success_receipt() {
  receipt_path="$1"
  receipt_command="$2"
  warnings_path="$tmp_dir/$receipt_command.warnings"
  : >"$warnings_path" || return 1
  [ "$(path_size_bytes "$receipt_path" 2>/dev/null || true)" -ge 1 ] &&
    [ "$(path_size_bytes "$receipt_path" 2>/dev/null || true)" -le 512 ] || return 1
  awk -v command="$receipt_command" '
    BEGIN { quote = sprintf("%c", 34); prefix = "{" quote "schema_version" quote ":${MANAGED_PAIR_APPLY_RECEIPT.schema_version}," quote "command" quote ":" quote command quote "," quote "ok" quote ":${MANAGED_PAIR_APPLY_RECEIPT.ok}," quote "status" quote ":" quote "${MANAGED_PAIR_APPLY_RECEIPT.status}" quote }
    NR != 1 { invalid = 1 }
    {
      receipt = $0
      if (receipt == prefix "}") next
      expected = prefix "," quote "warnings" quote ":["
      if (index(receipt, expected) != 1 || substr(receipt, length(receipt) - 1) != "]}") { invalid = 1; next }
      warnings = substr(receipt, length(expected) + 1, length(receipt) - length(expected) - 2)
      while (length(warnings)) {
        if (substr(warnings, 1, 1) != quote) { invalid = 1; break }
        quote_end = index(substr(warnings, 2), quote)
        if (!quote_end) { invalid = 1; break }
        warning = substr(warnings, 2, quote_end - 1)
        if (length(warning) < 1 || length(warning) > 160 || warning !~ /^[A-Za-z0-9 .,:;()\\/_-]+$/) { invalid = 1; break }
        print warning
        count++
        warnings = substr(warnings, quote_end + 2)
        if (!length(warnings)) break
        if (substr(warnings, 1, 1) != ",") { invalid = 1; break }
        warnings = substr(warnings, 2)
      }
      if (count < 1 || count > 4) invalid = 1
    }
    END { exit invalid }
  ' "$receipt_path" >"$warnings_path" || return 1
  return 0
}

try_resume_interrupted_managed_pair() {
  [ -f "$bin_dir/.ctx.upgrade-install-transaction.json" ] &&
    [ ! -L "$bin_dir/.ctx.upgrade-install-transaction.json" ] || return 0
  if [ -f "$install_path.install.json" ] && [ ! -L "$install_path.install.json" ] &&
     [ -f "$install_path" ] && [ ! -L "$install_path" ] &&
     [ "$(sha256_file "$install_path" 2>/dev/null | tr 'A-F' 'a-f')" = \\
       "$(ownership_json_string_field "$install_path.install.json" sha256 2>/dev/null | tr 'A-F' 'a-f')" ]; then
    return 0
  fi
  if [ "$(compare_release_versions "$version" "1.5.0")" != "-1" ]; then
    # Only the authenticated downloaded candidate executes. Its existing
    # transaction owner validates these fixed retained slots under its lock.
    (
      retained_pair_root="\${bin_dir%/*}/share/ctx/.managed-pair-apply-v1"
      pair_envelope_path="$retained_pair_root/share/ctx/managed-pair-envelope.json"
      companion_artifact_path="$retained_pair_root/libexec/ctx-pro"
      apply_managed_pair_candidate "$retained_pair_root/bin/ctx.install.json" 1
    ) || fail "ctx could not resume the retained managed-pair transaction"
  elif [ -n "$pair_envelope_artifact" ] &&
       [ -f "$install_path.install.json" ] && [ ! -L "$install_path.install.json" ]; then
    stage_install_marker "$tmp_dir/install-marker.XXXXXX"
    apply_managed_pair_candidate "$marker_tmp_path" 0 || :
  fi
}`;
}

export function renderCliInstallShellManagedPairReconciliation() {
  return `reconcile_managed_pair_integration() {
  case "\${bin_dir##*/}" in
    bin) pair_reconcile_root="\${bin_dir%/*}" ;;
    *) return 1 ;;
  esac
  case "$pair_reconcile_root" in /*) ;; *) return 1 ;; esac
  pair_reconcile_receipt="$tmp_dir/managed-pair-reconcile.json"
  pair_reconcile_error="$tmp_dir/managed-pair-reconcile.err"
  if ! "$install_path" --ctx-core-managed-pair-reconcile-integration-v1 "$pair_reconcile_root" - "$integration_manifest_tmp" >"$pair_reconcile_receipt" 2>"$pair_reconcile_error"; then
    relay_bounded_child_stderr "$pair_reconcile_error"
    return 1
  fi
  if ! managed_pair_success_receipt "$pair_reconcile_receipt" managed_pair_reconcile_integration; then
    relay_bounded_child_stderr "$pair_reconcile_error"
    return 1
  fi
  while IFS= read -r pair_warning; do
    receipt_warning "$pair_warning"
  done <"$tmp_dir/managed_pair_reconcile_integration.warnings"
  return 0
}`;
}

/** @param {{corePublication: string, pairManagedRerun: string}} fragments */
export function renderCliInstallShellManagedPairPublication({
  corePublication,
  pairManagedRerun,
}) {
  return `if [ -n "$pair_envelope_artifact" ]; then
  actual_companion_checksum="$(sha256_file "$companion_artifact_path")"
  [ "$(printf '%s' "$actual_companion_checksum" | tr 'A-F' 'a-f')" = "$(printf '%s' "$pair_companion_checksum" | tr 'A-F' 'a-f')" ] ||
    fail "checksum mismatch for $companion_artifact: expected $pair_companion_checksum, got $actual_companion_checksum"
  if [ "$managed_reinstall" = "1" ]; then
${pairManagedRerun}
  else
    stage_install_marker "$tmp_dir/install-marker.XXXXXX"
    apply_managed_pair_candidate "$marker_tmp_path" 1
    managed_core_handoff=1
  fi
else
${corePublication}
    stage_integration_ownership
    stage_install_marker "$tmp_dir/install-marker.XXXXXX"
    publish_fresh_or_legacy_binary
  fi
fi`;
}
