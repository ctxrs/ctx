param(
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [Parameter(Mandatory = $true)][string]$HelperPath,
    [Parameter(Mandatory = $true)][string]$ProfilePath,
    [Parameter(Mandatory = $true)][string]$ExpectedVersion,
    [Parameter(Mandatory = $true)][string]$ExpectedCoreSha256,
    [string]$ExpectedProSha256 = "",
    [Parameter(Mandatory = $true)][string]$WorkRoot
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'hosted installer fixture requires Windows' }
if ([Console]::OpenStandardInput().ReadByte() -ne -1) { throw 'installer fixture requires empty stdin' }
foreach ($digest in @($ExpectedCoreSha256)) {
    if ($digest -cnotmatch '^[0-9a-f]{64}$') { throw 'expected artifact digest is invalid' }
}
if ($ExpectedVersion -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw 'expected version is invalid' }
if ([version]$ExpectedVersion -lt [version]"1.5.0" -and $ExpectedProSha256 -cnotmatch '^[0-9a-f]{64}$') {
    throw 'expected legacy companion digest is invalid'
}
. $HelperPath
. $ProfilePath
Test-LoadedUserProfile
$tempRoot = Join-Path $WorkRoot ('installer-' + [Guid]::NewGuid().ToString('n'))
$bin = Join-Path $tempRoot 'installation\bin'
$core = Join-Path $bin 'ctx.exe'
$pro = Join-Path $tempRoot 'installation\libexec\ctx-pro.exe'
$shell = (Get-Process -Id $PID).Path
$environment = @{
    CTX_DATA_ROOT = (Join-Path $tempRoot 'data')
    CTX_INSTALL_TELEMETRY = '0'
    CTX_ANALYTICS_ENABLED = 'false'
    CTX_DAEMON_ENABLED = 'false'
    # This checks signed file installation only. Ambient feature selections
    # must not turn it into setup, trial activation, skill installation, or
    # semantic-runtime provisioning. Restore every value in finally below.
    CTX_INSTALL_SEMANTIC = '0'
    CTX_SEARCH_SEMANTIC = 'false'
    CTX_INSTALL_PRO_TRIAL = '0'
    CTX_INSTALL_NO_PRO_TRIAL = '1'
    CTX_INSTALL_SKILL_AGENTS = $null
    CTX_INSTALL_ALL_SKILL_AGENTS = '0'
    # The approved live feed must be selected by ordinary installer discovery,
    # including on managed reinstall. Never inherit a test metadata override.
    CTX_RELEASE_METADATA_URL = $null
    CTX_RELEASE_METADATA_SIGNATURE_URL = $null
    CTX_UPGRADE_FUNCTIONS_BASE = $null
    CTX_UPGRADE_CHANNEL = $null
    CTX_ALLOW_CUSTOM_RELEASE_BASE_URL = $null
    DO_NOT_TRACK = '1'
    NO_COLOR = '1'
}
foreach ($name in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME',
    'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'COPILOT_HOME', 'TEMP', 'TMP')) {
    $environment[$name] = Join-Path $tempRoot $name.ToLowerInvariant()
}
$environment['CTX_UPGRADE_AUTO'] = 'off'
$prior = @{}
$outcomes = @()
$status = 0
$phase = 'prepare'
try {
    $null = New-Item -ItemType Directory -Path $tempRoot
    foreach ($name in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME',
        'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'COPILOT_HOME', 'TEMP', 'TMP', 'CTX_DATA_ROOT')) {
        $null = New-Item -ItemType Directory -Path $environment[$name] -Force
    }
    foreach ($name in $environment.Keys) {
        $prior[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $environment[$name], 'Process')
    }
    foreach ($phase in @('fresh', 'managed-reinstall')) {
        $result = Invoke-ExecutableCaptured $shell @(
            '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $InstallerPath,
            '-BinDir', $bin,
            '-NoSetup', '-NoDaemon', '-NoSkill', '-NoProTrial', '-NoModifyPath'
        )
        if (-not $result.Started -or $null -eq $result.ExitCode) { $status = 1 }
        elseif ($result.ExitCode -ne 0) { $status = [int]$result.ExitCode }
        if ($status -ne 0) {
            $detail = [IO.File]::ReadAllText($result.ErrorPath)
            if ($detail.Length -gt 8192) { $detail = $detail.Substring(0, 8192) }
            [Console]::Error.WriteLine($detail)
            throw 'installer invocation failed'
        }
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $core).Hash.ToLowerInvariant() -cne $ExpectedCoreSha256) {
            throw "$phase did not publish the exact signed binary"
        }
        $proDigest = $null
        $singleBinary = [version]$ExpectedVersion -ge [version]"1.5.0"
        if ($singleBinary) {
            if (Test-Path -LiteralPath $pro) { throw "1.5 installed a legacy companion" }
        } else {
            $proDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $pro).Hash.ToLowerInvariant()
            if ($proDigest -cne $ExpectedProSha256) { throw "$phase companion digest differs" }
        }
        $marker = [IO.File]::ReadAllText($core + '.install.json') | ConvertFrom-Json
        if ($marker.version -cne $ExpectedVersion -or $marker.sha256 -cne $ExpectedCoreSha256) {
            throw "$phase marker does not bind the expected release"
        }
        $pairPath = Join-Path $tempRoot 'installation\share\ctx\managed-pair-state.json'
        if ($singleBinary) {
            $envelopePath = Join-Path $tempRoot 'installation\share\ctx\managed-pair-envelope.json'
            if ((Test-Path -LiteralPath $pairPath) -or (Test-Path -LiteralPath $envelopePath) -or
                ($marker.PSObject.Properties['managed_pair'] -and $marker.managed_pair -eq $true)) {
                throw "$phase unexpectedly retained a legacy pair receipt"
            }
        } elseif (-not (Test-Path -LiteralPath $pairPath -PathType Leaf)) {
            throw "$phase has no Core-owned pair receipt"
        }
        $outcomes += [pscustomobject]@{ phase = $phase; exit_code = $result.ExitCode; core_sha256 = $ExpectedCoreSha256; pro_sha256 = $proDigest; single_binary = $singleBinary }
    }
} catch {
    if ($status -eq 0) { $status = 1 }
    [Console]::Error.WriteLine((@{ stage = $phase; exit_code = $status; error_type = $_.Exception.GetType().Name } | ConvertTo-Json -Compress))
} finally {
    foreach ($name in $prior.Keys) { [Environment]::SetEnvironmentVariable($name, $prior[$name], 'Process') }
    try { if (Test-Path -LiteralPath $tempRoot) { Remove-Item -LiteralPath $tempRoot -Recurse -Force } }
    catch { if ($status -eq 0) { $status = 1 }; [Console]::Error.WriteLine('installer fixture cleanup failed') }
}
if ($status -eq 0) {
    [pscustomobject]@{ powershell = $PSVersionTable.PSVersion.ToString(); version = $ExpectedVersion; loaded_profile = $true; passed = $outcomes } | ConvertTo-Json -Depth 4 -Compress
}
exit $status
