// Released 1.x and 2.0 teardown changes policy even while the hosted transaction is
// fenced. Save policy beside that transaction before calling it, then undo only
// its mode change. Interrupted retries reuse the same attempt-bound record.
export const UNINSTALL_SHELL_INDEXING_PREFERENCE = `
capture_legacy_indexing_preferences() {
  case "$version_major.$version_minor" in 1.*|2.0) ;; *) return 0 ;; esac
  indexing_restore_roots="\${install_path%/*}/.\${install_path##*/}.hosted-install-transaction.json.indexing-preferences"
  if [ -e "$indexing_restore_roots" ] || [ -L "$indexing_restore_roots" ]; then
    validate_recovery_private_file "$indexing_restore_roots"
    [ "$(sed -n '1p' "$indexing_restore_roots")" = "$hosted_uninstall_attempt_id" ] ||
      fail "saved indexing preferences belong to a different uninstall transaction"
    return 0
  fi
  indexing_save="$(mktemp "$indexing_restore_roots.XXXXXX")" ||
    fail "could not save indexing preferences before legacy teardown"
  printf '%s\\n' "$hosted_uninstall_attempt_id" >"$indexing_save"
  indexing_roots="$(mktemp "\${TMPDIR:-/tmp}/ctx-uninstall-roots.XXXXXX")" ||
    fail "could not inspect legacy daemon roots"
  printf '%s' "$install_path" >"$indexing_roots"
  indexing_namespace="$(sha256_file "$indexing_roots")"
  printf '%s\\n' "$data_dir" "$home_dir/.ctx" >"$indexing_roots"
  for registration in "$home_dir/.ctx/daemon-installations/$indexing_namespace/daemon-quiescence-acks/"*.json; do
    [ -f "$registration" ] || continue
    registration_root="$(json_string_field "$registration" data_root)" ||
      fail "could not inspect a registered legacy daemon root"
    indexing_root="$(printf '%s\\n' "$registration_root" | sed 's/\\\\"/"/g; s/\\\\\\\\/\\\\/g')"
    [ "$(json_escape "$indexing_root")" = "$registration_root" ] ||
      fail "could not decode a registered legacy daemon root"
    printf '%s\\n' "$indexing_root" >>"$indexing_roots"
  done
  while IFS= read -r indexing_root; do
    indexing_mode="$(
      unset CTX_DAEMON_OFF CTX_DISABLE_DAEMON
      CTX_DAEMON_ENABLED=true "$install_path" --data-root "$indexing_root" index mode --format=json
    )" || fail "could not read indexing preference before legacy teardown"
    indexing_mode="$(printf '%s\\n' "$indexing_mode" | sed -n -e 's/.*"mode"[[:space:]]*:[[:space:]]*"auto".*/auto/p' -e 's/.*"mode"[[:space:]]*:[[:space:]]*"manual".*/manual/p')"
    case "$indexing_mode" in
      auto) printf '%s\\n' "$indexing_root" >>"$indexing_save" ;;
      manual) ;;
      *) fail "invalid indexing preference from legacy ctx" ;;
    esac
  done <"$indexing_roots"
  rm -f "$indexing_roots"
  indexing_roots=
  chmod 600 "$indexing_save"
  mv -f "$indexing_save" "$indexing_restore_roots"
  indexing_save=
}

restore_legacy_indexing_preferences() {
  [ -n "$indexing_restore_roots" ] || return 0
  {
  IFS= read -r indexing_attempt
  [ "$indexing_attempt" = "$hosted_uninstall_attempt_id" ] ||
    fail "saved indexing preferences belong to a different uninstall transaction"
  while IFS= read -r indexing_root; do
    indexing_config="$indexing_root/config.toml"
    [ -e "$indexing_config" ] || continue
    [ -f "$indexing_config" ] && [ ! -L "$indexing_config" ] ||
      fail "legacy teardown indexing config is not a regular file"
    indexing_tmp="$(mktemp "$indexing_config.XXXXXX")" ||
      fail "could not restore indexing preference after legacy teardown"
    if ! awk '
      /^[[:space:]]*\\[/ { section = $0; sub(/[[:space:]]*#.*/, "", section); gsub(/[[:space:]]/, "", section) }
      section == "[indexing]" && /^[[:space:]]*mode[[:space:]]*=/ {
        count++; $0 = "mode = \\"auto\\""
      }
      { print }
      END { if (count != 1) exit 1 }
    ' "$indexing_config" >"$indexing_tmp"; then
      rm -f "$indexing_tmp"
      fail "could not restore the legacy teardown indexing mode"
    fi
    chmod 600 "$indexing_tmp"
    mv -f "$indexing_tmp" "$indexing_config"
  done
  } <"$indexing_restore_roots"
  rm -f "$indexing_restore_roots"
  indexing_restore_roots=
}
`;

