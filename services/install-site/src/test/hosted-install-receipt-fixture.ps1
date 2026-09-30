param([string]$HelperPath, [string]$WorkRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. $HelperPath
function Fail([string]$Message) { throw $Message }
function Test-ManagedInstallPathIdentity { param($Candidate, $Expected) return $Candidate -ceq $Expected }
# Portable proof fixture: real file hashes and production identity validation,
# with inert files and a leaf guard stub. Native Windows guards have their own tests.
class CtxInstallerPathGuard {
    [bool]$LeafExists
    CtxInstallerPathGuard([string]$Path) { $this.LeafExists = [IO.File]::Exists($Path) }
    static [CtxInstallerPathGuard] AcquireLeaf([string]$Path) { return [CtxInstallerPathGuard]::new($Path) }
    [void] AssertUnchanged() {}
    [void] Dispose() {}
}
function Invoke-ExecutableCaptured {
    param([string]$Executable, [string[]]$Arguments)
    if (($Arguments[0..2] -join ',') -cne ('upgrade,--hosted-transaction,' + $script:hostedAction) -or
        $Arguments[6] -cne $installAttemptId) {
        throw 'wrong CLI invocation'
    }
    return [pscustomobject]@{ ExitCode = $script:exitCode; OutputPath = $script:receiptPath; ErrorPath = $script:receiptPath }
}
function Write-BoundedCapturedChildError { param($ErrorPath) }
$downloadPath = 'candidate.exe'
$installPath = Join-Path $WorkRoot 'ctx.exe'
$markerPath = "$installPath.install.json"
$installAttemptId = 'fixture-attempt'
$markerSourcePath = 'marker.json'
$version = '2.2.1'
$releasePhase = 'final'
$managedPair = $false
[IO.File]::WriteAllText($installPath, 'inert signed candidate; never executed')
$actualChecksum = (Get-FileHash -Algorithm SHA256 -LiteralPath $installPath).Hash.ToLowerInvariant()
$marker = [ordered]@{
    schema_version = 1; manager = 'ctx-hosted-installer'; install_attempt_id = $installAttemptId
    install_path = $installPath; platform = 'windows-x64'; version = $version; sha256 = $actualChecksum
}
[IO.File]::WriteAllText($markerPath, ($marker | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$markerDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $markerPath).Hash.ToLowerInvariant()
$script:receiptPath = Join-Path $WorkRoot 'hosted.out'
$receipt = [ordered]@{
    schema_version = 1; command = 'hosted_install_transaction'; ok = $true; status = 'committed'
    attempt_id = $installAttemptId; install_path = $installPath
    binary_sha256 = $actualChecksum; marker_sha256 = $markerDigest
}
$pretty = $receipt | ConvertTo-Json -Depth 5
$compact = $receipt | ConvertTo-Json -Depth 5 -Compress
$newline = [Environment]::NewLine
$cases = @(
    @{ name = 'released-pretty'; body = $pretty + $newline; accepted = $true },
    @{ name = 'compact'; body = $compact + $newline; accepted = $true },
    @{ name = 'retained-attempt-new-installer'; body = $pretty + $newline; accepted = $true; retry = $true },
    @{ name = 'retry-unbound-new-attempt'; body = $pretty.Replace('fixture-attempt', 'new-installer-attempt') + $newline; accepted = $false; retry = $true },
    @{ name = 'wrong-marker-digest'; body = $pretty.Replace($markerDigest, ('b' * 64)) + $newline; accepted = $false },
    @{ name = 'wrong-binary-digest'; body = $pretty.Replace($actualChecksum, ('c' * 64)) + $newline; accepted = $false },
    @{ name = 'two-documents'; body = $pretty + $newline + $pretty + $newline; accepted = $false },
    @{ name = 'missing-newline'; body = $pretty; accepted = $false },
    @{ name = 'wrong-status'; body = $pretty.Replace('committed', 'FAILED') + $newline; accepted = $false },
    @{ name = 'receipt-with-failed-process'; body = $pretty + $newline; accepted = $false; exitCode = 75 }
)
$passed = 0
foreach ($case in $cases) {
    [IO.File]::WriteAllText($script:receiptPath, $case.body, [Text.UTF8Encoding]::new($false))
    $retry = $case.ContainsKey('retry') -and $case.retry
    $installAttemptId = if ($retry) { 'new-installer-attempt' } else { 'fixture-attempt' }
    $script:hostedAction = if ($retry) { 'migrate' } else { 'install' }
    $script:exitCode = if ($case.ContainsKey('exitCode')) { $case.exitCode } else { 0 }
    $accepted = $true
    try { Invoke-HostedInstallTransaction -Migrate:$retry } catch { $accepted = $false }
    if ($accepted -ne $case.accepted) {
        throw ('hosted transaction receipt case failed: ' + $case.name)
    }
    $passed++
}
[pscustomobject]@{ stage = 'hosted_install_receipt'; passed = $passed } | ConvertTo-Json -Compress
