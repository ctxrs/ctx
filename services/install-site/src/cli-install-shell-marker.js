export function renderCliInstallShellMarker({ stagingDogfoodMarker }) {
  return `stage_install_marker() {
  marker_path="$install_path.install.json"
  installed_at="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  marker_tmp_pattern="\${1:-$marker_path.tmp.XXXXXX}"
  marker_tmp_path="$(mktemp "$marker_tmp_pattern")" ||
    fail "could not create managed install marker"
  if ! chmod 0600 "$marker_tmp_path"; then
    rm -f "$marker_tmp_path"
    marker_tmp_path=
    fail "could not secure managed install marker"
  fi
  marker_man_pages_line=
  if [ "$man_pages_receipt_present" = "1" ] || [ "\${2:-0}" = "1" ]; then
    marker_man_pages_line=',
  "man_pages": '"$man_pages_json"
  fi
  marker_integrations_lines=
  if [ "\${integration_path+x}" = "x" ] || [ "\${integration_sha256+x}" = "x" ]; then
    [ -n "\${integration_path:-}" ] && [ -n "\${integration_sha256:-}" ] || {
      rm -f "$marker_tmp_path"
      marker_tmp_path=
      fail "managed integration ownership is incomplete"
    }
    marker_integrations_lines=',
  "integrations_path": "'"$(json_escape "$integration_path")"'",
  "integrations_sha256": "'"$(json_escape "$integration_sha256")"'"'
  fi
  marker_pair_line=
  if [ -n "\${pair_envelope_artifact:-}" ]; then
    marker_pair_line=',
  "managed_pair": true'
  fi
  if ! cat >"$marker_tmp_path" <<EOF
{
  "schema_version": 1,
  "manager": "ctx-hosted-installer",${stagingDogfoodMarker}
  "install_attempt_id": "$(json_escape "$install_attempt_id")",
  "install_path": "$(json_escape "$install_path")",
  "platform": "$(json_escape "$platform")",
  "channel": "$(json_escape "$release_channel")",
  "version": "$(json_escape "$version")",
  "sha256": "$(json_escape "$actual_checksum")",
  "metadata_url": "$(json_escape "$metadata_url")",
  "artifact_url": "$(json_escape "$artifact_url")",
  "source_commit": "$(json_escape "$source_commit")",
  "published_at": "$(json_escape "$published_at")",
  "installed_at": "$(json_escape "$installed_at")"$marker_pair_line$marker_man_pages_line$marker_integrations_lines
}
EOF
  then
    rm -f "$marker_tmp_path"
    marker_tmp_path=
    fail "could not write managed install marker"
  fi
}

write_install_marker() {
  stage_install_marker "$marker_path.tmp.XXXXXX"
  if [ -L "$marker_path" ] ||
     { [ -e "$marker_path" ] && [ ! -f "$marker_path" ]; }; then
    rm -f "$marker_tmp_path"
    marker_tmp_path=
    fail "managed install marker destination is not a regular file"
  fi
  if ! mv -f "$marker_tmp_path" "$marker_path"; then
    rm -f "$marker_tmp_path"
    marker_tmp_path=
    fail "could not publish managed install marker"
  fi
  marker_tmp_path=
}
disable_core_man_pages_before_upgrade() {
  preupgrade_marker="$install_path.install.json"
  if ! awk '
    /^  "man_pages"[[:space:]]*:/ { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$preupgrade_marker"; then
    return 0
  fi
  "$install_path" --ctx-core-disable-managed-man-pages-v1 ||
    fail "installed ctx could not disable automatic man-page refresh"
}
`;
}