export const UNINSTALL_POWERSHELL_INDEXING_PREFERENCE = `
function Get-LegacyIndexingPreferences([object]$Version, [object]$Transaction) {
    if ($Version.major -ne 1 -and ($Version.major -ne 2 -or $Version.minor -ne 0)) { return $null }
    $recordPath = $TransactionPath + '.indexing-preferences'
    if (Test-Path -LiteralPath $recordPath) {
        Assert-RegularManagedFile -Path $recordPath -Label 'saved indexing preferences'
        $record = Get-Content -LiteralPath $recordPath -Raw | ConvertFrom-Json
        if ($record.schema_version -ne 1 -or $record.attempt_id -cne $Transaction.attempt_id) {
            Fail 'saved indexing preferences belong to a different uninstall transaction'
        }
        return $record
    }
    $roots = @($DataRoot, (Join-Path $homeDirectory '.ctx'))
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $namespace = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::Unicode.GetBytes($Transaction.install_path)))).Replace('-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
    $registrations = Join-Path $homeDirectory ".ctx/daemon-installations/$namespace/daemon-quiescence-acks"
    if (Test-Path -LiteralPath $registrations -PathType Container) {
        foreach ($registration in Get-ChildItem -LiteralPath $registrations -Filter '*.json' -File) {
            $roots += (Get-Content -LiteralPath $registration.FullName -Raw | ConvertFrom-Json).data_root
        }
    }
    $automatic = @()
    $saved = @{}
    foreach ($name in @('CTX_DAEMON_ENABLED', 'CTX_DAEMON_OFF', 'CTX_DISABLE_DAEMON')) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $null, 'Process')
    }
    try {
        [Environment]::SetEnvironmentVariable('CTX_DAEMON_ENABLED', 'true', 'Process')
        foreach ($root in $roots | Select-Object -Unique) {
            $output = & $InstallPath --data-root $root index mode --format=json
            if ($LASTEXITCODE -ne 0) { Fail 'could not read indexing preference before legacy teardown' }
            $mode = ($output | Out-String | ConvertFrom-Json).indexing.mode
            if ($mode -ceq 'auto') { $automatic += $root }
            elseif ($mode -cne 'manual') { Fail 'invalid indexing preference from legacy ctx' }
        }
    } finally {
        foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    }
    $record = [ordered]@{ schema_version = 1; attempt_id = $Transaction.attempt_id; automatic_roots = @($automatic) }
    $temporary = $recordPath + '.' + [Guid]::NewGuid().ToString('N')
    try {
        [IO.File]::WriteAllText($temporary, ($record | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
        [IO.File]::Move($temporary, $recordPath)
    } finally {
        if (Test-Path -LiteralPath $temporary) { [IO.File]::Delete($temporary) }
    }
    return $record
}

function Restore-LegacyIndexingPreferences([object]$Preference) {
    if ($null -eq $Preference) { return }
    foreach ($root in $Preference.automatic_roots) {
        $config = Join-Path $root 'config.toml'
        if (-not (Test-Path -LiteralPath $config)) { continue }
        Assert-RegularManagedFile -Path $config -Label 'legacy teardown indexing config'
        $lines = [IO.File]::ReadAllLines($config)
        $section = ''
        $count = 0
        for ($i = 0; $i -lt $lines.Length; $i++) {
            if ($lines[$i] -match '^\\s*\\[') { $section = ($lines[$i] -replace '#.*$', '') -replace '\\s', '' }
            if ($section -ceq '[indexing]' -and $lines[$i] -match '^\\s*mode\\s*=') {
                $lines[$i] = 'mode = "auto"'
                $count++
            }
        }
        if ($count -ne 1) { Fail 'could not restore the legacy teardown indexing mode' }
        $temporary = $config + '.' + [Guid]::NewGuid().ToString('N')
        try {
            [IO.File]::WriteAllLines($temporary, $lines, [Text.UTF8Encoding]::new($false))
            [IO.File]::Replace($temporary, $config, $null)
        } finally {
            if (Test-Path -LiteralPath $temporary) { [IO.File]::Delete($temporary) }
        }
    }
    $recordPath = $TransactionPath + '.indexing-preferences'
    if (Test-Path -LiteralPath $recordPath) { [IO.File]::Delete($recordPath) }
}
`;
