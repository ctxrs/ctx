param([string]$HelperPath, [string]$WorkRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. $HelperPath
function Fail([string]$Message) { throw $Message }
function Write-BoundedCapturedChildError([string]$ErrorPath) {}
function Test-InstalledTargetIdentity { return $script:identity }
function Invoke-ExecutableCaptured([string]$Executable, [string[]]$Arguments) {
    $script:calls++
    if ($Executable -cne 'installed-Core' -or $Arguments.Count -ne 5 -or
        $Arguments[0] -cne '--ctx-core-hosted-pair-install-v1' -or
        $Arguments[1] -cne 'envelope' -or $Arguments[2] -cne 'download' -or
        $Arguments[3] -cne 'Pro' -or $Arguments[4] -cne 'marker') { throw 'wrong protocol or authority' }
    return [pscustomobject]@{ ExitCode = $script:exitCode; OutputPath = $script:output; ErrorPath = $script:output }
}
$releasedPairInstall = $true
$version = '1.3.1'
$installPath = 'installed-Core'; $downloadPath = 'download'
$pairEnvelopePath = 'envelope'; $pairCompanionPath = 'Pro'; $markerSourcePath = 'marker'
$script:identity = $true; $script:exitCode = 0; $script:calls = 0
$script:output = Join-Path $WorkRoot 'receipt.json'
$valid = '{"command":"hosted_managed_pair_install","release_name":"v1.3.1","rollback_generation":14,"schema_version":1,"status":"committed"}'
$pretty = ($valid | ConvertFrom-Json | ConvertTo-Json) + [Environment]::NewLine
$cases = @(
    @{ Body = $pretty; Accept = $true },
    @{ Body = $valid + "`n"; Accept = $true },
    @{ Body = $pretty.Replace('v1.3.1', 'v1.3.2'); Accept = $false },
    @{ Body = $valid.Replace(':14', ':-1') + "`n"; Accept = $false },
    @{ Body = $valid.Replace(':1,', ':true,') + "`n"; Accept = $false },
    @{ Body = $valid.Replace('committed', 'pending') + "`n"; Accept = $false },
    @{ Body = $valid.Replace('"schema_version":1,', '') + "`n"; Accept = $false },
    @{ Body = $valid.Replace('}', ',"extra":1}') + "`n"; Accept = $false },
    @{ Body = $pretty + 'not-json'; Accept = $false },
    @{ Body = $pretty + (' ' * 600); Accept = $false }
)
$passed = 0
foreach ($case in $cases) {
    [IO.File]::WriteAllText($script:output, $case.Body)
    $accepted = $true
    try { Invoke-ReleasedManagedPairInstall } catch { $accepted = $false }
    if ($accepted -ne $case.Accept) { throw "released receipt acceptance mismatch, case $passed" }
    $passed++
}
[IO.File]::WriteAllText($script:output, $pretty)
$script:exitCode = 7
$failure = ''
try { Invoke-ReleasedManagedPairInstall } catch { $failure = $_.Exception.Message }
if ($failure -notmatch 'exit code 7') { throw 'native failure exit was lost' }; $passed++
$script:identity = $false; $before = $script:calls
try { Invoke-ReleasedManagedPairInstall } catch {}
if ($script:calls -ne $before) { throw 'uncertified installed Core was executed' }; $passed++
$script:identity = $true; $releasedPairInstall = $false
try { Invoke-ReleasedManagedPairInstall } catch {}
if ($script:calls -ne $before) { throw 'released protocol used for another release' }; $passed++
[pscustomobject]@{ passed = $passed } | ConvertTo-Json -Compress
