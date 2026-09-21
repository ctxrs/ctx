import { FROZEN_BRIDGE_VERSION } from "./cli-install-bridge.js";
import {
  renderCliInstallPowerShellManagedPairMetadata,
  renderCliInstallPowerShellManagedPairPaths,
} from "./cli-install-powershell-managed-pair.js";

export function renderCliInstallPowerShellReleasePreparation() {
  return `    $readReleasePhaseMetadata = {
    $phaseRoot = Join-Path $tempRoot $releasePhase
    if (-not (Test-Path -LiteralPath $phaseRoot)) { New-Item -ItemType Directory -Path $phaseRoot | Out-Null }
    $metadataFile = Join-Path $phaseRoot "metadata.env"
    $metadataSignatureFile = Join-Path $phaseRoot "metadata.env.sig"
    if (-not (Test-Path -LiteralPath $metadataFile)) {
        Read-Metadata -Source $Metadata -Destination $metadataFile
        Read-DetachedSignature -Source $metadataSignature -Destination $metadataSignatureFile
    }
    Verify-MetadataSignature -MetadataPath $metadataFile -SignaturePath $metadataSignatureFile
    $metadataText = Get-Content -LiteralPath $metadataFile | Where-Object {
        $_ -notmatch '^\\s*#' -and $_ -match '='
    }
    $metadataValues = ConvertFrom-StringData -StringData ($metadataText -join "\`n")

    $schemaVersion = Get-MetadataValue $metadataValues "CTX_RELEASE_SCHEMA_VERSION"
    $version = Get-MetadataValue $metadataValues "CTX_RELEASE_VERSION"
    $baseUrl = Get-MetadataValue $metadataValues "CTX_RELEASE_BASE_URL"
    $artifact = Get-MetadataValue $metadataValues "CTX_RELEASE_ARTIFACT_windows_x64"
    $checksum = Get-MetadataValue $metadataValues "CTX_RELEASE_SHA256_windows_x64"
    $releaseChannel = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_CHANNEL" $channel
    $sourceCommit = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_SOURCE_COMMIT" ""
    $publishedAt = Get-MetadataValueOrDefault $metadataValues "CTX_RELEASE_PUBLISHED_AT" ""

    if ($schemaVersion -ne "1") {
        Fail "unsupported metadata schema: $schemaVersion"
    }
    if ($releaseChannel -cne $channel) {
        Fail "metadata channel $releaseChannel does not match requested channel $channel"
    }
    if ($baseUrl -notmatch '^https://' -and
        -not ($env:CTX_ALLOW_CUSTOM_RELEASE_BASE_URL -eq "1" -and
            $baseUrl -match '^file:///')) {
        Fail "metadata base URL must be HTTPS"
    }
    Assert-AllowedBaseUrl $baseUrl
    if ($checksum -notmatch '^[0-9a-fA-F]{64}$') {
        Fail "checksum for windows-x64 is not a SHA-256 hex digest"
    }
    if ($checksum -eq "0000000000000000000000000000000000000000000000000000000000000000") {
        Fail "checksum for windows-x64 is a placeholder"
    }
    Assert-SafeArtifactName $artifact
    ${renderCliInstallPowerShellManagedPairMetadata()}
    $pairInstallRoot = Split-Path -Parent $BinDir
    if ($managedPair -and
        (Split-Path -Leaf $BinDir) -cne "bin") {
        Fail "managed-pair install directory must be <root>\\bin"
    }

    $artifactUrl = $baseUrl.TrimEnd("/") + "/" + $artifact
    $downloadPath = Join-Path $phaseRoot $artifact
    $compressedArtifactUrl = $artifactUrl + ".gz"
    $compressedDownloadPath = $downloadPath + ".gz"
    ${renderCliInstallPowerShellManagedPairPaths()}

    }
    $finalMetadata = $Metadata
    $finalMetadataSignature = $metadataSignature
    $releasePhase = "final"
    . $readReleasePhaseMetadata
    $finalVersion = $version
    $finalChecksum = $checksum
    if ($channel -ceq "stable" -and (Compare-ReleaseVersion $version "${FROZEN_BRIDGE_VERSION}") -lt 0) {
        Fail "stable installer targets before ${FROZEN_BRIDGE_VERSION} are unsupported"
    }
    if ($explicitMetadata -and $semanticEnabled) {
        Fail "explicit metadata cannot authorize Semantic repair through the installed release; use the default installer feed"
    }
    $bridgeRequired = $false
    if ((Test-Path -LiteralPath $installPath) -or (Test-Path -LiteralPath $markerPath)) {
        if ($explicitMetadata) {
            Fail "managed reinstall cannot honor an explicit metadata target; use the default installer feed"
        }
        $pendingPath = Join-Path $BinDir ".ctx.upgrade-install-transaction.json"
        $priorInstall = $null
        if (Test-Path -LiteralPath $pendingPath) {
            # An intact old image still selects B. Partial publication is
            # classified after the existing recovery owner has run.
            try { $priorInstall = Read-ExistingManagedInstall } catch { }
        } else {
            $priorInstall = Read-ExistingManagedInstall
        }
        if ($null -ne $priorInstall) {
            if ($channel -ceq "stable") {
                $order = Compare-ReleaseVersion $finalVersion $priorInstall.version
                if ($order -lt 0) { Fail "refusing to downgrade the managed ctx installation" }
                if ($order -eq 0 -and $priorInstall.sha256.ToLowerInvariant() -cne $checksum.ToLowerInvariant()) {
                    Fail "signed release differs from the installed identity at the same version"
                }
                $bridgeRequired = (Compare-ReleaseVersion $priorInstall.version "${FROZEN_BRIDGE_VERSION}") -lt 0
            }
        }
    }
`;
}
