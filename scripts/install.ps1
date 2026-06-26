param(
    [Parameter(Mandatory = $true)]
    [string]$Metadata,
    [string]$Platform = "",
    [string]$BinDir = "",
    [switch]$NoSetup,
    [string]$SetupProgress = "",
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    throw "install.ps1: $Message"
}

function Detect-Platform {
    if (-not [System.Environment]::Is64BitOperatingSystem) {
        Fail "only 64-bit Windows hosts are supported by this installer"
    }
    return "windows-x64"
}

function Read-Metadata([string]$Source, [string]$Destination) {
    if ($Source -match '^https://') {
        Invoke-WebRequest -Uri $Source -OutFile $Destination -UseBasicParsing
        return
    }
    if ($Source -match '^http://') {
        Fail "refusing insecure metadata URL: $Source"
    }
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        Fail "metadata file not found: $Source"
    }
    Copy-Item -LiteralPath $Source -Destination $Destination
}

function Get-MetadataValue([hashtable]$Values, [string]$Key) {
    if (-not $Values.ContainsKey($Key)) {
        Fail "metadata missing $Key"
    }
    return [string]$Values[$Key]
}

function Assert-SafeArtifactName([string]$Value) {
    if ($Value.Contains("..") -or $Value.Contains("/") -or $Value.Contains("\")) {
        Fail "unsafe artifact name: $Value"
    }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

if ([string]::IsNullOrWhiteSpace($Platform)) {
    $Platform = Detect-Platform
}

if ($Platform -ne "windows-x64") {
    Fail "unsupported platform for install.ps1: $Platform"
}

if ([string]::IsNullOrWhiteSpace($BinDir)) {
    $BinDir = Join-Path $HOME ".local\bin"
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-install-" + [System.Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
    $metadataFile = Join-Path $tempRoot "metadata.env"
    Read-Metadata -Source $Metadata -Destination $metadataFile

    $metadataText = Get-Content -LiteralPath $metadataFile | Where-Object {
        $_ -notmatch '^\s*#' -and $_ -match '='
    }
    $metadataValues = ConvertFrom-StringData -StringData ($metadataText -join "`n")

    $schemaVersion = Get-MetadataValue $metadataValues "CTX_RELEASE_SCHEMA_VERSION"
    $version = Get-MetadataValue $metadataValues "CTX_RELEASE_VERSION"
    $baseUrl = Get-MetadataValue $metadataValues "CTX_RELEASE_BASE_URL"
    $artifact = Get-MetadataValue $metadataValues "CTX_RELEASE_ARTIFACT_windows_x64"
    $checksum = Get-MetadataValue $metadataValues "CTX_RELEASE_SHA256_windows_x64"

    if ($schemaVersion -ne "1") {
        Fail "unsupported metadata schema: $schemaVersion"
    }
    if ($baseUrl -notmatch '^https://') {
        Fail "metadata base URL must be HTTPS"
    }
    if ($checksum -notmatch '^[0-9a-fA-F]{64}$') {
        Fail "checksum for windows-x64 is not a SHA-256 hex digest"
    }
    if ($checksum -eq "0000000000000000000000000000000000000000000000000000000000000000") {
        Fail "checksum for windows-x64 is a placeholder"
    }
    Assert-SafeArtifactName $artifact

    $artifactUrl = $baseUrl.TrimEnd("/") + "/" + $artifact
    $downloadPath = Join-Path $tempRoot $artifact
    $installPath = Join-Path $BinDir "ctx.exe"

    Write-Host "ctx install plan: version=$version platform=$Platform artifact=$artifact bin=$installPath"
    if ($DryRun) {
        exit 0
    }

    if ($artifactUrl -notmatch '^https://') {
        Fail "refusing non-HTTPS artifact URL: $artifactUrl"
    }
    Invoke-WebRequest -Uri $artifactUrl -OutFile $downloadPath -UseBasicParsing

    $actualChecksum = Get-Sha256 $downloadPath
    if ($actualChecksum -ne $checksum.ToLowerInvariant()) {
        Fail "checksum mismatch for $artifact`: expected $checksum, got $actualChecksum"
    }

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    Copy-Item -LiteralPath $downloadPath -Destination $installPath -Force
    Write-Host "installed ctx to $installPath"

    $runSetup = -not $NoSetup -and $env:CTX_INSTALL_NO_SETUP -ne "1"
    if ($runSetup) {
        if ([string]::IsNullOrWhiteSpace($SetupProgress)) {
            if ([string]::IsNullOrWhiteSpace($env:CTX_SETUP_PROGRESS)) {
                $SetupProgress = "auto"
            } else {
                $SetupProgress = $env:CTX_SETUP_PROGRESS
            }
        }
        Write-Host "running ctx setup to index local history (pass -NoSetup or set CTX_INSTALL_NO_SETUP=1 to skip next time)"
        & $installPath setup --progress $SetupProgress
        if ($LASTEXITCODE -ne 0) {
            Fail "ctx setup failed after install"
        }
    } else {
        Write-Host "setup skipped; run $installPath setup to index local history"
    }
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
