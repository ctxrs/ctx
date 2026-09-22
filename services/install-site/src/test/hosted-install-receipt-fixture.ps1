param([string]$HelperPath, [string]$WorkRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. $HelperPath
function Fail([string]$Message) { throw $Message }
function Test-ManagedInstallPathIdentity { param($Candidate, $Expected) return $Candidate -ceq $Expected }
function Test-InstalledTargetIdentity { $script:identityReads++; return $true }
function Invoke-ExecutableCaptured {
    param([string]$Executable, [string[]]$Arguments)
    if (($Arguments[0..2] -join ',') -cne 'upgrade,--hosted-transaction,install') {
        throw 'wrong CLI invocation'
    }
    return [pscustomobject]@{ ExitCode = 0; OutputPath = $script:receiptPath; ErrorPath = $script:receiptPath }
}
$downloadPath = 'candidate.exe'
$installPath = 'C:\managed\bin\ctx.exe'
$installAttemptId = 'fixture-attempt'
$markerSourcePath = 'marker.json'
$actualChecksum = 'a' * 64
$version = '1.5.0'
$script:receiptPath = Join-Path $WorkRoot 'hosted.out'
$receipt = [ordered]@{
    schema_version = 1; command = 'hosted_install_transaction'; ok = $true; status = 'committed'
    attempt_id = $installAttemptId; install_path = $installPath
    binary_sha256 = $actualChecksum; marker_sha256 = ('b' * 64)
}
$pretty = $receipt | ConvertTo-Json -Depth 5
$compact = $receipt | ConvertTo-Json -Depth 5 -Compress
$newline = [Environment]::NewLine
$cases = @(
    @{ name = 'released-pretty'; body = $pretty + $newline; accepted = $true },
    @{ name = 'compact'; body = $compact + $newline; accepted = $true },
    @{ name = 'two-documents'; body = $pretty + $newline + $pretty + $newline; accepted = $false },
    @{ name = 'missing-newline'; body = $pretty; accepted = $false },
    @{ name = 'wrong-status'; body = $pretty.Replace('committed', 'FAILED') + $newline; accepted = $false }
)
$passed = 0
foreach ($case in $cases) {
    [IO.File]::WriteAllText($script:receiptPath, $case.body, [Text.UTF8Encoding]::new($false))
    $script:identityReads = 0
    $accepted = $true
    try { Invoke-HostedInstallTransaction } catch { $accepted = $false }
    if ($accepted -ne $case.accepted -or $script:identityReads -ne [int]$accepted) {
        throw ('hosted transaction receipt case failed: ' + $case.name)
    }
    $passed++
}
[pscustomobject]@{ stage = 'hosted_install_receipt'; passed = $passed } | ConvertTo-Json -Compress
