import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";

export const releaseVersionVectors = [
  ["0.25.0", "1.3.2", -1], ["1.2.2", "1.3.2", -1],
  ["1.3.2", "1.3.2", 0], ["1.3.3", "1.3.2", 1],
  ["1.3.2-rc.1", "1.3.2", -1], ["1.3.2+build", "1.3.2", 0],
  ["1.3.2-rc.2", "1.3.2-rc.10", -1],
  ["1.3.2-1", "1.3.2-alpha", -1],
  ["1.3.2-alpha", "1.3.2-alpha.1", -1],
  ["999999999999999999999999.0.0", "2.0.0", 1],
];
export const invalidReleaseVersions = ["1.3", "01.3.2", "1.3.2-01", "1.3.2+", "1.3.2-", "1.3.2+a..b"];

// Executable PowerShell owner tests. Native OS parsing/receipt/transport logic is
// real; installed Core and its helper are deliberately mocked here.
export function powerShellBridgeFixture() {
  const body = renderCliInstallPowerShellScript();
  const fn = (name) => {
    const value = body.match(new RegExp(`function ${name}[^\\n]*[\\s\\S]*?^}`, "m"))?.[0];
    if (!value) throw new Error(`missing rendered function ${name}`);
    return value;
  };
  const selection = body.slice(body.indexOf("$functionsBase ="), body.indexOf("function Read-Metadata"));
  const legacySelection = body.slice(body.indexOf("        $legacyManagedReinstall = $managedReinstall -and ("), body.indexOf("    } finally {", body.indexOf("        $legacyManagedReinstall = $managedReinstall -and (")));
  const dispatch = body.slice(body.indexOf("    if ($managedReinstall) {"), body.indexOf('    Send-InstallStage -Stage "binary_install"'));
  return `Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Fail([string]$Message) { throw $Message }
function Assert-Equal($Actual, $Expected) { if ($Actual -cne $Expected) { throw "expected <$Expected>, got <$Actual>" } }
${fn("Compare-ReleaseVersion")}
${fn("ConvertTo-ManagedInstallComparablePath")}
${fn("Test-ManagedInstallPathIdentity")}
${fn("Assert-LegacyManagedUpgradeResult")}
${fn("Send-InstallStage")}
function Test-CanonicalAnalyticsDisabled { return $false }
function Invoke-WebRequest { param($Uri, $Method, $ContentType, $Body, [switch]$UseBasicParsing, $TimeoutSec) $script:telemetryUri = $Uri }
$parseTokens = $null; $parseErrors = $null
$null = [System.Management.Automation.Language.Parser]::ParseInput([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${Buffer.from(body).toString("base64")}')), [ref]$parseTokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw ($parseErrors | Out-String) }
${releaseVersionVectors.map(([a,b,n]) => `Assert-Equal (Compare-ReleaseVersion '${a}' '${b}') ${n}`).join("\n")}
${invalidReleaseVersions.map((v) => `$rejected = $false; try { $null = Compare-ReleaseVersion '${v}' '1.3.2' } catch { $rejected = $true }; Assert-Equal $rejected $true`).join("\n")}
foreach ($route in @(
    @('', '', 'https://cli.ctx.rs/functions/v2', 'https://cli.ctx.rs/functions/v1', $false),
    @('staging', '', 'https://cli.ctx.rs/functions/v1', 'https://cli.ctx.rs/functions/v1', $false),
    @('stable', 'https://test.invalid/custom', 'https://test.invalid/custom', 'https://test.invalid/custom', $true),
    @('stable', 'http://test.invalid/custom', 'http://test.invalid/custom', 'http://test.invalid/custom', $true)
)) {
    $env:CTX_UPGRADE_CHANNEL = $route[0]; $env:CTX_UPGRADE_FUNCTIONS_BASE = $route[1]
    $env:CTX_RELEASE_METADATA_URL = ''; $env:CTX_RELEASE_METADATA_SIGNATURE_URL = ''
    $Metadata = ''; $DryRun = $false
    ${selection}
    Assert-Equal $functionsBase $route[2]
    Assert-Equal $installTelemetryBase $route[3]
    Assert-Equal $explicitMetadata $route[4]
    Assert-Equal $Metadata "$($route[2])/releases/$channel/ctx-release-metadata.env"
    $script:installStageDeliveryEnabled = $true; $script:telemetryUri = ''
    Send-InstallStage installer started
    if ($route[3].StartsWith('https:')) { Assert-Equal $script:telemetryUri "$($route[3])/install-attempt" }
    else { Assert-Equal $script:telemetryUri '' }
}
$channel = 'stable'; $version = '1.3.2'; $installPath = 'C:\\fixture\\bin\\ctx.exe'
foreach ($prior in @('0.11.0', '0.16.0', '0.17.0', '0.24.0', '0.25.0', '0.26.0', '1.2.2', '1.3.2')) {
    $existingManagedInstall = [pscustomobject]@{version=$prior}; $managedReinstall = $true; $releasePhase = 'bridge'
    ${legacySelection}
    Assert-Equal $legacyManagedReinstall ($prior -cin @('0.11.0', '0.16.0', '0.17.0', '0.24.0', '0.25.0'))
    if (-not $legacyManagedReinstall) { continue }
    $receipt = [pscustomobject]@{
        schema_version=1; command='upgrade'; ok=$true; status='scheduled'; message='scheduled'
        current_version=$prior; latest_version='1.3.2'; update_available=$true; channel='stable'
        platform='windows-x64'; metadata_url='https://test.invalid'; artifact_url='https://test.invalid/ctx'
        install_path=$installPath; managed=$true; applied=$false; dry_run=$false
    }
    if ($legacyReceiptHasPath) { $receipt | Add-Member path ([pscustomobject]@{}) }
    Assert-Equal (Assert-LegacyManagedUpgradeResult $receipt $prior $legacyReceiptHasPath) 'scheduled'
    $receipt.current_version = '0.10.0'; $rejected = $false
    try { $null = Assert-LegacyManagedUpgradeResult $receipt $prior $legacyReceiptHasPath } catch { $rejected = $true }
    Assert-Equal $rejected $true
    $releasePhase = 'final'
    ${legacySelection}
    Assert-Equal $legacyManagedReinstall ($prior -ceq '0.25.0')
}
$markerSourcePath = 'candidate-marker'; $managedReinstall = $true
function Invoke-ManagedPairApply { param($MarkerSource, $Required) Assert-Equal $Required $true; $script:dispatch = 'candidate'; return $true }
function Invoke-ManagedCoreUpgrade { $script:dispatch = 'installed' }
function Invoke-ReleasedManagedPairInstall { $script:dispatch += '+released' }
function Test-InstalledTargetIdentity { return $script:identityMatches }
foreach ($case in @(
    @('0.25.0', $true, $false, 'candidate'),
    @('0.25.0', $true, $true, 'installed+released'),
    @('0.25.0', $false, $false, 'installed'),
    @('0.24.0', $true, $false, 'installed'),
    @('1.3.1', $true, $false, 'installed'),
    @('1.3.2', $true, $false, 'installed')
)) {
    $existingManagedInstall = [pscustomobject]@{version=$case[0]}
    $managedPair = $case[1]; $releasedPairInstall = $case[2]
    $script:dispatch = ''; $script:identityMatches = $true
    ${dispatch}
    Assert-Equal $script:dispatch $case[3]
}
$existingManagedInstall = [pscustomobject]@{version='0.25.0'}
$managedPair = $true; $releasedPairInstall = $false; $script:identityMatches = $false
$rejected = $false
try { ${dispatch} } catch { $rejected = $true }
Assert-Equal $rejected $true
'PASS: PowerShell parsing, version order, stable/staging/custom telemetry, explicit transport classification, legacy receipt range, exact-0.25 recovery dispatch'
`;
}
