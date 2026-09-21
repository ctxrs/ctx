param(
    [Parameter(Mandatory = $true)][string]$HelperPath,
    [Parameter(Mandatory = $true)][string]$FixtureExecutable,
    [Parameter(Mandatory = $true)][string]$WorkRoot
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'native process fixture requires Windows' }
. $HelperPath
$tempRoot = Join-Path $WorkRoot ('capture-' + [Guid]::NewGuid().ToString('n'))
$null = New-Item -ItemType Directory -Path $tempRoot
$passed = [Collections.Generic.List[string]]::new()
function Assert-True([bool]$Value, [string]$Message) {
    if (-not $Value) { throw $Message }
}
try {
    foreach ($exit in @(0, 7)) {
        $result = Invoke-ExecutableCaptured $FixtureExecutable @('streams', [string]$exit)
        Assert-True ($result.Started -and $null -eq $result.ProcessError) 'native child did not start normally'
        Assert-True ($result.ExitCode -eq $exit) "native exit $exit was not preserved"
        Assert-True ([IO.File]::ReadAllText($result.OutputPath) -ceq "native stdout`n") 'stdout mismatch'
        Assert-True ([IO.File]::ReadAllText($result.ErrorPath) -ceq "native stderr`n") 'stderr mismatch'
        $passed.Add("stderr-with-exit-$exit")
    }
    $result = Invoke-ExecutableCaptured $FixtureExecutable @()
    Assert-True ($result.Started -and $result.ExitCode -eq 0) 'zero-argument command failed'
    $passed.Add('zero-arguments')
    $result = Invoke-ExecutableCaptured (Join-Path $tempRoot 'missing.exe') @()
    Assert-True (-not $result.Started -and $result.ExitCode -ne 0) 'missing executable was reported successful'
    Assert-True (-not [string]::IsNullOrWhiteSpace($result.ProcessError)) 'launch diagnostic was lost'
    Assert-True ((Get-Item -LiteralPath $result.ErrorPath).Length -gt 0) 'launch stderr was lost'
    $passed.Add('missing-executable')
    $arguments = @('', 'with spaces', 'double"quote', 'trailing\', 'space then\', '\\"', 'semi;dollar$and&', ('unicode-' + [char]0x03bb))
    $result = Invoke-ExecutableCaptured $FixtureExecutable (@('arguments') + $arguments)
    Assert-True ($result.ExitCode -eq 0) 'argv child failed'
    $actual = [IO.File]::ReadAllLines($result.OutputPath)
    Assert-True ($actual.Count -eq $arguments.Count) 'argv count changed'
    for ($i = 0; $i -lt $arguments.Count; $i++) {
        $decoded = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($actual[$i]))
        Assert-True ($decoded -ceq $arguments[$i]) "argv value changed at index $i"
    }
    $passed.Add('native-argv')
    $result = Invoke-ExecutableCaptured $FixtureExecutable @('large')
    Assert-True ($result.ExitCode -eq 0) 'large dual-stream child failed'
    Assert-True (([IO.File]::ReadAllText($result.OutputPath) -ceq ('o' * 1048576)) -and
        ([IO.File]::ReadAllText($result.ErrorPath) -ceq ('e' * 1048576))) 'dual-stream capture changed bytes'
    $passed.Add('large-dual-streams')
    $result = Invoke-ExecutableCaptured $FixtureExecutable @('streams', '0') -InheritStandardError
    Assert-True ($result.ExitCode -eq 0 -and (Get-Item $result.ErrorPath).Length -eq 0) 'inherited stderr was recaptured or failed'
    $passed.Add('inherited-stderr')
    $childFile = Join-Path $tempRoot 'child.pid'
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $result = Invoke-ExecutableCaptured $FixtureExecutable @('descendant', $childFile)
        $watch.Stop()
        Assert-True ($result.ExitCode -eq 0 -and $watch.Elapsed.TotalSeconds -lt 8) 'capture waited for a background descendant'
    } finally {
        if (Test-Path -LiteralPath $childFile) {
            Stop-Process -Id ([int][IO.File]::ReadAllText($childFile)) -ErrorAction SilentlyContinue
        }
    }
    $passed.Add('foreground-lifetime')
    [pscustomobject]@{ powershell = $PSVersionTable.PSVersion.ToString(); passed = $passed.ToArray() } | ConvertTo-Json -Compress
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
}
