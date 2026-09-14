param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,
    [string]$Companion = "",
    [string]$PairEnvelope = "",
    [Parameter(Mandatory = $true)]
    [string]$Fixture,
    [string]$ExpectedVersion,
    [string]$ExpectedVersionFile,
    [Parameter(Mandatory = $true)]
    [string]$ResultPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Core verifies the exact protected current-user/SYSTEM DACL before it accepts
# managed-pair paths. The smoke copies supplied artifacts into this private
# root so it proves the real hidden apply path without mutating its caller's
# candidate files.
function Set-ManagedPairPrivateAcl(
    [string]$Path,
    [bool]$Directory
) {
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail "managed-pair private path must not be a reparse point: $Path"
    }
    if ($Directory -ne $item.PSIsContainer) {
        Fail "managed-pair private path has an unexpected type: $Path"
    }
    $userSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $inheritance = if ($Directory) { "OICI" } else { "" }
    $aces = @("(A;$inheritance;FA;;;$userSid)")
    if ($userSid -cne "S-1-5-18") {
        $aces += "(A;$inheritance;FA;;;SY)"
    }
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetSecurityDescriptorSddlForm(
        "D:P" + ($aces -join ""),
        [System.Security.AccessControl.AccessControlSections]::Access
    )
    Set-Acl -LiteralPath $Path -AclObject $acl
}

# ProcessStartInfo cannot establish a Job Object before the child executes.
# Start suspended so every descendant is born inside this invocation's job.
if ($null -eq ("CtxNativeOwnedProcess" -as [type])) {
    Add-Type -Path (Join-Path $PSScriptRoot "windows\CtxNativeOwnedProcess.cs")
}

