param(
    [Parameter(Mandatory = $true)]
    [string]$Metadata,
    [string]$Platform = "",
    [string]$BinDir = "",
    [switch]$NoModifyPath,
    [switch]$NoSetup,
    [switch]$NoSkill,
    [string[]]$SkillAgent = @(),
    [switch]$AllSkillAgents,
    [string]$SetupProgress = "",
    [switch]$EnableAnalytics,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Local development and explicit-metadata testing helper. The production hosted
# installer is https://cli.ctx.rs/install.ps1 and verifies detached metadata
# signatures before trusting artifact URLs or checksums.

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

function Get-MetadataValueOrDefault([hashtable]$Values, [string]$Key, [string]$Default) {
    if (-not $Values.ContainsKey($Key)) {
        return $Default
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

function Normalize-PathEntry([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return ""
    }
    return $Path.Trim().Trim('"').TrimEnd("\", "/")
}

function Test-PathContainsDirectory([string]$PathValue, [string]$Directory) {
    $needle = Normalize-PathEntry $Directory
    if ([string]::IsNullOrWhiteSpace($needle)) {
        return $false
    }
    foreach ($entry in ($PathValue -split [regex]::Escape([System.IO.Path]::PathSeparator))) {
        if ((Normalize-PathEntry $entry).Equals($needle, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Format-CurrentPathCommand([string]$Directory) {
    $escaped = $Directory.Replace('`', '``').Replace('"', '`"')
    return "`$env:Path = `"$escaped;`$env:Path`""
}

function Write-CurrentPathCommand([string]$Directory) {
    Write-Host "For this PowerShell session, run:"
    Write-Host ("  " + (Format-CurrentPathCommand $Directory))
}

function Add-InstallDirToPathIfNeeded([string]$Directory, [bool]$ModifyPath) {
    $dir = $Directory.TrimEnd("\", "/")
    if (Test-PathContainsDirectory -PathValue $env:Path -Directory $dir) {
        return
    }

    if (-not $ModifyPath) {
        Write-Host ""
        Write-Host "$dir is not on PATH; user PATH update skipped."
        Write-CurrentPathCommand $dir
        return
    }

    if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_PATH)) {
        Add-Content -LiteralPath $env:GITHUB_PATH -Value $dir
        if (-not (Test-PathContainsDirectory -PathValue $env:Path -Directory $dir)) {
            $env:Path = "$dir$([System.IO.Path]::PathSeparator)$env:Path"
        }
        Write-Host ""
        Write-Host "Added $dir to GITHUB_PATH for later GitHub Actions steps."
        return
    }

    if ($env:CI -eq "1" -or $env:CI -eq "true") {
        $env:Path = "$dir$([System.IO.Path]::PathSeparator)$env:Path"
        Write-Host ""
        Write-Host "$dir is not on PATH; CI detected, not editing the user PATH."
        Write-CurrentPathCommand $dir
        return
    }

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    Write-Host ""
    if (Test-PathContainsDirectory -PathValue $userPath -Directory $dir) {
        Write-Host "Found existing user PATH setup for $dir."
    } else {
        if ([string]::IsNullOrWhiteSpace($userPath)) {
            $newUserPath = $dir
        } else {
            $newUserPath = "$dir$([System.IO.Path]::PathSeparator)$userPath"
        }
        try {
            [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
            Write-Host "Added $dir to the user PATH."
        } catch {
            Write-Warning "could not update the user PATH: $($_.Exception.Message)"
        }
    }

    $updatedCurrentPath = $false
    if (-not (Test-PathContainsDirectory -PathValue $env:Path -Directory $dir)) {
        $env:Path = "$dir$([System.IO.Path]::PathSeparator)$env:Path"
        $updatedCurrentPath = $true
    }
    if ($updatedCurrentPath) {
        Write-Host "$dir was not on PATH at startup; this PowerShell session has been updated."
    }
    Write-Host "Open a new PowerShell window or run:"
    Write-Host ("  " + (Format-CurrentPathCommand $dir))
    Write-Host "Then verify with:"
    Write-Host "  ctx status"
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
    $channel = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_CHANNEL" "stable"
    $sourceCommit = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_SOURCE_COMMIT" ""
    $publishedAt = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_PUBLISHED_AT" ""

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

    $skillAgents = @()
    foreach ($agent in $SkillAgent) {
        $trimmed = $agent.Trim()
        if (-not [string]::IsNullOrWhiteSpace($trimmed)) {
            $skillAgents += $trimmed
        }
    }
    $allSkillAgentsRequested = [bool]$AllSkillAgents
    $explicitSkillRequest = $allSkillAgentsRequested -or $skillAgents.Count -gt 0

    if ($env:CTX_INSTALL_ALL_SKILL_AGENTS -eq "1") {
        $allSkillAgentsRequested = $true
        $explicitSkillRequest = $true
    }
    if (-not [string]::IsNullOrWhiteSpace($env:CTX_INSTALL_SKILL_AGENTS)) {
        foreach ($agent in ($env:CTX_INSTALL_SKILL_AGENTS -split ",")) {
            $trimmed = $agent.Trim()
            if (-not [string]::IsNullOrWhiteSpace($trimmed)) {
                $skillAgents += $trimmed
                $explicitSkillRequest = $true
            }
        }
    }

    $noSkillRequested = [bool]$NoSkill -or $env:CTX_INSTALL_NO_SKILL -eq "1"
    if ($noSkillRequested -and $explicitSkillRequest) {
        Fail "cannot combine -NoSkill or CTX_INSTALL_NO_SKILL=1 with skill agent options"
    }
    if ($allSkillAgentsRequested -and $skillAgents.Count -gt 0) {
        Fail "cannot combine -AllSkillAgents with -SkillAgent or CTX_INSTALL_SKILL_AGENTS"
    }

    $runSetup = -not $NoSetup -and $env:CTX_INSTALL_NO_SETUP -ne "1"
    $runSkill = -not $noSkillRequested
    if (-not $runSetup -and -not $explicitSkillRequest) {
        $runSkill = $false
    }
    $modifyPath = -not $NoModifyPath -and $env:CTX_INSTALL_NO_MODIFY_PATH -ne "1"
    $optInAnalytics = $EnableAnalytics -or $env:CTX_INSTALL_ENABLE_ANALYTICS -eq "1"

    # Disclose before doing anything: what stays local, what (optionally) does not.
    Write-Host "Privacy:"
    Write-Host "  history index: built and searched locally; transcript content never leaves this machine"
    Write-Host "  usage analytics: opt-in, disabled by default (coarse product metadata only when enabled)"
    Write-Host "  auto-upgrade: managed installs check for signed releases daily (ctx upgrade disable to stop)"

    if ($DryRun) {
        Write-Host "Dry run: would install ctx $version ($Platform)"
    } else {
        Write-Host "Installing ctx $version ($Platform)"
    }
    Write-Host "  binary: $installPath"
    if ($runSkill) {
        if ($allSkillAgentsRequested) {
            Write-Host "  skill: all supported agents"
        } elseif ($skillAgents.Count -gt 0) {
            Write-Host ("  skill: " + ($skillAgents -join ","))
        } else {
            Write-Host "  skill: universal + detected agent folders"
        }
    } else {
        Write-Host "  skill: skipped"
    }
    if ($runSetup) {
        Write-Host "  history: index discovered sessions"
    } else {
        Write-Host "  history: skipped"
    }
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

    $markerPath = "$installPath.install.json"
    $marker = [ordered]@{
        schema_version = 1
        manager = "ctx-hosted-installer"
        install_path = $installPath
        platform = $Platform
        channel = $channel
        version = $version
        sha256 = $actualChecksum
        metadata_url = $Metadata
        artifact_url = $artifactUrl
        source_commit = $sourceCommit
        published_at = $publishedAt
        installed_at = ([DateTime]::UtcNow.ToString("o"))
    }
    $markerJson = $marker | ConvertTo-Json -Depth 4
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($markerPath, $markerJson + [Environment]::NewLine, $utf8NoBom)
    Write-Host ""
    Write-Host "Installed ctx binary."

    if ($optInAnalytics) {
        & $installPath analytics enable
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "could not record the analytics opt-in; run $installPath analytics enable to retry"
        }
    } else {
        Write-Host "Anonymous usage analytics: disabled (opt in later with $installPath analytics enable)"
    }

    if ($runSkill) {
        $skillArgs = @("integrations", "install", "skills")
        if ($allSkillAgentsRequested) {
            $skillArgs += "--all-agents"
        } else {
            foreach ($agent in $skillAgents) {
                $skillArgs += @("--agent", $agent)
            }
        }
        Write-Host ""
        & $installPath @skillArgs
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "ctx integrations install skills failed after install; run $installPath integrations install skills to retry"
        }
    } else {
        Write-Host ""
        Write-Host "Agent skill skipped. Run $installPath integrations install skills to install it later."
    }

    $setupStatus = 0
    if ($runSetup) {
        if ([string]::IsNullOrWhiteSpace($SetupProgress)) {
            if ([string]::IsNullOrWhiteSpace($env:CTX_SETUP_PROGRESS)) {
                $SetupProgress = "auto"
            } else {
                $SetupProgress = $env:CTX_SETUP_PROGRESS
            }
        }
        Write-Host ""
        Write-Host "Indexing local agent history..."
        & $installPath setup --progress $SetupProgress
        if ($LASTEXITCODE -ne 0) {
            $setupStatus = $LASTEXITCODE
            Write-Warning "ctx setup failed after install; run $installPath setup --progress $SetupProgress to retry"
        }
    } else {
        Write-Host ""
        Write-Host "Setup skipped. Run $installPath setup to index local history."
    }

    Add-InstallDirToPathIfNeeded -Directory $BinDir -ModifyPath $modifyPath

    if ($setupStatus -ne 0) {
        exit $setupStatus
    }
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
