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
    [switch]$NoDaemon,
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

function Get-ManagedPairObjectName(
    [string]$ObjectKey,
    [string]$ExpectedChecksum,
    [string]$Label
) {
    if ($ObjectKey -cnotmatch '^sha256/([0-9a-fA-F]{64})/([A-Za-z0-9][A-Za-z0-9._+-]{0,127})$') {
        Fail "$Label object key is malformed: $ObjectKey"
    }
    $keyChecksum = $Matches[1].ToLowerInvariant()
    $objectName = $Matches[2]
    if ($ExpectedChecksum -notmatch '^[0-9a-fA-F]{64}$' -or
        $keyChecksum -cne $ExpectedChecksum.ToLowerInvariant()) {
        Fail "$Label object key does not match its checksum"
    }
    return $objectName
}

function ConvertTo-NativeArgument([string]$Value) {
    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
        return $Value
    }
    $quoted = New-Object System.Text.StringBuilder
    [void]$quoted.Append('"')
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes++
        } elseif ($character -eq '"') {
            [void]$quoted.Append(('\' * (($backslashes * 2) + 1)))
            [void]$quoted.Append('"')
            $backslashes = 0
        } else {
            [void]$quoted.Append(('\' * $backslashes))
            [void]$quoted.Append($character)
            $backslashes = 0
        }
    }
    [void]$quoted.Append(('\' * ($backslashes * 2)))
    [void]$quoted.Append('"')
    return $quoted.ToString()
}

function Invoke-ManagedPairApply([string]$Core, [string[]]$Arguments) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $Core
    $start.Arguments = ($Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $start
    try {
        [void]$process.Start()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult()
            Stderr = $stderr.GetAwaiter().GetResult()
        }
    } finally {
        $process.Dispose()
    }
}

function Expand-WindowsRuntimeArchive(
    [string]$ArchivePath,
    [string]$Destination,
    [string]$ExpectedVersion
) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
        [string[]]@(
            "LICENSE",
            "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
            "ThirdPartyNotices.txt",
            "VERSION_NUMBER",
            "GIT_COMMIT_ID",
            "lib/onnxruntime.dll",
            "lib/msvcp140.dll",
            "lib/msvcp140_1.dll",
            "lib/vcruntime140.dll",
            "lib/vcruntime140_1.dll"
        ),
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
    $pairEnvelopeArtifact = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_ENVELOPE_windows_x64" ""
    $pairCoreObjectKey = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_windows_x64" ""
    $pairCoreChecksum = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_windows_x64" ""
    $pairCompanionObjectKey = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_windows_x64" ""
    $pairCompanionChecksum = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_windows_x64" ""
    $artifact = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_ARTIFACT_windows_x64" ""
    $checksum = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_SHA256_windows_x64" ""
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
    $metadataTrust = "explicit-unsigned"
    if ([string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)) {
        if (-not [string]::IsNullOrWhiteSpace(
            $pairCoreObjectKey + $pairCoreChecksum +
            $pairCompanionObjectKey + $pairCompanionChecksum
        )) {
            Fail "managed-pair component metadata is present without an envelope"
        }
        if ([string]::IsNullOrWhiteSpace($artifact)) {
            Fail "metadata missing artifact for windows-x64"
        }
        if ($checksum -notmatch '^[0-9a-fA-F]{64}$') {
            Fail "checksum for windows-x64 is not a SHA-256 hex digest"
        }
        if ($checksum -eq "0000000000000000000000000000000000000000000000000000000000000000") {
            Fail "checksum for windows-x64 is a placeholder"
        }
        Assert-SafeArtifactName $artifact
    } else {
        Assert-SafeArtifactName $pairEnvelopeArtifact
        $pairCoreName = Get-ManagedPairObjectName `
            $pairCoreObjectKey $pairCoreChecksum "managed-pair Core"
        $pairCompanionName = Get-ManagedPairObjectName `
            $pairCompanionObjectKey $pairCompanionChecksum "managed-pair companion"
        if ($checksum -notmatch '^[0-9a-fA-F]{64}$' -or
            $pairCoreChecksum.ToLowerInvariant() -cne $checksum.ToLowerInvariant()) {
            Fail "managed-pair Core checksum differs from release metadata"
        }
        $metadataTrust = "signed-managed-pair-v1"
    }
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

    $artifactUrl = ""
    $downloadPath = ""
    $installPath = Join-Path $BinDir "ctx.exe"
    $installRoot = Split-Path -Parent ([System.IO.Path]::GetFullPath($BinDir))
    $companionPath = Join-Path $installRoot "libexec\ctx-pro.exe"
    $pairEnvelopePath = ""
    $companionDownloadPath = ""
    if (-not [string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)) {
        if ((Split-Path -Leaf ([System.IO.Path]::GetFullPath($BinDir))) -ine "bin") {
            Fail "signed managed-pair installation requires -BinDir to name <install-root>\bin"
        }
        $pairEnvelopePath = Join-Path $tempRoot $pairEnvelopeArtifact
        Get-ReleaseArtifact `
            -Url ($baseUrl.TrimEnd("/") + "/" + $pairEnvelopeArtifact) `
            -Name $pairEnvelopeArtifact `
            -Destination $pairEnvelopePath
        $artifact = $pairCoreName
        $checksum = $pairCoreChecksum
        $companionArtifact = $pairCompanionName
        $artifactUrl = $baseUrl.TrimEnd("/") + "/" + $pairCoreObjectKey
        $downloadPath = Join-Path $tempRoot $pairCoreName
        $companionDownloadPath = Join-Path $tempRoot $pairCompanionName
    } else {
        $artifactUrl = $baseUrl.TrimEnd("/") + "/" + $artifact
        $downloadPath = Join-Path $tempRoot $artifact
    }

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
    $setupNoDaemon = [bool]$NoDaemon -or $env:CTX_INSTALL_NO_DAEMON -eq "1"
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
    if (-not [string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)) {
        Write-Host "  companion: $companionPath"
        Write-Host "  pair metadata: signed target envelope"
    }
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

    if (-not [string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)) {
        Get-ReleaseArtifact -Url $artifactUrl -Name $pairCoreObjectKey -Destination $downloadPath
        Get-ReleaseArtifact `
            -Url ($baseUrl.TrimEnd("/") + "/" + $pairCompanionObjectKey) `
            -Name $pairCompanionObjectKey `
            -Destination $companionDownloadPath
        $actualChecksum = Get-Sha256 $downloadPath
        $actualCompanionChecksum = Get-Sha256 $companionDownloadPath
        if ($actualChecksum -cne $pairCoreChecksum.ToLowerInvariant()) {
            Fail "checksum mismatch for $artifact`: expected $pairCoreChecksum, got $actualChecksum"
        }
        if ($actualCompanionChecksum -cne $pairCompanionChecksum.ToLowerInvariant()) {
            Fail "checksum mismatch for $companionArtifact`: expected $pairCompanionChecksum, got $actualCompanionChecksum"
        }
    } else {
        Get-ReleaseArtifact -Url $artifactUrl -Name $artifact -Destination $downloadPath
        $actualChecksum = Get-Sha256 $downloadPath
        if ($actualChecksum -ne $checksum.ToLowerInvariant()) {
            Fail "checksum mismatch for $artifact`: expected $checksum, got $actualChecksum"
        }
    }

    $markerPath = "$installPath.install.json"
    $markerSourcePath = Join-Path $tempRoot "ctx.install.json"
    $markerManager = if ([string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)) {
        "ctx-explicit-metadata-installer"
    } else {
        "ctx-hosted-installer"
    }
    $marker = [ordered]@{
        schema_version = 1
        manager = $markerManager
        metadata_trust = $metadataTrust
        install_path = $installPath
        platform = $Platform
        channel = $channel
        version = $version
        sha256 = $actualChecksum
        staging_dogfood = (
            -not [string]::IsNullOrWhiteSpace($pairEnvelopeArtifact) -and
            $channel -ceq "staging"
        )
        metadata_url = $Metadata
        artifact_url = $artifactUrl
        source_commit = $sourceCommit
        published_at = $publishedAt
        installed_at = ([DateTime]::UtcNow.ToString("o"))
    }
    $markerJson = $marker | ConvertTo-Json -Depth 4
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText(
        $markerSourcePath,
        $markerJson + [Environment]::NewLine,
        $utf8NoBom
    )

    New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    if (-not [string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)) {
        $pairResult = Invoke-ManagedPairApply $downloadPath @(
            "--ctx-core-managed-pair-apply-v1", $installRoot, "-",
            $pairEnvelopePath, $downloadPath, $companionDownloadPath, $markerSourcePath)
        if ($pairResult.ExitCode -ne 0) {
            Fail ("candidate Core could not apply the signed managed pair: " + $pairResult.Stderr.Trim())
        }
        $expectedReceipt = '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}' + "`n"
        if ([Text.Encoding]::UTF8.GetByteCount($pairResult.Stdout) -ne 83 -or
            $pairResult.Stdout -cne $expectedReceipt -or $pairResult.Stderr.Length -ne 0) {
            Fail "candidate Core returned an invalid managed-pair apply receipt"
        }
    } else {
        Copy-Item -LiteralPath $downloadPath -Destination $installPath -Force
        Copy-Item -LiteralPath $markerSourcePath -Destination $markerPath -Force
    }
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
        $skillArgs = @("integrations", "install", "skill")
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
            Write-Warning "ctx integrations install skill failed after install; run $installPath integrations install skill to retry"
        }
    } else {
        Write-Host ""
        Write-Host "Agent skill skipped. Run $installPath integrations install skill to install it later."
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
        $setupArgs = @("setup")
        $setupArgs += @("--progress", $SetupProgress)
        if ($setupNoDaemon) {
            $setupArgs += "--no-daemon"
        }
        & $installPath @setupArgs
        if ($LASTEXITCODE -ne 0) {
            $setupStatus = $LASTEXITCODE
            $retryNoDaemon = if ($setupNoDaemon) { " --no-daemon" } else { "" }
            Write-Warning "ctx setup failed after install; run $installPath setup --progress $SetupProgress$retryNoDaemon to retry"
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
