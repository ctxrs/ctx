param(
    [Parameter(Mandatory = $true)]
    [string]$Metadata,
    [string]$ArtifactDir = "",
    [string]$Platform = "",
    [string]$BinDir = "",
    [string]$RuntimeDir = "",
    [switch]$NoRuntime,
    [switch]$NoModifyPath,
    [switch]$NoSetup,
    [switch]$NoSkill,
    [string[]]$SkillAgent = @(),
    [switch]$AllSkillAgents,
    [string]$SetupProgress = "",
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$expectedOnnxRuntimeVersion = "1.27.0"

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

function Get-ReleaseArtifact(
    [string]$Url,
    [string]$Name,
    [string]$Destination
) {
    if ([string]::IsNullOrWhiteSpace($ArtifactDir)) {
        if ($Url -notmatch '^https://') {
            Fail "refusing non-HTTPS artifact URL: $Url"
        }
        Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing
        return
    }

    $source = Join-Path $ArtifactDir $Name
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        Fail "local artifact is missing: $source"
    }
    $sourceItem = Get-Item -LiteralPath $source -Force
    if (($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail "local artifact must not be a symlink or reparse point: $source"
    }
    Copy-Item -LiteralPath $source -Destination $Destination -Force
}

function Expand-WindowsRuntimeArchive(
    [string]$ArchivePath,
    [string]$Destination,
    [string]$ExpectedVersion
) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
        [string[]]@("LICENSE", "ThirdPartyNotices.txt", "VERSION_NUMBER", "GIT_COMMIT_ID", "lib/onnxruntime.dll"),
        [System.StringComparer]::Ordinal
    )
    $expectedEntries = [System.Collections.Generic.HashSet[string]]::new($expectedFiles, [System.StringComparer]::Ordinal)
    [void]$expectedEntries.Add("lib")
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $entries = @{}
    [long]$totalLength = 0
    $archive = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        foreach ($entry in $archive.Entries) {
            $rawName = $entry.FullName
            if (
                [string]::IsNullOrEmpty($rawName) -or
                $rawName.Contains("\") -or
                $rawName.StartsWith("/", [System.StringComparison]::Ordinal) -or
                $rawName -match '^[A-Za-z]:'
            ) {
                Fail "unsafe runtime archive entry path: '$rawName'"
            }
            $isDirectory = $rawName.EndsWith("/", [System.StringComparison]::Ordinal)
            $name = if ($isDirectory) { $rawName.Substring(0, $rawName.Length - 1) } else { $rawName }
            $expectedRawName = if ($name -ceq "lib") { "lib/" } else { $name }
            if (
                $rawName -cne $expectedRawName -or
                -not $expectedEntries.Contains($name) -or
                -not $seen.Add($name)
            ) {
                Fail "unexpected, duplicate, or non-canonical runtime archive entry: '$rawName'"
            }

            $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF
            $fileType = $unixMode -band 0xF000
            if (($unixMode -band 0x0E00) -ne 0) {
                Fail "unsafe permission bits on runtime archive entry: '$rawName'"
            }
            if ($name -ceq "lib") {
                if (-not $isDirectory -or $fileType -ne 0x4000) {
                    Fail "runtime lib entry is not a directory"
                }
            } elseif ($isDirectory -or $fileType -ne 0x8000) {
                Fail "runtime archive entry is not a regular file: '$rawName'"
            }

            $totalLength += $entry.Length
            if ($totalLength -gt 1GB) {
                Fail "runtime archive expands beyond the 1 GiB safety limit"
            }
            $entries[$name] = $entry
        }

        if ($seen.Count -ne $expectedEntries.Count) {
            $missing = @($expectedEntries | Where-Object { -not $seen.Contains($_) })
            Fail "runtime archive entries do not exactly match the expected layout; missing: $($missing -join ', ')"
        }

        $versionStream = $entries["VERSION_NUMBER"].Open()
        try {
            $reader = [System.IO.StreamReader]::new($versionStream, [System.Text.UTF8Encoding]::new($false, $true))
            try {
                $versionText = $reader.ReadToEnd()
            } finally {
                $reader.Dispose()
            }
        } finally {
            $versionStream.Dispose()
        }
        if ($versionText -cne ($ExpectedVersion + "`n")) {
            Fail "runtime VERSION_NUMBER is not exactly $ExpectedVersion"
        }

        New-Item -ItemType Directory -Path (Join-Path $Destination "lib") -Force | Out-Null
        foreach ($name in $expectedFiles) {
            $target = Join-Path $Destination ($name.Replace("/", "\"))
            $sourceStream = $entries[$name].Open()
            try {
                $targetStream = [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
                try {
                    $sourceStream.CopyTo($targetStream)
                } finally {
                    $targetStream.Dispose()
                }
            } finally {
                $sourceStream.Dispose()
            }
        }
    } finally {
        $archive.Dispose()
    }
}

function Install-RuntimeAsset(
    [string]$ArtifactName,
    [string]$Checksum,
    [string]$RuntimeVersion,
    [string]$BaseUrl,
    [string]$TempRoot,
    [string]$DestinationRoot
) {
    Assert-SafeArtifactName $ArtifactName
    if ($Checksum -notmatch '^[0-9a-fA-F]{64}$') {
        Fail "checksum for ONNX Runtime $Platform is not a SHA-256 hex digest"
    }
    if ($Checksum -eq "0000000000000000000000000000000000000000000000000000000000000000") {
        Fail "checksum for ONNX Runtime $Platform is a placeholder"
    }
    if ([string]::IsNullOrWhiteSpace($DestinationRoot)) {
        Fail "-RuntimeDir cannot be empty when ONNX Runtime metadata is present"
    }

    $runtimeUrl = $BaseUrl.TrimEnd("/") + "/" + $ArtifactName
    $runtimeDownload = Join-Path $TempRoot $ArtifactName
    Get-ReleaseArtifact -Url $runtimeUrl -Name $ArtifactName -Destination $runtimeDownload

    $actualRuntimeChecksum = Get-Sha256 $runtimeDownload
    if ($actualRuntimeChecksum -ne $Checksum.ToLowerInvariant()) {
        Fail "checksum mismatch for $ArtifactName`: expected $Checksum, got $actualRuntimeChecksum"
    }

    $runtimeParent = Join-Path $DestinationRoot ("onnxruntime\" + $RuntimeVersion)
    $runtimePath = Join-Path $runtimeParent $Platform
    $tmpRuntimePath = "$runtimePath.tmp.$PID"
    Remove-Item -LiteralPath $tmpRuntimePath -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $tmpRuntimePath -Force | Out-Null

    if (-not $ArtifactName.EndsWith(".zip", [System.StringComparison]::OrdinalIgnoreCase)) {
        Fail "unsupported ONNX Runtime archive format for windows-x64: $ArtifactName"
    }
    Expand-WindowsRuntimeArchive -ArchivePath $runtimeDownload -Destination $tmpRuntimePath -ExpectedVersion $RuntimeVersion

    Remove-Item -LiteralPath $runtimePath -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $runtimeParent -Force | Out-Null
    Move-Item -LiteralPath $tmpRuntimePath -Destination $runtimePath -Force

    $manifestPath = Join-Path $runtimePath "ctx-runtime-install.json"
    $marker = [ordered]@{
        schema_version = 1
        manager = "ctx-explicit-metadata-installer"
        metadata_trust = "explicit-unsigned"
        runtime = "onnxruntime"
        platform = $Platform
        version = $RuntimeVersion
        sha256 = $actualRuntimeChecksum
        artifact_url = $runtimeUrl
        installed_at = ([DateTime]::UtcNow.ToString("o"))
    }
    $markerJson = $marker | ConvertTo-Json -Depth 4
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($manifestPath, $markerJson + [Environment]::NewLine, $utf8NoBom)
    Write-Host "Installed ONNX Runtime sidecar: $runtimePath"
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

if ([string]::IsNullOrWhiteSpace($RuntimeDir)) {
    $RuntimeDir = Join-Path $HOME ".ctx\runtime"
}
if (-not [string]::IsNullOrWhiteSpace($ArtifactDir)) {
    if (-not (Test-Path -LiteralPath $ArtifactDir -PathType Container)) {
        Fail "-ArtifactDir is not a directory: $ArtifactDir"
    }
    $ArtifactDir = (Resolve-Path -LiteralPath $ArtifactDir).Path
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
    $runtimeArtifact = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_windows_x64" ""
    $runtimeChecksum = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_ONNXRUNTIME_SHA256_windows_x64" ""
    $runtimeVersion = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_ONNXRUNTIME_VERSION" ""
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
    if (-not [string]::IsNullOrWhiteSpace($runtimeArtifact) -or -not [string]::IsNullOrWhiteSpace($runtimeChecksum)) {
        if ([string]::IsNullOrWhiteSpace($runtimeArtifact)) {
            Fail "metadata missing ONNX Runtime artifact for windows-x64"
        }
        if ([string]::IsNullOrWhiteSpace($runtimeChecksum)) {
            Fail "metadata missing ONNX Runtime checksum for windows-x64"
        }
        if ([string]::IsNullOrWhiteSpace($runtimeVersion)) {
            Fail "metadata missing CTX_RELEASE_ONNXRUNTIME_VERSION"
        }
        if ($runtimeVersion -ne $expectedOnnxRuntimeVersion) {
            Fail "unsupported ONNX Runtime version $runtimeVersion; expected $expectedOnnxRuntimeVersion"
        }
        Assert-SafeArtifactName $runtimeArtifact
    }

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
    $installRuntime = -not $NoRuntime -and $env:CTX_INSTALL_NO_RUNTIME -ne "1"
    if (-not $runSetup -and -not $explicitSkillRequest) {
        $runSkill = $false
    }
    $modifyPath = -not $NoModifyPath -and $env:CTX_INSTALL_NO_MODIFY_PATH -ne "1"

    if ($DryRun) {
        Write-Host "Dry run: would install ctx $version ($Platform)"
    } else {
        Write-Host "Installing ctx $version ($Platform)"
    }
    Write-Host "  binary: $installPath"
    if ($installRuntime -and -not [string]::IsNullOrWhiteSpace($runtimeArtifact)) {
        Write-Host "  onnxruntime: $(Join-Path $RuntimeDir ("onnxruntime\" + $runtimeVersion + "\" + $Platform))"
    } elseif (-not [string]::IsNullOrWhiteSpace($runtimeArtifact)) {
        Write-Host "  onnxruntime: skipped"
    } else {
        Write-Host "  onnxruntime: not present in metadata"
    }
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

    Get-ReleaseArtifact -Url $artifactUrl -Name $artifact -Destination $downloadPath

    $actualChecksum = Get-Sha256 $downloadPath
    if ($actualChecksum -ne $checksum.ToLowerInvariant()) {
        Fail "checksum mismatch for $artifact`: expected $checksum, got $actualChecksum"
    }

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    Copy-Item -LiteralPath $downloadPath -Destination $installPath -Force

    $markerPath = "$installPath.install.json"
    $marker = [ordered]@{
        schema_version = 1
        manager = "ctx-explicit-metadata-installer"
        metadata_trust = "explicit-unsigned"
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

    if ($installRuntime -and -not [string]::IsNullOrWhiteSpace($runtimeArtifact)) {
        Install-RuntimeAsset `
            -ArtifactName $runtimeArtifact `
            -Checksum $runtimeChecksum `
            -RuntimeVersion $runtimeVersion `
            -BaseUrl $baseUrl `
            -TempRoot $tempRoot `
            -DestinationRoot $RuntimeDir
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
