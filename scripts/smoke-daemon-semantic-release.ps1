param(
    [string]$Ctx = "ctx.exe",
    [string]$BuildInfo = "",
    [string]$RuntimeArchive = "",
    [string]$RuntimeMode = "onnxruntime",
    [string]$RuntimePlatform = "",
    [string]$DataRoot = "",
    [int]$TimeoutSeconds = 900,
    [switch]$SignedProvisioned,
    [switch]$RequireAuthoritative,
    [switch]$KeepRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This smoke must run on Windows"
}

. (Join-Path $PSScriptRoot "smoke-daemon-semantic-release-support.ps1")

if ($TimeoutSeconds -lt 30) {
    throw "TimeoutSeconds must be at least 30"
}
if ([string]::IsNullOrWhiteSpace($Ctx)) {
    throw "Ctx cannot be empty"
}
if ([string]::IsNullOrWhiteSpace($RuntimeArchive)) {
    throw "RuntimeArchive is required"
}
if ($RuntimePlatform -ne "windows-x64") {
    throw "RuntimePlatform must be windows-x64"
}

function Get-WindowsRuntimeContract {
    param([string]$Mode)

    switch -CaseSensitive ($Mode) {
        "onnxruntime" {
            return [PSCustomObject]@{
                Mode = "onnxruntime"
                Version = "1.27.0"
                Asset = "ctx-onnxruntime-windows-x64.zip"
                EmbeddingBackend = "cpu"
                BackendPreference = "cpu"
                StatusBackend = "cpu"
                ExecutionProvider = "CPUExecutionProvider"
                Files = @(
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
                )
            }
        }
        "windows-ml" {
            return [PSCustomObject]@{
                Mode = "windows-ml"
                Version = "2.1.74"
                Asset = "ctx-windowsml-windows-x64.zip"
                EmbeddingBackend = "windows-ml"
                BackendPreference = "windowsml"
                StatusBackend = "windows_ml"
                ExecutionProvider = "WindowsML:DmlExecutionProvider:GPU"
                Files = @(
                    "LICENSE",
                    "ThirdPartyNotices.txt",
                    "lib/Microsoft.Windows.AI.MachineLearning.dll",
                    "lib/onnxruntime.dll",
                    "lib/DirectML.dll"
                )
            }
        }
        default {
            throw "RuntimeMode must be exactly onnxruntime or windows-ml"
        }
    }
}

$runtimeContract = Get-WindowsRuntimeContract -Mode $RuntimeMode
$signedWindowsMl = $RuntimeMode -ceq "windows-ml" -and $SignedProvisioned
if ($RuntimeMode -ceq "windows-ml" -and -not $SignedProvisioned) {
    throw "Windows ML proof requires -SignedProvisioned after hosted signed model/runtime provisioning"
}
if ($SignedProvisioned -and $RuntimeMode -cne "windows-ml") {
    throw "-SignedProvisioned is only valid with -RuntimeMode windows-ml"
}
if ($signedWindowsMl -and [string]::IsNullOrWhiteSpace($DataRoot)) {
    throw "Signed Windows ML proof requires the exact provisioned DataRoot"
}
$runtimeVersion = $runtimeContract.Version
$expectedRuntimeAsset = $runtimeContract.Asset
if ([System.IO.Path]::GetFileName($RuntimeArchive) -ne $expectedRuntimeAsset) {
    throw "RuntimeArchive for $RuntimeMode must be named $expectedRuntimeAsset"
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
        [PSCustomObject]$Contract
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    @($Contract.Files) | ForEach-Object { [void]$expectedFiles.Add($_) }
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
        if ($Contract.Mode -ceq "onnxruntime") {
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
            if ($versionText -cne ($Contract.Version + "`n")) {
                throw "Runtime archive VERSION_NUMBER is not exactly $($Contract.Version)"
            }
        }
    } finally {
        $archive.Dispose()
    }
}

