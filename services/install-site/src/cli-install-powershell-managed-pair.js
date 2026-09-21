import { managedPairApplyArguments, MANAGED_PAIR_APPLY_RECEIPT } from "./cli-install-managed-pair-contract.js";

export function renderCliInstallPowerShellManagedPairMetadata() {
  return `$managedPair = $false
$releasedPairInstall = $false
$pairEnvelopeArtifact = ""
if ((Compare-ReleaseVersion $version "1.5.0") -lt 0) {
$pairEnvelopeArtifact = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_ENVELOPE_windows_x64" ""
$pairCoreObjectKey = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_windows_x64" ""
$pairCoreChecksum = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_windows_x64" ""
$pairCompanionObjectKey = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_windows_x64" ""
$pairCompanionChecksum = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_windows_x64" ""
$managedPair = -not [string]::IsNullOrWhiteSpace($pairEnvelopeArtifact)
# This released Core predates candidate-owned pair apply. Select its documented
# installed-Core protocol from authenticated release metadata, never by retrying
# a failed command with a different protocol.
$releasedPairInstall = $managedPair -and $version -ceq "1.3.1"

function Get-ManagedPairObjectName([string]$ObjectKey, [string]$ExpectedChecksum, [string]$Label) {
    if ($ObjectKey -cnotmatch '^sha256/([0-9a-fA-F]{64})/([^/]+)$') {
        Fail "$Label object key is invalid"
    }
    $keyChecksum = $Matches[1].ToLowerInvariant()
    $objectName = $Matches[2]
    Assert-SafeArtifactName $objectName
    if ($ExpectedChecksum -notmatch '^[0-9a-fA-F]{64}$' -or
        $keyChecksum -cne $ExpectedChecksum.ToLowerInvariant()) {
        Fail "$Label object key does not match its checksum"
    }
    return $objectName
}

if ($managedPair) {
    Assert-SafeArtifactName $pairEnvelopeArtifact
    $pairCoreName = Get-ManagedPairObjectName $pairCoreObjectKey $pairCoreChecksum "managed-pair Core"
    $pairCompanionName = Get-ManagedPairObjectName $pairCompanionObjectKey $pairCompanionChecksum "managed-pair companion"
    if ($pairCoreChecksum.ToLowerInvariant() -cne $checksum.ToLowerInvariant()) {
        Fail "managed-pair Core checksum differs from release metadata"
    }
} elseif (-not [string]::IsNullOrWhiteSpace(
    $pairCoreObjectKey + $pairCoreChecksum + $pairCompanionObjectKey + $pairCompanionChecksum
)) {
    Fail "managed-pair component metadata is present without an envelope"
}
}`;
}

export function renderCliInstallPowerShellManagedPairPaths() {
  return `$pairEnvelopePath = Join-Path $phaseRoot "managed-pair-envelope.json"
$pairCompanionPath = Join-Path $phaseRoot "ctx-pro.exe"
if ($managedPair) {
    $pairEnvelopeUrl = $baseUrl.TrimEnd("/") + "/" + $pairEnvelopeArtifact
    $pairCompanionUrl = $baseUrl.TrimEnd("/") + "/" + $pairCompanionObjectKey
}`;
}

export function renderCliInstallPowerShellManagedPairDownload() {
  return `if ($managedPair) {
        Read-Artifact -Source $pairEnvelopeUrl -Destination $pairEnvelopePath -MaxBytes 2097152 -TimeoutSeconds 300
        Read-Artifact -Source $pairCompanionUrl -Destination $pairCompanionPath
        $actualCompanionChecksum = (
            Get-FileHash -Algorithm SHA256 -LiteralPath $pairCompanionPath
        ).Hash.ToLowerInvariant()
        if ($actualCompanionChecksum -cne $pairCompanionChecksum.ToLowerInvariant()) {
            Fail "checksum mismatch for $($pairCompanionName): expected $pairCompanionChecksum, got $actualCompanionChecksum"
        }
    }`;
}

