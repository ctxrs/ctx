export const CLI_INSTALL_POWERSHELL_PATH_IDENTITY = `function ConvertTo-ManagedInstallComparablePath([string]$Path) {
    $comparablePath = $Path
    if ($comparablePath.StartsWith("\\\\?\\", [System.StringComparison]::Ordinal)) {
        if ($comparablePath.Substring(4) -cnotmatch '^[A-Za-z]:\\\\') {
            return $null
        }
        $comparablePath = $comparablePath.Substring(4)
    }
    if ($comparablePath -cnotmatch '^[A-Za-z]:\\\\') {
        return $null
    }
    $relativePath = $comparablePath.Substring(3)
    if ([string]::IsNullOrEmpty($relativePath) -or
        $relativePath.Contains("/") -or
        $relativePath.Contains("\\\\") -or
        $relativePath.EndsWith([IO.Path]::DirectorySeparatorChar) -or
        $relativePath -cmatch '(^|\\\\)\\.{1,2}(\\\\|$)') {
        return $null
    }
    return $comparablePath
}

function Test-ManagedInstallPathIdentity([object]$Candidate, [string]$Expected) {
    if ($Candidate -isnot [string]) {
        return $false
    }
    try {
        $candidatePath = ConvertTo-ManagedInstallComparablePath -Path $Candidate
        $expectedPath = ConvertTo-ManagedInstallComparablePath -Path $Expected
        return ($null -ne $candidatePath -and
            $null -ne $expectedPath -and
            $candidatePath -ceq $expectedPath)
    } catch {
        return $false
    }
}
`;