function Get-BoundWindowsBuildInfoSha256 {
    param(
        [string]$ArtifactPath,
        [string]$ExpectedArtifactSha256,
        [string]$BuildInfoPath,
        [string]$MatrixPath
    )

    foreach ($inputFile in @(
        [PSCustomObject]@{ Path = $ArtifactPath; Label = "Windows release artifact"; Maximum = 268435456 },
        [PSCustomObject]@{ Path = $BuildInfoPath; Label = "Windows release build-info"; Maximum = 65536 },
        [PSCustomObject]@{ Path = $MatrixPath; Label = "release-target matrix"; Maximum = 262144 }
    )) {
        if (-not (Test-Path -LiteralPath $inputFile.Path -PathType Leaf)) {
            throw "$($inputFile.Label) is unavailable: $($inputFile.Path)"
        }
        $item = Get-Item -LiteralPath $inputFile.Path -Force
        if (
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $item.Length -le 0 -or
            $item.Length -gt $inputFile.Maximum
        ) {
            throw "$($inputFile.Label) is not an allowed regular file: $($inputFile.Path)"
        }
    }

    $artifactSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArtifactPath).Hash.ToLowerInvariant()
    if ($artifactSha256 -cne $ExpectedArtifactSha256) {
        throw "Windows release build-info artifact changed before validation"
    }
    $buildInfo = Get-Content -LiteralPath $BuildInfoPath -Raw | ConvertFrom-Json -ErrorAction Stop
    $matrix = Get-Content -LiteralPath $MatrixPath -Raw | ConvertFrom-Json -ErrorAction Stop
    $targets = @($matrix.targets | Where-Object { $_.id -ceq "windows-x64" })
    if ($matrix.schema_version -ne 1 -or $targets.Count -ne 1) {
        throw "release-target matrix does not contain the exact Windows target"
    }
    $target = $targets[0]
    $source = $buildInfo.PSObject.Properties["source"].Value
    $gates = $buildInfo.PSObject.Properties["gates"].Value
    $sourceCommit = $source.PSObject.Properties["commit"].Value
    $sourceClean = $source.PSObject.Properties["clean"].Value
    $buildLinux = $buildInfo.PSObject.Properties["linux_build"]
    $targetLinux = $target.PSObject.Properties["linux_build"]
    if (
        $buildInfo.schema_version -ne 1 -or
        $buildInfo.platform -cne "windows-x64" -or
        $buildInfo.target -cne $target.public_rust_target -or
        $buildInfo.artifact_sha256 -cne $artifactSha256 -or
        $buildInfo.cargo_lock_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $sourceClean -ne $true -or
        $sourceCommit -cnotmatch '^[0-9a-f]{40}$' -or
        $sourceCommit -ceq "0000000000000000000000000000000000000000" -or
        $gates.static -cne "passed" -or
        $gates.static_abi -cne "passed" -or
        $null -eq $buildLinux -or
        $null -eq $targetLinux -or
        $null -ne $buildLinux.Value -or
        $null -ne $targetLinux.Value
    ) {
        throw "Windows release build-info does not bind the clean exact matrix artifact"
    }
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $BuildInfoPath).Hash.ToLowerInvariant()
}

Assert-WindowsRuntimeArchive -ArchivePath $runtimeArchivePath -Contract $runtimeContract

$ctxCommand = Get-Command -Name $Ctx -CommandType Application -ErrorAction Stop
$Ctx = $ctxCommand.Source
$ctxSource = $Ctx
$ctxBuildInfoPath = if ([string]::IsNullOrWhiteSpace($BuildInfo)) {
    "$ctxSource.build-info.json"
} else {
    (Resolve-Path -LiteralPath $BuildInfo).Path
}
$releaseTargetMatrix = Join-Path (Split-Path -Parent $PSScriptRoot) "contracts\release-targets-v1.json"

