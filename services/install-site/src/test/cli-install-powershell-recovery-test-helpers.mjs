import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";
import { CLI_INSTALL_POWERSHELL_PROCESS_HELPERS } from "../cli-install-powershell-process.js";

// Runs the rendered validator and caller, with captured Core output and an
// installation-identity witness supplied by the fixture. No installer writes.
export function powerShellRecoveryFixture() {
  const body = renderCliInstallPowerShellScript();
  const fn = (name) => {
    const value = body.match(new RegExp(`function ${name}[^\\n]*[\\s\\S]*?^}`, "m"))?.[0];
    if (!value) throw new Error(`missing rendered function ${name}`);
    return value;
  };
  const handoff = fn("Invoke-ManagedCoreUpgrade");
  const deadline = "[DateTime]::UtcNow.AddSeconds(60)";
  if (handoff.split(deadline).length !== 2) throw new Error("expected the existing single 60s deadline");
  // Keep the actual polling control flow; shorten only its clock budget here.
  const boundedHandoff = handoff.replace(deadline, "[DateTime]::UtcNow.AddMilliseconds(300)");
  return `Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Fail([string]$Message) { throw $Message }
function Assert-True([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
${CLI_INSTALL_POWERSHELL_PROCESS_HELPERS}
${fn("ConvertTo-ManagedInstallComparablePath")}
${fn("Test-ManagedInstallPathIdentity")}
${fn("Assert-ManagedUpgradeResult")}
${boundedHandoff}
function Invoke-CtxCaptured([string[]]$Arguments) {
    Assert-True (($Arguments -join ' ') -ceq 'upgrade --channel stable --format=json') 'ordinary Core owner required'
    return [pscustomobject]@{ExitCode=0; OutputPath=$script:receiptPath; ErrorPath=$script:receiptPath+'.err'}
}
function Test-InstalledTargetIdentity {
    $script:identityReads += 1
    return $script:identityWillMatch -and $script:identityReads -ge $script:matchAfter
}
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ('ctx-recovery-receipt-' + [guid]::NewGuid().ToString('n'))
$null = New-Item -ItemType Directory -Path $tempRoot
try {
    $metadataFile = Join-Path $tempRoot 'metadata.env'; $metadataSignatureFile = "$metadataFile.sig"
    $script:receiptPath = Join-Path $tempRoot 'receipt.json'
    $legacyManagedReinstall = $false; $legacyReceiptHasPath = $false
    $channel = 'stable'; $installPath = 'C:\\fixture\\bin\\ctx.exe'
    # Exact existing public outcome_json projection for plan: None / Scheduled.
    $recovery = [ordered]@{
        schema_version=1; command='upgrade'; ok=$true; status='scheduled'; message='rescheduled interrupted replacement'
        current_version=$null; latest_version=$null; update_available=$false; update_was_available=$false
        channel=$null; platform=$null; metadata_url=$null; artifact_url=$null; install_path=$null
        managed=$false; applied=$false; dry_run=$false; warnings=@(); upgrade_attempt_id='ua_recovery'
    } | ConvertTo-Json -Compress
    function Write-Receipt([string]$Json) { [IO.File]::WriteAllText($script:receiptPath, $Json + [Environment]::NewLine) }
    foreach ($case in @(@('bridge','1.3.2','1.3.1'), @('final','1.3.2','1.3.2'), @('final','1.3.3','1.3.2'))) {
        $releasePhase=$case[0]; $version=$case[1]
        $existingManagedInstall = [pscustomobject]@{version=$case[2]}
        $script:identityWillMatch=$true; $script:matchAfter=2; $script:identityReads=0
        Write-Receipt $recovery
        Invoke-ManagedCoreUpgrade
        Assert-True ($script:identityReads -ge 2) 'scheduled recovery must await actual target identity'
    }
    $releasePhase='final'; $version='1.3.3'
    $existingManagedInstall = [pscustomobject]@{version='1.3.2'}
    $script:identityWillMatch=$false; $script:identityReads=0
    Write-Receipt $recovery
    $timedOut=$false
    try { Invoke-ManagedCoreUpgrade } catch { $timedOut=$_.Exception.Message -match 'lifecycle deadline' }
    Assert-True ($timedOut -and $script:identityReads -ge 1) 'wrong recovered identity must hit the existing deadline'

    $mutations = @(
        @('schema_version', '1'), @('schema_version', 2), @('command', 'UPGRADE'), @('ok', $false),
        @('status', 'SCHEDULED'), @('status', 'applied'), @('status', 'up_to_date'), @('message', $null),
        @('update_available', $true), @('update_was_available', $true), @('managed', $true),
        @('applied', $true), @('dry_run', $true), @('managed', 'false'), @('applied', 0),
        @('update_available', $null), @('upgrade_attempt_id', $null), @('upgrade_attempt_id', ''),
        @('upgrade_attempt_id', ' '), @('upgrade_attempt_id', 12), @('warnings', 'scalar')
    )
    foreach ($field in @('current_version','latest_version','channel','platform','metadata_url','artifact_url','install_path')) {
        $mutations += ,@($field, '')
        $mutations += ,@($field, 'unexpected')
    }
    $rejectedCount=0
    foreach ($mutation in $mutations) {
        $receipt=$recovery | ConvertFrom-Json
        $receipt.($mutation[0])=$mutation[1]
        Write-Receipt ($receipt | ConvertTo-Json -Compress)
        $script:identityReads=0; $script:identityWillMatch=$true; $script:matchAfter=1
        $rejected=$false
        try { Invoke-ManagedCoreUpgrade } catch { $rejected=$true }
        Assert-True ($rejected -and $script:identityReads -eq 0) "invalid recovery $($mutation[0]) reached identity wait"
        $rejectedCount += 1
    }
    foreach ($field in @('current_version','latest_version','channel','platform','metadata_url','artifact_url','install_path','upgrade_attempt_id')) {
        $receipt=$recovery | ConvertFrom-Json
        $receipt.PSObject.Properties.Remove($field)
        Write-Receipt ($receipt | ConvertTo-Json -Compress)
        $script:identityReads=0; $rejected=$false
        try { Invoke-ManagedCoreUpgrade } catch { $rejected=$true }
        Assert-True ($rejected -and $script:identityReads -eq 0) "missing recovery field $field reached identity wait"
        $rejectedCount += 1
    }

    $planned = [ordered]@{
        schema_version=1; command='upgrade'; ok=$true; status='applied'; message='applied'
        current_version='1.3.3'; latest_version='1.3.3'; update_available=$false; update_was_available=$true
        channel='stable'; platform='windows-x64'; metadata_url='https://test.invalid/metadata'
        artifact_url='https://test.invalid/ctx'; install_path=$installPath
        managed=$true; applied=$true; dry_run=$false; warnings=@(); upgrade_attempt_id='ua_apply'
    } | ConvertTo-Json -Compress
    foreach ($status in @('applied','up_to_date','scheduled')) {
        $receipt=$planned | ConvertFrom-Json
        $receipt.status=$status; $receipt.applied=$status -ceq 'applied'
        if ($status -ceq 'scheduled') { $receipt.current_version='1.3.2' }
        if ($status -ceq 'up_to_date') { $receipt.upgrade_attempt_id=$null; $receipt.update_was_available=$false }
        $existingManagedInstall = [pscustomobject]@{version=$receipt.current_version}
        Write-Receipt ($receipt | ConvertTo-Json -Compress)
        $script:identityReads=0; $script:identityWillMatch=$true; $script:matchAfter=1
        Invoke-ManagedCoreUpgrade
        Assert-True ($script:identityReads -eq 1) 'ordinary receipt must retain final identity validation'
    }
    $existingManagedInstall = [pscustomobject]@{version='1.3.3'}
    foreach ($mutation in @(
        @('current_version','1.3.2'), @('latest_version','1.3.2'), @('channel','staging'),
        @('platform','linux-x64'), @('install_path','C:\\other\\bin\\ctx.exe'), @('managed',$false),
        @('metadata_url',$null), @('artifact_url',$null), @('applied',$false), @('upgrade_attempt_id','')
    )) {
        $receipt=$planned | ConvertFrom-Json; $receipt.($mutation[0])=$mutation[1]
        Write-Receipt ($receipt | ConvertTo-Json -Compress)
        $script:identityReads=0; $rejected=$false
        try { Invoke-ManagedCoreUpgrade } catch { $rejected=$true }
        Assert-True ($rejected -and $script:identityReads -eq 0) "ordinary target guard weakened: $($mutation[0])"
        $rejectedCount += 1
    }
    # Pretty JSON belongs only to the explicitly known released 1.3.1 owner.
    foreach ($prior in @('1.3.1','1.3.2','1.3.3')) {
        $existingManagedInstall = [pscustomobject]@{version=$prior}
        Write-Receipt ($planned | ConvertFrom-Json | ConvertTo-Json)
        $script:identityReads=0; $script:identityWillMatch=$true; $script:matchAfter=1
        $accepted=$true
        try { Invoke-ManagedCoreUpgrade } catch { $accepted=$false }
        Assert-True ($accepted -eq ($prior -ceq '1.3.1')) 'pretty JSON prior-owner boundary changed'
        Assert-True ($script:identityReads -eq [int]$accepted) 'rejected pretty receipt reached identity wait'
    }
    "PASS: 3 recovered schedules awaited identity; wrong identity timed out; 3 ordinary outcomes; $rejectedCount malformed receipts rejected before wait"
} finally { Remove-Item -LiteralPath $tempRoot -Recurse -Force }
`;
}
