export function renderCliInstallPowerShellValidation({
  normalizedBase,
  normalizedChannel,
  normalizedInstallAttemptId,
  normalizedMetadataPublicKeyModulusBase64Url,
  normalizedMetadataPublicKeyExponentBase64Url,
}) {
  return `function Remove-ConfigComment([string]$Line) {
    $inSingleQuote = $false
    $inDoubleQuote = $false
    $escaped = $false
    for ($index = 0; $index -lt $Line.Length; $index += 1) {
        $character = $Line[$index]
        if ($inDoubleQuote) {
            if ($escaped) {
                $escaped = $false
            } elseif ($character -eq [char]92) {
                $escaped = $true
            } elseif ($character -eq '"') {
                $inDoubleQuote = $false
            }
            continue
        }
        if ($inSingleQuote) {
            if ($character -eq "'") {
                $inSingleQuote = $false
            }
            continue
        }
        if ($character -eq "#") {
            return $Line.Substring(0, $index)
        }
        if ($character -eq '"') {
            $inDoubleQuote = $true
        } elseif ($character -eq "'") {
            $inSingleQuote = $true
        }
    }
    return $Line
}

function Test-ConfigQuotedString([string]$Value) {
    if ($Value.Length -lt 2) {
        return $false
    }
    return (
        ($Value.StartsWith('"') -and $Value.EndsWith('"')) -or
        ($Value.StartsWith("'") -and $Value.EndsWith("'"))
    )
}

function Get-ConfigStringValue(
    [string]$FullKey,
    [string]$Value,
    [int]$LineNumber
) {
    if (-not (Test-ConfigQuotedString -Value $Value)) {
        Fail "$FullKey at line $LineNumber must be a quoted string"
    }
    $parsed = $Value.Substring(1, $Value.Length - 2)
    if ([string]::IsNullOrWhiteSpace($parsed)) {
        Fail "$FullKey at line $LineNumber must not be empty"
    }
    return $parsed
}

function Assert-PersistedConfigValue(
    [string]$FullKey,
    [string]$Value,
    [int]$LineNumber
) {
    # Only validate values consumed by installer preflight. Core owns the full schema.
    switch -CaseSensitive ($FullKey) {
        { $_ -cin @(
            "daemon.enabled",
            "search.semantic"
        ) } {
            if ($Value -cnotin @("true", "false")) {
                Fail "$FullKey at line $LineNumber must be a boolean"
            }
            return
        }
        "indexing.mode" {
            $parsed = Get-ConfigStringValue -FullKey $FullKey -Value $Value -LineNumber $LineNumber
            if ($parsed.ToLowerInvariant() -notin @("auto", "automatic", "manual")) {
                Fail "$FullKey at line $LineNumber must be either auto or manual"
            }
            return
        }
    }
}

function Get-PersistedConfigControls {
    $controls = [pscustomobject]@{
        SemanticEnabled = $false
        DaemonDisabled = $false
    }
    $dataRoot = [Environment]::GetEnvironmentVariable("CTX_DATA_ROOT", "Process")
    if ([string]::IsNullOrWhiteSpace($dataRoot)) {
        if ([string]::IsNullOrWhiteSpace($HOME)) {
            return $controls
        }
        $dataRoot = Join-Path $HOME ".ctx"
    }
    $configPath = Join-Path $dataRoot "config.toml"
    if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
        return $controls
    }

    $seen = [System.Collections.Generic.Dictionary[string,int]]::new(
        [System.StringComparer]::Ordinal
    )
    $legacyDaemonDisabled = $false
    $indexingModeSet = $false
    $section = ""
    $lineNumber = 0
    $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
    try {
        $configText = $strictUtf8.GetString([System.IO.File]::ReadAllBytes($configPath))
    } catch [System.Text.DecoderFallbackException] {
        Fail "persisted config is not valid UTF-8: $configPath"
    }
    foreach ($rawLine in ($configText -split "\`n")) {
        $lineNumber += 1
        $line = (Remove-ConfigComment -Line $rawLine).Trim()
        if ([string]::IsNullOrEmpty($line)) {
            continue
        }
        if ($line.StartsWith("[", [System.StringComparison]::Ordinal)) {
            if (-not $line.EndsWith("]", [System.StringComparison]::Ordinal)) {
                Fail "invalid config section header at line $($lineNumber): $line"
            }
            $section = $line.Substring(1, $line.Length - 2).Trim()
            if ([string]::IsNullOrEmpty($section)) {
                Fail "empty config section header at line $lineNumber"
            }
            continue
        }
        $equals = $line.IndexOf("=")
        if ($equals -lt 0) {
            Fail "invalid config line $($lineNumber): expected \`"[section]\`" or \`"key = value\`""
        }
        $key = $line.Substring(0, $equals).Trim()
        if ([string]::IsNullOrEmpty($key)) {
            Fail "empty config key at line $lineNumber"
        }
        $value = $line.Substring($equals + 1).Trim()
        $fullKey = if ([string]::IsNullOrEmpty($section)) {
            $key
        } else {
            "$section.$key"
        }
        $firstLine = 0
        if ($seen.TryGetValue($fullKey, [ref]$firstLine)) {
            Fail (
                'duplicate config key \`{0}\` at line {1}; first set at line {2}' -f
                $fullKey, $lineNumber, $firstLine
            )
        }
        $seen.Add($fullKey, $lineNumber)
        Assert-PersistedConfigValue -FullKey $fullKey -Value $value -LineNumber $lineNumber
        if ($fullKey -ceq "search.semantic") {
            $controls.SemanticEnabled = $value -ceq "true"
        } elseif ($fullKey -ceq "daemon.enabled") {
            $legacyDaemonDisabled = $value -ceq "false"
        } elseif ($fullKey -ceq "indexing.mode") {
            $indexingModeSet = $true
            $indexingMode = (Get-ConfigStringValue -FullKey $fullKey -Value $value -LineNumber $lineNumber).ToLowerInvariant()
            $controls.DaemonDisabled = $indexingMode -ceq "manual"
        }
    }
    if (-not $indexingModeSet) {
        $controls.DaemonDisabled = $legacyDaemonDisabled
    }
    return $controls
}

$deprecatedControlMappings = [System.Collections.Generic.List[string]]::new()
function Apply-DeprecatedControl(
    [string]$Name,
    [string]$Replacement,
    [string]$CanonicalName,
    [string]$CanonicalValue
) {
    $legacyValue = [Environment]::GetEnvironmentVariable($Name, "Process")
    if ($null -eq $legacyValue) {
        return
    }
    $deprecatedControlMappings.Add("$Name -> $Replacement")
    if (Test-LegacyControlTruthy -Value $legacyValue) {
        [Environment]::SetEnvironmentVariable($CanonicalName, $CanonicalValue, "Process")
    }
    [Environment]::SetEnvironmentVariable($Name, $null, "Process")
}

Apply-DeprecatedControl "CTX_ANALYTICS_OFF" "CTX_ANALYTICS_ENABLED=false" "CTX_ANALYTICS_ENABLED" "false"
Apply-DeprecatedControl "CTX_DISABLE_ANALYTICS" "CTX_ANALYTICS_ENABLED=false" "CTX_ANALYTICS_ENABLED" "false"
Apply-DeprecatedControl "CTX_INSTALL_DIAGNOSTICS_OFF" "CTX_ANALYTICS_ENABLED=false" "CTX_ANALYTICS_ENABLED" "false"
Apply-DeprecatedControl "CTX_DAEMON_OFF" "CTX_DAEMON_ENABLED=false" "CTX_DAEMON_ENABLED" "false"
Apply-DeprecatedControl "CTX_DISABLE_DAEMON" "CTX_DAEMON_ENABLED=false" "CTX_DAEMON_ENABLED" "false"
Apply-DeprecatedControl "CTX_UPGRADE_OFF" "CTX_UPGRADE_AUTO=off" "CTX_UPGRADE_AUTO" "off"
Apply-DeprecatedControl "CTX_DISABLE_AUTO_UPGRADE" "CTX_UPGRADE_AUTO=off" "CTX_UPGRADE_AUTO" "off"
if ($deprecatedControlMappings.Count -gt 0) {
    [Console]::Error.WriteLine(
        "warning: deprecated environment variables detected: " +
        ($deprecatedControlMappings -join "; ") +
        ". Update your environment to use the replacements."
    )
}

if (-not [System.Environment]::Is64BitOperatingSystem) {
    Fail "only 64-bit Windows hosts are supported"
}

if ([string]::IsNullOrWhiteSpace($BinDir)) {
    $BinDir = Join-Path $HOME ".local\\bin"
}
try {
    $binPathRoot = [System.IO.Path]::GetPathRoot($BinDir)
    $binDirIsAbsolute = [System.IO.Path]::IsPathRooted($BinDir) -and
        -not [string]::IsNullOrWhiteSpace($binPathRoot)
    if ($binDirIsAbsolute -and
        [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT) {
        $binDirIsAbsolute = $binPathRoot -ne "\\" -and $binPathRoot -notmatch '^[A-Za-z]:$'
    }
    if (-not $binDirIsAbsolute) {
        Fail "ctx install directory must be an absolute path"
    }
    $BinDir = [System.IO.Path]::GetFullPath($BinDir)
} catch {
    Fail "ctx install directory must be an absolute path"
}

$functionsBase = if ([string]::IsNullOrWhiteSpace($env:CTX_UPGRADE_FUNCTIONS_BASE)) { "${normalizedBase}" } else { $env:CTX_UPGRADE_FUNCTIONS_BASE.TrimEnd("/") }
$channel = if ([string]::IsNullOrWhiteSpace($env:CTX_UPGRADE_CHANNEL)) { "${normalizedChannel}" } else { $env:CTX_UPGRADE_CHANNEL }
if ($channel -cne "stable" -and $functionsBase -ceq "https://cli.ctx.rs/functions/v2" -and
    [string]::IsNullOrWhiteSpace($env:CTX_UPGRADE_FUNCTIONS_BASE)) {
    $functionsBase = "https://cli.ctx.rs/functions/v1"
}
# Release selection changes only the default stable feed. Telemetry remains v1;
# an injected transport base retains its existing endpoint and HTTPS gate.
$installTelemetryBase = if ($functionsBase -ceq "https://cli.ctx.rs/functions/v2") {
    "https://cli.ctx.rs/functions/v1"
} else { $functionsBase }
$installAttemptId = "${normalizedInstallAttemptId}"
if (-not [string]::IsNullOrWhiteSpace($env:CTX_INSTALL_ATTEMPT_ID) -and $env:CTX_INSTALL_ATTEMPT_ID -match '^ia_[A-Za-z0-9_-]{8,128}$') {
    $installAttemptId = $env:CTX_INSTALL_ATTEMPT_ID
}
[Environment]::SetEnvironmentVariable("CTX_INSTALL_ATTEMPT_ID", $null, "Process")
$explicitMetadata = -not [string]::IsNullOrWhiteSpace(
    "$Metadata$env:CTX_RELEASE_METADATA_URL$env:CTX_RELEASE_METADATA_SIGNATURE_URL$env:CTX_UPGRADE_FUNCTIONS_BASE"
)
if ([string]::IsNullOrWhiteSpace($Metadata)) {
    if ([string]::IsNullOrWhiteSpace($env:CTX_RELEASE_METADATA_URL)) {
        $Metadata = "$functionsBase/releases/$channel/ctx-release-metadata.env"
    } else {
        $Metadata = $env:CTX_RELEASE_METADATA_URL
    }
}
$metadataSignature = if ([string]::IsNullOrWhiteSpace($env:CTX_RELEASE_METADATA_SIGNATURE_URL)) { "$Metadata.sig" } else { $env:CTX_RELEASE_METADATA_SIGNATURE_URL }

function Copy-LimitedStream(
    [System.IO.Stream]$InputStream, [System.IO.Stream]$OutputStream,
    [long]$MaxBytes, [System.Diagnostics.Stopwatch]$Clock = $null,
    [int]$TimeoutSeconds = 3600
) {
    $buffer = [byte[]]::new(1048576)
    $total = [long]0
    while ($true) {
        $count = [int][Math]::Min($buffer.Length, $MaxBytes - $total + 1)
        if ($null -eq $Clock) {
            $read = $InputStream.Read($buffer, 0, $count)
        } else {
            $remaining = [long]$TimeoutSeconds * 1000 - $Clock.ElapsedMilliseconds
            if ($remaining -le 0) { throw 'download deadline exceeded' }
            $pendingRead = $InputStream.ReadAsync($buffer, 0, $count)
            if (-not $pendingRead.Wait([int]$remaining)) {
                throw 'download deadline exceeded'
            }
            $read = $pendingRead.GetAwaiter().GetResult()
        }
        if ($read -eq 0) { return }
        if ($read -gt $MaxBytes - $total) { throw "download or expansion exceeds $MaxBytes bytes" }
        $OutputStream.Write($buffer, 0, $read)
        $total += $read
    }
}

function Read-HttpsFile(
    [string]$Source, [string]$Destination, [long]$MaxBytes, [int]$TimeoutSeconds
) {
    Add-Type -AssemblyName System.Net.Http
    $handler = [System.Net.Http.HttpClientHandler]::new()
    # Follow redirects ourselves so .NET Framework cannot downgrade HTTPS.
    $handler.AllowAutoRedirect = $false
    $client = [System.Net.Http.HttpClient]::new($handler)
    $cancellation = [System.Threading.CancellationTokenSource]::new()
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $response = $null
    $outputStream = $null
    try {
        $client.Timeout = [System.Threading.Timeout]::InfiniteTimeSpan
        $cancellation.CancelAfter([int]($TimeoutSeconds * 1000))
        $uri = [Uri]$Source
        for ($redirects = 0; ; $redirects += 1) {
            if ($uri.Scheme -cne 'https') { throw 'refusing non-HTTPS download URL' }
            $response = $client.GetAsync(
                $uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead, $cancellation.Token
            ).GetAwaiter().GetResult()
            if ([int]$response.StatusCode -notin @(301, 302, 303, 307, 308)) { break }
            if ($redirects -ge 5 -or $null -eq $response.Headers.Location) {
                throw 'invalid or excessive download redirects'
            }
            $uri = [Uri]::new($uri, $response.Headers.Location)
            $response.Dispose()
            $response = $null
        }
        [void]$response.EnsureSuccessStatusCode()
        if ($response.Content.Headers.ContentLength -gt $MaxBytes) {
            throw "download exceeds $MaxBytes bytes"
        }
        $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $outputStream = [System.IO.File]::Create($Destination)
        # HttpClient's headers deadline alone does not cover streamed bodies.
        # A timed-out pending read is closed with its owning response in finally.
        Copy-LimitedStream $inputStream $outputStream $MaxBytes $clock $TimeoutSeconds
    } finally {
        $cancellation.Cancel()
        if ($null -ne $response) { $response.Dispose() }
        if ($null -ne $outputStream) { $outputStream.Dispose() }
        $client.Dispose()
        $cancellation.Dispose()
    }
}

function Copy-LimitedFile([string]$Source, [string]$Destination, [long]$MaxBytes) {
    $inputStream = [System.IO.File]::OpenRead($Source)
    $outputStream = $null
    try {
        if ($inputStream.Length -gt $MaxBytes) { throw "download exceeds $MaxBytes bytes" }
        $outputStream = [System.IO.File]::Create($Destination)
        Copy-LimitedStream $inputStream $outputStream $MaxBytes
    } finally {
        if ($null -ne $outputStream) { $outputStream.Dispose() }
        $inputStream.Dispose()
    }
}

function Read-Metadata([string]$Source, [string]$Destination, [long]$MaxBytes = 1048576) {
    if ($Source -match '^https://') {
        Read-HttpsFile $Source $Destination $MaxBytes 300
        return
    }
    if ($Source -match '^http://') {
        Fail "refusing insecure metadata URL: $Source"
    }
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        Fail "metadata file not found: $Source"
    }
    Copy-LimitedFile $Source $Destination $MaxBytes
}

function Read-DetachedSignature([string]$Source, [string]$Destination) {
    Read-Metadata -Source $Source -Destination $Destination -MaxBytes 65536
}

function Read-Artifact(
    [string]$Source, [string]$Destination,
    [long]$MaxBytes = 268435456, [int]$TimeoutSeconds = 3600
) {
    if ($Source -match '^https://') {
        Read-HttpsFile $Source $Destination $MaxBytes $TimeoutSeconds
        return
    }
    if ($Source -match '^http://') {
        Fail "refusing insecure artifact URL: $Source"
    }
    if ($Source -notmatch '^file:///') {
        Fail "artifact URL must be HTTPS"
    }
    if ($env:CTX_ALLOW_CUSTOM_RELEASE_BASE_URL -ne "1") {
        Fail "file artifact URLs are available only for explicit development inputs"
    }
    try {
        $sourceUri = [Uri]$Source
    } catch {
        Fail "invalid file artifact URL: $Source"
    }
    if (-not $sourceUri.IsFile -or
        -not (Test-Path -LiteralPath $sourceUri.LocalPath -PathType Leaf)) {
        Fail "artifact file not found: $Source"
    }
    $sourceItem = Get-Item -LiteralPath $sourceUri.LocalPath -Force
    if ($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        Fail "artifact file must not be a reparse point: $Source"
    }
    Copy-LimitedFile $sourceUri.LocalPath $Destination $MaxBytes
}

function ConvertFrom-Base64Url([string]$Value) {
    $base64 = $Value.Replace("-", "+").Replace("_", "/")
    switch ($base64.Length % 4) {
        0 { }
        2 { $base64 += "==" }
        3 { $base64 += "=" }
        default { Fail "invalid base64url value in metadata public key" }
    }
    try {
        return ,([System.Convert]::FromBase64String($base64))
    } catch {
        Fail "invalid base64url value in metadata public key"
    }
}

function Get-MetadataSignaturePublicKeyParameters() {
    $parameters = [System.Security.Cryptography.RSAParameters]::new()
    $parameters.Modulus = [byte[]](ConvertFrom-Base64Url "${normalizedMetadataPublicKeyModulusBase64Url}")
    $parameters.Exponent = [byte[]](ConvertFrom-Base64Url "${normalizedMetadataPublicKeyExponentBase64Url}")
    return $parameters
}

function Verify-MetadataSignature([string]$MetadataPath, [string]$SignaturePath) {
    $signatureB64 = (Get-Content -LiteralPath $SignaturePath -Raw).Trim()
    try {
        [byte[]]$signatureBytes = [System.Convert]::FromBase64String($signatureB64)
    } catch {
        Fail "metadata signature is not base64-encoded RSA-SHA256 bytes"
    }
    if ($signatureBytes.Length -eq 0) {
        Fail "metadata signature is empty"
    }
    [byte[]]$metadataBytes = [System.IO.File]::ReadAllBytes($MetadataPath)
    $rsa = [System.Security.Cryptography.RSA]::Create()
    try {
        $rsa.ImportParameters((Get-MetadataSignaturePublicKeyParameters))
        $verified = $rsa.VerifyData(
            $metadataBytes,
            $signatureBytes,
            [System.Security.Cryptography.HashAlgorithmName]::SHA256,
            [System.Security.Cryptography.RSASignaturePadding]::Pkcs1
        )
    } finally {
        $rsa.Dispose()
    }
    if (-not $verified) {
        Fail "metadata signature verification failed"
    }
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
    if ($Value.Contains("..") -or $Value.Contains("/") -or $Value.Contains("\\")) {
        Fail "unsafe artifact name: $Value"
    }
}

function Expand-GzipFile([string]$Source, [string]$Destination, [long]$MaxBytes = 268435456) {
    $inputStream = [System.IO.File]::OpenRead($Source)
    $outputStream = $null
    $gzipStream = $null
    try {
        $outputStream = [System.IO.File]::Create($Destination)
        $gzipStream = [System.IO.Compression.GZipStream]::new(
            $inputStream,
            [System.IO.Compression.CompressionMode]::Decompress
        )
        Copy-LimitedStream $gzipStream $outputStream $MaxBytes
    } finally {
        if ($null -ne $gzipStream) {
            $gzipStream.Dispose()
        }
        if ($null -ne $outputStream) {
            $outputStream.Dispose()
        }
        $inputStream.Dispose()
    }
}

function Assert-AllowedBaseUrl([string]$Value) {
    if ($Value -match '^https://cli\\.ctx\\.rs/storage/v1/object/public/releases/artifacts/') {
        return
    }
    if ($env:CTX_ALLOW_CUSTOM_RELEASE_BASE_URL -eq "1" -and
        ($Value -match '^https://' -or $Value -match '^file:///')) {
        return
    }
    Fail "metadata base URL must be under https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/"
}

`;
}
