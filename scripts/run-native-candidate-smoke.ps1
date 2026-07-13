param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,
    [Parameter(Mandatory = $true)]
    [string]$Fixture,
    [string]$ExpectedVersion,
    [string]$ExpectedVersionFile,
    [Parameter(Mandatory = $true)]
    [string]$ResultPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

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
if ($ExpectedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$') {
    Fail "expected version is invalid: $ExpectedVersion"
}

$resultParent = Split-Path -Parent $ResultPath
if ([string]::IsNullOrWhiteSpace($resultParent)) {
    $resultParent = (Get-Location).Path
}
New-Item -ItemType Directory -Path $resultParent -Force | Out-Null
Remove-Item -LiteralPath $ResultPath -Force -ErrorAction SilentlyContinue
$resultTemp = "$ResultPath.tmp.$PID"

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-native-candidate-smoke-" + [Guid]::NewGuid().ToString("n"))
$profile = Join-Path $root "profile"
$dataRoot = Join-Path $root "data"
$configRoot = Join-Path $root "config"
$cacheRoot = Join-Path $root "cache"
$stateRoot = Join-Path $root "state"
$tmpRoot = Join-Path $root "tmp"
$workRoot = Join-Path $root "work"
foreach ($path in @($profile, $dataRoot, $configRoot, $cacheRoot, $stateRoot, $tmpRoot, $workRoot)) {
    New-Item -ItemType Directory -Path $path -Force | Out-Null
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
$isolation = [ordered]@{
    HOME = $profile
    USERPROFILE = $profile
    APPDATA = $configRoot
    LOCALAPPDATA = $dataRoot
    XDG_CONFIG_HOME = $configRoot
    XDG_CACHE_HOME = $cacheRoot
    XDG_DATA_HOME = (Join-Path $root "xdg-data")
    XDG_STATE_HOME = $stateRoot
    TEMP = $tmpRoot
    TMP = $tmpRoot
    CTX_DATA_ROOT = $dataRoot
    CTX_ANALYTICS_OFF = "1"
    CTX_UPGRADE_OFF = "1"
    CTX_DAEMON_AUTOSTART_OFF = "1"
    CTX_DISABLE_DAEMON = "1"
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

function Invoke-CtxRaw([string[]]$Arguments) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    $isCommandScript = [System.IO.Path]::GetExtension($Binary) -ieq ".cmd"
    if ($isCommandScript) {
        $start.FileName = $env:ComSpec
    } else {
        $start.FileName = $Binary
    }

    if ($null -ne $start.PSObject.Properties["ArgumentList"] -and -not $isCommandScript) {
        foreach ($argument in $Arguments) {
            [void]$start.ArgumentList.Add($argument)
        }
    } elseif ($isCommandScript) {
        $command = (@($Binary) + $Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
        $start.Arguments = "/d /s /c `"$command`""
    } else {
        $start.Arguments = ($Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
    }
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $start
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit($timeoutSeconds * 1000)) {
        try {
            $process.Kill($true)
        } catch [System.Management.Automation.MethodException] {
            $taskkill = Join-Path $env:SystemRoot "System32\taskkill.exe"
            & $taskkill /PID $process.Id /T /F 2>&1 | Out-Null
            if (-not $process.HasExited) {
                $process.Kill()
            }
        }
        $process.WaitForExit()
        Fail ("ctx command exceeded {0} seconds: {1}" -f $timeoutSeconds, ($Arguments -join " "))
    }
    $text = @($stdout.GetAwaiter().GetResult(), $stderr.GetAwaiter().GetResult()) |
        Where-Object { -not [string]::IsNullOrEmpty($_) }
    return [pscustomobject]@{
        ExitCode = $process.ExitCode
        Text = ($text -join [Environment]::NewLine).TrimEnd()
    }
}

function Invoke-Ctx([string[]]$Arguments) {
    $result = Invoke-CtxRaw $Arguments
    if ($result.ExitCode -ne 0) {
        Fail ("ctx {0} failed: {1}" -f ($Arguments -join " "), $result.Text)
    }
    return $result.Text
}

function Candidate-ProcessIds {
    $ids = @()
    foreach ($process in @(Get-Process -ErrorAction SilentlyContinue)) {
        try {
            if ($process.Path -eq $Binary) {
                $ids += [int]$process.Id
            }
        } catch {
            # Protected system processes may not expose Path. They cannot be
            # this user-owned candidate and are irrelevant to this assertion.
        }
    }
    return @($ids | Sort-Object -Unique)
}

try {
    foreach ($name in $isolation.Keys) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, [string]$isolation[$name], "Process")
    }
    Set-Location -LiteralPath $workRoot
    $baseline = @(Candidate-ProcessIds)

    $version = Invoke-Ctx @("--version")
    if ($version.Trim() -ne "ctx $ExpectedVersion") {
        Fail "version mismatch: expected ctx $ExpectedVersion, got $version"
    }

    [void](Invoke-Ctx @("setup", "--catalog-only", "--no-daemon", "--progress", "none"))
    $import = Invoke-Ctx @(
        "import", "--format", "ctx-history-jsonl-v1", "--path", $Fixture,
        "--no-daemon", "--json", "--progress", "none"
    )
    if ($import -notmatch '"imported_events"\s*:\s*[1-9][0-9]*') {
        Fail "fixture import did not import events"
    }

    $search = Invoke-Ctx @("search", "parser test", "--backend", "lexical", "--refresh", "off", "--json")
    if ($search -notmatch '"requested_mode"\s*:\s*"lexical"' -or
        $search -notmatch '"effective_mode"\s*:\s*"lexical"' -or
        $search -notmatch [regex]::Escape("Add a parser test.")) {
        Fail "lexical search did not return the expected fixture result"
    }

    $env:CTX_SEARCH_SEMANTIC = $null
    $env:CTX_DISABLE_DAEMON = $null
    try {
        $status = Invoke-Ctx @("status", "--json")
    } finally {
        $env:CTX_SEARCH_SEMANTIC = "0"
        $env:CTX_DISABLE_DAEMON = "1"
    }
    if ($status -notmatch '"read_only"\s*:\s*true') {
        Fail "read-only status command returned an unexpected payload"
    }
    if ($status -notmatch '"config_source"\s*:\s*"default"' -or
        $status -notmatch '"reason"\s*:\s*"semantic_disabled"') {
        Fail "native candidate does not report semantic search as disabled by default"
    }
    if ($status -match '"source"\s*:\s*"unsupported"') {
        Fail "native candidate unexpectedly reports semantic search as unsupported"
    }

    # Semantic search is supported but opt-in. Without a provisioned model, an
    # explicit offline request must fail before fallback, state, or network.
    $env:CTX_SEARCH_SEMANTIC = "1"
    $env:CTX_DAEMON_ENABLED = "1"
    $env:CTX_DISABLE_DAEMON = "0"
    $savedErrorActionPreference = $ErrorActionPreference
    try {
        # This command must fail. Windows PowerShell promotes native stderr to
        # NativeCommandError when the global preference is Stop, so capture it
        # under Continue and validate the exit status and message ourselves.
        $ErrorActionPreference = "Continue"
        $capabilityResult = Invoke-CtxRaw @("search", "parser test", "--backend", "semantic", "--refresh", "off", "--json")
        $capabilityOutput = $capabilityResult.Text
        $capabilityExit = $capabilityResult.ExitCode
    } finally {
        $ErrorActionPreference = $savedErrorActionPreference
    }
    $env:CTX_SEARCH_SEMANTIC = "0"
    $env:CTX_DAEMON_ENABLED = "0"
    $env:CTX_DISABLE_DAEMON = "1"
    $capabilityText = $capabilityOutput -join [Environment]::NewLine
    if ($capabilityExit -eq 0) {
        Fail "semantic-only search unexpectedly succeeded"
    }
    if ($capabilityText -notmatch 'semantic-only search will not initialize or download') {
        Fail "semantic-only search did not report the fail-closed capability contract"
    }
    if ($capabilityText -match '"effective_mode"\s*:\s*"lexical"') {
        Fail "semantic-only search silently fell back to lexical"
    }
    foreach ($unexpected in @(
        (Join-Path $root "semantic-cache"),
        (Join-Path $root "huggingface"),
        (Join-Path $dataRoot "vectors.sqlite"),
        (Join-Path $dataRoot "daemon")
    )) {
        if (Test-Path -LiteralPath $unexpected) {
            Fail "semantic-only search created semantic or daemon state"
        }
    }

    Start-Sleep -Milliseconds 200
    $remaining = @(Candidate-ProcessIds | Where-Object { $baseline -notcontains $_ })
    if ($remaining.Count -ne 0) {
        Fail ("candidate left background processes running: " + ($remaining -join ","))
    }
    if (Test-Path -LiteralPath (Join-Path $dataRoot "daemon\daemon.lock")) {
        Fail "candidate left a daemon lock behind"
    }

    $result = [ordered]@{
        schema_version = 1
        kind = "ctx-native-candidate-smoke"
        status = "passed"
        steps = [ordered]@{
            version = "passed"
            setup = "passed"
            import = "passed"
            search = "passed"
            read_only = "passed"
            semantic_offline_fail_closed = "passed"
        }
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
