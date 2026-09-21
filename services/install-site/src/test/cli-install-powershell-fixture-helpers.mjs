import assert from "node:assert/strict";
import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";

// Select production statements, never substitutes for the Windows path/ACL
// guards. These fixtures own portable control/crypto evidence, not installation.
function section(body, start, end) {
  const first = body.indexOf(start);
  const last = body.indexOf(end, first + start.length);
  assert.ok(first >= 0 && last > first, `missing rendered section: ${start}`);
  assert.equal(body.indexOf(start, first + start.length), -1, `ambiguous section: ${start}`);
  return body.slice(first, last);
}

function renderedFunction(body, name) {
  const matches = [...body.matchAll(new RegExp(`^function ${name}\\b[^\\n]*[\\s\\S]*?^}`, "gm"))];
  assert.equal(matches.length, 1, `expected one rendered function ${name}`);
  return matches[0][0];
}

function plainExceptionCapture(statements) {
  return `try {
${statements}
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
`;
}

export function powerShellControlFixture() {
  const body = renderCliInstallPowerShellScript();
  return [
    // Keep the real parameter declaration, Boolean helpers and config parser.
    body.slice(0, body.indexOf("function Remove-ConfigComment")),
    section(body, "function Remove-ConfigComment", "$deprecatedControlMappings ="),
    renderedFunction(body, "Apply-DeprecatedControl"),
    plainExceptionCapture([
      section(body, "$deprecatedControlMappings =", "function Apply-DeprecatedControl"),
      section(body, 'Apply-DeprecatedControl "CTX_ANALYTICS_OFF"', "if (-not [System.Environment]::Is64BitOperatingSystem)"),
      section(body, "$persistedConfigControls = Get-PersistedConfigControls", '$installPath = Join-Path'),
      "[ordered]@{ semantic = $semanticEnabled; daemon = $daemonEnabled } | ConvertTo-Json -Compress",
    ].join("\n")),
  ].join("\n");
}

export function powerShellPairStateFixture() {
  const body = renderCliInstallPowerShellScript();
  return `param([string]$BinDir)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
${renderedFunction(body, "Fail")}
${renderedFunction(body, "Get-ExistingInstallPairState")}
${plainExceptionCapture(`$installPath = Join-Path $BinDir 'ctx.exe'
$markerPath = "$installPath.install.json"
Get-ExistingInstallPairState
`)}
`;
}

export function powerShellStageFixture() {
  const body = renderCliInstallPowerShellScript();
  return `Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
${renderedFunction(body, "Test-CanonicalAnalyticsDisabled")}
${renderedFunction(body, "Send-InstallStage")}
function Invoke-WebRequest {
    param($Uri, $Method, $ContentType, $Body, [switch]$UseBasicParsing, $TimeoutSec)
    $script:requests += 1; $script:uri = $Uri; $script:body = $Body
}
$script:requests = 0; $script:uri = ''; $script:body = ''
$script:installStageDeliveryEnabled = $true; $DryRun = $false
$installTelemetryBase = 'https://example.invalid'; $installAttemptId = 'ia_ps_test_attempt'
Send-InstallStage -Stage 'artifact_download' -Status 'completed'
[ordered]@{ requests=$script:requests; uri=$script:uri; body=($script:body | ConvertFrom-Json) } | ConvertTo-Json -Depth 4 -Compress
`;
}

