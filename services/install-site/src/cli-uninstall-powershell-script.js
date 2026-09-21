import { CLI_INSTALL_POWERSHELL_PATH_IDENTITY } from "./cli-install-powershell-managed-install.js";
import { CLI_INSTALL_POWERSHELL_PATH_TYPES } from "./cli-install-powershell-platform.js";
import {
  generateInstallAttemptId,
  normalizeEmbeddedInstallAttemptId,
} from "./install-attempt-id.js";
import {
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
} from "./install-stage-contract.js";

const DEFAULT_FUNCTIONS_BASE = "https://cli.ctx.rs/functions/v1";

export function renderCliUninstallPowerShellScript({
  functionsBase = DEFAULT_FUNCTIONS_BASE,
  installAttemptId = generateInstallAttemptId(),
} = {}) {
  const normalizedBase = String(functionsBase).replace(/\/+$/, "");
  const normalizedInstallAttemptId = normalizeEmbeddedInstallAttemptId(installAttemptId);
  return `[CmdletBinding()]
param(
    [string]$InstallPath = "",
    [string]$MarkerPath = "",
    [string]$DataRoot = "",
    [switch]$DeleteData,
    [switch]$KeepData,
    [switch]$NonInteractive,
    [switch]$Json
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    throw "uninstall.ps1: $Message"
}

function Test-LegacyControlTruthy([string]$Value) {
    $normalized = $Value.Trim().ToLowerInvariant()
    return $normalized -notin @("", "0", "false", "no", "off")
}

function Test-CanonicalAnalyticsDisabled {
    $value = [Environment]::GetEnvironmentVariable("CTX_ANALYTICS_ENABLED", "Process")
    if ($null -eq $value) {
        return $false
    }
    return $value.Trim().ToLowerInvariant() -in @("0", "false", "no", "off")
}

$deprecatedControlMappings = [System.Collections.Generic.List[string]]::new()
function Apply-DeprecatedAnalyticsControl([string]$Name) {
    $legacyValue = [Environment]::GetEnvironmentVariable($Name, "Process")
    if ($null -eq $legacyValue) {
        return
    }
    $deprecatedControlMappings.Add("$Name -> CTX_ANALYTICS_ENABLED=false")
    if (Test-LegacyControlTruthy -Value $legacyValue) {
        [Environment]::SetEnvironmentVariable(
            "CTX_ANALYTICS_ENABLED",
            "false",
            "Process"
        )
    }
    [Environment]::SetEnvironmentVariable($Name, $null, "Process")
}

foreach ($legacyName in @(
    "CTX_ANALYTICS_OFF",
    "CTX_DISABLE_ANALYTICS",
    "CTX_INSTALL_DIAGNOSTICS_OFF"
)) {
    Apply-DeprecatedAnalyticsControl -Name $legacyName
}
if ($deprecatedControlMappings.Count -gt 0) {
    [Console]::Error.WriteLine(
        "warning: deprecated environment variables detected: " +
        ($deprecatedControlMappings -join "; ") +
        ". Update your environment to use the replacements."
    )
}

if (-not [Environment]::Is64BitOperatingSystem) {
    Fail "only 64-bit Windows hosts are supported"
}
if ($DeleteData -and $KeepData) {
    Fail "choose exactly one of -DeleteData or -KeepData"
}

$homeDirectory = if (-not [string]::IsNullOrWhiteSpace($HOME)) {
    $HOME
} else {
    [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
}
if ([string]::IsNullOrWhiteSpace($homeDirectory)) {
    Fail "the user profile directory is unavailable"
}
if ([string]::IsNullOrWhiteSpace($InstallPath)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CTX_UNINSTALL_INSTALL_PATH)) {
        $InstallPath = $env:CTX_UNINSTALL_INSTALL_PATH
    } else {
        $InstallPath = Join-Path $homeDirectory ".local\\bin\\ctx.exe"
    }
}
if ([string]::IsNullOrWhiteSpace($MarkerPath)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CTX_UNINSTALL_MARKER_PATH)) {
        $MarkerPath = $env:CTX_UNINSTALL_MARKER_PATH
    } else {
        $MarkerPath = "$InstallPath.install.json"
    }
}
if ([string]::IsNullOrWhiteSpace($DataRoot)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CTX_DATA_ROOT)) {
        $DataRoot = $env:CTX_DATA_ROOT
    } else {
        $DataRoot = Join-Path $homeDirectory ".ctx"
    }
}

$InstallPath = [IO.Path]::GetFullPath($InstallPath)
$MarkerPath = [IO.Path]::GetFullPath($MarkerPath)
$DataRoot = [IO.Path]::GetFullPath($DataRoot)
$binDirectory = Split-Path -Parent $InstallPath
$functionsBase = "${normalizedBase}"
$installAttemptId = "${normalizedInstallAttemptId}"
if (-not [string]::IsNullOrWhiteSpace($env:CTX_INSTALL_ATTEMPT_ID) -and
    $env:CTX_INSTALL_ATTEMPT_ID -match '^ia_[A-Za-z0-9_-]{8,128}$') {
    $installAttemptId = $env:CTX_INSTALL_ATTEMPT_ID
}
[Environment]::SetEnvironmentVariable("CTX_INSTALL_ATTEMPT_ID", $null, "Process")

$script:installStageDeliveryEnabled = $true
function Send-InstallStage([string]$Status) {
    if (-not $script:installStageDeliveryEnabled -or
        (Test-CanonicalAnalyticsDisabled) -or
        $functionsBase -notmatch '^https://') {
        return
    }
    $body = [ordered]@{
        event_name = "${INSTALL_STAGE_EVENT_NAME}"
        event_version = ${INSTALL_STAGE_EVENT_VERSION}
        install_attempt_id = $installAttemptId
        stage = "uninstall"
        status = $Status
        platform = "windows"
        arch = "x64"
        script_family = "powershell"
    } | ConvertTo-Json -Compress
    try {
        Invoke-WebRequest -Uri ($functionsBase.TrimEnd("/") + "/install-attempt") \
            -Method POST -ContentType "application/json" -Body $body \
            -UseBasicParsing -TimeoutSec 1 | Out-Null
    } catch {
        $script:installStageDeliveryEnabled = $false
    }
}

function Assert-RegularManagedFile([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "$Label is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        Fail "$Label must not be a reparse point: $Path"
    }
}

${CLI_INSTALL_POWERSHELL_PATH_IDENTITY}
function Read-ManagedMarker {
    Assert-RegularManagedFile -Path $MarkerPath -Label "managed install marker"
    $markerItem = Get-Item -LiteralPath $MarkerPath
    if ($markerItem.Length -lt 2 -or $markerItem.Length -gt 64KB) {
        Fail "managed install marker has an invalid size"
    }
    try {
        $marker = [IO.File]::ReadAllText($MarkerPath) | ConvertFrom-Json
    } catch {
        Fail "managed install marker is invalid JSON"
    }
    if ($marker.schema_version -ne 1 -or
        $marker.manager -cne "ctx-hosted-installer" -or
        $marker.platform -cne "windows-x64") {
        Fail "managed install marker identity is invalid"
    }
    if (-not (Test-ManagedInstallPathIdentity -Candidate $marker.install_path -Expected $InstallPath)) {
        Fail "managed install marker does not own the requested executable"
    }
    if ([string]$marker.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
        Fail "managed install marker has an invalid executable digest"
    }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $InstallPath).Hash
    if ($actual -ine [string]$marker.sha256) {
        Fail "installed executable differs from its managed install marker"
    }
    return $marker
}

function Get-InstalledVersion([string]$Executable = $InstallPath) {
    $output = (& $Executable --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or
        $output -notmatch '^ctx (?<major>[0-9]+)\\.(?<minor>[0-9]+)\\.(?<patch>[0-9]+)(?:[-+][0-9A-Za-z.-]+)?$') {
        Fail "the installed ctx binary returned an invalid version"
    }
    return [ordered]@{
        text = $output.Substring(4)
        major = [int]$Matches.major
        minor = [int]$Matches.minor
        patch = [int]$Matches.patch
    }
}

function Assert-CoreDaemonTeardownResult([object]$Result) {
    if ($null -eq $Result -or $Result -isnot [pscustomobject]) {
        Fail "Core daemon teardown did not return a typed result; ctx remains installed"
    }
    $requiredNames = @(
        "schema_version",
        "command",
        "ok",
        "scope",
        "requested_data_root",
        "canonical_data_root",
        "quiesced_roots",
        "quiesced_root_count",
        "installation_quiescent",
        "daemon_enabled",
        "daemon_running",
        "owner_lock_released",
        "endpoint_released",
        "supervisor_removed",
        "coordination_state_removed",
        "binary_retained",
        "retry_safe",
        "local_only"
    )
    $properties = @($Result.PSObject.Properties)
    if ($properties.Count -ne $requiredNames.Count) {
        Fail "Core daemon teardown did not prove complete cleanup; ctx remains installed"
    }
    foreach ($name in $requiredNames) {
        $matchedProperties = @($properties | Where-Object { $_.Name -ceq $name })
        if ($matchedProperties.Count -ne 1) {
            Fail "Core daemon teardown did not prove complete cleanup; ctx remains installed"
        }
    }
    if (-not (($Result.schema_version -is [int]) -or
            ($Result.schema_version -is [long])) -or
        $Result.schema_version -ne 1 -or
        $Result.command -isnot [string] -or
        $Result.command -cne "daemon_prepare_uninstall" -or
        $Result.scope -isnot [string] -or
        $Result.scope -cne "installation" -or
        $Result.requested_data_root -isnot [string] -or
        [IO.Path]::GetFullPath($Result.requested_data_root) -cne $DataRoot -or
        $Result.canonical_data_root -isnot [string] -or
        [string]::IsNullOrWhiteSpace($Result.canonical_data_root) -or
        -not (($Result.quiesced_root_count -is [int]) -or
            ($Result.quiesced_root_count -is [long])) -or
        $Result.quiesced_root_count -lt 1) {
        Fail "Core daemon teardown returned an unsupported contract; ctx remains installed"
    }
    foreach ($name in @(
        "ok",
        "installation_quiescent",
        "daemon_enabled",
        "daemon_running",
        "owner_lock_released",
        "endpoint_released",
        "supervisor_removed",
        "coordination_state_removed",
        "binary_retained",
        "retry_safe",
        "local_only"
    )) {
        if ($Result.$name -isnot [bool]) {
            Fail "Core daemon teardown did not return typed lifecycle proof; ctx remains installed"
        }
    }
    $quiescedRoots = @($Result.quiesced_roots)
    if ($quiescedRoots.Count -ne $Result.quiesced_root_count) {
        Fail "Core daemon teardown returned inconsistent all-root proof; ctx remains installed"
    }
    $normalizedRoots = @(
        $quiescedRoots | ForEach-Object {
            if ($_ -isnot [string] -or [string]::IsNullOrWhiteSpace($_)) {
                Fail "Core daemon teardown returned malformed all-root proof; ctx remains installed"
            }
            [IO.Path]::GetFullPath($_)
        }
    )
    $canonicalRoot = [IO.Path]::GetFullPath($Result.canonical_data_root)
    if ($DataRoot -cnotin $normalizedRoots -or
        $canonicalRoot -cnotin $normalizedRoots) {
        Fail "Core daemon teardown did not bind all-root proof to this installation; ctx remains installed"
    }
    if (-not $Result.ok -or
        -not $Result.installation_quiescent -or
        $Result.daemon_enabled -or
        $Result.daemon_running -or
        -not $Result.owner_lock_released -or
        -not $Result.endpoint_released -or
        -not $Result.supervisor_removed -or
        -not $Result.coordination_state_removed -or
        -not $Result.binary_retained -or
        -not $Result.retry_safe -or
        -not $Result.local_only) {
        Fail "Core daemon teardown did not prove complete cleanup; ctx remains installed"
    }
}

function Invoke-CoreDaemonTeardown([object]$Version) {
    if ($Version.major -eq 0 -and $Version.minor -le 25) {
        if (-not $Json) {
            Write-Host "Installed ctx $($Version.text) predates the persistent Core daemon; no Core daemon teardown is required."
        }
        return
    }
    $arguments = @(
        "--data-root",
        $DataRoot,
        "daemon",
        "disable",
        "--prepare-uninstall",
        "--format=json"
    )
    $output = & $InstallPath @arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "Core daemon teardown failed with status $LASTEXITCODE; ctx remains installed"
    }
    $jsonResult = ($output | Out-String)
    if ([string]::IsNullOrWhiteSpace($jsonResult) -or $jsonResult.Length -gt 64KB) {
        Fail "Core daemon teardown returned an invalid result; ctx remains installed"
    }
    try {
        $result = $jsonResult | ConvertFrom-Json
    } catch {
        Fail "Core daemon teardown did not return valid JSON; ctx remains installed"
    }
    Assert-CoreDaemonTeardownResult -Result $result
}

function Invoke-ProLifecycle([object]$Version) {
    if ($Version.major -gt 1 -or ($Version.major -eq 1 -and $Version.minor -ge 5)) {
        return "unchanged"
    }
    if ($Version.major -eq 0 -and $Version.minor -le 25) {
        if (-not $Json) {
            Write-Host "Installed ctx $($Version.text) predates Local Pro; no Pro lifecycle cleanup is required."
        }
        return "not_applicable"
    }
    $null = & $InstallPath pro uninstall --help 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "installed ctx $($Version.text) does not expose the required Pro uninstall capability"
    }
    $arguments = @("--data-root", $DataRoot, "pro", "uninstall")
    if ($DeleteData) {
        $arguments += "--delete-data"
    } elseif ($KeepData) {
        $arguments += "--keep-data"
    }
    $arguments += "--json"
    $output = & $InstallPath @arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw ($output | Out-String)
    }
    try {
        $result = ($output | Out-String) | ConvertFrom-Json
    } catch {
        Fail "native Pro uninstall did not return valid JSON"
    }
    if (-not $result.uninstalled -or -not $result.canonical_history_preserved) {
        Fail "native Pro uninstall did not satisfy its lifecycle contract"
    }
    return [string]$result.local_pro_data
}

function Remove-ManagedFile([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "refusing to remove a non-file $Label path: $Path"
    }
    Remove-Item -LiteralPath $Path -Force
    if (Test-Path -LiteralPath $Path) {
        Fail "failed to remove \${Label}: $Path"
    }
}

function Remove-InstallDirectoryFromUserPath {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ([string]::IsNullOrWhiteSpace($userPath)) {
        return $false
    }
    $separator = [IO.Path]::PathSeparator
    $retained = @(
        $userPath -split [regex]::Escape([string]$separator) | Where-Object {
            if ([string]::IsNullOrWhiteSpace($_)) {
                return $false
            }
            try {
                [IO.Path]::GetFullPath($_.Trim().Trim('"').TrimEnd("\\", "/")) -ine
                    [IO.Path]::GetFullPath($binDirectory.TrimEnd("\\", "/"))
            } catch {
                $true
            }
        }
    )
    if ($retained.Count -eq ($userPath -split [regex]::Escape([string]$separator)).Count) {
        return $false
    }
    [Environment]::SetEnvironmentVariable("Path", ($retained -join $separator), "User")
    return $true
}

function Invoke-HostedUninstallTransaction(
    [string]$Executable,
    [string]$Action,
    [string]$ExpectedStatus,
    [switch]$IncludeAttempt,
    [switch]$AllowNotReady
) {
    $arguments = @(
        "upgrade", "--hosted-transaction", $Action,
        "--install-path", $InstallPath
    )
    if ($IncludeAttempt) {
        $arguments += @("--attempt-id", $installAttemptId)
    }
    $output = & $Executable @arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        if ($AllowNotReady) {
            return $null
        }
        Fail "ctx could not complete hosted uninstall transaction phase $Action"
    }
    try {
        $result = ($output | Out-String) | ConvertFrom-Json
    } catch {
        Fail "ctx returned invalid hosted uninstall transaction proof"
    }
    $required = @(
        "schema_version", "command", "ok", "status", "daemon_admission_fenced", "attempt_id",
        "install_path", "helper_path", "binary_sha256", "marker_sha256"
    )
    if (@($result.PSObject.Properties).Count -ne $required.Count) {
        Fail "ctx returned invalid hosted uninstall transaction proof"
    }
    foreach ($name in $required) {
        if (@($result.PSObject.Properties | Where-Object { $_.Name -ceq $name }).Count -ne 1) {
            Fail "ctx returned invalid hosted uninstall transaction proof"
        }
    }
    if ($result.schema_version -ne 2 -or
        $result.command -cne "hosted_uninstall_transaction" -or
        $result.ok -isnot [bool] -or -not $result.ok -or
        $result.daemon_admission_fenced -isnot [bool] -or
        -not $result.daemon_admission_fenced -or
        $result.status -cne $ExpectedStatus -or
        -not (Test-ManagedInstallPathIdentity -Candidate $result.install_path -Expected $InstallPath) -or
        -not (Test-ManagedInstallPathIdentity -Candidate $result.helper_path -Expected $HelperPath)) {
        Fail "ctx returned mismatched hosted uninstall transaction proof"
    }
    return $result
}

function Open-RecoveryPathGuard([string]$Path, [switch]$Directory) {
    $guard = $null
    try {
        $guard = if ($Directory) { [CtxInstallerPathGuard]::AcquireDirectory($Path, $true) }
                 else { [CtxInstallerPathGuard]::AcquireLeaf($Path) }
        $guard.AssertCanonical()
        # Same protected, exact user/SYSTEM DACL and token-owner policy as the
        # native private-path verifier. Never repair an existing descriptor.
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
        $acl = Get-Acl -LiteralPath $Path
        $descriptor = [Security.AccessControl.RawSecurityDescriptor]::new($acl.GetSecurityDescriptorBinaryForm(), 0)
        if ($null -eq $descriptor.Owner -or
            ($descriptor.Owner.Value -cne $identity.User.Value -and $descriptor.Owner.Value -cne $identity.Owner.Value) -or
            ($descriptor.ControlFlags -band [Security.AccessControl.ControlFlags]::DiscretionaryAclProtected) -eq 0 -or
            $null -eq $descriptor.DiscretionaryAcl) {
            Fail "interrupted uninstall path has unsafe ownership or access controls"
        }
        $expected = @(@($identity.User.Value, 'S-1-5-18') | Select-Object -Unique)
        if ($descriptor.DiscretionaryAcl.Count -ne $expected.Count) {
            Fail "interrupted uninstall path has an unsafe DACL"
        }
        $seen = @()
        $flags = if ($Directory) { 3 } else { 0 }
        foreach ($ace in $descriptor.DiscretionaryAcl) {
            if ($ace -isnot [Security.AccessControl.CommonAce] -or
                $ace.AceType -ne [Security.AccessControl.AceType]::AccessAllowed -or
                [int]$ace.AceFlags -ne $flags -or $ace.AccessMask -ne 0x001f01ff -or
                $ace.SecurityIdentifier.Value -cnotin $expected -or $ace.SecurityIdentifier.Value -cin $seen) {
                Fail "interrupted uninstall path has an unsafe DACL"
            }
            $seen += $ace.SecurityIdentifier.Value
        }
        $guard.AssertUnchanged()
        return $guard
    } catch {
        if ($null -ne $guard) { $guard.Dispose() }
        throw
    }
}

function Assert-RecoveryDataChoice {
    if ($KeepData -or (-not (Test-Path -LiteralPath $TransactionPath) -and
        -not (Test-Path -LiteralPath $HelperPath))) { return }
    # Read-only version classification; native commit retains full journal,
    # lock and mutation ownership.
    if (-not $DeleteData -and -not (Test-Path -LiteralPath $TransactionPath)) { return }
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { Fail "cannot classify Windows recovery path ownership on this platform" }
${CLI_INSTALL_POWERSHELL_PATH_TYPES}
    $guards = [Collections.Generic.List[IDisposable]]::new()
    try {
        $guards.Add((Open-RecoveryPathGuard -Path $binDirectory -Directory))
        $guards.Add((Open-RecoveryPathGuard -Path $TransactionPath))
        if (-not (Test-ManagedInstallPathIdentity -Candidate $MarkerPath -Expected "$InstallPath.install.json")) { Fail "recovery marker must be the fixed adjacent path" }
    if (-not (Test-Path -LiteralPath $HelperPath) -and
        (Test-Path -LiteralPath $InstallPath -PathType Leaf) -and (Test-Path -LiteralPath $MarkerPath -PathType Leaf)) {
        $guards.Add((Open-RecoveryPathGuard -Path $InstallPath))
        $guards.Add((Open-RecoveryPathGuard -Path $MarkerPath))
        $null = Read-ManagedMarker
        $recoveryVersion = Get-InstalledVersion
    } else {
    $guards.Add((Open-RecoveryPathGuard -Path $HelperPath))
    if ((Get-Item -LiteralPath $TransactionPath).Length -gt 16MB -or
        (Get-Item -LiteralPath $HelperPath).Length -gt 512MB) {
        Fail "interrupted uninstall identity exceeds its size limit"
    }
    $journal = [IO.File]::ReadAllText($TransactionPath) | ConvertFrom-Json
    if ($journal.schema_version -ne 1 -or $journal.kind -cne "uninstall" -or
        -not (Test-ManagedInstallPathIdentity -Candidate $journal.install_path -Expected $InstallPath) -or
        [string]$journal.binary_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        (Get-FileHash -LiteralPath $HelperPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $journal.binary_sha256) {
        Fail "interrupted uninstall helper differs from its recorded executable identity"
    }
    $recoveryVersion = Get-InstalledVersion -Executable $HelperPath
    }
    } finally {
        foreach ($guard in $guards) { $guard.Dispose() }
    }
    if ($recoveryVersion.major -gt 1 -or ($recoveryVersion.major -eq 1 -and $recoveryVersion.minor -ge 5)) {
        if ($DeleteData) {
            Fail "-DeleteData was legacy derived-data cleanup and is retired in ctx 1.5. History and legacy data are preserved; rerun without -DeleteData"
        }
    } elseif (-not ($DeleteData -or $KeepData)) {
        Fail "uninstall requires an explicit data choice: -DeleteData or -KeepData"
    }
}

$completed = $false
$installLeaf = [IO.Path]::GetFileName($InstallPath)
$HelperPath = Join-Path $binDirectory ("." + $installLeaf + ".hosted-uninstall-helper.exe")
$TransactionPath = Join-Path $binDirectory ("." + $installLeaf + ".hosted-install-transaction.json")
Send-InstallStage -Status "started"
try {
    Assert-RecoveryDataChoice
    if (-not (Test-Path -LiteralPath $InstallPath) -and
        -not (Test-Path -LiteralPath $MarkerPath)) {
        if (Test-Path -LiteralPath $TransactionPath -PathType Leaf) {
            $null = Invoke-HostedUninstallTransaction \
                -Executable $HelperPath \
                -Action "uninstall-commit" \
                -ExpectedStatus "committed"
            Remove-ManagedFile -Path $HelperPath -Label "hosted uninstall helper"
        } elseif (Test-Path -LiteralPath $HelperPath -PathType Leaf) {
            Remove-ManagedFile -Path $HelperPath -Label "completed uninstall helper"
        }
        $result = [ordered]@{
            schema_version = 1
            uninstalled = $true
            already_uninstalled = $true
            platform = "windows-x64"
            canonical_history_preserved = $true
            install_path_removed_from_user_path = $false
        }
        if ($Json) {
            $result | ConvertTo-Json -Compress
        } else {
            Write-Host "ctx is already uninstalled. Local ctx history was preserved."
        }
        Send-InstallStage -Status "completed"
        $completed = $true
        return
    }
    if (-not (Test-Path -LiteralPath $InstallPath) -and
        (Test-Path -LiteralPath $MarkerPath -PathType Leaf)) {
        $null = Invoke-HostedUninstallTransaction \
            -Executable $HelperPath \
            -Action "uninstall-commit" \
            -ExpectedStatus "committed"
        Remove-ManagedFile -Path $HelperPath -Label "hosted uninstall helper"
        Send-InstallStage -Status "completed"
        $completed = $true
        return
    }
    if ((Test-Path -LiteralPath $TransactionPath -PathType Leaf) -and
        (Test-Path -LiteralPath $HelperPath -PathType Leaf)) {
        $recovered = Invoke-HostedUninstallTransaction \
            -Executable $HelperPath \
            -Action "uninstall-commit" \
            -ExpectedStatus "committed" \
            -AllowNotReady
        if ($null -ne $recovered) {
            Remove-ManagedFile -Path $HelperPath -Label "hosted uninstall helper"
            Send-InstallStage -Status "completed"
            $completed = $true
            return
        }
    }
    Assert-RegularManagedFile -Path $InstallPath -Label "installed ctx executable"
    $marker = Read-ManagedMarker
    $version = Get-InstalledVersion
    if ($DeleteData -and ($version.major -gt 1 -or ($version.major -eq 1 -and $version.minor -ge 5))) {
        Fail "-DeleteData was legacy derived-data cleanup and is retired in ctx 1.5. History and legacy data are preserved; rerun without -DeleteData"
    }
    if (-not ($DeleteData -or $KeepData) -and
        -not ($version.major -gt 1 -or ($version.major -eq 1 -and $version.minor -ge 5))) {
        Fail "uninstall requires an explicit data choice: -DeleteData or -KeepData"
    }
    $legacyUninstall = $version.major -eq 0 -and $version.minor -le 25
    if (-not $legacyUninstall) {
        $null = Invoke-HostedUninstallTransaction \
            -Executable $InstallPath \
            -Action "uninstall-prepare" \
            -ExpectedStatus "prepared" \
            -IncludeAttempt
    }
    Invoke-CoreDaemonTeardown -Version $version
    $marker = Read-ManagedMarker
    $proData = Invoke-ProLifecycle -Version $version
    $marker = Read-ManagedMarker
    $pathRemoved = Remove-InstallDirectoryFromUserPath
    if ($legacyUninstall) {
        Remove-ManagedFile -Path $InstallPath -Label "managed executable"
        Remove-ManagedFile -Path $MarkerPath -Label "managed install marker"
    } else {
        $null = Invoke-HostedUninstallTransaction \
            -Executable $HelperPath \
            -Action "uninstall-arm" \
            -ExpectedStatus "armed"
        $null = Invoke-HostedUninstallTransaction \
            -Executable $HelperPath \
            -Action "uninstall-commit" \
            -ExpectedStatus "committed"
        Remove-ManagedFile -Path $HelperPath -Label "hosted uninstall helper"
    }
    $result = [ordered]@{
        schema_version = 1
        uninstalled = $true
        platform = "windows-x64"
        version = $version.text
        canonical_history_preserved = $true
        install_path_removed_from_user_path = $pathRemoved
    }
    if ($version.major -lt 1 -or ($version.major -eq 1 -and $version.minor -lt 5)) {
        $result.local_pro_data = $proData
    }
    if ($Json) {
        $result | ConvertTo-Json -Compress
    } else {
        Write-Host "ctx uninstall complete. Local ctx history was preserved."
    }
    Send-InstallStage -Status "completed"
    $completed = $true
} finally {
    if (-not $completed) {
        Send-InstallStage -Status "failed"
    }
}
`;
}