function New-UniqueFixtureRoot {
    param(
        [string]$Parent,
        [string]$DataRoot
    )

    if ([string]::IsNullOrWhiteSpace($Parent)) {
        throw "Fixture root requires a parent outside DataRoot"
    }
    $resolvedParent = (Resolve-Path -LiteralPath $Parent).Path
    $resolvedDataRoot = (Resolve-Path -LiteralPath $DataRoot).Path
    $dataRootPrefix = $resolvedDataRoot
    $directorySeparator = [string][System.IO.Path]::DirectorySeparatorChar
    if (-not $dataRootPrefix.EndsWith($directorySeparator, [System.StringComparison]::Ordinal)) {
        $dataRootPrefix += $directorySeparator
    }

    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        $suffix = [System.Guid]::NewGuid().ToString("n").Substring(0, 12)
        $candidate = [System.IO.Path]::GetFullPath((Join-Path $resolvedParent ("f-" + $suffix)))
        if (
            $candidate.Equals($resolvedDataRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
            $candidate.StartsWith($dataRootPrefix, [System.StringComparison]::OrdinalIgnoreCase)
        ) {
            throw "Fixture root must be outside ctx DataRoot: $resolvedDataRoot"
        }
        try {
            return (New-Item -ItemType Directory -Path $candidate -ErrorAction Stop).FullName
        } catch {
            if (Test-Path -LiteralPath $candidate) {
                continue
            }
            throw
        }
    }
    throw "Could not create a unique fixture root under $resolvedParent"
}

function Complete-SmokeTeardown {
    [CmdletBinding()]
    param(
        [AllowNull()][System.Management.Automation.ErrorRecord]$PrimaryError,
        [AllowNull()][System.Exception]$TeardownError,
        [AllowNull()][System.Exception]$FixtureCleanupError,
        [AllowNull()][System.Exception]$EnvironmentCleanupError,
        [string]$RunRoot,
        [string]$FixtureRoot
    )

    if ($null -ne $PrimaryError) {
        if ($null -ne $TeardownError) {
            Write-Error `
                "ctx semantic smoke cleanup also failed to stop the daemon; retained isolated root $RunRoot`: $($TeardownError.Message)" `
                -ErrorAction Continue
        }
        if ($null -ne $FixtureCleanupError) {
            Write-Error `
                "ctx semantic smoke cleanup also failed to remove task-owned fixture root $FixtureRoot`: $($FixtureCleanupError.Message)" `
                -ErrorAction Continue
        }
        if ($null -ne $EnvironmentCleanupError) {
            Write-Error `
                "ctx semantic smoke cleanup also failed to restore the process environment: $($EnvironmentCleanupError.Message)" `
                -ErrorAction Continue
        }
        $PSCmdlet.ThrowTerminatingError($PrimaryError)
    }

    if ($null -ne $TeardownError) {
        Write-Error `
            "ctx semantic smoke retained isolated root for survivor diagnosis: $RunRoot" `
            -ErrorAction Continue
        throw $TeardownError
    }
    if ($null -ne $FixtureCleanupError) {
        throw "ctx semantic smoke failed to remove task-owned fixture root $FixtureRoot`: $($FixtureCleanupError.Message)"
    }
    if ($null -ne $EnvironmentCleanupError) {
        throw "ctx semantic smoke failed to restore the process environment: $($EnvironmentCleanupError.Message)"
    }
}

