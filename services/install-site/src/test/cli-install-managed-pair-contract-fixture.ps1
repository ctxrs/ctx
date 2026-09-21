param([string]$HelperPath, [string]$WorkRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. $HelperPath
function Fail([string]$Message) { throw $Message }
function Write-SuccessReceiptWarnings($Warnings) {}
$tempRoot = $WorkRoot
$version = '1.3.2'
$pairInstallRoot = 'root with spaces $literal'
$downloadPath = 'candidate "quoted"'
$pairEnvelopePath = 'signed envelope'
$pairCompanionPath = 'companion & file'
$output = Join-Path $WorkRoot 'receipt.json'
$errorPath = Join-Path $WorkRoot ('ctx-command-' + ('a' * 32) + '.err')
$exitCode = 0
function Invoke-ExecutableCaptured([string]$Executable, [string[]]$Arguments) {
    $expected = @('--ctx-core-managed-pair-apply-v1', 'root with spaces $literal', '-',
        'signed envelope', 'candidate "quoted"', 'companion & file', 'marker ; file')
    if ($Executable -cne 'candidate "quoted"' -or $Arguments.Count -ne 7) { throw 'wrong executable or arity' }
    for ($i = 0; $i -lt 7; $i++) {
        if ($Arguments[$i] -cne $expected[$i]) { throw "wrong argument $i" }
    }
    return [pscustomobject]@{ ExitCode = $exitCode; OutputPath = $output; ErrorPath = $errorPath }
}
$valid = '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
$cases = @(
    @{ Body = $valid; Accept = $true },
    @{ Body = $valid.Replace(':1,', ':true,'); Accept = $false },
    @{ Body = $valid.Replace('managed_pair_apply', 'managed_pair_reconcile_integration'); Accept = $false },
    @{ Body = $valid.Replace('true', '"true"'); Accept = $false },
    @{ Body = $valid.Replace('committed', 'pending'); Accept = $false },
    @{ Body = $valid.Replace('}', ',"extra":1}'); Accept = $false }
)
[IO.File]::WriteAllText($errorPath, "underlying pair error`nBearer private-token`n" + ('x' * 9000))
$passed = 0
foreach ($case in $cases) {
    [IO.File]::WriteAllText($output, $case.Body + "`n")
    # Optional recovery must quietly return false for malformed proof.
    $accepted = Invoke-ManagedPairApply -MarkerSource 'marker ; file' -Required $false
    if ($accepted -ne $case.Accept) { throw "receipt acceptance mismatch: $passed" }
    $passed++
}
$failure = ''
try { Invoke-ManagedPairApply -MarkerSource 'marker ; file' -Required $true } catch { $failure = $_.Exception.Message }
if ($failure -notmatch 'invalid managed-pair apply proof \(release 1.3.2\); rerun') { throw 'invalid proof lost context' }
$passed++
$exitCode = 70
[IO.File]::WriteAllText($output, $valid + "`n")
if (Invoke-ManagedPairApply -MarkerSource 'marker ; file' -Required $false) { throw 'optional failure succeeded' }
$passed++
$failure = ''
try { Invoke-ManagedPairApply -MarkerSource 'marker ; file' -Required $true } catch { $failure = $_.Exception.Message }
if ($failure -notmatch 'exit code 70, release 1.3.2.*rerun') { throw 'child failure lost context' }
$passed++
[pscustomobject]@{ passed = $passed } | ConvertTo-Json -Compress
