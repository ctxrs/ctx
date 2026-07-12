param(
    [string]$Ctx = "ctx.exe",
    [string]$RuntimeArchive = "",
    [string]$RuntimePlatform = "",
    [string]$DataRoot = "",
    [string]$ProofOutput = "",
    [int]$TimeoutSeconds = 900,
    [switch]$RequireAuthoritative,
    [switch]$KeepRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This smoke must run on Windows"
}

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class CtxWindowsNativeArchitecture
{
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool IsWow64Process2(
        IntPtr process,
        out ushort processMachine,
        out ushort nativeMachine);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    public static string Probe()
    {
        try
        {
            ushort processMachine;
            ushort nativeMachine;
            if (!IsWow64Process2(GetCurrentProcess(), out processMachine, out nativeMachine))
            {
                return "error";
            }
            return processMachine.ToString("X4") + ":" + nativeMachine.ToString("X4");
        }
        catch (EntryPointNotFoundException)
        {
            return "unavailable";
        }
    }
}
"@

if ($TimeoutSeconds -lt 30) {
    throw "TimeoutSeconds must be at least 30"
}
if ([string]::IsNullOrWhiteSpace($Ctx)) {
    throw "Ctx cannot be empty"
}
if (-not [string]::IsNullOrEmpty($ProofOutput) -and [string]::IsNullOrWhiteSpace($ProofOutput)) {
    throw "ProofOutput cannot be whitespace-only"
}
if ([string]::IsNullOrWhiteSpace($RuntimeArchive)) {
    throw "RuntimeArchive is required"
}
if ($RuntimePlatform -ne "windows-x64") {
    throw "RuntimePlatform must be windows-x64"
}

$runtimeVersion = "1.27.0"
$expectedRuntimeAsset = "ctx-onnxruntime-windows-x64.zip"
if ([System.IO.Path]::GetFileName($RuntimeArchive) -ne $expectedRuntimeAsset) {
    throw "RuntimeArchive for windows-x64 must be named $expectedRuntimeAsset"
}
$runtimeArchivePath = (Resolve-Path -LiteralPath $RuntimeArchive).Path
$runtimeShaPath = "$runtimeArchivePath.sha256"
if (-not (Test-Path -LiteralPath $runtimeShaPath -PathType Leaf)) {
    throw "Runtime archive checksum not found: $runtimeShaPath"
}
$expectedRuntimeSha = ([System.IO.File]::ReadAllText($runtimeShaPath)).Trim()
if ($expectedRuntimeSha -notmatch '^[0-9a-fA-F]{64}$') {
    throw "Runtime archive checksum is not a SHA-256 digest: $runtimeShaPath"
}
$actualRuntimeSha = (Get-FileHash -Algorithm SHA256 -LiteralPath $runtimeArchivePath).Hash.ToLowerInvariant()
if ($actualRuntimeSha -ne $expectedRuntimeSha.ToLowerInvariant()) {
    throw "Runtime archive checksum mismatch: expected $expectedRuntimeSha, got $actualRuntimeSha"
}