$environmentVariableNames = @(
    "USERPROFILE", "HOME", "LOCALAPPDATA", "APPDATA", "XDG_CACHE_HOME", "XDG_CONFIG_HOME",
    "CTX_DATA_ROOT",
    "CTX_DAEMON_ENABLED",
    "CTX_DAEMON_AUTOSTART_OFF", "CTX_DAEMON_AUTOSTART_EXE", "CTX_DAEMON_BACKGROUND_CHILD",
    "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
    "CTX_SEARCH_SEMANTIC", "CTX_SEMANTIC_WORKER_OFF",
    "CTX_INTERNAL_SEMANTIC_BACKEND",
    "CTX_SEMANTIC_WORKER_MAX_CHUNKS", "CTX_SEMANTIC_WORKER_MAX_SECONDS",
    "CTX_SEMANTIC_THREADS", "CTX_SEMANTIC_EMBED_BATCH",
    "CTX_ANALYTICS_ENABLED",
    "CTX_ANALYTICS_ENDPOINT", "CTX_ANALYTICS_DRY_RUN", "CTX_ANALYTICS_DEBUG",
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL", "CTX_UPGRADE_FUNCTIONS_BASE",
    "CTX_UPGRADE_INTERVAL_SECONDS", "CTX_UPGRADE_TARGET",
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
$ownsRunRoot = $false
$fixtureRoot = ""
$ownsFixtureRoot = $false
$daemon = $null
$primaryError = $null

function Invoke-Ctx {
    param([string[]]$CommandArgs)
    & $Ctx --data-root $DataRoot @CommandArgs
}

function Invoke-CtxChecked {
    param(
        [string[]]$CommandArgs,
        [string]$FailureLabel
    )

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $outputLines = @(Invoke-Ctx -CommandArgs $CommandArgs 2>&1)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if ($exitCode -ne 0) {
        throw "ctx semantic smoke: $FailureLabel failed with status $exitCode`n$($outputLines -join [Environment]::NewLine)"
    }
    return $outputLines
}

function Stop-OwnedDaemon {
    param([System.Diagnostics.Process]$Process)

    if ($null -eq $Process -or $Process.HasExited) {
        return
    }
    Stop-Process -InputObject $Process -Force -ErrorAction SilentlyContinue
    if (-not $Process.WaitForExit(5000) -or -not $Process.HasExited) {
        throw "ctx semantic smoke: daemon process $($Process.Id) survived bounded teardown"
    }
}

function Read-OwnedDaemonStatus {
    param([int]$ExpectedPid)

    $statusLines = @()
    try {
        $statusLines = @(Invoke-Ctx -CommandArgs @("daemon", "status", "--format=json") 2>&1)
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
    if ($signedWindowsMl) {
        if (-not (Test-Path -LiteralPath $DataRoot -PathType Container)) {
            throw "Signed Windows ML DataRoot is not an existing directory: $DataRoot"
        }
        $DataRoot = (Resolve-Path -LiteralPath $DataRoot).Path
        $runRoot = $DataRoot
        $fixtureParent = Split-Path -Parent $DataRoot
    } else {
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
        $ownsRunRoot = $true
        $DataRoot = Join-Path $runRoot "data"
        New-Item -ItemType Directory -Path $DataRoot | Out-Null
        $DataRoot = [System.IO.Path]::GetFullPath($DataRoot)
        $fixtureParent = $dataRootParent
    }

    $fixtureRoot = New-UniqueFixtureRoot -Parent $fixtureParent -DataRoot $DataRoot
    $ownsFixtureRoot = $true
    $fixturePath = Join-Path $fixtureRoot "history.jsonl"
    $smokeHome = Join-Path $DataRoot "home"
    $smokeCache = Join-Path $DataRoot "cache"
    $smokeConfig = Join-Path $DataRoot "config-home"
    $smokeLocalAppData = Join-Path $DataRoot "local-app-data"
    $smokeAppData = Join-Path $DataRoot "app-data"
    $semanticCache = if ($signedWindowsMl) {
        Join-Path $DataRoot "semantic-model-cache"
    } else {
        Join-Path $DataRoot "semantic-cache"
    }
    New-Item -ItemType Directory -Path $smokeHome -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeCache -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeConfig -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeLocalAppData -Force | Out-Null
    New-Item -ItemType Directory -Path $smokeAppData -Force | Out-Null
    if ($signedWindowsMl) {
        if (-not (Test-Path -LiteralPath $semanticCache -PathType Container)) {
            throw "Signed Windows ML model cache is missing: $semanticCache"
        }
    } else {
        New-Item -ItemType Directory -Path $semanticCache -Force | Out-Null
    }

    Write-Host "ctx semantic smoke: run_root=$runRoot"
    Write-Host "ctx semantic smoke: data_root=$DataRoot"
    Write-Host "ctx semantic smoke: fixture_root=$fixtureRoot"

    $runtimeRoot = Join-Path $DataRoot "runtime"
    $runtimeInstallDir = Join-Path $runtimeRoot ("onnxruntime\" + $runtimeVersion + "\" + $RuntimePlatform)

    $versionLine = (& $Ctx --version | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or $versionLine -notmatch '^ctx\s+(\S+)') {
        throw "Could not determine ctx version from $Ctx"
    }
    $ctxVersion = $Matches[1]
    $binarySha = (Get-FileHash -Algorithm SHA256 -LiteralPath $ctxSource).Hash.ToLowerInvariant()
    [void](Get-BoundWindowsBuildInfoSha256 `
        -ArtifactPath $ctxSource `
        -ExpectedArtifactSha256 $binarySha `
        -BuildInfoPath $ctxBuildInfoPath `
        -MatrixPath $releaseTargetMatrix)
    if (-not $signedWindowsMl) {
        $releaseArtifactDir = Join-Path $runRoot "release-artifacts"
        $installBinDir = Join-Path $runRoot "installed\bin"
        $releaseMetadata = Join-Path $runRoot "release-metadata.env"
        New-Item -ItemType Directory -Path $releaseArtifactDir -Force | Out-Null
        New-Item -ItemType Directory -Path $installBinDir -Force | Out-Null
        $releaseBinary = "ctx-windows-x64.exe"
        Copy-Item -LiteralPath $Ctx -Destination (Join-Path $releaseArtifactDir $releaseBinary) -Force
        Copy-Item -LiteralPath $runtimeArchivePath -Destination (Join-Path $releaseArtifactDir $expectedRuntimeAsset) -Force
        Copy-Item -LiteralPath $runtimeShaPath -Destination (Join-Path $releaseArtifactDir "$expectedRuntimeAsset.sha256") -Force
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
        [System.IO.File]::WriteAllLines(
            $releaseMetadata,
            $metadataLines,
            [System.Text.UTF8Encoding]::new($false)
        )

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
    }

    $runtimeDylib = Join-Path $runtimeInstallDir "lib\onnxruntime.dll"
    if (
        -not (Test-Path -LiteralPath $Ctx -PathType Leaf) -or
        -not (Test-Path -LiteralPath $runtimeDylib -PathType Leaf)
    ) {
        throw "Semantic smoke input does not contain the expected binary/runtime layout"
    }
    $runtimeDylib = [System.IO.Path]::GetFullPath($runtimeDylib)
    $binaryMarker = Get-Content -LiteralPath "$Ctx.install.json" -Raw | ConvertFrom-Json
    $runtimeMarker = Get-Content -LiteralPath (Join-Path $runtimeInstallDir "ctx-runtime-install.json") -Raw | ConvertFrom-Json
    if ($signedWindowsMl) {
        if (
            $binaryMarker.manager -cne "ctx-hosted-installer" -or
            $binaryMarker.platform -cne "windows-x64" -or
            $binaryMarker.version -cne $ctxVersion -or
            $binaryMarker.sha256 -cne $binarySha
        ) {
            throw "Signed hosted binary install provenance marker is missing or incorrect"
        }
        if (
            $runtimeMarker.manager -cne "ctx-hosted-installer" -or
            $runtimeMarker.metadata_trust -cne "signed-release-metadata" -or
            $runtimeMarker.runtime -cne "windows-ml" -or
            $runtimeMarker.version -cne $runtimeVersion -or
            $runtimeMarker.sha256 -cne $actualRuntimeSha
        ) {
            throw "Signed Windows ML runtime install provenance marker is missing or incorrect"
        }
    } else {
        if (
            $binaryMarker.manager -cne "ctx-explicit-metadata-installer" -or
            $binaryMarker.metadata_trust -cne "explicit-unsigned" -or
            $runtimeMarker.manager -cne "ctx-explicit-metadata-installer" -or
            $runtimeMarker.metadata_trust -cne "explicit-unsigned"
        ) {
            throw "Explicit-metadata binary/runtime install provenance marker is missing or incorrect"
        }
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
    Set-ProcessEnvironmentVariable -Name "CTX_ANALYTICS_ENABLED" -Value "false"
    Set-ProcessEnvironmentVariable -Name "CTX_UPGRADE_AUTO" -Value "off"
    Set-ProcessEnvironmentVariable -Name "CTX_DAEMON_ENABLED" -Value "true"
    Set-ProcessEnvironmentVariable -Name "CTX_SEARCH_SEMANTIC" -Value "true"
    Set-ProcessEnvironmentVariable `
        -Name "CTX_INTERNAL_SEMANTIC_BACKEND" `
        -Value $runtimeContract.BackendPreference
    Set-ProcessEnvironmentVariable -Name "CTX_RUNTIME_DIR" -Value $runtimeRoot
    Set-ProcessEnvironmentVariable -Name "PATH" -Value $savedEnvironment["PATH"]

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
    $marker = "ctx-release-semantic-smoke-" + [System.Guid]::NewGuid().ToString("n")
    $query = "synthetic release retrieval cobalt willow transit"
    $embeddingModel = "intfloat/multilingual-e5-small"
    $lines = @(
        [PSCustomObject]@{
            record_type = "manifest"
            schema_version = "ctx-history-jsonl-v2"
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
            provider_session_id = "semantic-daemon-smoke"
            cwd = "C:\ctx-release-smoke"
            started_at = "2026-07-10T00:00:00Z"
            agent_scope = "primary"
            role_hint = "developer"
            status = "completed"
        },
        [PSCustomObject]@{
            record_type = "event"
            source_id = "release-smoke"
            provider_session_id = "semantic-daemon-smoke"
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
            provider_session_id = "semantic-daemon-smoke"
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
    Invoke-CtxChecked -FailureLabel "fixture import" -CommandArgs @(
        "import", "--no-daemon", "--input-format", "ctx-history-jsonl-v2", "--path", $fixturePath
    ) | Out-Null

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
        "--loop-interval-seconds", "2",
        "--format=json"
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
                $outputLines = @(Invoke-Ctx -CommandArgs @("search", $query, "--backend", "semantic", "--refresh", "off", "--format=json") 2>&1)
                $searchOk = $LASTEXITCODE -eq 0
            } catch {
                $lastSearchError = $_.Exception.Message
            }
            $lastOutput = $outputLines -join [Environment]::NewLine
            if ($searchOk) {
                $attestingModules = $false
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
                        $attestingModules = $true
                        $finalStatusReport = Read-OwnedDaemonStatus -ExpectedPid $daemon.Id
                        $lastStatusOutput = $finalStatusReport.Text
                        $lastStatusError = $finalStatusReport.Error
                        if ($finalStatusReport.Ready) {
                            $embeddingRuntime = $finalStatusReport.Json.daemon.jobs.semantic_index.embedding_runtime
                            if (
                                $null -eq $embeddingRuntime -or
                                $embeddingRuntime.backend -cne $runtimeContract.StatusBackend -or
                                $embeddingRuntime.preference -cne $runtimeContract.BackendPreference -or
                                $embeddingRuntime.execution_provider -cne $runtimeContract.ExecutionProvider
                            ) {
                                throw "Daemon did not report the selected $RuntimeMode embedding runtime"
                            }
                            $runtimeArtifactIdentity = [string]$embeddingRuntime.runtime_artifact_identity
                            if (
                                $RuntimeMode -ceq "windows-ml" -and (
                                    $embeddingRuntime.canary -cne "passed" -or
                                    $runtimeArtifactIdentity.IndexOf(
                                        "sha256=$actualRuntimeSha",
                                        [System.StringComparison]::Ordinal
                                    ) -lt 0
                                )
                            ) {
                                throw "Windows ML runtime did not pass the exact-archive semantic contract canary"
                            }

                            $runtimeLibDir = [System.IO.Path]::GetFullPath((Join-Path $runtimeInstallDir "lib"))
                            $daemonModules = @(Get-Process -Id $daemon.Id -Module -ErrorAction Stop)
                            $onnxRuntimeModules = @(
                                $daemonModules | Where-Object { $_.ModuleName -ieq "onnxruntime.dll" }
                            )
                            if ($onnxRuntimeModules.Count -ne 1) {
                                throw "Expected exactly one loaded onnxruntime.dll module, got $($onnxRuntimeModules.Count)"
                            }
                            $actualOnnxRuntime = [System.IO.Path]::GetFullPath($onnxRuntimeModules[0].FileName)
                            if (-not $actualOnnxRuntime.Equals($runtimeDylib, [System.StringComparison]::OrdinalIgnoreCase)) {
                                throw "Loaded onnxruntime.dll from $actualOnnxRuntime instead of $runtimeDylib"
                            }
                            if ($RuntimeMode -ceq "onnxruntime") {
                                foreach ($dependencyName in @(
                                    "msvcp140.dll",
                                    "msvcp140_1.dll",
                                    "vcruntime140.dll",
                                    "vcruntime140_1.dll"
                                )) {
                                    $dependencyModules = @(
                                        $daemonModules | Where-Object { $_.ModuleName -ieq $dependencyName }
                                    )
                                    if ($dependencyModules.Count -ne 1) {
                                        throw "Expected exactly one loaded $dependencyName module, got $($dependencyModules.Count)"
                                    }
                                    $actualDependency = [System.IO.Path]::GetFullPath($dependencyModules[0].FileName)
                                    $expectedDependency = [System.IO.Path]::GetFullPath((Join-Path $runtimeLibDir $dependencyName))
                                    if (-not $actualDependency.Equals($expectedDependency, [System.StringComparison]::OrdinalIgnoreCase)) {
                                        throw "Loaded $dependencyName from $actualDependency instead of $expectedDependency"
                                    }
                                }
                            } else {
                                foreach ($moduleContract in @(
                                    [PSCustomObject]@{
                                        Name = "Microsoft.Windows.AI.MachineLearning.dll"
                                    },
                                    [PSCustomObject]@{
                                        Name = "DirectML.dll"
                                    }
                                )) {
                                    $matchingModules = @(
                                        $daemonModules | Where-Object { $_.ModuleName -ieq $moduleContract.Name }
                                    )
                                    if ($matchingModules.Count -ne 1) {
                                        throw "Expected exactly one loaded $($moduleContract.Name) module, got $($matchingModules.Count)"
                                    }
                                    $actualModule = [System.IO.Path]::GetFullPath($matchingModules[0].FileName)
                                    $expectedModule = [System.IO.Path]::GetFullPath(
                                        (Join-Path $runtimeLibDir $moduleContract.Name)
                                    )
                                    if (-not $actualModule.Equals($expectedModule, [System.StringComparison]::OrdinalIgnoreCase)) {
                                        throw "Loaded $($moduleContract.Name) from $actualModule instead of $expectedModule"
                                    }
                                }
                            }
                            Write-Host "ctx semantic smoke ok: strict semantic search found $marker with $embeddingModel"
                            exit 0
                        }
                    }
                } catch {
                    if ($attestingModules) {
                        throw
                    }
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
} catch {
    $primaryError = $_
} finally {
    $teardownError = $null
    $fixtureCleanupError = $null
    $environmentCleanupError = $null
    try {
        Stop-OwnedDaemon -Process $daemon
    } catch {
        $teardownError = $_.Exception
    }
    try {
        if (
            $ownsFixtureRoot -and
            -not [string]::IsNullOrWhiteSpace($fixtureRoot) -and
            (Test-Path -LiteralPath $fixtureRoot)
        ) {
            Remove-Item -LiteralPath $fixtureRoot -Recurse -Force -ErrorAction Stop
        }
    } catch {
        $fixtureCleanupError = $_.Exception
    }
    if (
        $null -eq $teardownError -and
        -not $KeepRoot -and
        $ownsRunRoot -and
        -not [string]::IsNullOrWhiteSpace($runRoot)
    ) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    foreach ($name in $environmentVariableNames) {
        try {
            Set-ProcessEnvironmentVariable -Name $name -Value $savedEnvironment[$name]
        } catch {
            if ($null -eq $environmentCleanupError) {
                $environmentCleanupError = $_.Exception
            }
        }
    }
    Complete-SmokeTeardown `
        -PrimaryError $primaryError `
        -TeardownError $teardownError `
        -FixtureCleanupError $fixtureCleanupError `
        -EnvironmentCleanupError $environmentCleanupError `
        -RunRoot $runRoot `
        -FixtureRoot $fixtureRoot
}
