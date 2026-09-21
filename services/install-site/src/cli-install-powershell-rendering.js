export function renderCliInstallPowerShellRendering() {
  return `param(
    [string]$BinDir = "",
    [string]$Metadata = "",
    [switch]$NoModifyPath,
    [switch]$NoSetup,
    [switch]$NoDaemon,
    [Parameter(DontShow)] [switch]$ProTrial,
    [Parameter(DontShow)] [switch]$NoProTrial,
    [switch]$Semantic,
    [switch]$NoSkill,
    [string[]]$SkillAgent = @(),
    [switch]$AllSkillAgents,
    [string]$SetupProgress = "",
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    throw "install.ps1: $Message"
}

$script:styledOutput = $null -eq [Environment]::GetEnvironmentVariable(
    "NO_COLOR",
    "Process"
)
try {
    $script:styledOutput = $script:styledOutput -and -not [Console]::IsOutputRedirected
} catch {
    $script:styledOutput = $false
}

function Write-ReceiptItem([string]$Message) {
    if ($script:styledOutput) {
        Write-Host ([char]0x2713) -NoNewline -ForegroundColor Green
        Write-Host " $Message"
    } else {
        Write-Host $Message
    }
}

function Write-ReceiptWarning([string]$Message) {
    [Console]::Error.WriteLine("warning: $Message")
}

function Format-ReceiptCount([long]$Count) {
    return $Count.ToString(
        "N0",
        [System.Globalization.CultureInfo]::InvariantCulture
    )
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
    $normalized = $value.Trim().ToLowerInvariant()
    return $normalized -in @("0", "false", "no", "off")
}

function Test-CanonicalDaemonDisabled {
    $value = [Environment]::GetEnvironmentVariable("CTX_DAEMON_ENABLED", "Process")
    if ($null -eq $value) {
        return $false
    }
    $normalized = $value.Trim().Trim('"').ToLowerInvariant()
    return $normalized -in @("0", "false", "no", "off")
}

function Test-CiEnvironment {
    if ([string]::IsNullOrWhiteSpace($env:CI)) {
        return $false
    }
    return $env:CI.Trim().ToLowerInvariant() -in @("1", "true", "yes", "on")
}

function Test-JsonIntegerValue($Value) {
    return $Value -is [byte] -or
        $Value -is [sbyte] -or
        $Value -is [int16] -or
        $Value -is [uint16] -or
        $Value -is [int32] -or
        $Value -is [uint32] -or
        $Value -is [int64] -or
        $Value -is [uint64]
}

function Get-OptionalUnsignedCount($Json, [string]$Name) {
    $property = $Json.PSObject.Properties[$Name]
    if ($null -eq $property -or $null -eq $property.Value) {
        return [long]-1
    }
    if (-not (Test-JsonIntegerValue -Value $property.Value)) {
        throw "invalid setup receipt count: $Name"
    }
    try {
        $value = [Convert]::ToInt64($property.Value)
    } catch {
        throw "invalid setup receipt count: $Name"
    }
    if ($value -lt 0) {
        throw "invalid setup receipt count: $Name"
    }
    return $value
}

`;
}