export function renderCliInstallPowerShellManagedPairPublication() {
  const applyArguments = managedPairApplyArguments({
    installRoot: "$pairInstallRoot",
    envelope: "$pairEnvelopePath",
    core: "$downloadPath",
    companion: "$pairCompanionPath",
    marker: "$MarkerSource",
  }, (value) => `"${value}"`).join(",\n            ");
  const requiredReceiptNames = Object.keys(MANAGED_PAIR_APPLY_RECEIPT).map((name) => `"${name}"`).join(", ");
  return `function Invoke-ManagedPairApply([string]$MarkerSource, [bool]$Required) {
        # The pair owner requires private directories and input files, whereas
        # the old installer protected only bin and its two existing leaves.
        if ($managedPair) {
        foreach ($directory in @($pairInstallRoot, (Join-Path $pairInstallRoot "libexec"),
            (Join-Path $pairInstallRoot "share"), (Join-Path $pairInstallRoot "share/ctx"))) {
            if (Test-Path -LiteralPath $directory) {
                Protect-ManagedPath -Path $directory -Directory
            }
        }
        foreach ($inputPath in @($pairEnvelopePath, $downloadPath, $pairCompanionPath, $MarkerSource)) {
            Protect-ManagedPath -Path (Split-Path -Parent $inputPath) -Directory
            Protect-ManagedPath -Path $inputPath
        }
        } else {
            # Recovery inputs already belong to the retained transaction. Do
            # not change their ACLs before the native owner validates them.
            Protect-ManagedPath -Path (Split-Path -Parent $downloadPath) -Directory
            Protect-ManagedPath -Path $downloadPath
        }
        # Candidate Core joins its fixed slots to this root. Use the native
        # local-disk namespace so those joins keep Windows separators.
        if ($pairInstallRoot -cmatch '^[A-Za-z]:\\\\') {
            $pairInstallRoot = "\\\\?\\" + $pairInstallRoot
        }
        $pairApply = Invoke-ExecutableCaptured -Executable $downloadPath -Arguments @(
            ${applyArguments}
        )
        if ($pairApply.ExitCode -ne 0) {
            if ($Required) {
                Write-BoundedCapturedChildError -ErrorPath $pairApply.ErrorPath
                Fail "ctx managed-pair installation did not complete (exit code $($pairApply.ExitCode), release $version); rerun this installer command to retry safely"
            }
            return $false
        }
        $pairProof = Read-BoundedSuccessReceipt -Path $pairApply.OutputPath -MaximumBytes 512 -RequiredNames @(${requiredReceiptNames}) -OptionalNames @("warnings")
        $pairReceiptValid = $null -ne $pairProof -and
            (($pairProof.Receipt.schema_version -is [int]) -or
                ($pairProof.Receipt.schema_version -is [long])) -and
            $pairProof.Receipt.schema_version -eq ${MANAGED_PAIR_APPLY_RECEIPT.schema_version} -and
            $pairProof.Receipt.command -is [string] -and
            $pairProof.Receipt.command -ceq "${MANAGED_PAIR_APPLY_RECEIPT.command}" -and
            $pairProof.Receipt.ok -is [bool] -and $pairProof.Receipt.ok -and
            $pairProof.Receipt.status -is [string] -and
            $pairProof.Receipt.status -ceq "${MANAGED_PAIR_APPLY_RECEIPT.status}"
        if (-not $pairReceiptValid) {
            if ($Required) {
                Write-BoundedCapturedChildError -ErrorPath $pairApply.ErrorPath
                Fail "candidate Core returned invalid managed-pair apply proof (release $version); rerun this installer command to retry safely"
            }
            return $false
        }
        Write-SuccessReceiptWarnings -Warnings $pairProof.Warnings
        return $true
}

function Resume-InterruptedManagedPair {
        $pendingPath = Join-Path $BinDir ".ctx.upgrade-install-transaction.json"
        if (-not (Test-Path -LiteralPath $pendingPath -PathType Leaf)) { return }
        # A usable installed Core owns live-daemon and pending upgrade recovery.
        if (Test-Path -LiteralPath $installPath -PathType Leaf) {
            try { if ($null -ne (Read-ExistingManagedInstall)) { return } } catch { }
        }
        if ((Compare-ReleaseVersion $version "1.5.0") -ge 0) {
            if ((Split-Path -Leaf $BinDir) -cne "bin") {
                Fail "managed-pair recovery directory must be <root>\\bin"
            }
            # Function-local inputs: the downloaded candidate remains request.core.
            $retainedRoot = Join-Path $pairInstallRoot "share/ctx/.managed-pair-apply-v1"
            $pairEnvelopePath = Join-Path $retainedRoot "share/ctx/managed-pair-envelope.json"
            $pairCompanionPath = Join-Path $retainedRoot "libexec/ctx-pro.exe"
            $retainedMarker = Join-Path $retainedRoot "bin/ctx.exe.install.json"
            $null = Invoke-ManagedPairApply -MarkerSource $retainedMarker -Required $true
        } elseif ($managedPair -and -not $releasedPairInstall -and
                  (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
            $null = Invoke-ManagedPairApply -MarkerSource $markerSourcePath -Required $false
        }
}

function Invoke-ReleasedManagedPairInstall {
        if (-not $releasedPairInstall -or -not (Test-InstalledTargetIdentity)) {
            Fail "released managed-pair installation requires the verified installed Core"
        }
        $pairInstall = Invoke-ExecutableCaptured -Executable $installPath -Arguments @(
            "--ctx-core-hosted-pair-install-v1",
            $pairEnvelopePath,
            $downloadPath,
            $pairCompanionPath,
            $markerSourcePath
        )
        if ($pairInstall.ExitCode -ne 0) {
            Write-BoundedCapturedChildError -ErrorPath $pairInstall.ErrorPath
            Fail "ctx Pro installation failed (exit code $($pairInstall.ExitCode)); the verified Core install was retained. Rerun this installer to complete the signed pair"
        }
        $proof = Read-BoundedSuccessReceipt -Path $pairInstall.OutputPath -MaximumBytes 512 -RequiredNames @("schema_version", "command", "release_name", "rollback_generation", "status") -OptionalNames @() -AllowPrettyJson
        if ($null -eq $proof -or
            -not (($proof.Receipt.schema_version -is [int]) -or ($proof.Receipt.schema_version -is [long])) -or
            $proof.Receipt.schema_version -ne 1 -or
            $proof.Receipt.command -isnot [string] -or
            $proof.Receipt.command -cne "hosted_managed_pair_install" -or
            $proof.Receipt.release_name -isnot [string] -or
            $proof.Receipt.release_name -cne ("v" + $version) -or
            -not (($proof.Receipt.rollback_generation -is [int]) -or ($proof.Receipt.rollback_generation -is [long])) -or
            $proof.Receipt.rollback_generation -lt 0 -or
            $proof.Receipt.status -isnot [string] -or
            $proof.Receipt.status -cne "committed" -or
            -not (Test-InstalledTargetIdentity)) {
            Fail "installed Core returned invalid released managed-pair transaction proof"
        }
}`;
}