function Assert-WindowsRuntimeArchive {
    param(
        [string]$ArchivePath,
        [string]$ExpectedVersion
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    @(
        "LICENSE",
        "ThirdPartyNotices.txt",
        "VERSION_NUMBER",
        "GIT_COMMIT_ID",
        "lib/onnxruntime.dll"
    ) | ForEach-Object { [void]$expectedFiles.Add($_) }
    $seenFiles = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $seenDirectories = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $versionEntry = $null
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
                throw "Unsafe runtime archive entry path: '$rawName'"
            }

            $isDirectory = $rawName.EndsWith("/", [System.StringComparison]::Ordinal)
            $canonicalName = if ($isDirectory) {
                $rawName.Substring(0, $rawName.Length - 1)
            } else {
                $rawName
            }
            $segments = $canonicalName.Split(
                [char[]]@('/'),
                [System.StringSplitOptions]::None
            )
            if (
                [string]::IsNullOrEmpty($canonicalName) -or
                @($segments | Where-Object { $_ -eq "" -or $_ -eq "." -or $_ -eq ".." }).Count -gt 0
            ) {
                throw "Unsafe runtime archive entry path: '$rawName'"
            }

            $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF
            if (($unixMode -band 0xF000) -eq 0xA000) {
                throw "Runtime archive contains a symbolic link entry: '$rawName'"
            }

            if ($isDirectory) {
                if ($canonicalName -cne "lib" -or -not $seenDirectories.Add($canonicalName)) {
                    throw "Unexpected or duplicate runtime archive directory entry: '$rawName'"
                }
                continue
            }
            if (-not $expectedFiles.Contains($canonicalName)) {
                throw "Unexpected runtime archive entry: '$rawName'"
            }
            if (-not $seenFiles.Add($canonicalName)) {
                throw "Duplicate runtime archive entry: '$rawName'"
            }
            if ($canonicalName -ceq "VERSION_NUMBER") {
                $versionEntry = $entry
            }
        }

        $missing = @($expectedFiles | Where-Object { -not $seenFiles.Contains($_) })
        if ($missing.Count -gt 0 -or $seenFiles.Count -ne $expectedFiles.Count) {
            throw "Runtime archive entries do not match the expected files; missing: $($missing -join ', ')"
        }
        if ($null -eq $versionEntry) {
            throw "Runtime archive is missing VERSION_NUMBER"
        }

        $versionStream = $versionEntry.Open()
        try {
            $memory = [System.IO.MemoryStream]::new()
            try {
                $versionStream.CopyTo($memory)
                $versionText = [System.Text.UTF8Encoding]::new($false, $true).GetString($memory.ToArray())
            } finally {
                $memory.Dispose()
            }
        } finally {
            $versionStream.Dispose()
        }
        if ($versionText -cne ($ExpectedVersion + "`n")) {
            throw "Runtime archive VERSION_NUMBER is not exactly $ExpectedVersion"
        }
    } finally {
        $archive.Dispose()
    }
}

Assert-WindowsRuntimeArchive -ArchivePath $runtimeArchivePath -ExpectedVersion $runtimeVersion

$ctxCommand = Get-Command -Name $Ctx -CommandType Application -ErrorAction Stop
$Ctx = $ctxCommand.Source

function New-UniqueRunRoot {
    param([string]$Parent)

    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        $candidate = Join-Path $Parent ("ctx-semantic-smoke-" + [System.Guid]::NewGuid().ToString("n"))
        try {
            return (New-Item -ItemType Directory -Path $candidate -ErrorAction Stop).FullName
        } catch {
            if (Test-Path -LiteralPath $candidate) {
                continue
            }
            throw
        }
    }
    throw "Could not create a unique semantic smoke run root under $Parent"
}

$environmentVariableNames = @(
    "USERPROFILE", "HOME", "LOCALAPPDATA", "APPDATA", "XDG_CACHE_HOME", "XDG_CONFIG_HOME",
    "CTX_DATA_ROOT",
    "CTX_DAEMON_ENABLED", "CTX_DAEMON_OFF", "CTX_DISABLE_DAEMON",
    "CTX_DAEMON_AUTOSTART_OFF", "CTX_DAEMON_AUTOSTART_EXE", "CTX_DAEMON_BACKGROUND_CHILD",
    "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
    "CTX_SEARCH_SEMANTIC", "CTX_DISABLE_SEMANTIC_SEARCH", "CTX_SEMANTIC_WORKER_OFF",
    "CTX_SEMANTIC_WORKER_MAX_CHUNKS", "CTX_SEMANTIC_WORKER_MAX_SECONDS",
    "CTX_SEMANTIC_THREADS", "CTX_SEMANTIC_EMBED_BATCH",
    "CTX_ANALYTICS_ENABLED", "CTX_ANALYTICS_OFF", "CTX_DISABLE_ANALYTICS",
    "CTX_ANALYTICS_ENDPOINT", "CTX_ANALYTICS_DRY_RUN", "CTX_ANALYTICS_DEBUG",
    "CTX_UPGRADE_OFF", "CTX_DISABLE_AUTO_UPGRADE", "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL", "CTX_CHANNEL", "CTX_FUNCTIONS_BASE",
    "CTX_UPGRADE_INTERVAL_SECONDS", "CTX_UPGRADE_TARGET", "CTX_UPGRADE_BACKGROUND_CHILD",
    "CTX_SEMANTIC_CACHE_DIR", "FASTEMBED_CACHE_DIR", "HF_HOME", "HF_HUB_CACHE",
    "HUGGINGFACE_HUB_CACHE", "TRANSFORMERS_CACHE",
    "CTX_RUNTIME_DIR", "CTX_ONNXRUNTIME_DYLIB", "ORT_DYLIB_PATH",
    "CTX_ONNXRUNTIME_DIR", "CTX_ONNXRUNTIME_CACHE_DIR",
    "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES",
    "DYLD_FORCE_FLAT_NAMESPACE", "DYLD_FALLBACK_LIBRARY_PATH", "PATH"
) | Select-Object -Unique
$savedEnvironment = @{}
foreach ($name in $environmentVariableNames) {
    $savedEnvironment[$name] = [System.Environment]::GetEnvironmentVariable(
        $name,
        [System.EnvironmentVariableTarget]::Process
    )
}

