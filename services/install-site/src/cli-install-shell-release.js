import { FROZEN_BRIDGE_VERSION, FROZEN_BRIDGE_METADATA_URL, CLI_INSTALL_SHELL_VERSION_COMPARE } from "./cli-install-bridge.js";
import {
  renderCliInstallShellManagedPairArtifactUrls,
  renderCliInstallShellManagedPairMetadata,
  renderCliInstallShellManagedPairPreparation,
  renderCliInstallShellManagedPairTemporaryPaths,
} from "./cli-install-shell-managed-pair.js";

export function renderCliInstallShellReleasePreparation() {
  return `load_release_phase_metadata() {
phase_dir="$tmp_dir/$release_phase"
mkdir -p "$phase_dir"
metadata_file="$phase_dir/metadata.env"
metadata_signature_file="$phase_dir/metadata.env.sig"
metadata_public_key_file="$tmp_dir/metadata-public-key.pem"
artifact_path="$phase_dir/ctx"
${renderCliInstallShellManagedPairTemporaryPaths()}
if [ ! -f "$metadata_file" ]; then
  download_file "$metadata_url" "$metadata_file" 1048576 300
  download_file "$metadata_signature_url" "$metadata_signature_file" 65536 300
fi
write_metadata_public_key "$metadata_public_key_file"
verify_release_metadata_signature "$metadata_file" "$metadata_signature_file" "$metadata_public_key_file"

schema_version="$(metadata_value "$metadata_file" CTX_RELEASE_SCHEMA_VERSION)" || fail "metadata missing CTX_RELEASE_SCHEMA_VERSION"
version="$(metadata_value "$metadata_file" CTX_RELEASE_VERSION)" || fail "metadata missing CTX_RELEASE_VERSION"
base_url="$(metadata_value "$metadata_file" CTX_RELEASE_BASE_URL)" || fail "metadata missing CTX_RELEASE_BASE_URL"
platform_key="$(printf '%s\\n' "$platform" | tr '-' '_')"
${renderCliInstallShellManagedPairMetadata()}
artifact="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_ARTIFACT_$platform_key")"
checksum="$(metadata_value_optional "$metadata_file" "CTX_RELEASE_SHA256_$platform_key")"
release_channel="$(metadata_value_optional "$metadata_file" CTX_RELEASE_CHANNEL)"
source_commit="$(metadata_value_optional "$metadata_file" CTX_RELEASE_SOURCE_COMMIT)"
published_at="$(metadata_value_optional "$metadata_file" CTX_RELEASE_PUBLISHED_AT)"
[ -n "$release_channel" ] || release_channel="$channel"

[ "$schema_version" = "1" ] || fail "unsupported metadata schema: $schema_version"
[ "$release_channel" = "$channel" ] || fail "metadata channel $release_channel does not match requested channel $channel"
case "$base_url" in https://*) ;; *) fail "metadata base URL must be HTTPS" ;; esac
case "$base_url" in
  https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/*) ;;
  *)
    [ "\${CTX_ALLOW_CUSTOM_RELEASE_BASE_URL:-0}" = "1" ] || fail "metadata base URL must be under https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/"
    ;;
esac
${renderCliInstallShellManagedPairPreparation()}
if [ -n "$pair_envelope_artifact" ] && [ "\${bin_dir##*/}" != "bin" ]; then
  fail "managed-pair install directory must be <root>/bin"
fi
case "$checksum" in
  [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]) ;;
  *) fail "checksum for $platform is not a SHA-256 hex digest" ;;
esac
[ "$checksum" != "0000000000000000000000000000000000000000000000000000000000000000" ] || fail "checksum for $platform is a placeholder"
validate_safe_value "artifact name" "$artifact"

${renderCliInstallShellManagedPairArtifactUrls()}
install_path="\${bin_dir%/}/ctx"

}

download_release_phase_artifacts() {
start_install_animation

report_install_stage "artifact_download" "started"
download_release_artifact "$artifact_url" "$artifact_path"
if [ -n "$pair_envelope_artifact" ]; then
  download_release_artifact "$companion_artifact_url" "$companion_artifact_path"
fi
report_install_stage "artifact_download" "completed"
actual_checksum="$(sha256_file "$artifact_path")"
if [ "$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')" != "$(printf '%s' "$checksum" | tr 'A-F' 'a-f')" ]; then
  fail "checksum mismatch for $artifact: expected $checksum, got $actual_checksum"
fi

}

`;
}