export const CLI_INSTALL_POWERSHELL_MANAGED_INSTALL = `${CLI_INSTALL_POWERSHELL_PATH_IDENTITY}
function Get-ExistingInstallPairState {
    $binaryExists = Test-Path -LiteralPath $installPath
    $markerExists = Test-Path -LiteralPath $markerPath
    if (-not $binaryExists -and -not $markerExists) {
        return "fresh"
    }
    if ($binaryExists -and -not $markerExists) {
        Fail "an unmanaged ctx executable already exists at $installPath; move it to a backup path outside $BinDir, rerun this installer, and delete the backup only after the managed install succeeds"
    }
    if ($markerExists -and -not $binaryExists) {
        Fail "the managed ctx install is corrupted: its hosted-install marker exists at $markerPath but its executable is missing"
    }
    return "managed"
}

function Read-ExistingManagedInstall {
    $installState = Get-ExistingInstallPairState
    if ($installState -ceq "fresh") {
        return $null
    }
    $binaryItem = Get-Item -LiteralPath $installPath -Force
    $markerItem = Get-Item -LiteralPath $markerPath -Force
    if (($binaryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        ($markerItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        Fail "an existing ctx install must not contain reparse points"
    }
    if ($markerItem.Length -lt 2 -or $markerItem.Length -gt 64KB) {
        Fail "prior managed install marker has an invalid size"
    }
    try {
        $existingMarker = [IO.File]::ReadAllText($markerPath) | ConvertFrom-Json
    } catch {
        Fail "prior managed install marker is invalid JSON"
    }
    if ($existingMarker.schema_version -ne 1 -or
        $existingMarker.manager -isnot [string] -or
        $existingMarker.manager -cne "ctx-hosted-installer" -or
        $existingMarker.platform -isnot [string] -or
        $existingMarker.platform -cne "windows-x64" -or
        -not (Test-ManagedInstallPathIdentity -Candidate $existingMarker.install_path -Expected $installPath) -or
        $existingMarker.version -isnot [string] -or
        $existingMarker.version -notmatch '^[0-9A-Za-z.+-]+$' -or
        $existingMarker.sha256 -isnot [string] -or
        $existingMarker.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
        Fail "prior managed install marker identity is invalid"
    }
    $existingDigest = (
        Get-FileHash -Algorithm SHA256 -LiteralPath $installPath
    ).Hash.ToLowerInvariant()
    if ($existingDigest -cne $existingMarker.sha256.ToLowerInvariant()) {
        Fail "prior managed binary differs from its install marker"
    }
    return $existingMarker
}

function Assert-ManagedUpgradeResult([object]$Result) {
    if ($null -eq $Result -or $Result -isnot [pscustomobject]) {
        Fail "managed ctx upgrade did not return a typed lifecycle receipt"
    }
    if (-not (($Result.schema_version -is [int]) -or
            ($Result.schema_version -is [long])) -or
        $Result.schema_version -ne 1 -or
        $Result.command -isnot [string] -or
        $Result.command -cne "upgrade" -or
        $Result.ok -isnot [bool] -or
        -not $Result.ok -or
        $Result.status -isnot [string] -or
        $Result.status -cnotin @("applied", "up_to_date", "scheduled") -or
        $Result.message -isnot [string] -or
        $Result.managed -isnot [bool] -or
        $Result.update_available -isnot [bool] -or
        $Result.update_was_available -isnot [bool] -or
        $Result.applied -isnot [bool] -or
        $Result.dry_run -isnot [bool] -or
        $Result.dry_run) {
        Fail "managed ctx upgrade did not return a typed lifecycle receipt"
    }
    if ($Result.status -cin @("applied", "scheduled") -and
        ($Result.upgrade_attempt_id -isnot [string] -or
            [string]::IsNullOrWhiteSpace($Result.upgrade_attempt_id))) {
        Fail "managed ctx upgrade omitted its lifecycle attempt identity"
    }
    if ($Result.status -ceq "scheduled" -and $null -eq $Result.current_version) {
        # Core reschedules retained Windows replacements before building a plan.
        # This permits only waiting; signed target identity still proves completion.
        foreach ($name in "current_version,latest_version,channel,platform,metadata_url,artifact_url,install_path".Split(",")) {
            if ($null -ne $Result.$name) {
                Fail "managed ctx recovery returned an inconsistent lifecycle receipt"
            }
        }
        if ($Result.managed -or $Result.update_available -or
            $Result.update_was_available -or $Result.applied) {
            Fail "managed ctx recovery returned an inconsistent lifecycle receipt"
        }
        return [string]$Result.status
    }
    if ($Result.current_version -isnot [string] -or
        $Result.latest_version -isnot [string] -or
        $Result.latest_version -cne $version -or
        $Result.channel -isnot [string] -or
        $Result.channel -cne $channel -or
        $Result.platform -isnot [string] -or
        $Result.platform -cne "windows-x64" -or
        $Result.metadata_url -isnot [string] -or
        $Result.artifact_url -isnot [string] -or
        -not (Test-ManagedInstallPathIdentity -Candidate $Result.install_path -Expected $installPath) -or
        -not $Result.managed) {
        Fail "managed ctx upgrade did not return a typed lifecycle receipt"
    }
    if (($Result.status -ceq "applied" -and -not $Result.applied) -or
        ($Result.status -cne "applied" -and $Result.applied) -or
        ($Result.status -cne "scheduled" -and
            $Result.current_version -cne $version)) {
        Fail "managed ctx upgrade returned an inconsistent lifecycle receipt"
    }
    return [string]$Result.status
}

function Assert-LegacyManagedUpgradeResult([object]$Result, [string]$PriorVersion, [bool]$RequirePath) {
    if ($null -eq $Result -or $Result -isnot [pscustomobject]) {
        Fail "released ctx upgrade did not return a typed lifecycle receipt"
    }
    foreach ($name in "command,status,message,current_version,latest_version,channel,platform,metadata_url,artifact_url,install_path".Split(",")) {
        if ($Result.$name -isnot [string]) { Fail "released ctx upgrade returned an untyped $name" }
    }
    foreach ($name in "ok,update_available,managed,applied,dry_run".Split(",")) {
        if ($Result.$name -isnot [bool]) { Fail "released ctx upgrade returned an untyped $name" }
    }
    if (-not (($Result.schema_version -is [int]) -or ($Result.schema_version -is [long])) -or
        $Result.schema_version -ne 1 -or $Result.command -cne "upgrade" -or
        -not $Result.ok -or $Result.status -cne "scheduled" -or
        $Result.current_version -cne $PriorVersion -or $Result.latest_version -cne $version -or
        -not $Result.update_available -or $Result.channel -cne $channel -or
        $Result.platform -cne "windows-x64" -or
        -not (Test-ManagedInstallPathIdentity -Candidate $Result.install_path -Expected $installPath) -or
        -not $Result.managed -or ($RequirePath -and $Result.path -isnot [pscustomobject]) -or
        $Result.applied -or $Result.dry_run) {
        Fail "released ctx upgrade returned an inconsistent lifecycle receipt"
    }
    return [string]$Result.status
}

function Test-InstalledTargetIdentity {
    try {
        $binaryGuard = [CtxInstallerPathGuard]::AcquireLeaf($installPath)
        $markerGuard = [CtxInstallerPathGuard]::AcquireLeaf($markerPath)
        try {
            if (-not $binaryGuard.LeafExists -or -not $markerGuard.LeafExists) {
                return $false
            }
            $binaryGuard.AssertUnchanged()
            $markerGuard.AssertUnchanged()
            $installedDigest = (
                Get-FileHash -Algorithm SHA256 -LiteralPath $installPath
            ).Hash.ToLowerInvariant()
            if ($installedDigest -cne $actualChecksum.ToLowerInvariant()) {
                return $false
            }
            $installedMarker = [IO.File]::ReadAllText($markerPath) |
                ConvertFrom-Json
            return (
                ($releasePhase -cne "final" -or -not $managedPair -or $installedMarker.managed_pair -eq $true) -and
                $installedMarker.schema_version -eq 1 -and
                $installedMarker.manager -is [string] -and
                $installedMarker.manager -ceq "ctx-hosted-installer" -and
                (Test-ManagedInstallPathIdentity -Candidate $installedMarker.install_path -Expected $installPath) -and
                $installedMarker.platform -is [string] -and
                $installedMarker.platform -ceq "windows-x64" -and
                $installedMarker.version -is [string] -and
                $installedMarker.version -ceq $version -and
                $installedMarker.sha256 -is [string] -and
                $installedMarker.sha256.ToLowerInvariant() -ceq
                    $actualChecksum.ToLowerInvariant()
            )
        } finally {
            $markerGuard.Dispose()
            $binaryGuard.Dispose()
        }
    } catch {
        return $false
    }
}

function Invoke-HostedInstallTransaction {
    $transaction = Invoke-ExecutableCaptured -Executable $downloadPath -Arguments @(
        "upgrade", "--hosted-transaction", "install",
        "--install-path", $installPath,
        "--attempt-id", $installAttemptId,
        "--marker-source", $markerSourcePath,
        "--binary-sha256", $actualChecksum
    )
    if ($transaction.ExitCode -ne 0) {
        Write-BoundedCapturedChildError -ErrorPath $transaction.ErrorPath
        Fail "ctx could not complete its crash-recoverable hosted install transaction (exit code $($transaction.ExitCode))"
    }
    $required = @(
        "schema_version", "command", "ok", "status", "attempt_id",
        "install_path", "binary_sha256", "marker_sha256"
    )
    $hostedProof = Read-BoundedSuccessReceipt -Path $transaction.OutputPath -MaximumBytes 64KB -RequiredNames $required -OptionalNames @("warnings") -AllowPrettyJson:($version -ceq "1.3.1")
    if ($null -eq $hostedProof) {
        Fail "ctx did not return valid hosted install transaction proof"
    }
    $result = $hostedProof.Receipt
    if (-not (($result.schema_version -is [int]) -or
            ($result.schema_version -is [long])) -or
        $result.schema_version -ne 1 -or
        $result.command -isnot [string] -or
        $result.command -cne "hosted_install_transaction" -or
        $result.ok -isnot [bool] -or -not $result.ok -or
        $result.status -isnot [string] -or
        $result.status -cne "committed" -or
        $result.attempt_id -isnot [string] -or
        $result.attempt_id -cne $installAttemptId -or
        $result.install_path -isnot [string] -or
        -not (Test-ManagedInstallPathIdentity -Candidate $result.install_path -Expected $installPath) -or
        $result.binary_sha256 -isnot [string] -or
        $result.binary_sha256 -cne $actualChecksum.ToLowerInvariant() -or
        $result.marker_sha256 -isnot [string] -or
        $result.marker_sha256 -cnotmatch '^[0-9a-f]{64}$') {
        Fail "ctx returned invalid hosted install transaction proof"
    }
    if (-not (Test-InstalledTargetIdentity)) {
        Fail "ctx hosted install transaction did not publish the signed managed identity"
    }
    Write-SuccessReceiptWarnings -Warnings $hostedProof.Warnings
}

function Invoke-ManagedCoreUpgrade {
    $upgradeMetadataUri = (
        [System.Uri]::new([System.IO.Path]::GetFullPath($metadataFile))
    ).AbsoluteUri
    $upgradeSignatureUri = (
        [System.Uri]::new([System.IO.Path]::GetFullPath($metadataSignatureFile))
    ).AbsoluteUri
    if ($legacyManagedReinstall) {
        # Released pre-0.26 readers do not use general Windows file-URI parsing.
        $upgradeMetadataUri = $Metadata
        $upgradeSignatureUri = $metadataSignature
    }
    $previousUpgradeSemantic = [Environment]::GetEnvironmentVariable("CTX_SEARCH_SEMANTIC", "Process")
    $previousUpgradeMetadata = [Environment]::GetEnvironmentVariable(
        "CTX_RELEASE_METADATA_URL",
        "Process"
    )
    $previousUpgradeSignature = [Environment]::GetEnvironmentVariable(
        "CTX_RELEASE_METADATA_SIGNATURE_URL",
        "Process"
    )
    try {
        if ($releasePhase -ceq "bridge") {
            [Environment]::SetEnvironmentVariable("CTX_SEARCH_SEMANTIC", "0", "Process")
        }
        [Environment]::SetEnvironmentVariable(
            "CTX_RELEASE_METADATA_URL",
            $upgradeMetadataUri,
            "Process"
        )
        [Environment]::SetEnvironmentVariable(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            $upgradeSignatureUri,
            "Process"
        )
        if ($legacyManagedReinstall) {
            $upgradeCommand = Invoke-CtxCaptured -Arguments @("upgrade", "--channel", $channel, "--json")
        } else {
            $upgradeCommand = Invoke-CtxCaptured -Arguments @("upgrade", "--channel", $channel, "--format=json")
        }
    } finally {
        [Environment]::SetEnvironmentVariable("CTX_SEARCH_SEMANTIC", $previousUpgradeSemantic, "Process")
        [Environment]::SetEnvironmentVariable(
            "CTX_RELEASE_METADATA_URL",
            $previousUpgradeMetadata,
            "Process"
        )
        [Environment]::SetEnvironmentVariable(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            $previousUpgradeSignature,
            "Process"
        )
    }
    if ($upgradeCommand.ExitCode -ne 0) {
        Write-BoundedCapturedChildError -ErrorPath $upgradeCommand.ErrorPath
        Fail "installed ctx could not complete its managed lifecycle handoff (exit code $($upgradeCommand.ExitCode)); rerun this installer to finish any retained attempt"
    }
    $currentRequired = "schema_version,command,ok,status,message,current_version,latest_version,update_available,update_was_available,channel,platform,metadata_url,artifact_url,install_path,managed,applied,dry_run,warnings,upgrade_attempt_id".Split(",")
    $legacyRequired = "schema_version,command,ok,status,message,current_version,latest_version,update_available,channel,platform,metadata_url,artifact_url,install_path,managed,applied,dry_run,warnings".Split(",")
    if ($legacyReceiptHasPath) { $legacyRequired += "path" }
    $upgradeRequired = if ($legacyManagedReinstall) { $legacyRequired } else { $currentRequired }
    # Ordinary CLI upgrade emits pretty JSON on supported versions; the compact
    # managed-pair-apply protocol keeps its separate stricter receipt contract.
    $upgradeProof = Read-BoundedSuccessReceipt -Path $upgradeCommand.OutputPath -MaximumBytes 64KB -RequiredNames $upgradeRequired -OptionalNames @() -AllowEmptyWarnings -AllowPrettyJson
    if ($null -eq $upgradeProof) {
        Fail "managed ctx upgrade did not return valid JSON lifecycle proof"
    }
    $upgradeResult = $upgradeProof.Receipt
    $upgradeStatus = if ($legacyManagedReinstall) {
        Assert-LegacyManagedUpgradeResult -Result $upgradeResult -PriorVersion $existingManagedInstall.version -RequirePath $legacyReceiptHasPath
    } else {
        Assert-ManagedUpgradeResult -Result $upgradeResult
    }
    Write-SuccessReceiptWarnings -Warnings $upgradeProof.Warnings
    if ($upgradeStatus -eq "scheduled") {
        $upgradeDeadline = [DateTime]::UtcNow.AddSeconds(60)
        while ([DateTime]::UtcNow -lt $upgradeDeadline) {
            if (Test-InstalledTargetIdentity) {
                return
            }
            Start-Sleep -Milliseconds 100
        }
        if ($legacyManagedReinstall) {
            Fail "released ctx replacement is still blocked; disable any opted-in legacy daemon, wait for it to exit or stop/reboot the host, then rerun this installer"
        }
        Fail "managed ctx replacement did not finish before its lifecycle deadline"
    }
    if (-not (Test-InstalledTargetIdentity)) {
        Fail "managed ctx upgrade did not publish the signed executable and marker"
    }
}`;
