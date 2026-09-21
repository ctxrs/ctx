param(
    [Parameter(Mandatory = $true)][string]$PacketRoot,
    [Parameter(Mandatory = $true)][string]$ProfilePath,
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [Parameter(Mandatory = $true)][string]$ExpectedInstallerSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedVersion,
    [Parameter(Mandatory = $true)][string]$ExpectedCoreSha256,
    [string]$ExpectedProSha256 = ""
)
# The release owner supplies the canonical, source/producer-verified packet,
# same-source profile script, and separately verified served installer readback.
# This is a comparison/execution gate, not a metadata or signature authority.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

function Assert-LiveResult($Result, [string]$ShellVersion) {
    if ($Result.powershell -notlike $ShellVersion -or $Result.version -cne $ExpectedVersion -or
        $Result.loaded_profile -isnot [bool] -or -not $Result.loaded_profile -or
        @($Result.passed).Count -ne 2 -or ($Result.passed.phase -join ',') -cne 'fresh,managed-reinstall') {
        throw 'live installer result is incomplete or has the wrong shell/release'
    }
    foreach ($phase in $Result.passed) {
        if (($phase.exit_code -isnot [int] -and $phase.exit_code -isnot [long]) -or $phase.exit_code -ne 0 -or
            $phase.core_sha256 -cne $ExpectedCoreSha256 -or
            ([version]$ExpectedVersion -lt [version]"1.5.0" -and $phase.pro_sha256 -cne $ExpectedProSha256) -or
            ([version]$ExpectedVersion -ge [version]"1.5.0" -and ($null -ne $phase.pro_sha256 -or $phase.single_binary -ne $true))) {
            throw 'live installer result differs from the approved candidate'
        }
    }
}

function Invoke-LiveFixture([string]$Shell, [string]$ShellVersion) {
    if (-not (Test-Path -LiteralPath $Shell -PathType Leaf)) { throw 'required PowerShell runtime is missing' }
    $arguments = @('-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
        $fixturePath, '-InstallerPath', $InstallerPath, '-HelperPath', $helperPath,
        '-ProfilePath', $ProfilePath, '-ExpectedVersion', $ExpectedVersion,
        '-ExpectedCoreSha256', $ExpectedCoreSha256, '-ExpectedProSha256', $ExpectedProSha256, '-WorkRoot', $root)
    # The caller's stdin is verified EOF. The shared capture helper retains
    # native argv/streams/exit; the governed guest owns the whole command bound.
    $capture = Invoke-ExecutableCaptured $Shell $arguments
    if (-not $capture.Started -or $null -eq $capture.ExitCode) { $script:status = 1 }
    elseif ($capture.ExitCode -ne 0) { $script:status = [int]$capture.ExitCode }
    foreach ($capturePath in @($capture.ErrorPath, $capture.OutputPath)) {
        $stream = [IO.File]::OpenRead($capturePath)
        try { $stream.CopyTo([Console]::OpenStandardError()) } finally { $stream.Dispose() }
    }
    if (-not $capture.Started -or $null -eq $capture.ExitCode -or $capture.ExitCode -ne 0) {
        throw 'live installer fixture failed'
    }
    $result = [IO.File]::ReadAllText($capture.OutputPath) | ConvertFrom-Json
    Assert-LiveResult $result $ShellVersion
    return $result
}

$root = $null; $status = 0; $stage = 'inputs'; $outcomes = @()
$oldTemp = $env:TEMP; $oldTmp = $env:TMP
try {
    if ($env:OS -ne 'Windows_NT') { throw 'live Windows installer gate requires Windows' }
    if ([Console]::OpenStandardInput().ReadByte() -ne -1) { throw 'live installer gate requires empty stdin' }
    foreach ($digest in @($ExpectedInstallerSha256, $ExpectedCoreSha256)) {
        if ($digest -cnotmatch '^[0-9a-f]{64}$') { throw 'approved digest is invalid' }
    }
    if ($ExpectedVersion -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw 'approved stable version is invalid' }
    if ([version]$ExpectedVersion -lt [version]"1.5.0" -and $ExpectedProSha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'approved legacy companion digest is invalid'
    }
    if ((Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $ExpectedInstallerSha256) {
        throw 'served installer readback digest differs'
    }
    $packet = Get-Content -LiteralPath (Join-Path $PacketRoot 'windows-installer-fixtures.json') -Raw | ConvertFrom-Json
    $files = Join-Path $PacketRoot 'files'
    $helperPath = Join-Path $files 'native-process.ps1'
    $fixturePath = Join-Path $files 'hosted-windows-install-fixture.ps1'
    foreach ($name in @('native-process.ps1', 'hosted-windows-install-fixture.ps1')) {
        $item = @( $packet.files | Where-Object { $_.path -ceq $name } )
        $file = Get-Item -LiteralPath (Join-Path $files $name)
        if ($item.Count -ne 1 -or $file.Length -ne $item[0].size_bytes -or
            (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $item[0].sha256) {
            throw 'verified packet fixture changed'
        }
    }
    $runtime = $packet.powershell
    if ($runtime.version -cnotmatch '^7\.[0-9]+\.[0-9]+$' -or
        $runtime.file -cne ('PowerShell-' + $runtime.version + '-win-x64.zip')) { throw 'pinned Windows PowerShell archive is missing' }
    $archive = Get-Item -LiteralPath (Join-Path $PacketRoot $runtime.file)
    if ($archive.Length -ne $runtime.size_bytes -or
        (Get-FileHash -LiteralPath $archive.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $runtime.sha256) {
        throw 'pinned PowerShell archive changed'
    }
    $root = Join-Path $env:SystemDrive ('q-live-' + [Guid]::NewGuid().ToString('n').Substring(0, 8))
    $null = New-Item -ItemType Directory -Path $root
    $owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetOwner($owner); $acl.SetAccessRuleProtection($true, $false)
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        $owner, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    Set-Acl -LiteralPath $root -AclObject $acl
    $env:TEMP = $root; $env:TMP = $root
    $tempRoot = $root # Shared capture helper's existing caller-owned directory.
    . $helperPath
    $ps5 = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $ps7Root = Join-Path $root 'powershell'
    Expand-Archive -LiteralPath $archive.FullName -DestinationPath $ps7Root
    $ps7 = Join-Path $ps7Root 'pwsh.exe'
    if ((Get-AuthenticodeSignature -LiteralPath $ps7).Status -ne 'Valid') { throw 'PowerShell runtime signature is invalid' }
    $stage = 'powershell_5_1'
    $outcomes += Invoke-LiveFixture $ps5 '5.1.*'
    $stage = 'powershell_7'
    $outcomes += Invoke-LiveFixture $ps7 $runtime.version
} catch {
    if ($status -eq 0) { $status = 1 }
    [Console]::Error.WriteLine((@{ stage = $stage; exit_code = $status; error_type = $_.Exception.GetType().Name } | ConvertTo-Json -Compress))
} finally {
    $env:TEMP = $oldTemp; $env:TMP = $oldTmp
    try { if ($null -ne $root -and (Test-Path -LiteralPath $root)) { Remove-Item -LiteralPath $root -Recurse -Force } }
    catch { if ($status -eq 0) { $status = 1 }; [Console]::Error.WriteLine('live installer cleanup failed') }
}
if ($status -eq 0) {
    @{ stage = 'windows_live_installer'; status = 'passed'; release_authority = $false;
        version = $ExpectedVersion; installer_sha256 = $ExpectedInstallerSha256; shells = $outcomes } | ConvertTo-Json -Depth 6 -Compress
}
exit $status