export function powerShellReleaseFixture(trust, pendingRecovery = null) {
  const body = renderCliInstallPowerShellScript(trust);
  const functions = [
    "Fail", "Copy-LimitedStream", "Copy-LimitedFile", "Read-Metadata", "Read-DetachedSignature", "Read-Artifact",
    "ConvertFrom-Base64Url", "Get-MetadataSignaturePublicKeyParameters",
    "Verify-MetadataSignature", "Get-MetadataValue", "Get-MetadataValueOrDefault",
    "Assert-SafeArtifactName", "Expand-GzipFile", "Assert-AllowedBaseUrl", "Compare-ReleaseVersion",
  ].map((name) => renderedFunction(body, name)).join("\n");
  // Release preparation includes the actual signature/channel/version/pair
  // checks. Stop before any native guard, Core invocation or publication.
  const preparation = section(body, "    $readReleasePhaseMetadata = {", "    $skillAgents = @()");
  const download = section(body, '    $compressedArtifactDownloaded = $false', '    $marker = [ordered]@{');
  const recovery = pendingRecovery === null ? "" : `
# Authored control-flow fixture, not native transaction/signature evidence.
${renderedFunction(body, "Resume-InterruptedManagedPair")}
function Read-ExistingManagedInstall {
    if (-not (Test-Path -LiteralPath $installPath -PathType Leaf)) { throw 'fixture missing Core' }
    $oldMarker = [IO.File]::ReadAllText($markerPath) | ConvertFrom-Json
    if ((Get-FileHash -LiteralPath $installPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $oldMarker.sha256) {
        throw 'fixture mismatched Core'
    }
    return $oldMarker
}
function Invoke-ManagedPairApply([string]$MarkerSource, [bool]$Required) {
    $retained = Join-Path $pairInstallRoot 'share/ctx/.managed-pair-apply-v1'
    if (-not $Required -or $MarkerSource -cne (Join-Path $retained 'bin/ctx.exe.install.json') -or
        $pairEnvelopePath -cne (Join-Path $retained 'share/ctx/managed-pair-envelope.json') -or
        $pairCompanionPath -cne (Join-Path $retained 'libexec/ctx-pro.exe') -or
        (Get-FileHash -LiteralPath $downloadPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $checksum.ToLowerInvariant()) {
        throw 'recovery did not pass the fixed retained slots and verified ordinary candidate'
    }
    [IO.File]::Copy((Join-Path $retained 'bin/ctx.exe'), $installPath, $true)
    [IO.File]::Copy($MarkerSource, $markerPath, $true)
    Remove-Item -LiteralPath (Join-Path $BinDir '.ctx.upgrade-install-transaction.json')
    $script:recoveryCalls += 1
    return $true
}
$script:recoveryCalls = 0
$null = New-Item -ItemType Directory -Path $BinDir -Force
$retained = Join-Path $pairInstallRoot 'share/ctx/.managed-pair-apply-v1'
foreach ($directory in @('bin', 'libexec', 'share/ctx')) {
    $null = New-Item -ItemType Directory -Path (Join-Path $retained $directory) -Force
}
$oldCore = Join-Path $retained 'bin/ctx.exe'
[IO.File]::WriteAllText($oldCore, 'authored old executable; never executed')
$oldDigest = (Get-FileHash -LiteralPath $oldCore -Algorithm SHA256).Hash.ToLowerInvariant()
$oldMarker = @{version='1.4.12';sha256=$oldDigest} | ConvertTo-Json
[IO.File]::WriteAllText((Join-Path $retained 'bin/ctx.exe.install.json'), $oldMarker)
[IO.File]::WriteAllText($markerPath, $oldMarker)
[IO.File]::WriteAllText((Join-Path $retained 'libexec/ctx-pro.exe'), 'inert old companion')
[IO.File]::WriteAllText((Join-Path $retained 'share/ctx/managed-pair-envelope.json'), 'authored owner fixture')
[IO.File]::WriteAllText((Join-Path $BinDir '.ctx.upgrade-install-transaction.json'), 'authored pending transaction')
if ('${pendingRecovery}' -ceq 'mismatch') { [IO.File]::WriteAllText($installPath, 'mismatched executable') }
$witnesses = @('history/record', 'search/attribution/segment', 'pro/key')
foreach ($name in $witnesses) {
    $file = Join-Path $WorkRoot ('data/' + $name)
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $file) -Force
    [IO.File]::WriteAllText($file, 'authored witness')
}
Resume-InterruptedManagedPair
$recovered = Read-ExistingManagedInstall
if ($script:recoveryCalls -ne 1 -or $recovered.version -cne '1.4.12' -or $version -cne '1.5.0' -or $managedPair) {
    throw 'retained recovery did not precede old identity validation with ordinary 1.5 selected'
}
foreach ($name in $witnesses) {
    if ([IO.File]::ReadAllText((Join-Path $WorkRoot ('data/' + $name))) -cne 'authored witness') { throw 'data changed' }
}
`;
  return `param([string]$WorkRoot, [string]$SourceRoot)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
${functions}
function Read-HttpsFile {
    param([string]$Source, [string]$Destination, [long]$MaxBytes, [int]$TimeoutSeconds)
    $routes = [IO.File]::ReadAllText((Join-Path $SourceRoot 'routes.json')) | ConvertFrom-Json
    $matches = @($routes | Where-Object { $_.uri -ceq $Source })
    if ($matches.Count -ne 1) { throw 'unexpected fixture download' }
    Copy-LimitedFile (Join-Path $SourceRoot $matches[0].file) $Destination $MaxBytes
}
function Send-InstallStage([string]$Stage, [string]$Status) {}
${plainExceptionCapture(`$tempRoot = $WorkRoot
$BinDir = Join-Path $WorkRoot 'installation/bin'
$installPath = Join-Path $BinDir 'ctx.exe'; $markerPath = "$installPath.install.json"
$channel = 'stable'; $semanticEnabled = $false
$Metadata = 'https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env'
$metadataSignature = "$Metadata.sig"; $explicitMetadata = $false
${preparation}
${download}
if (Test-Path -LiteralPath $BinDir) { throw 'preparation must not publish an installation' }
${recovery}
[ordered]@{ version=$version; core=$actualChecksum; pro=$(if ($managedPair) { $actualCompanionChecksum } else { $null }); managed_pair=$managedPair } | ConvertTo-Json -Compress
`)}
`;
}
