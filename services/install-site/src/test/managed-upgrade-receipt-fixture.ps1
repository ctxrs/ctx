param([string]$HelperPath, [string]$WorkRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. $HelperPath
function Fail([string]$Message) { throw $Message }
function Test-InstalledTargetIdentity { return $true }
function Invoke-CtxCaptured {
    param([string[]]$Arguments)
    if (($Arguments -join ',') -cne 'upgrade,--channel,stable,--format=json') { throw 'wrong CLI invocation' }
    return [pscustomobject]@{ ExitCode = 0; OutputPath = $script:receiptPath; ErrorPath = $script:receiptPath }
}
$installPath = 'C:\managed\bin\ctx.exe'
$version = '1.4.2'; $channel = 'stable'; $releasePhase = 'final'
$existingManagedInstall = [pscustomobject]@{ version = $version }
$legacyManagedReinstall = $false; $legacyReceiptHasPath = $false
$metadataFile = Join-Path $WorkRoot 'metadata.env'
$metadataSignatureFile = Join-Path $WorkRoot 'metadata.env.sig'
$script:receiptPath = Join-Path $WorkRoot 'upgrade.out'
# The public upgrade command renders a whole pretty-printed JSON document,
# unlike the private compact managed-pair-apply protocol.
$value = [ordered]@{
    schema_version = 1; command = 'upgrade'; ok = $true; status = 'up_to_date'
    message = 'ctx is up to date.'; current_version = $version; latest_version = $version
    update_available = $false; update_was_available = $false; channel = $channel
    platform = 'windows-x64'; metadata_url = 'https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env'
    artifact_url = 'https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.4.2/ctx.exe'
    install_path = $installPath; managed = $true; applied = $false; dry_run = $false
    warnings = @(); upgrade_attempt_id = $null
}
$pretty = $value | ConvertTo-Json -Depth 5
$compact = $value | ConvertTo-Json -Depth 5 -Compress
$newline = [Environment]::NewLine
$cases = @(
    @{ name = 'pretty'; body = $pretty + $newline; accepted = $true },
    @{ name = 'compact'; body = $compact + $newline; accepted = $true },
    @{ name = 'progress'; body = 'starting' + $newline + $pretty + $newline; accepted = $false },
    @{ name = 'two-documents'; body = $pretty + $newline + $pretty + $newline; accepted = $false },
    @{ name = 'trailing-content'; body = $pretty + 'x' + $newline; accepted = $false },
    @{ name = 'missing-newline'; body = $pretty; accepted = $false },
    @{ name = 'oversized'; body = $pretty + (' ' * 65536) + $newline; accepted = $false },
    @{ name = 'wrong-status'; body = $pretty.Replace('up_to_date', 'APPLIED') + $newline; accepted = $false },
    @{ name = 'extra-field'; body = $pretty.Replace('"schema_version"', '"extra": 1, "schema_version"') + $newline; accepted = $false },
    @{ name = 'null-warnings'; body = ($pretty -replace '"warnings"\s*:\s*\[\s*\]', '"warnings": null') + $newline; accepted = $false }
)
$passed = 0
foreach ($case in $cases) {
    [IO.File]::WriteAllText($script:receiptPath, $case.body, [Text.UTF8Encoding]::new($false))
    $accepted = $true
    try { Invoke-ManagedCoreUpgrade } catch { $accepted = $false }
    if ($accepted -ne $case.accepted) { throw ('managed upgrade receipt case failed: ' + $case.name) }
    $passed++
}
[pscustomobject]@{ stage = 'managed_upgrade_receipt'; passed = $passed } | ConvertTo-Json -Compress