function Set-ProcessEnvironmentVariable {
    param(
        [string]$Name,
        [AllowNull()][string]$Value
    )
    [System.Environment]::SetEnvironmentVariable(
        $Name,
        $Value,
        [System.EnvironmentVariableTarget]::Process
    )
}

$runRoot = ""
$daemon = $null

function Invoke-Ctx {
    param([string[]]$Args)
    & $Ctx --data-root $DataRoot @Args
}

function Read-OwnedDaemonStatus {
    param([int]$ExpectedPid)

    $statusLines = @()
    try {
        $statusLines = @(Invoke-Ctx -Args @("daemon", "status", "--json") 2>&1)
        $statusExitCode = $LASTEXITCODE
    } catch {
        return [PSCustomObject]@{
            Ready = $false
            Text = ($statusLines -join [Environment]::NewLine)
            Error = $_.Exception.Message
            Json = $null
        }
    }
    $statusText = $statusLines -join [Environment]::NewLine
    if ($statusExitCode -ne 0) {
        return [PSCustomObject]@{
            Ready = $false
            Text = $statusText
            Error = "ctx daemon status exited with $statusExitCode"
            Json = $null
        }
    }
    try {
        $statusJson = $statusText | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw "ctx daemon status returned invalid JSON: $($_.Exception.Message)"
    }
    $daemonProperty = $statusJson.PSObject.Properties["daemon"]
    if ($null -eq $daemonProperty -or $null -eq $daemonProperty.Value) {
        throw "ctx daemon status JSON is missing daemon"
    }
    $daemonStatus = $daemonProperty.Value
    $pidProperty = $daemonStatus.PSObject.Properties["pid"]
    if ($null -ne $pidProperty -and $null -ne $pidProperty.Value) {
        $reportedPid = [long]$pidProperty.Value
        if ($reportedPid -ne $ExpectedPid) {
            throw "ctx daemon status PID mismatch: expected $ExpectedPid, got $reportedPid"
        }
    } else {
        $reportedPid = $null
    }
    $statusProperty = $daemonStatus.PSObject.Properties["status"]
    $runningProperty = $daemonStatus.PSObject.Properties["running"]
    $ready = (
        $null -ne $statusProperty -and $statusProperty.Value -ceq "running" -and
        $null -ne $runningProperty -and $runningProperty.Value -eq $true -and
        $reportedPid -eq $ExpectedPid
    )
    return [PSCustomObject]@{
        Ready = $ready
        Text = $statusText
        Error = ""
        Json = $statusJson
    }
}