function Fail([string]$Message) {
    throw "native candidate smoke: $Message"
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
            continue
        }
        if ($character -eq '"') {
            [void]$quoted.Append(('\' * (($backslashes * 2) + 1)))
            [void]$quoted.Append('"')
        } else {
            [void]$quoted.Append(('\' * $backslashes))
            [void]$quoted.Append($character)
        }
        $backslashes = 0
    }
    [void]$quoted.Append(('\' * ($backslashes * 2)))
    [void]$quoted.Append('"')
    return $quoted.ToString()
}

$Binary = [System.IO.Path]::GetFullPath($Binary)
$pairMode = -not [string]::IsNullOrWhiteSpace($Companion) -or
    -not [string]::IsNullOrWhiteSpace($PairEnvelope)
if ($pairMode -and (
    [string]::IsNullOrWhiteSpace($Companion) -or
    [string]::IsNullOrWhiteSpace($PairEnvelope)
)) {
    Fail "Companion and PairEnvelope must be provided together"
}
if ($pairMode) {
    $Companion = [System.IO.Path]::GetFullPath($Companion)
    $PairEnvelope = [System.IO.Path]::GetFullPath($PairEnvelope)
}
$Fixture = [System.IO.Path]::GetFullPath($Fixture)
$ResultPath = [System.IO.Path]::GetFullPath($ResultPath)

if ([string]::IsNullOrWhiteSpace($ExpectedVersion) -eq
    [string]::IsNullOrWhiteSpace($ExpectedVersionFile)) {
    Fail "provide exactly one of ExpectedVersion or ExpectedVersionFile"
}
if (-not [string]::IsNullOrWhiteSpace($ExpectedVersionFile)) {
    $ExpectedVersionFile = [System.IO.Path]::GetFullPath($ExpectedVersionFile)
    if (-not (Test-Path -LiteralPath $ExpectedVersionFile -PathType Leaf)) {
        Fail "expected-version file is missing: $ExpectedVersionFile"
    }
    $ExpectedVersion = (Get-Content -LiteralPath $ExpectedVersionFile -Raw).Trim()
}

if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    Fail "binary is missing: $Binary"
}
if (-not (Test-Path -LiteralPath $Fixture -PathType Leaf)) {
    Fail "fixture is missing: $Fixture"
}
if ($pairMode -and -not (Test-Path -LiteralPath $Companion -PathType Leaf)) {
    Fail "companion is missing: $Companion"
}
if ($pairMode -and -not (Test-Path -LiteralPath $PairEnvelope -PathType Leaf)) {
    Fail "signed pair envelope is missing: $PairEnvelope"
}
if ($ExpectedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$') {
    Fail "expected version is invalid: $ExpectedVersion"
}
$versionParts = (($ExpectedVersion -split '[+-]', 2)[0]).Split(".")
$freshEpochRequired = [int]$versionParts[0] -gt 0 -or [int]$versionParts[1] -ge 26

$resultParent = Split-Path -Parent $ResultPath
if ([string]::IsNullOrWhiteSpace($resultParent)) {
    $resultParent = (Get-Location).Path
}
New-Item -ItemType Directory -Path $resultParent -Force | Out-Null
Remove-Item -LiteralPath $ResultPath -Force -ErrorAction SilentlyContinue
$resultTemp = "$ResultPath.tmp.$PID"

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-native-candidate-smoke-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
Set-ManagedPairPrivateAcl -Path $root -Directory $true
$profile = Join-Path $root "profile"
$dataRoot = Join-Path $root "data"
$configRoot = Join-Path $root "config"
$cacheRoot = Join-Path $root "cache"
$stateRoot = Join-Path $root "state"
$tmpRoot = Join-Path $root "tmp"
$workRoot = Join-Path $root "work"
$analyticsDefaultEvents = Join-Path $root "analytics-default.jsonl"
$analyticsOptOutEvents = Join-Path $root "analytics-opt-out.jsonl"
$analyticsDefaultEndpoint = ([System.Uri]::new($analyticsDefaultEvents)).AbsoluteUri
$analyticsOptOutEndpoint = ([System.Uri]::new($analyticsOptOutEvents)).AbsoluteUri
foreach ($path in @($profile, $dataRoot, $configRoot, $cacheRoot, $stateRoot, $tmpRoot, $workRoot)) {
    New-Item -ItemType Directory -Path $path -Force | Out-Null
}
if ($pairMode) {
    $pairChannel = if ([string]::IsNullOrWhiteSpace($env:CTX_MANAGED_PAIR_CHANNEL)) { "stable" } else { $env:CTX_MANAGED_PAIR_CHANNEL }
    if ($pairChannel -cnotin @("stable", "staging")) { Fail "managed-pair channel must be stable or staging" }
    $installRoot = Join-Path $root "installation"
    $installedBinary = Join-Path $installRoot "bin\ctx.exe"
    $installedMarker = "$installedBinary.install.json"
    $markerSource = Join-Path $root "ctx.install.json"
    $pairInputRoot = Join-Path $root "managed-pair-input"
    $pairCoreInput = Join-Path $pairInputRoot "ctx.exe"
    $pairCompanionInput = Join-Path $pairInputRoot "ctx-pro.exe"
    $pairEnvelopeInput = Join-Path $pairInputRoot "managed-pair-envelope.json"
    New-Item -ItemType Directory -Path (Join-Path $installRoot "bin") -Force | Out-Null
    New-Item -ItemType Directory -Path $pairInputRoot | Out-Null
    Set-ManagedPairPrivateAcl -Path $installRoot -Directory $true
    Set-ManagedPairPrivateAcl -Path (Join-Path $installRoot "bin") -Directory $true
    Set-ManagedPairPrivateAcl -Path $pairInputRoot -Directory $true
    Copy-Item -LiteralPath $Binary -Destination $pairCoreInput
    Copy-Item -LiteralPath $Companion -Destination $pairCompanionInput
    Copy-Item -LiteralPath $PairEnvelope -Destination $pairEnvelopeInput
    foreach ($pairInput in @($pairCoreInput, $pairCompanionInput, $pairEnvelopeInput)) {
        Set-ManagedPairPrivateAcl -Path $pairInput -Directory $false
    }
    $marker = [ordered]@{
        schema_version = 1
        manager = "ctx-hosted-installer"
        managed_pair = $true
        install_attempt_id = "ia_native_smoke_$PID"
        install_path = $installedBinary
        platform = "windows-x64"
        channel = $pairChannel
        version = $ExpectedVersion
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Binary).Hash.ToLowerInvariant()
        staging_dogfood = $pairChannel -ceq "staging"
        metadata_url = "native-candidate-smoke"
        artifact_url = "native-candidate-smoke"
        installed_at = "1970-01-01T00:00:00Z"
    }
    [IO.File]::WriteAllText($markerSource, ($marker | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
    Set-ManagedPairPrivateAcl -Path $markerSource -Directory $false
}

$savedLocation = (Get-Location).Path
$savedEnvironment = @{}
$timeoutText = if ([string]::IsNullOrWhiteSpace($env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS)) {
    "60"
} else {
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
}
$timeoutSeconds = 0
if (-not [int]::TryParse($timeoutText, [ref]$timeoutSeconds) -or
    $timeoutSeconds -lt 1 -or $timeoutSeconds -gt 900) {
    Fail "timeout must be a whole number of seconds between 1 and 900"
}
$deprecatedControlNames = @(
    "ANALYTICS_OFF",
    "DISABLE_ANALYTICS",
    "INSTALL_DIAGNOSTICS_OFF",
    "DAEMON_OFF",
    "DISABLE_DAEMON",
    "UPGRADE_OFF",
    "DISABLE_AUTO_UPGRADE"
) | ForEach-Object { "CTX_$_" }
$isolation = [ordered]@{
    HOME = $profile
    USERPROFILE = $profile
    APPDATA = $configRoot
    LOCALAPPDATA = $stateRoot
    XDG_CONFIG_HOME = $configRoot
    XDG_CACHE_HOME = $cacheRoot
    XDG_DATA_HOME = (Join-Path $root "xdg-data")
    XDG_STATE_HOME = $stateRoot
    TEMP = $tmpRoot
    TMP = $tmpRoot
    CTX_DATA_ROOT = $dataRoot
    CTX_ANALYTICS_ENABLED = "false"
    CTX_ANALYTICS_ENDPOINT = $analyticsDefaultEndpoint
    CTX_UPGRADE_AUTO = "off"
    CTX_DAEMON_AUTOSTART_OFF = "1"
    CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS = "1"
    CTX_DAEMON_ENABLED = "false"
    CTX_DAEMON_MODE = "source-refresh-only"
    CTX_SEARCH_SEMANTIC = "0"
    CTX_SEMANTIC_CACHE_DIR = (Join-Path $root "semantic-cache")
    HF_HOME = (Join-Path $root "huggingface")
    HF_HUB_OFFLINE = "1"
    TRANSFORMERS_OFFLINE = "1"
    CODEX_HOME = (Join-Path $profile ".codex")
    CLAUDE_CONFIG_DIR = (Join-Path $profile ".claude")
    COPILOT_HOME = (Join-Path $profile ".copilot")
    OPENCLAW_STATE_DIR = (Join-Path $profile ".openclaw")
    HERMES_HOME = (Join-Path $profile ".hermes")
    ASTRBOT_ROOT = (Join-Path $profile ".astrbot")
    SHELLEY_DB = (Join-Path $profile "shelley.db")
    KILO_DB = (Join-Path $profile "kilo.db")
    MIMOCODE_HOME = (Join-Path $profile ".mimocode")
    MIMOCODE_CONFIG_DIR = (Join-Path $profile ".mimocode-config")
    MIMOCODE_DB = (Join-Path $profile "mimocode.db")
    MIMOCODE_DISABLE_CHANNEL_DB = "1"
    FORGE_CONFIG = (Join-Path $profile "forge.json")
    VIBE_HOME = (Join-Path $profile ".vibe")
}
foreach ($name in $deprecatedControlNames) {
    $isolation[$name] = $null
}

function Get-RemainingMilliseconds(
    [System.Diagnostics.Stopwatch]$Clock,
    [int]$LimitMilliseconds
) {
    $remaining = [long]$LimitMilliseconds - $Clock.ElapsedMilliseconds
    if ($remaining -le 0) {
        return 0
    }
    return [int]$remaining
}

function Wait-ProcessUntil(
    [CtxNativeOwnedProcess]$Process,
    [System.Diagnostics.Stopwatch]$Clock,
    [int]$LimitMilliseconds
) {
    if ($Process.HasExited) {
        return $true
    }
    return $Process.WaitForExit((Get-RemainingMilliseconds $Clock $LimitMilliseconds))
}

function Wait-TaskUntil(
    [System.Threading.Tasks.Task]$Task,
    [System.Diagnostics.Stopwatch]$Clock,
    [int]$LimitMilliseconds
) {
    if ($Task.IsCompleted) {
        return $true
    }
    try {
        return $Task.Wait((Get-RemainingMilliseconds $Clock $LimitMilliseconds))
    } catch [System.AggregateException] {
        # A faulted task is complete. GetResult below will preserve its precise
        # stream error instead of misclassifying it as a timeout.
        return $true
    }
}

function Invoke-CtxRaw(
    [string[]]$Arguments,
    [string]$CompletionPath = ""
) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    [void]$start.EnvironmentVariables.Remove("CI")
    $isCommandScript = [System.IO.Path]::GetExtension($Binary) -ieq ".cmd"
    if ($isCommandScript) {
        $start.FileName = $env:ComSpec
    } else {
        $start.FileName = $Binary
    }

    if ($isCommandScript) {
        $command = (@($Binary) + $Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
        $start.Arguments = "/d /s /c `"$command`""
    } else {
        $start.Arguments = ($Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
    }
    $commandLine = @((ConvertTo-NativeArgument $start.FileName), $start.Arguments) |
        Where-Object { -not [string]::IsNullOrEmpty($_) }
    $commandLine = $commandLine -join " "
    $timeoutMilliseconds = $timeoutSeconds * 1000
    $commandClock = [System.Diagnostics.Stopwatch]::StartNew()
    $process = $null
    try {
        $process = [CtxNativeOwnedProcess]::Start(
            $start.FileName,
            $commandLine,
            (Get-Location).Path,
            $start.EnvironmentVariables)
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()

        $timeoutPhase = $null
        $rootExitCode = $null
        $cleanupAfterExit = $false
        if (-not [string]::IsNullOrWhiteSpace($CompletionPath)) {
            while ((Get-RemainingMilliseconds $commandClock $timeoutMilliseconds) -gt 0) {
                $completion = Get-Item -LiteralPath $CompletionPath -ErrorAction SilentlyContinue
                if ($null -ne $completion -and $completion.Length -gt 0) {
                    $cleanupAfterExit = $true
                    break
                }
                if ($process.HasExited) {
                    break
                }
                Start-Sleep -Milliseconds 100
            }
            if (-not $cleanupAfterExit) {
                $timeoutPhase = "analytics delivery"
            }
        } elseif (-not (Wait-ProcessUntil $process $commandClock $timeoutMilliseconds)) {
            $timeoutPhase = "process exit"
        } else {
            # Preserve the root result before terminating any descendants that
            # retained inherited pipe handles. A short grace lets ordinary
            # buffered output finish without turning a successful root exit
            # into a full command-deadline wait.
            $rootExitCode = $process.ExitCode
            $postExitDrainClock = [System.Diagnostics.Stopwatch]::StartNew()
            $postExitDrainMilliseconds = 1000
            [void](Wait-TaskUntil $stdout $postExitDrainClock $postExitDrainMilliseconds)
            [void](Wait-TaskUntil $stderr $postExitDrainClock $postExitDrainMilliseconds)
            $pendingStreams = @()
            if (-not $stdout.IsCompleted) {
                $pendingStreams += "stdout"
            }
            if (-not $stderr.IsCompleted) {
                $pendingStreams += "stderr"
            }
            if ($pendingStreams.Count -ne 0) {
                $cleanupAfterExit = $true
            }
        }

        if ($null -eq $timeoutPhase -and -not $cleanupAfterExit) {
            $stdoutText = $stdout.GetAwaiter().GetResult()
            $stderrText = $stderr.GetAwaiter().GetResult()
            $text = @($stdoutText, $stderrText) |
                Where-Object { -not [string]::IsNullOrEmpty($_) }
            return [pscustomobject]@{
                ExitCode = $rootExitCode
                Stdout = $stdoutText
                Stderr = $stderrText
                Text = ($text -join [Environment]::NewLine).TrimEnd()
            }
        }
        $terminationErrors = @()
        try {
            # The root may already have exited while a descendant retains its
            # inherited pipe handles. Terminate the owned job unconditionally.
            $process.Terminate()
        } catch {
            $terminationErrors += $_.Exception.Message
        }

        $finalDrainClock = [System.Diagnostics.Stopwatch]::StartNew()
        $finalDrainMilliseconds = 5000
        $pendingFinal = @()
        if (-not (Wait-ProcessUntil $process $finalDrainClock $finalDrainMilliseconds)) {
            $pendingFinal += "process exit"
        }
        if (-not (Wait-TaskUntil $stdout $finalDrainClock $finalDrainMilliseconds)) {
            $pendingFinal += "stdout"
        }
        if (-not (Wait-TaskUntil $stderr $finalDrainClock $finalDrainMilliseconds)) {
            $pendingFinal += "stderr"
        }

        $terminationDiagnostic = if ($terminationErrors.Count -eq 0) {
            "owned tree termination completed"
        } else {
            "owned tree termination failed: " + ($terminationErrors -join "; ")
        }
        $finalDrainDiagnostic = if ($pendingFinal.Count -eq 0) {
            "final drain completed"
        } else {
            "final drain still pending: " + ($pendingFinal -join ",")
        }

        if ($null -ne $timeoutPhase) {
            Fail ("ctx command exceeded {0} seconds during {1}; {2}; {3}: {4}" -f
                $timeoutSeconds,
                $timeoutPhase,
                $terminationDiagnostic,
                $finalDrainDiagnostic,
                ($Arguments -join " "))
        }
        if ($terminationErrors.Count -ne 0 -or $pendingFinal.Count -ne 0) {
            Fail ("ctx command root exited but owned tree cleanup failed; {0}; {1}: {2}" -f
                $terminationDiagnostic,
                $finalDrainDiagnostic,
                ($Arguments -join " "))
        }

        $stdoutText = $stdout.GetAwaiter().GetResult()
        $stderrText = $stderr.GetAwaiter().GetResult()
        $text = @($stdoutText, $stderrText) |
            Where-Object { -not [string]::IsNullOrEmpty($_) }
        return [pscustomobject]@{
            ExitCode = $rootExitCode
            Stdout = $stdoutText
            Stderr = $stderrText
            Text = ($text -join [Environment]::NewLine).TrimEnd()
        }
    } finally {
        if ($null -ne $process) {
            $process.Dispose()
        }
    }
}

function Invoke-Ctx([string[]]$Arguments) {
    $result = Invoke-CtxRaw $Arguments
    if ($result.ExitCode -ne 0) {
        Fail ("ctx {0} failed: {1}" -f ($Arguments -join " "), $result.Text)
    }
    return $result.Text
}

function Get-StatusAnalyticsEventId([string]$Path, [bool]$Outbox) {
    try {
        if ($Outbox) {
            $document = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
            if ($document.schema_version -ne 3) { throw "unexpected outbox schema" }
            $payloads = @($document.entries | Where-Object { $_.kind -ceq "ordinary" } |
                ForEach-Object { $_.payload | ConvertFrom-Json })
        } else {
            $payloads = @(Get-Content -LiteralPath $Path |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
                ForEach-Object { $_ | ConvertFrom-Json })
        }
        $ids = @($payloads.events | Where-Object {
            $_.event_name -ceq "operation_completed" -and $_.event_version -eq 1 -and
            $_.surface -ceq "cli" -and $_.operation -ceq "status" -and
            $_.outcome -ceq "success"
        } | ForEach-Object { [string]$_.event_id })
    } catch {
        Fail "candidate produced malformed analytics evidence"
    }
    if ($ids.Count -ne 1 -or $ids[0] -cnotmatch
        '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$') {
        Fail "analytics evidence does not contain exactly one status UUIDv4"
    }
    return $ids[0]
}

try {
    foreach ($name in $isolation.Keys) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, $isolation[$name], "Process")
    }
    Set-Location -LiteralPath $workRoot

    if ($pairMode) {
        $pairApply = Invoke-CtxRaw @(
            "--ctx-core-managed-pair-apply-v1", $installRoot, "-",
            $pairEnvelopeInput, $pairCoreInput, $pairCompanionInput, $markerSource)
        if ($pairApply.ExitCode -ne 0) {
            Fail ("candidate Core could not apply the signed managed pair: " + $pairApply.Stderr.Trim())
        }
        $expectedReceipt = '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}' + "`n"
        if ([Text.Encoding]::UTF8.GetByteCount($pairApply.Stdout) -ne 83 -or
            $pairApply.Stdout -cne $expectedReceipt -or $pairApply.Stderr.Length -ne 0) {
            Fail "candidate Core returned an invalid managed-pair apply receipt"
        }
        if (-not (Test-Path -LiteralPath $installedMarker -PathType Leaf)) { Fail "candidate Core did not publish ctx.exe.install.json" }
        $Binary = $installedBinary
    }

    $version = Invoke-Ctx @("--version")
    if ($version.Trim() -ne "ctx $ExpectedVersion") {
        Fail "version mismatch: expected ctx $ExpectedVersion, got $version"
    }
    if ($pairMode) {
        [void](Invoke-Ctx @("pro", "--help"))
    }

    [void](Invoke-Ctx @("setup", "--no-daemon", "--progress", "none"))
    $importArguments = @(
        "import", "--input-format", "ctx-history-jsonl-v2", "--path", $Fixture,
        "--no-daemon", "--format=json", "--progress", "none"
    )
    $importResult = Invoke-CtxRaw $importArguments
    $coreManifestRequired = $freshEpochRequired
    if ($importResult.ExitCode -eq 0) {
        $import = $importResult.Text
    } else {
        if ($importResult.Text -notmatch 'no foreground writer was started') {
            Fail ("ctx {0} failed: {1}" -f ($importArguments -join " "), $importResult.Text)
        }
        $coreManifestRequired = $true
        $env:CTX_DAEMON_ENABLED = "true"
        $env:CTX_DAEMON_AUTOSTART_OFF = "0"
        try {
            $import = Invoke-Ctx @(
                "import", "--input-format", "ctx-history-jsonl-v2", "--path", $Fixture,
                "--format=json", "--progress", "none"
            )
        } finally {
            $env:CTX_DAEMON_ENABLED = "false"
            $env:CTX_DAEMON_AUTOSTART_OFF = "1"
        }
    }
    if ($freshEpochRequired) {
        if ($import -notmatch '"current_source_count"\s*:\s*[1-9][0-9]*' -or
            $import -notmatch '"current_indexed_documents"\s*:\s*[1-9][0-9]*' -or
            $import -notmatch '"published_generation"\s*:\s*"[0-9a-f]{64}"') {
            Fail "fixture import did not publish Core-generation authority"
        }
    } elseif ($import -notmatch '"imported_events"\s*:\s*[1-9][0-9]*' -and
            ($import -notmatch '"imported_sources"\s*:\s*[1-9][0-9]*' -or
             $import -notmatch '"published_generation"\s*:\s*"[0-9a-f]{64}"')) {
        Fail "fixture import did not report imported data"
    }

    $search = Invoke-Ctx @("search", "parser test", "--backend", "lexical", "--refresh", "off", "--format=json")
    if ($search -notmatch '"requested_mode"\s*:\s*"lexical"' -or
        $search -notmatch '"effective_mode"\s*:\s*"lexical"' -or
        $search -notmatch [regex]::Escape("Add a parser test.")) {
        Fail "lexical search did not return the expected fixture result"
    }
    # Import and search execute in separate candidate processes. The expected
    # hit plus the absence of the old Store proves fresh Core-generation
    # authority carried the fixture across that boundary.
    if (Test-Path -LiteralPath (Join-Path $dataRoot "work.sqlite")) {
        Fail "candidate created or opened the pre-v0.26 Store"
    }
    if ($coreManifestRequired) {
        $lexicalRoot = Join-Path $dataRoot "search\lexical"
        if (-not (Test-Path -LiteralPath (Join-Path $lexicalRoot "active-generation.json") -PathType Leaf)) {
            Fail "candidate did not publish the fresh lexical generation"
        }
        $manifestRoot = Join-Path $lexicalRoot "ctx-generations"
        $coreManifests = @(Get-ChildItem -LiteralPath $manifestRoot -Filter "*.json" -File -ErrorAction SilentlyContinue)
        if ($coreManifests.Count -eq 0) {
            Fail "candidate did not publish Core-generation authority"
        }
    }

    # Empty-config foreground work must append durably without delivery. The
    # isolated daemon then owns bounded delivery to this local file endpoint.
    $env:CTX_ANALYTICS_ENABLED = $null
    $env:CTX_UPGRADE_AUTO = $null
    $env:CTX_DAEMON_ENABLED = $null
    $env:CTX_SEARCH_SEMANTIC = $null
    try {
        $status = Invoke-Ctx @("status", "--format=json")
    } finally {
        $env:CTX_ANALYTICS_ENABLED = "false"
        $env:CTX_UPGRADE_AUTO = "off"
        $env:CTX_SEARCH_SEMANTIC = "0"
        $env:CTX_DAEMON_ENABLED = "false"
    }
    try {
        $statusValue = $status | ConvertFrom-Json
    } catch {
        Fail "read-only status command returned malformed JSON"
    }
    if ($statusValue.read_only -ne $true) {
        Fail "read-only status command returned an unexpected payload"
    }
    if ($statusValue.daemon.enabled -ne $true) {
        Fail "candidate does not report daemon maintenance as enabled by default"
    }
    if ($pairMode -and
        ($statusValue.upgrade.auto -cne "apply" -or
         $statusValue.upgrade.auto_enabled -ne $true)) {
        Fail "candidate does not enable managed auto-upgrade by default"
    }
    if (-not $pairMode -and
        ($statusValue.upgrade.auto -cne "off" -or
         $statusValue.upgrade.auto_enabled -ne $false)) {
        Fail "candidate does not disable auto-upgrade in the unmanaged validation layout"
    }
    if ($statusValue.semantic.config_source -cne "default" -or
        $statusValue.semantic.reason -cne "semantic_disabled") {
        Fail "native candidate does not report semantic search as disabled by default"
    }
    if ($status -match '"source"\s*:\s*"unsupported"') {
        Fail "native candidate unexpectedly reports semantic search as unsupported"
    }
    if (Test-Path -LiteralPath $analyticsDefaultEvents) {
        Fail "foreground CLI delivered analytics before daemon ownership"
    }
    $analyticsOutboxes = @(Get-ChildItem -LiteralPath $root -Recurse -File |
        Where-Object { $_.Name -ceq "analytics-outbox-v1.json" })
    if ($analyticsOutboxes.Count -ne 1) {
        Fail "candidate did not create exactly one durable analytics outbox"
    }
    $analyticsOutboxBeforeDaemon = Join-Path $root "analytics-outbox-before-daemon.json"
    Copy-Item -LiteralPath $analyticsOutboxes[0].FullName -Destination $analyticsOutboxBeforeDaemon

    $env:CTX_ANALYTICS_ENABLED = $null
    $env:CTX_UPGRADE_AUTO = "off"
    $env:CTX_DAEMON_ENABLED = "true"
    $env:CTX_DAEMON_MODE = "source-refresh-only"
    $env:CTX_SEARCH_SEMANTIC = "0"
    try {
        [void](Invoke-CtxRaw @(
            "daemon", "run", "--force", "--loop-interval-seconds", "600", "--format", "json"
        ) $analyticsDefaultEvents)
    } finally {
        $env:CTX_ANALYTICS_ENABLED = "false"
        $env:CTX_UPGRADE_AUTO = "off"
        $env:CTX_DAEMON_ENABLED = "false"
    }

    $queuedStatusId = Get-StatusAnalyticsEventId $analyticsOutboxBeforeDaemon $true
    $deliveredStatusId = Get-StatusAnalyticsEventId $analyticsDefaultEvents $false
    if ($queuedStatusId -cne $deliveredStatusId) {
        Fail "daemon did not deliver the queued status analytics UUID"
    }

    $env:CTX_ANALYTICS_ENDPOINT = $analyticsOptOutEndpoint
    $optOutStatus = Invoke-Ctx @("status", "--format=json")
    try {
        $optOutStatusValue = $optOutStatus | ConvertFrom-Json
    } catch {
        Fail "explicit opt-out status returned malformed JSON"
    } finally {
        $env:CTX_ANALYTICS_ENDPOINT = $analyticsDefaultEndpoint
    }
    if ($optOutStatusValue.daemon.enabled -ne $false) {
        Fail "candidate daemon opt-out did not override the released default"
    }
    if ($optOutStatusValue.upgrade.auto -cne "off" -or
        $optOutStatusValue.upgrade.auto_enabled -ne $false) {
        Fail "candidate upgrade opt-out did not override the released default"
    }
    if (Test-Path -LiteralPath $analyticsOptOutEvents) {
        Fail "candidate analytics opt-out did not override the released default"
    }

    # Semantic search is supported but opt-in. Without a provisioned model, an
    # explicit offline request must fail before fallback, state, or network.
    $env:CTX_SEARCH_SEMANTIC = "1"
    $env:CTX_DAEMON_ENABLED = "true"
    $savedErrorActionPreference = $ErrorActionPreference
    try {
        # This command must fail. Windows PowerShell promotes native stderr to
        # NativeCommandError when the global preference is Stop, so capture it
        # under Continue and validate the exit status and message ourselves.
        $ErrorActionPreference = "Continue"
        $capabilityResult = Invoke-CtxRaw @("search", "parser test", "--backend", "semantic", "--refresh", "off", "--format=json")
        $capabilityOutput = $capabilityResult.Text
        $capabilityExit = $capabilityResult.ExitCode
    } finally {
        $ErrorActionPreference = $savedErrorActionPreference
    }
    $env:CTX_SEARCH_SEMANTIC = "0"
    $env:CTX_DAEMON_ENABLED = "false"
    $capabilityText = $capabilityOutput -join [Environment]::NewLine
    if ($capabilityExit -eq 0) {
        Fail "semantic-only search unexpectedly succeeded"
    }
    if ($capabilityText -notmatch 'semantic_store_missing|semantic-only search will not initialize or download') {
        Fail "semantic-only search did not report the fail-closed capability contract"
    }
    if ($capabilityText -match '"effective_mode"\s*:\s*"lexical"') {
        Fail "semantic-only search silently fell back to lexical"
    }
    foreach ($unexpected in @(
        (Join-Path $root "semantic-cache"),
        (Join-Path $root "huggingface"),
        (Join-Path $dataRoot "search\semantic")
    )) {
        if (Test-Path -LiteralPath $unexpected) {
            Fail "semantic-only search created semantic state"
        }
    }

    $resultSteps = [ordered]@{
        version = "passed"
        setup = "passed"
        import = "passed"
        search = "passed"
        read_only = "passed"
        released_defaults = "passed"
        explicit_opt_outs = "passed"
        semantic_offline_fail_closed = "passed"
    }
    if ($pairMode) {
        $resultSteps = [ordered]@{
            managed_pair_apply = "passed"
            companion_selection = "passed"
            version = "passed"
            setup = "passed"
            import = "passed"
            search = "passed"
            read_only = "passed"
            released_defaults = "passed"
            explicit_opt_outs = "passed"
            semantic_offline_fail_closed = "passed"
        }
    }

    $result = [ordered]@{
        schema_version = 1
        kind = "ctx-native-candidate-smoke"
        status = "passed"
        steps = $resultSteps
    }
    $resultJson = $result | ConvertTo-Json -Compress -Depth 3
    [System.IO.File]::WriteAllText($resultTemp, $resultJson, (New-Object System.Text.UTF8Encoding($false)))
    Move-Item -LiteralPath $resultTemp -Destination $ResultPath -Force
    Write-Host "native candidate smoke passed: Windows $([Environment]::Is64BitProcess)"
} finally {
    Set-Location -LiteralPath $savedLocation
    foreach ($name in $isolation.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], "Process")
    }
    Remove-Item -LiteralPath $resultTemp -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
