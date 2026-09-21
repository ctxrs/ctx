import { renderCliInstallPowerShellReleasePreparation } from "./cli-install-powershell-release.js";
import { FROZEN_BRIDGE_VERSION, FROZEN_BRIDGE_METADATA_URL, CLI_INSTALL_POWERSHELL_VERSION_COMPARE } from "./cli-install-bridge.js";
import {
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
} from "./install-stage-contract.js";
import { CLI_INSTALL_POWERSHELL_PATH_SETUP } from "./cli-install-powershell-path.js";
import { CLI_INSTALL_POWERSHELL_MANAGED_INSTALL } from "./cli-install-powershell-managed-install.js";
import {
  renderCliInstallPowerShellManagedPairDownload,
  renderCliInstallPowerShellManagedPairPublication,
} from "./cli-install-powershell-managed-pair.js";
import { CLI_INSTALL_POWERSHELL_PROCESS_HELPERS } from "./cli-install-powershell-process.js";

export function renderCliInstallPowerShellWorkflow() {
  return `function Protect-ManagedPath([string]$Path, [switch]$Directory) {
    try {
        $principalSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        [CtxInstallerAcl]::Protect($Path, [bool]$Directory, $principalSid)
    } catch {
        Fail "failed to protect managed installer path: $Path"
    }
}

function Normalize-PathEntry([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return ""
    }
    return $Path.Trim().Trim('"').TrimEnd("\\", "/")
}

${CLI_INSTALL_POWERSHELL_PATH_SETUP}
$script:installStageDeliveryEnabled = $true
function Send-InstallStage([string]$Stage, [string]$Status) {
    if (-not $script:installStageDeliveryEnabled) {
        return
    }
    if (Test-CanonicalAnalyticsDisabled) {
        return
    }
    if ($DryRun) {
        return
    }
    if ($installTelemetryBase -notmatch '^https://') {
        return
    }
    $body = [ordered]@{
        event_name = "${INSTALL_STAGE_EVENT_NAME}"
        event_version = ${INSTALL_STAGE_EVENT_VERSION}
        install_attempt_id = $installAttemptId
        stage = $Stage
        status = $Status
        platform = "windows"
        arch = "x64"
        script_family = "powershell"
    } | ConvertTo-Json -Compress
    try {
        Invoke-WebRequest -Uri "$installTelemetryBase/install-attempt" -Method POST -ContentType "application/json" -Body $body -UseBasicParsing -TimeoutSec 1 | Out-Null
    } catch {
        $script:installStageDeliveryEnabled = $false
    }
}

$persistedConfigControls = Get-PersistedConfigControls
$runSetup = -not $NoSetup -and $env:CTX_INSTALL_NO_SETUP -ne "1"
$setupNoDaemon = [bool]$NoDaemon -or $env:CTX_INSTALL_NO_DAEMON -eq "1"
$semanticEnabled = [bool]$Semantic
$semanticInstallControlValue = [Environment]::GetEnvironmentVariable(
    "CTX_INSTALL_SEMANTIC",
    "Process"
)
if ($null -ne $semanticInstallControlValue) {
    $semanticInstallControl = $semanticInstallControlValue.ToLowerInvariant()
    if ($semanticInstallControl -in @("1", "true", "yes", "on")) {
        $semanticEnabled = $true
    } elseif ($semanticInstallControl -notin @("", "0", "false", "no", "off")) {
        Fail "CTX_INSTALL_SEMANTIC must be a canonical boolean"
    }
}
$semanticSearchControl = "unset"
$semanticSearchControlValue = [Environment]::GetEnvironmentVariable(
    "CTX_SEARCH_SEMANTIC",
    "Process"
)
if ($null -ne $semanticSearchControlValue) {
    $semanticSearchControl = $semanticSearchControlValue.Trim().Trim('"').ToLowerInvariant()
    if ($semanticSearchControl -in @("1", "true", "yes", "on")) {
        $semanticSearchControl = "true"
        $semanticEnabled = $true
    } elseif ($semanticSearchControl -eq "") {
        $semanticSearchControl = "unset"
    } elseif ($semanticSearchControl -in @("0", "false", "no", "off")) {
        $semanticSearchControl = "false"
    } else {
        Fail "CTX_SEARCH_SEMANTIC must be a canonical boolean"
    }
}
if (-not $semanticEnabled -and
    $semanticSearchControl -ne "false" -and
    $persistedConfigControls.SemanticEnabled) {
    $semanticEnabled = $true
}

$daemonConfigurationDisabled =
    (Test-CanonicalDaemonDisabled) -or $persistedConfigControls.DaemonDisabled
$daemonEnabled = -not $setupNoDaemon -and -not $daemonConfigurationDisabled

if ($semanticEnabled -and -not $daemonEnabled) {
    Fail "Semantic installation requires an enabled daemon; remove installer no-daemon controls, clear daemon-disable environment controls, or set [daemon] enabled = true"
}

$installPath = Join-Path $BinDir "ctx.exe"
$markerPath = "$installPath.install.json"
${CLI_INSTALL_POWERSHELL_MANAGED_INSTALL}
${CLI_INSTALL_POWERSHELL_VERSION_COMPARE}
${renderCliInstallPowerShellManagedPairPublication()}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-install-" + [System.Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $tempRoot | Out-Null
$releaseChannel = $channel
$version = ""
$installerCompleted = $false

${CLI_INSTALL_POWERSHELL_PROCESS_HELPERS}
try {
    Send-InstallStage -Stage "installer" -Status "started"
${renderCliInstallPowerShellReleasePreparation()}    $skillAgents = @()
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

    $runSkill = -not $noSkillRequested
    if (-not $runSetup -and -not $explicitSkillRequest) {
        $runSkill = $false
    }
    $modifyPath = -not $NoModifyPath -and $env:CTX_INSTALL_NO_MODIFY_PATH -ne "1"
    if ($DryRun) {
        Write-Host "Would install ctx $version."
    } else {
        Write-Host "Installing ctx $version..."
    }
    if ($DryRun) {
        $installerCompleted = $true
        exit 0
    }

    $releasePhases = if ($bridgeRequired) { @("bridge", "final") } else { @("final") }
    foreach ($releasePhase in $releasePhases) {
        if ($releasePhase -ceq "bridge") {
            $Metadata = "${FROZEN_BRIDGE_METADATA_URL}"
            $metadataSignature = "$Metadata.sig"
        } else {
            $Metadata = $finalMetadata
            $metadataSignature = $finalMetadataSignature
        }
        . $readReleasePhaseMetadata
        if ($releasePhase -ceq "bridge") {
            if ($version -cne "${FROZEN_BRIDGE_VERSION}" -or -not $managedPair) {
                Fail "frozen bridge metadata does not identify the required signed ${FROZEN_BRIDGE_VERSION} pair"
            }
            if ($finalVersion -ceq $version -and $finalChecksum.ToLowerInvariant() -cne $checksum.ToLowerInvariant()) {
                Fail "final metadata conflicts with the frozen bridge identity"
            }
        }
    Send-InstallStage -Stage "artifact_download" -Status "started"
    $compressedArtifactDownloaded = $false
    try {
        # G(256MiB) for the official single-member gzip producers; see README.
        Read-Artifact -Source $compressedArtifactUrl -Destination $compressedDownloadPath -MaxBytes 306184224
        $compressedArtifactDownloaded = $true
    } catch {
        Remove-Item -LiteralPath $compressedDownloadPath -Force -ErrorAction SilentlyContinue
    }
    if ($compressedArtifactDownloaded) {
        Expand-GzipFile -Source $compressedDownloadPath -Destination $downloadPath
    } else {
        Read-Artifact -Source $artifactUrl -Destination $downloadPath
    }
    Send-InstallStage -Stage "artifact_download" -Status "completed"
    $actualChecksum = (Get-FileHash -Algorithm SHA256 -LiteralPath $downloadPath).Hash.ToLowerInvariant()
    if ($actualChecksum -ne $checksum.ToLowerInvariant()) {
        Fail "checksum mismatch for \${artifact}: expected $checksum, got $actualChecksum"
    }
    ${renderCliInstallPowerShellManagedPairDownload()}

    $marker = [ordered]@{
        schema_version = 1
        manager = "ctx-hosted-installer"
        install_attempt_id = $installAttemptId
        install_path = $installPath
        platform = "windows-x64"
        channel = $releaseChannel
        version = $version
        sha256 = $actualChecksum
        metadata_url = $Metadata
        artifact_url = $artifactUrl
        source_commit = $sourceCommit
        published_at = $publishedAt
        installed_at = ([DateTime]::UtcNow.ToString("o"))
    }
    if ($managedPair) {
        $marker.managed_pair = $true
    }
    $markerJson = $marker | ConvertTo-Json -Depth 4
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $markerBytes = $utf8NoBom.GetBytes($markerJson + [Environment]::NewLine)
    $markerSourcePath = Join-Path $tempRoot "ctx.install.json"
    [IO.File]::WriteAllBytes($markerSourcePath, $markerBytes)

    Resume-InterruptedManagedPair

    $creationGuard = $null
    try {
        $creationGuard = [CtxInstallerPathGuard]::AcquireDirectory($BinDir, $false)
        $creationGuard.AssertUnchanged()
        if (-not $creationGuard.LeafExists) {
            New-Item -ItemType Directory -Path $BinDir | Out-Null
        }
    } finally {
        if ($null -ne $creationGuard) {
            $creationGuard.Dispose()
        }
    }

    $managedReinstall = $false
    $legacyManagedReinstall = $false
    $installGuard = $null
    $binaryDestinationGuard = $null
    $markerDestinationGuard = $null
    try {
        $installGuard = [CtxInstallerPathGuard]::AcquireDirectory($BinDir, $true)
        Protect-ManagedPath -Path $BinDir -Directory
        $installGuard.AssertUnchanged()

        # Validate both pre-existing destination leaves before either write.
        $binaryDestinationGuard = [CtxInstallerPathGuard]::AcquireLeaf($installPath)
        $markerDestinationGuard = [CtxInstallerPathGuard]::AcquireLeaf($markerPath)
        if ($binaryDestinationGuard.LeafExists) {
            Protect-ManagedPath -Path $installPath
        }
        if ($markerDestinationGuard.LeafExists) {
            Protect-ManagedPath -Path $markerPath
        }
        $binaryDestinationGuard.AssertUnchanged()
        $markerDestinationGuard.AssertUnchanged()
        $installGuard.AssertUnchanged()
        $existingManagedInstall = Read-ExistingManagedInstall
        $managedReinstall = $null -ne $existingManagedInstall
        if ($managedReinstall -and $channel -ceq "stable" -and
            (Compare-ReleaseVersion $version $existingManagedInstall.version) -lt 0) {
            Fail "refusing to downgrade the managed ctx installation"
        }
        if ($managedReinstall -and $existingManagedInstall.version -ceq "0.25.0" -and $existingManagedInstall.sha256.ToLowerInvariant() -cne "32aa550cc5c56d4d2989d0f929bbc1e634d8b730219feb8e4a4ba770b02a9867") {
            Fail "managed ctx v0.25 executable is not the immutable released Windows artifact"
        }
        $legacyManagedReinstall = $managedReinstall -and (
            $existingManagedInstall.version -ceq "0.25.0" -or
            ($releasePhase -ceq "bridge" -and
                (Compare-ReleaseVersion $existingManagedInstall.version "0.11.0") -ge 0 -and
                (Compare-ReleaseVersion $existingManagedInstall.version "0.26.0") -lt 0))
        $legacyReceiptHasPath = $legacyManagedReinstall -and
            (Compare-ReleaseVersion $existingManagedInstall.version "0.17.0") -ge 0
    } finally {
        if ($null -ne $markerDestinationGuard) {
            $markerDestinationGuard.Dispose()
        }
        if ($null -ne $binaryDestinationGuard) {
            $binaryDestinationGuard.Dispose()
        }
        if ($null -ne $installGuard) {
            $installGuard.Dispose()
        }
    }
    if ($managedReinstall) {
        if ($existingManagedInstall.version -ceq "0.25.0" -and $managedPair -and -not $releasedPairInstall) {
            # The immutable 0.25 Windows upgrader locks its runtime ZIP against
            # its own extractor. Its pinned identity was checked above; recover
            # through the authenticated candidate's existing pair transaction.
            $null = Invoke-ManagedPairApply -MarkerSource $markerSourcePath -Required $true
            if (-not (Test-InstalledTargetIdentity)) {
                Fail "ctx 0.25 recovery did not publish the signed managed identity"
            }
        } else {
            Invoke-ManagedCoreUpgrade
            if ($releasedPairInstall) {
                Invoke-ReleasedManagedPairInstall
            }
        }
    } elseif ($managedPair) {
        if ($releasedPairInstall) {
            Invoke-HostedInstallTransaction
            Invoke-ReleasedManagedPairInstall
        } else {
            $null = Invoke-ManagedPairApply -MarkerSource $markerSourcePath -Required $true
        }
    } else {
        Invoke-HostedInstallTransaction
    }
    Send-InstallStage -Stage "binary_install" -Status "completed"


    }
    Write-ReceiptItem "Installed and verified"

    if ($semanticEnabled) {
        $previousRepairMetadata = [Environment]::GetEnvironmentVariable(
            "CTX_RELEASE_METADATA_URL",
            "Process"
        )
        $previousRepairSignature = [Environment]::GetEnvironmentVariable(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            "Process"
        )
        $previousRepairSemantic = [Environment]::GetEnvironmentVariable(
            "CTX_SEARCH_SEMANTIC",
            "Process"
        )
        $repairMetadataUri = (
            [System.Uri]::new([System.IO.Path]::GetFullPath($metadataFile))
        ).AbsoluteUri
        $repairMetadataSignatureUri = (
            [System.Uri]::new([System.IO.Path]::GetFullPath($metadataSignatureFile))
        ).AbsoluteUri
        $semanticRepairStatus = 1
        try {
            [Environment]::SetEnvironmentVariable(
                "CTX_RELEASE_METADATA_URL",
                $repairMetadataUri,
                "Process"
            )
            [Environment]::SetEnvironmentVariable(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                $repairMetadataSignatureUri,
                "Process"
            )
            [Environment]::SetEnvironmentVariable("CTX_SEARCH_SEMANTIC", "1", "Process")
            $semanticRepairStatus = Invoke-CtxQuiet -Arguments @(
                "upgrade",
                "--channel",
                $channel,
                "--format=json"
            )
        } finally {
            [Environment]::SetEnvironmentVariable(
                "CTX_RELEASE_METADATA_URL",
                $previousRepairMetadata,
                "Process"
            )
            [Environment]::SetEnvironmentVariable(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                $previousRepairSignature,
                "Process"
            )
            [Environment]::SetEnvironmentVariable(
                "CTX_SEARCH_SEMANTIC",
                $previousRepairSemantic,
                "Process"
            )
        }
        if ($semanticRepairStatus -ne 0) {
            Fail "ctx Semantic runtime repair failed"
        }
    }

    $skillInstallFailed = $false
    if ($runSkill) {
        $skillArgs = @("integrations", "install", "skills")
        if ($allSkillAgentsRequested) {
            $skillArgs += "--all-agents"
        } else {
            foreach ($agent in $skillAgents) {
                $skillArgs += @("--agent", $agent)
            }
        }
        $skillArgs += "--format=json"
        Send-InstallStage -Stage "skill_install" -Status "started"
        $skillStatus = Invoke-CtxQuiet -Arguments $skillArgs
        if ($skillStatus -ne 0) {
            Send-InstallStage -Stage "skill_install" -Status "failed"
            $skillInstallFailed = $true
        } else {
            Send-InstallStage -Stage "skill_install" -Status "completed"
        }
    } else {
        Send-InstallStage -Stage "skill_install" -Status "skipped"
    }

    $setupStatus = 0
    function Assert-ExactSetupReceiptProperties([string]$ReceiptJson) {
        if ($ReceiptJson.Length -lt 2 -or $ReceiptJson.Length -gt 1MB) {
            throw "invalid setup receipt"
        }
        $position = 0
        $properties = [System.Collections.Generic.List[object]]::new()

        function Skip-ReceiptJsonWhitespace([string]$Json, [ref]$Position) {
            while ($Position.Value -lt $Json.Length -and
                [int][char]$Json[$Position.Value] -in @(32, 9, 13, 10)) {
                $Position.Value++
            }
        }

        function Read-ReceiptJsonString([string]$Json, [ref]$Position) {
            if ($Position.Value -ge $Json.Length -or $Json[$Position.Value] -ne '"') {
                throw "invalid setup receipt"
            }
            $Position.Value++
            $value = [Text.StringBuilder]::new()
            $escaped = $false
            while ($Position.Value -lt $Json.Length) {
                $character = $Json[$Position.Value]
                if ($character -eq '"') {
                    $Position.Value++
                    return [pscustomobject]@{ Name = $value.ToString(); Escaped = $escaped }
                }
                if ([int][char]$character -lt 0x20) {
                    throw "invalid setup receipt"
                }
                if ($character -eq '\\') {
                    $escaped = $true
                    [void]$value.Append($character)
                    $Position.Value++
                    if ($Position.Value -ge $Json.Length) {
                        throw "invalid setup receipt"
                    }
                    $escape = $Json[$Position.Value]
                    if ($escape -notin @('"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u')) {
                        throw "invalid setup receipt"
                    }
                    [void]$value.Append($escape)
                    if ($escape -eq 'u') {
                        for ($offset = 1; $offset -le 4; $offset++) {
                            if ($Position.Value + $offset -ge $Json.Length -or
                                $Json[$Position.Value + $offset] -notmatch '[0-9A-Fa-f]') {
                                throw "invalid setup receipt"
                            }
                            [void]$value.Append($Json[$Position.Value + $offset])
                        }
                        $Position.Value += 5
                    } else {
                        $Position.Value++
                    }
                } else {
                    [void]$value.Append($character)
                    $Position.Value++
                }
            }
            throw "invalid setup receipt"
        }

        function Skip-ReceiptJsonValue([string]$Json, [ref]$Position, [int]$Depth) {
            if ($Depth -gt 64) {
                throw "invalid setup receipt"
            }
            Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
            if ($Position.Value -ge $Json.Length) {
                throw "invalid setup receipt"
            }
            if ($Json[$Position.Value] -eq '{') {
                Read-ReceiptJsonObject -Json $Json -Position $Position -Depth ($Depth + 1) -Path ""
                return
            }
            if ($Json[$Position.Value] -eq '[') {
                $Position.Value++
                Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
                if ($Position.Value -lt $Json.Length -and $Json[$Position.Value] -eq ']') {
                    $Position.Value++
                    return
                }
                while ($true) {
                    Skip-ReceiptJsonValue -Json $Json -Position $Position -Depth ($Depth + 1)
                    Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
                    if ($Position.Value -ge $Json.Length) {
                        throw "invalid setup receipt"
                    }
                    if ($Json[$Position.Value] -eq ']') {
                        $Position.Value++
                        return
                    }
                    if ($Json[$Position.Value] -ne ',') {
                        throw "invalid setup receipt"
                    }
                    $Position.Value++
                }
            }
            if ($Json[$Position.Value] -eq '"') {
                [void](Read-ReceiptJsonString -Json $Json -Position $Position)
                return
            }
            $start = $Position.Value
            while ($Position.Value -lt $Json.Length -and
                [int][char]$Json[$Position.Value] -notin @(32, 9, 13, 10, 44, 93, 125)) {
                $Position.Value++
            }
            if ($Position.Value -eq $start) {
                throw "invalid setup receipt"
            }
        }

        function Read-ReceiptJsonObject(
            [string]$Json,
            [ref]$Position,
            [int]$Depth,
            [string]$Path
        ) {
            if ($Depth -gt 64 -or $Json[$Position.Value] -ne '{') {
                throw "invalid setup receipt"
            }
            $Position.Value++
            Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
            if ($Position.Value -lt $Json.Length -and $Json[$Position.Value] -eq '}') {
                $Position.Value++
                return
            }
            while ($true) {
                Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
                $property = Read-ReceiptJsonString -Json $Json -Position $Position
                Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
                if ($Position.Value -ge $Json.Length -or $Json[$Position.Value] -ne ':') {
                    throw "invalid setup receipt"
                }
                $Position.Value++
                Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
                if ($Path -ceq "root") {
                    [void]$properties.Add([pscustomobject]@{
                        Path = $Path
                        Name = $property.Name
                        Escaped = $property.Escaped
                    })
                }
                Skip-ReceiptJsonValue -Json $Json -Position $Position -Depth ($Depth + 1)
                Skip-ReceiptJsonWhitespace -Json $Json -Position $Position
                if ($Position.Value -ge $Json.Length) {
                    throw "invalid setup receipt"
                }
                if ($Json[$Position.Value] -eq '}') {
                    $Position.Value++
                    return
                }
                if ($Json[$Position.Value] -ne ',') {
                    throw "invalid setup receipt"
                }
                $Position.Value++
            }
        }

        Read-ReceiptJsonObject -Json $ReceiptJson -Position ([ref]$position) -Depth 0 -Path "root"
        Skip-ReceiptJsonWhitespace -Json $ReceiptJson -Position ([ref]$position)
        if ($position -ne $ReceiptJson.Length -or
            @($properties | Where-Object { $_.Escaped -and $_.Path -ceq "root" }).Count -ne 0) {
            throw "invalid setup receipt"
        }
        function Test-ExactReceiptField([string]$Path, [string]$Name, [bool]$Required) {
            $atPath = @($properties | Where-Object { $_.Path -ceq $Path })
            $exact = @($atPath | Where-Object { $_.Name -ceq $Name })
            $aliases = @($atPath | Where-Object { $_.Name -ieq $Name })
            return $exact.Count -le 1 -and $aliases.Count -eq $exact.Count -and
                (-not $Required -or $exact.Count -eq 1)
        }

        foreach ($name in @("schema_version", "initialized", "mode")) {
            if (-not (Test-ExactReceiptField -Path "root" -Name $name -Required $true)) {
                throw "invalid setup receipt"
            }
        }
        foreach ($name in @("indexed_sessions", "indexed_items")) {
            if (-not (Test-ExactReceiptField -Path "root" -Name $name -Required $false)) {
                throw "invalid setup receipt"
            }
        }
    }
    $setupVerified = $false
    $setupInitialized = $false
    $setupMode = "invalid"
    [long]$indexedSessions = -1
    [long]$indexedItems = -1
    $setupWaitRequested = $false
    if ($runSetup) {
        $setupWaitRequested = -not $setupNoDaemon
        if ([string]::IsNullOrWhiteSpace($SetupProgress)) {
            if ([string]::IsNullOrWhiteSpace($env:CTX_SETUP_PROGRESS)) {
                $SetupProgress = "auto"
            } else {
                $SetupProgress = $env:CTX_SETUP_PROGRESS
            }
        }
        Send-InstallStage -Stage "setup" -Status "started"
        if ($SetupProgress -cne "none" -and -not [Console]::IsOutputRedirected) {
            Write-Host ""
        }
        $setupArgs = @("setup", "--quiet", "--format", "json")
        if ($setupWaitRequested) {
            $setupArgs += "--wait"
        }
        if ($semanticEnabled) {
            $setupArgs += "--semantic"
        }
        $setupArgs += @("--progress", $SetupProgress)
        if ($setupNoDaemon) {
            $setupArgs += "--no-daemon"
        }
        $setupCommand = Invoke-HostedInstallerSetupCtxCaptured -Arguments $setupArgs -InheritStandardError:($SetupProgress -cne "none")
        $setupStatus = $setupCommand.ExitCode
        if ($setupStatus -eq 0) {
            try {
                $setupReceipt = Get-Content -LiteralPath $setupCommand.OutputPath -Raw
                Assert-ExactSetupReceiptProperties -ReceiptJson $setupReceipt
                $setupReceipt = $setupReceipt | ConvertFrom-Json
                $schemaProperty = $setupReceipt.PSObject.Properties["schema_version"]
                $initializedProperty = $setupReceipt.PSObject.Properties["initialized"]
                $modeProperty = $setupReceipt.PSObject.Properties["mode"]
                if ($null -eq $schemaProperty -or
                    -not (Test-JsonIntegerValue -Value $schemaProperty.Value) -or
                    [Convert]::ToInt64($schemaProperty.Value) -notin @(2, 3) -or
                    $null -eq $initializedProperty -or
                    $initializedProperty.Value -isnot [bool] -or
                    $null -eq $modeProperty -or
                    $modeProperty.Value -isnot [string] -or
                    $modeProperty.Value -cnotin @("ready", "pending", "stale", "unavailable")) {
                    throw "invalid setup receipt"
                }
                $setupInitialized = [bool]$initializedProperty.Value
                $setupMode = [string]$modeProperty.Value
                $indexedSessions = Get-OptionalUnsignedCount -Json $setupReceipt -Name "indexed_sessions"
                $indexedItems = Get-OptionalUnsignedCount -Json $setupReceipt -Name "indexed_items"
                $setupVerified = $true

            } catch {
                $setupVerified = $false
                $setupStatus = 1
            }
        }
        if ($setupStatus -eq 0) {
            Send-InstallStage -Stage "setup" -Status "completed"
        } else {
            Send-InstallStage -Stage "setup" -Status "failed"
        }
    } else {
        Send-InstallStage -Stage "setup" -Status "skipped"
    }
    Configure-InstallPath -InstallPath $installPath -ModifyPath $modifyPath
    if ($setupVerified -and $setupInitialized -and $indexedSessions -gt 0) {
        Write-ReceiptItem (
            "Found $(Format-ReceiptCount $indexedSessions) sessions"
        )
    } elseif ($setupVerified -and $setupInitialized -and $indexedItems -ge 0) {
        Write-ReceiptItem (
            "Found $(Format-ReceiptCount $indexedItems) records"
        )
    }

    $indexingContinues = $false
    if ($setupVerified) {
        if ($setupMode -ceq "ready") {
            Write-ReceiptItem "Index ready"
        } elseif ($setupMode -in @("pending", "stale")) {
            Write-ReceiptItem "Indexing started"
            $indexingContinues = $daemonEnabled
        } elseif ($setupMode -ceq "unavailable") {
            if ($daemonConfigurationDisabled) {
                Write-ReceiptItem ("Indexing deferred " + [char]0x2014 + " daemon disabled")
            } elseif ($setupNoDaemon) {
                Write-ReceiptItem ("Indexing deferred " + [char]0x2014 + " daemon not started")
            }
        }
    }

    if ($runSetup -and $setupStatus -ne 0) {
        Write-ReceiptWarning "Setup failed. Retry: ctx setup"
    }
    if ($skillInstallFailed) {
        Write-ReceiptWarning (
            "Agent skill setup failed. Retry: ctx integrations install skills"
        )
    }
    if ($indexingContinues) {
        Write-Host ""
        Write-Host "Indexing will continue in the background."
    }
    if ($setupVerified) {
        Write-Host ""
        Write-Host '  Search:    ctx search "test failure"'
        Write-Host "  Progress:  ctx index watch"
        Write-Host "  Status:    ctx status"
    }
    if (-not [string]::IsNullOrWhiteSpace($script:pathResult)) {
        Write-Host ""
        Write-Host $script:pathResult
    }
    if ($setupStatus -ne 0) {
        Send-InstallStage -Stage "installer" -Status "failed"
        $installerCompleted = $true
        exit $setupStatus
    }
    Send-InstallStage -Stage "installer" -Status "completed"
    $installerCompleted = $true
} catch {
    if (-not $installerCompleted) {
        Send-InstallStage -Stage "installer" -Status "failed"
    }
    throw
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
`;
}