try {
    if ([string]::IsNullOrWhiteSpace($DataRoot)) {
        $dataRootParent = [System.IO.Path]::GetTempPath()
    } else {
        if (Test-Path -LiteralPath $DataRoot -PathType Leaf) {
            throw "DataRoot parent is a file: $DataRoot"
        }
        New-Item -ItemType Directory -Path $DataRoot -Force | Out-Null
        $dataRootParent = (Resolve-Path -LiteralPath $DataRoot).Path
    }
    $runRoot = New-UniqueRunRoot -Parent $dataRootParent
    $DataRoot = Join-Path $runRoot "data"
    New-Item -ItemType Directory -Path $DataRoot | Out-Null
    $DataRoot = [System.IO.Path]::GetFullPath($DataRoot)

    $fixtureDir = Join-Path $DataRoot "smoke-fixture"
    $fixturePath = Join-Path $fixtureDir "history.jsonl"
    $smokeHome = Join-Path $DataRoot "home"
    $smokeCache = Join-Path $DataRoot "cache"
    $smokeConfig = Join-Path $DataRoot "config-home"
    $smokeLocalAppData = Join-Path $DataRoot "local-app-data"
    $smokeAppData = Join-Path $DataRoot "app-data"
    $semanticCache = Join-Path $DataRoot "semantic-cache"
    New-Item -ItemType Directory -Path $fixtureDir -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeHome -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeCache -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeConfig -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeLocalAppData -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeAppData -Force | Out-Null
    New-Item -ItemType Directory -Path $semanticCache -Force | Out-Null

    Write-Host "ctx semantic smoke: run_root=$runRoot"
    Write-Host "ctx semantic smoke: data_root=$DataRoot"

    $runtimeRoot = Join-Path $DataRoot "runtime"
    $runtimeInstallDir = Join-Path $runtimeRoot ("onnxruntime\" + $runtimeVersion + "\" + $RuntimePlatform)
    $releaseArtifactDir = Join-Path $runRoot "release-artifacts"
    $installBinDir = Join-Path $runRoot "installed\bin"
    $releaseMetadata = Join-Path $runRoot "release-metadata.env"
    New-Item -ItemType Directory -Path $releaseArtifactDir -Force | Out-Null
    New-Item -ItemType Directory -Path $installBinDir -Force | Out-Null

    $versionLine = (& $Ctx --version | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or $versionLine -notmatch '^ctx\s+(\S+)') {
        throw "Could not determine ctx version from $Ctx"
    }
    $ctxVersion = $Matches[1]
    $releaseBinary = "ctx-windows-x64.exe"
    Copy-Item -LiteralPath $Ctx -Destination (Join-Path $releaseArtifactDir $releaseBinary) -Force
    Copy-Item -LiteralPath $runtimeArchivePath -Destination (Join-Path $releaseArtifactDir $expectedRuntimeAsset) -Force
    Copy-Item -LiteralPath $runtimeShaPath -Destination (Join-Path $releaseArtifactDir "$expectedRuntimeAsset.sha256") -Force
    $binarySha = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $releaseArtifactDir $releaseBinary)).Hash.ToLowerInvariant()
    $metadataLines = @(
        "CTX_RELEASE_SCHEMA_VERSION=1",
        "CTX_RELEASE_VERSION=$ctxVersion",
        "CTX_RELEASE_BASE_URL=https://release-smoke.invalid",
        "CTX_RELEASE_ARTIFACT_windows_x64=$releaseBinary",
        "CTX_RELEASE_SHA256_windows_x64=$binarySha",
        "CTX_RELEASE_ONNXRUNTIME_VERSION=$runtimeVersion",
        "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_windows_x64=$expectedRuntimeAsset",
        "CTX_RELEASE_ONNXRUNTIME_SHA256_windows_x64=$actualRuntimeSha"
    )
    [System.IO.File]::WriteAllLines($releaseMetadata, $metadataLines, [System.Text.UTF8Encoding]::new($false))

    & (Join-Path $PSScriptRoot "install.ps1") `
        -Metadata $releaseMetadata `
        -ArtifactDir $releaseArtifactDir `
        -Platform $RuntimePlatform `
        -BinDir $installBinDir `
        -RuntimeDir $runtimeRoot `
        -NoSetup -NoSkill -NoModifyPath | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Explicit-metadata installer failed with status $LASTEXITCODE"
    }

    $Ctx = Join-Path $installBinDir "ctx.exe"
    $runtimeDylib = Join-Path $runtimeInstallDir "lib\onnxruntime.dll"
    if (
        -not (Test-Path -LiteralPath $Ctx -PathType Leaf) -or
        -not (Test-Path -LiteralPath $runtimeDylib -PathType Leaf)
    ) {
        throw "Explicit-metadata installer did not create the expected binary/runtime layout"
    }
    $runtimeDylib = [System.IO.Path]::GetFullPath($runtimeDylib)
    $binaryMarker = Get-Content -LiteralPath "$Ctx.install.json" -Raw | ConvertFrom-Json
    $runtimeMarker = Get-Content -LiteralPath (Join-Path $runtimeInstallDir "ctx-runtime-install.json") -Raw | ConvertFrom-Json
    if (
        $binaryMarker.manager -cne "ctx-explicit-metadata-installer" -or
        $binaryMarker.metadata_trust -cne "explicit-unsigned" -or
        $runtimeMarker.manager -cne "ctx-explicit-metadata-installer" -or
        $runtimeMarker.metadata_trust -cne "explicit-unsigned"
    ) {
        throw "Explicit-metadata install provenance marker is missing or incorrect"
    }

    foreach ($name in $environmentVariableNames) {
        Set-ProcessEnvironmentVariable -Name $name -Value $null
    }
    Set-ProcessEnvironmentVariable -Name "USERPROFILE" -Value $smokeHome
    Set-ProcessEnvironmentVariable -Name "HOME" -Value $smokeHome
    Set-ProcessEnvironmentVariable -Name "LOCALAPPDATA" -Value $smokeLocalAppData
    Set-ProcessEnvironmentVariable -Name "APPDATA" -Value $smokeAppData
    Set-ProcessEnvironmentVariable -Name "XDG_CACHE_HOME" -Value $smokeCache
    Set-ProcessEnvironmentVariable -Name "XDG_CONFIG_HOME" -Value $smokeConfig
    Set-ProcessEnvironmentVariable -Name "HF_HOME" -Value $semanticCache
    Set-ProcessEnvironmentVariable -Name "HF_HUB_CACHE" -Value $semanticCache
    Set-ProcessEnvironmentVariable -Name "FASTEMBED_CACHE_DIR" -Value $semanticCache
    Set-ProcessEnvironmentVariable -Name "CTX_SEMANTIC_CACHE_DIR" -Value $semanticCache
    Set-ProcessEnvironmentVariable -Name "CTX_ANALYTICS_OFF" -Value "1"
    Set-ProcessEnvironmentVariable -Name "CTX_DISABLE_ANALYTICS" -Value "1"
    Set-ProcessEnvironmentVariable -Name "CTX_UPGRADE_OFF" -Value "1"
    Set-ProcessEnvironmentVariable -Name "CTX_DISABLE_AUTO_UPGRADE" -Value "1"
    Set-ProcessEnvironmentVariable -Name "CTX_UPGRADE_AUTO" -Value "off"
    Set-ProcessEnvironmentVariable -Name "CTX_DAEMON_ENABLED" -Value "1"
    Set-ProcessEnvironmentVariable -Name "CTX_SEARCH_SEMANTIC" -Value "1"
    Set-ProcessEnvironmentVariable -Name "CTX_RUNTIME_DIR" -Value $runtimeRoot
    Set-ProcessEnvironmentVariable -Name "PATH" -Value $savedEnvironment["PATH"]

    $runtimeProof = Join-Path $DataRoot "packaged-runtime-proof.txt"
    $hostArch = $env:PROCESSOR_ARCHITECTURE
    $machineProbe = [CtxWindowsNativeArchitecture]::Probe()
    $hostNativeArch = if ($machineProbe.EndsWith(":8664", [System.StringComparison]::Ordinal)) {
        "AMD64"
    } elseif ($machineProbe.EndsWith(":AA64", [System.StringComparison]::Ordinal)) {
        "ARM64"
    } else {
        "unknown"
    }
    $processTranslated = if ($hostArch -ceq "AMD64" -and $machineProbe -ceq "0000:8664") { 0 } else { 1 }
    $runtimeAuthority = if ($processTranslated -eq 0) { "authoritative" } else { "non_authoritative" }
    if ($RequireAuthoritative -and $runtimeAuthority -cne "authoritative") {
        throw "Windows semantic smoke requires native AMD64 execution; probe was $machineProbe"
    }
    $runtimeProofLines = @(
        "runtime=onnxruntime",
        "version=$runtimeVersion",
        "platform=$RuntimePlatform",
        "host_system=Windows_NT",
        "host_arch=$hostArch",
        "host_native_arch=$hostNativeArch",
        "process_translated=$processTranslated",
        "native_arch_probe=iswow64process2",
        "runtime_authority=$runtimeAuthority",
        "artifact=$Ctx",
        "artifact_sha256=$binarySha",
        "archive=$runtimeArchivePath",
        "runtime_archive_sha256=$actualRuntimeSha",
        "CTX_RUNTIME_DIR=$runtimeRoot",
        "runtime_dylib=$runtimeDylib",
        "loader_overrides=unset",
        "CTX_SEMANTIC_CACHE_DIR=$semanticCache",
        "installer=explicit-metadata"
    )

    $marker = "ctx-release-semantic-smoke-" + [System.Guid]::NewGuid().ToString("n")
    $query = "synthetic release retrieval cobalt willow transit"
    $embeddingModel = "intfloat/multilingual-e5-small"
    $lines = @(
        [PSCustomObject]@{
            record_type = "manifest"
            schema_version = "ctx-history-jsonl-v1"
            metadata = [PSCustomObject]@{ exporter = "ctx-release-smoke" }
        },
        [PSCustomObject]@{
            record_type = "source"
            source_id = "release-smoke"
            provider_key = "ctx-smoke"
            source_format = "release-smoke-jsonl"
            raw_source_path = $fixturePath
        },
        [PSCustomObject]@{
            record_type = "session"
            source_id = "release-smoke"
            session_id = "semantic-daemon-smoke"
            cwd = "C:\ctx-release-smoke"
            started_at = "2026-07-10T00:00:00Z"
            agent_type = "primary"
            role_hint = "developer"
            is_primary = $true
            status = "completed"
        },
        [PSCustomObject]@{
            record_type = "event"
            source_id = "release-smoke"
            session_id = "semantic-daemon-smoke"
            event_index = 0
            event_type = "message"
            role = "user"
            occurred_at = "2026-07-10T00:00:01Z"
            payload = [PSCustomObject]@{ text = "Please remember the $marker validation task for daemon semantic search." }
            preview = "Please remember the $marker validation task for daemon semantic search."
            native_cursor = "line:1"
        },
        [PSCustomObject]@{
            record_type = "event"
            source_id = "release-smoke"
            session_id = "semantic-daemon-smoke"
            event_index = 1
            event_type = "message"
            role = "assistant"
            occurred_at = "2026-07-10T00:00:02Z"
            payload = [PSCustomObject]@{ text = "Recorded $marker as the release smoke semantic retrieval target." }
            preview = "Recorded $marker as the release smoke semantic retrieval target."
            native_cursor = "line:2"
        }
    ) | ForEach-Object { $_ | ConvertTo-Json -Depth 8 -Compress }
    [System.IO.File]::WriteAllLines($fixturePath, $lines, [System.Text.UTF8Encoding]::new($false))

    Write-Host "ctx semantic smoke: isolated_home=$smokeHome"
    Write-Host "ctx semantic smoke: semantic_cache=$semanticCache"
    Write-Host "ctx semantic smoke: packaged_runtime=$runtimeDylib"
    Invoke-Ctx -Args @("import", "--no-daemon", "--format", "ctx-history-jsonl-v1", "--path", $fixturePath) | Out-Null

    $configPath = Join-Path $DataRoot "config.toml"
    [System.IO.File]::WriteAllText(
        $configPath,
        "[analytics]`nenabled = false`n`n[upgrade]`nauto = `"off`"`n`n[daemon]`nenabled = true`n`n[search]`nsemantic = true`n",
        [System.Text.UTF8Encoding]::new($false)
    )

    $daemonLog = Join-Path $DataRoot "daemon-smoke.log"
    $daemonErr = Join-Path $DataRoot "daemon-smoke.err.log"
    $daemonArgs = @(
        "--data-root", $DataRoot,
        "daemon", "run",
        "--idle-exit-seconds", [string]$TimeoutSeconds,
        "--loop-interval-seconds", "2",
        "--json"
    )
    $daemon = Start-Process -FilePath $Ctx -ArgumentList $daemonArgs -PassThru -NoNewWindow -RedirectStandardOutput $daemonLog -RedirectStandardError $daemonErr

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastOutput = ""
    $lastSearchError = ""
    $lastStatusOutput = ""
    $lastStatusError = ""
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($daemon.HasExited) {
            $daemonOutput = Get-Content -LiteralPath $daemonLog -Raw -ErrorAction SilentlyContinue
            $daemonError = Get-Content -LiteralPath $daemonErr -Raw -ErrorAction SilentlyContinue
            throw "ctx semantic smoke: daemon exited before search succeeded`n$daemonOutput`n$daemonError"
        }

        $statusReport = Read-OwnedDaemonStatus -ExpectedPid $daemon.Id
        $lastStatusOutput = $statusReport.Text
        $lastStatusError = $statusReport.Error
        if ($statusReport.Ready) {
            $outputLines = @()
            $searchOk = $false
            try {
                $outputLines = @(Invoke-Ctx -Args @("search", $query, "--backend", "semantic", "--refresh", "off", "--json") 2>&1)
                $searchOk = $LASTEXITCODE -eq 0
            } catch {
                $lastSearchError = $_.Exception.Message
            }
            $lastOutput = $outputLines -join [Environment]::NewLine
            if ($searchOk) {
                try {
                    $searchJson = $lastOutput | ConvertFrom-Json -ErrorAction Stop
                    $retrievalProperty = $searchJson.PSObject.Properties["retrieval"]
                    $resultsProperty = $searchJson.PSObject.Properties["results"]
                    $modelMatches = (
                        $null -ne $retrievalProperty -and
                        $null -ne $retrievalProperty.Value -and
                        $null -ne $retrievalProperty.Value.PSObject.Properties["embedding_model"] -and
                        $retrievalProperty.Value.embedding_model -ceq $embeddingModel
                    )
                    $markerMatches = $false
                    if ($null -ne $resultsProperty -and $null -ne $resultsProperty.Value) {
                        foreach ($result in @($resultsProperty.Value)) {
                            $resultJson = $result | ConvertTo-Json -Depth 20 -Compress
                            if ($resultJson.IndexOf($marker, [System.StringComparison]::Ordinal) -ge 0) {
                                $markerMatches = $true
                                break
                            }
                        }
                    }
                    if ($modelMatches -and $markerMatches) {
                        $finalStatusReport = Read-OwnedDaemonStatus -ExpectedPid $daemon.Id
                        $lastStatusOutput = $finalStatusReport.Text
                        $lastStatusError = $finalStatusReport.Error
                        if ($finalStatusReport.Ready) {
                            $runtimeProofLines += @(
                                "daemon_status=running",
                                "daemon_pid=$($daemon.Id)",
                                "embedding_model=$embeddingModel",
                                "marker=$marker",
                                "semantic_search=passed"
                            )
                            [System.IO.File]::WriteAllLines(
                                $runtimeProof,
                                $runtimeProofLines,
                                [System.Text.UTF8Encoding]::new($false)
                            )
                            if (-not [string]::IsNullOrWhiteSpace($ProofOutput)) {
                                $proofParent = Split-Path -Parent $ProofOutput
                                if (-not [string]::IsNullOrWhiteSpace($proofParent)) {
                                    New-Item -ItemType Directory -Path $proofParent -Force | Out-Null
                                }
                                Copy-Item -LiteralPath $runtimeProof -Destination $ProofOutput -Force
                            }
                            Write-Host "ctx semantic smoke ok: strict semantic search found $marker with $embeddingModel"
                            exit 0
                        }
                    }
                } catch {
                    $lastSearchError = $_.Exception.Message
                }
            }
        }

        Start-Sleep -Seconds 5
    }

    $daemonOutput = Get-Content -LiteralPath $daemonLog -Raw -ErrorAction SilentlyContinue
    $daemonError = Get-Content -LiteralPath $daemonErr -Raw -ErrorAction SilentlyContinue
    throw @"
ctx semantic smoke failed: semantic search did not find fixture before timeout
Last search output:
$lastOutput
Last search error:
$lastSearchError
Last daemon status:
$lastStatusOutput
Last daemon status error:
$lastStatusError
Daemon stdout:
$daemonOutput
Daemon stderr:
$daemonError
"@
} finally {
    if ($null -ne $daemon -and -not $daemon.HasExited) {
        Stop-Process -InputObject $daemon -Force -ErrorAction SilentlyContinue
        $daemon.WaitForExit()
    }
    if (-not $KeepRoot -and -not [string]::IsNullOrWhiteSpace($runRoot)) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    foreach ($name in $environmentVariableNames) {
        Set-ProcessEnvironmentVariable -Name $name -Value $savedEnvironment[$name]
    }
}