export function renderCliInstallShellReleaseSequence({ stagingDogfood }) {
  return `${CLI_INSTALL_SHELL_VERSION_COMPARE}
final_metadata_url="$metadata_url"
final_metadata_signature_url="$metadata_signature_url"
release_phase=final
load_release_phase_metadata
final_version="$version"
final_checksum="$checksum"
bridge_required=0
pending_hosted_migration=0
stable_bridge=${stagingDogfood ? 0 : 1}
[ "$channel" = "stable" ] || stable_bridge=0
if [ "$stable_bridge" = "1" ]; then
  final_order="$(compare_release_versions "$final_version" "${FROZEN_BRIDGE_VERSION}")" ||
    fail "invalid release version: $final_version"
  [ "$final_order" != "-1" ] || fail "stable installer targets before ${FROZEN_BRIDGE_VERSION} are unsupported"
fi
if [ "$explicit_metadata" = "1" ] && [ "$semantic_enabled" = "1" ]; then
  fail "explicit metadata cannot authorize Semantic repair through the installed release; use the default installer feed"
fi
if [ -L "$install_path.install.json" ] ||
   { [ -e "$install_path.install.json" ] && [ ! -f "$install_path.install.json" ]; }; then
  fail "managed install marker destination is not a regular file"
fi
if [ -e "$install_path" ] || [ -L "$install_path" ] ||
   [ -e "$install_path.install.json" ] || [ -L "$install_path.install.json" ]; then
  if [ "$explicit_metadata" = "1" ] && [ "${stagingDogfood ? 1 : 0}" != "1" ]; then
    fail "managed reinstall cannot honor an explicit metadata target; use the default installer feed"
  fi
  if [ -e "$bin_dir/.ctx.hosted-install-transaction.json" ]; then
    pending_hosted_migration=1
  fi
  # An intact old image still selects B while its owner resumes a pending B.
  # A partially published image is classified after the existing recovery call.
  if [ "$pending_hosted_migration" != "1" ] &&
     { [ ! -e "$bin_dir/.ctx.upgrade-install-transaction.json" ] ||
       (actual_checksum=; validate_existing_managed_install >/dev/null 2>&1); }; then
    actual_checksum="$checksum"
    validate_existing_managed_install
    if [ "$stable_bridge" = "1" ]; then
      installed_order="$(compare_release_versions "$final_version" "$previous_version")" ||
        fail "invalid managed release version: $previous_version"
      [ "$installed_order" != "-1" ] || fail "refusing to downgrade the managed ctx installation"
      if [ "$installed_order" = "0" ] && [ "$previous_binary_actual" = "$previous_binary_digest" ] &&
         [ "$previous_binary_digest" != "$(printf '%s' "$checksum" | tr 'A-F' 'a-f')" ]; then
        fail "signed release differs from the installed identity at the same version"
      fi
      bridge_order="$(compare_release_versions "$previous_version" "${FROZEN_BRIDGE_VERSION}")" ||
        fail "invalid managed release version: $previous_version"
      if [ "$previous_binary_digest" = "$previous_binary_actual" ] && [ "$bridge_order" = "-1" ]; then
        bridge_required=1
      fi
    fi
  fi
fi
if [ "$bridge_required" = "1" ]; then
  release_phase=bridge
  metadata_url="${FROZEN_BRIDGE_METADATA_URL}"
  metadata_signature_url="$metadata_url.sig"
  load_release_phase_metadata
  [ "$version" = "${FROZEN_BRIDGE_VERSION}" ] && [ -n "$pair_envelope_artifact" ] ||
    fail "frozen bridge metadata does not identify the required signed ${FROZEN_BRIDGE_VERSION} pair"
  if [ "$final_version" = "$version" ] && [ "$checksum" != "$final_checksum" ]; then
    fail "final metadata conflicts with the frozen bridge identity"
  fi
  download_release_phase_artifacts
  publish_release_phase
fi
release_phase=final
metadata_url="$final_metadata_url"
metadata_signature_url="$final_metadata_signature_url"
load_release_phase_metadata
download_release_phase_artifacts
publish_release_phase
`;
}
