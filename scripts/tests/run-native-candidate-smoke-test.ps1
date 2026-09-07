Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$smoke = Join-Path $repoRoot "scripts\run-native-candidate-smoke.ps1"
$installer = Join-Path $repoRoot "scripts\install.ps1"
$smokeSource = [IO.File]::ReadAllText($smoke)
$installerSource = [IO.File]::ReadAllText($installer)
$outboxJson = [IO.File]::ReadAllText((Join-Path $PSScriptRoot "native-candidate-outbox.json")).Trim()
$analyticsPayload = ($outboxJson | ConvertFrom-Json).entries[0].payload
$outboxCSharp = $outboxJson.Replace('\', '\\').Replace('"', '\"')
if ($smokeSource -notmatch [regex]::Escape("--ctx-core-managed-pair-apply-v1") -or
    $smokeSource -notmatch [regex]::Escape('ctx.exe.install.json') -or
    $smokeSource -notmatch [regex]::Escape('[Text.Encoding]::UTF8.GetByteCount($pairApply.Stdout) -ne 83') -or
    $installerSource -notmatch [regex]::Escape('[Text.Encoding]::UTF8.GetByteCount($pairResult.Stdout) -ne 83') -or
    $installerSource -notmatch [regex]::Escape('$markerPath = "$installPath.install.json"')) {
    throw "Windows candidate smoke does not use the single Core managed-pair authority"
}
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx native smoke test " + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
$savedCI = $env:CI
$env:CI = "true"
$unrelated = $null
$unrelatedLauncher = $null
$pipeHolderPidPath = $null
$unrelatedPidPath = $null
$deprecatedControlNames = @(
    "CTX_ANALYTICS_OFF",
    "CTX_DISABLE_ANALYTICS",
    "CTX_INSTALL_DIAGNOSTICS_OFF",
    "CTX_DAEMON_OFF",
    "CTX_DISABLE_DAEMON",
    "CTX_UPGRADE_OFF",
    "CTX_DISABLE_AUTO_UPGRADE"
)
$testEnvironmentNames = @(
    "CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER",
    "CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER_PID",
    "CTX_NATIVE_CANDIDATE_TEST_READY",
    "CTX_NATIVE_CANDIDATE_TEST_BINARY",
    "CTX_NATIVE_CANDIDATE_TEST_UNRELATED_PID",
    "CTX_NATIVE_CANDIDATE_TEST_ROOT_EXIT_CODE",
    "CTX_NATIVE_CANDIDATE_TEST_ANALYTICS_DAEMON_PID",
    "CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS",
    "CTX_FAKE_MANAGED_PAIR_EXTRA_OUTPUT"
) + $deprecatedControlNames + @(
    "TEMP",
    "TMP"
)
$savedTestEnvironment = @{}
foreach ($name in $testEnvironmentNames) {
    $savedTestEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
$env:TEMP = $root
$env:TMP = $root
foreach ($name in $deprecatedControlNames) {
    [Environment]::SetEnvironmentVariable($name, "1", "Process")
}

try {
    $fake = Join-Path $root "ctx.cmd"
    $fakeTemplate = @'
@echo off
if "%HOME%"=="" exit /b 94
if "%USERPROFILE%"=="" exit /b 95
if not "%CI%"=="" exit /b 97
if not "%CTX_DAEMON_AUTOSTART_OFF%"=="1" exit /b 93
if defined CTX_ANALYTICS_OFF exit /b 84
if defined CTX_DISABLE_ANALYTICS exit /b 84
if defined CTX_INSTALL_DIAGNOSTICS_OFF exit /b 84
if defined CTX_DAEMON_OFF exit /b 84
if defined CTX_DISABLE_DAEMON exit /b 84
if defined CTX_UPGRADE_OFF exit /b 84
if defined CTX_DISABLE_AUTO_UPGRADE exit /b 84
if /I "%1"=="status" goto status
if /I "%1"=="daemon" goto daemon
if not "%CTX_ANALYTICS_ENABLED%"=="false" exit /b 91
if not "%CTX_UPGRADE_AUTO%"=="off" exit /b 92
set "CTX_FAKE_VERSION=0.25.0"
if /I "%~n0"=="ctx-v1" set "CTX_FAKE_VERSION=1.0.0"
echo %* | findstr /c:"--backend semantic" >nul
if not errorlevel 1 (
  if not "%CTX_SEARCH_SEMANTIC%"=="1" exit /b 96
  if not "%CTX_DAEMON_ENABLED%"=="true" exit /b 98
  1>&2 echo semantic-only search will not initialize or download intfloat/multilingual-e5-small during search
  exit /b 1
)
if "%1"=="--version" (
  echo ctx %CTX_FAKE_VERSION%
  exit /b 0
)
if "%1"=="setup" exit /b 0
if "%1"=="import" (
  for /L %%I in (1,1,2048) do (
    echo ordinary-stdout-%%I-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
    1>&2 echo ordinary-stderr-%%I-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
  )
  if "%CTX_FAKE_VERSION%"=="1.0.0" (
    mkdir "%CTX_DATA_ROOT%\search\lexical\ctx-generations" >nul
    mkdir "%CTX_DATA_ROOT%\search\lexical\index-generations\generation-11111111111111111111111111111111" >nul
    > "%CTX_DATA_ROOT%\search\lexical\active-generation.json" echo {"version":1,"active":{"generation_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","directory":"generation-11111111111111111111111111111111"},"previous":null}
    type nul > "%CTX_DATA_ROOT%\search\lexical\index-generations\generation-11111111111111111111111111111111\meta.json"
    type nul > "%CTX_DATA_ROOT%\search\lexical\ctx-generations\aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"
    echo {"totals":{"current_source_count":1,"current_indexed_documents":2},"sources":[{"published_generation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}
    exit /b 0
  )
  echo {"totals":{"imported_events":2}}
  exit /b 0
)
if "%1"=="search" (
  echo {"retrieval":{"requested_mode":"lexical","effective_mode":"lexical"},"results":[{"text":"Add a parser test."}]}
  exit /b 0
)
exit /b 99

:status
if "%CTX_ANALYTICS_ENABLED%"=="" goto status_default
if not "%CTX_ANALYTICS_ENABLED%"=="false" exit /b 91
if not "%CTX_UPGRADE_AUTO%"=="off" exit /b 92
if not "%CTX_DAEMON_ENABLED%"=="false" exit /b 90
echo {"read_only":true,"daemon":{"enabled":false},"upgrade":{"auto":"off","auto_enabled":false},"semantic":{"config_source":"default","enabled":false,"reason":"semantic_disabled","embed_policy":{"source":"dynamic_quiet"}}}
exit /b 0

:status_default
if not "%CTX_UPGRADE_AUTO%"=="" exit /b 92
if not "%CTX_DAEMON_ENABLED%"=="" exit /b 90
if not "%CTX_SEARCH_SEMANTIC%"=="" exit /b 89
if /I "%~n0"=="ctx-foreground-analytics" goto foreground_delivery
if /I "%LOCALAPPDATA%"=="%CTX_DATA_ROOT%" exit /b 85
if not exist "%LOCALAPPDATA%\ctx" mkdir "%LOCALAPPDATA%\ctx"
> "%LOCALAPPDATA%\ctx\analytics-outbox-v1.json" echo @ANALYTICS_OUTBOX@
if /I "%~n0"=="ctx-no-embed-policy" goto status_without_embed_policy
echo {"read_only":true,"daemon":{"enabled":true},"upgrade":{"auto":"off","auto_enabled":false},"semantic":{"config_source":"default","enabled":false,"reason":"semantic_disabled","embed_policy":{"source":"dynamic_quiet"}}}
exit /b 0

:status_without_embed_policy
echo {"read_only":true,"daemon":{"enabled":true},"upgrade":{"auto":"off","auto_enabled":false},"semantic":{"config_source":"default","enabled":false,"reason":"semantic_disabled"}}
exit /b 0

:foreground_delivery
call :write_analytics || exit /b 86
echo {"read_only":true,"daemon":{"enabled":true},"upgrade":{"auto":"off","auto_enabled":false},"semantic":{"config_source":"default","enabled":false,"reason":"semantic_disabled","embed_policy":{"source":"dynamic_quiet"}}}
exit /b 0

:daemon
if not "%2"=="run" exit /b 88
if not "%CTX_ANALYTICS_ENABLED%"=="" exit /b 91
if not "%CTX_UPGRADE_AUTO%"=="off" exit /b 92
if not "%CTX_DAEMON_ENABLED%"=="true" exit /b 90
if not "%CTX_DAEMON_MODE%"=="source-refresh-only" exit /b 87
if not "%CTX_SEARCH_SEMANTIC%"=="0" exit /b 96
if /I "%~n0"=="ctx-no-analytics-delivery" goto daemon_wait
call :write_analytics || exit /b 86
:daemon_wait
if not "%CTX_NATIVE_CANDIDATE_TEST_ANALYTICS_DAEMON_PID%"=="" powershell.exe -NoLogo -NoProfile -NonInteractive -Command "[IO.File]::WriteAllText($env:CTX_NATIVE_CANDIDATE_TEST_ANALYTICS_DAEMON_PID, [string]$PID); Start-Sleep -Seconds 30"
ping -n 30 127.0.0.1 >nul
exit /b 0

:write_analytics
set "CTX_FAKE_ANALYTICS_PAYLOAD=@ANALYTICS_PAYLOAD@"
powershell.exe -NoLogo -NoProfile -NonInteractive -Command "[IO.File]::WriteAllText(([Uri]$env:CTX_ANALYTICS_ENDPOINT).LocalPath, $env:CTX_FAKE_ANALYTICS_PAYLOAD + [Environment]::NewLine)"
exit /b %errorlevel%
'@
    function Write-AnalyticsFake([string]$Path, [string]$Outbox, [string]$Payload) {
        $fakeTemplate.Replace("@ANALYTICS_OUTBOX@", $Outbox).Replace(
            "@ANALYTICS_PAYLOAD@", $Payload) | Set-Content -LiteralPath $Path -Encoding Ascii
    }
    Write-AnalyticsFake $fake $outboxJson $analyticsPayload

    $fixture = Join-Path $root "fixture.jsonl"
    '{"record_type":"manifest","schema_version":"ctx-history-jsonl-v2"}' |
        Set-Content -LiteralPath $fixture -Encoding Ascii
    $result = Join-Path $root "result.json"
    $expectedVersionFile = Join-Path $root "expected-version"
    "0.25.0`n" | Set-Content -LiteralPath $expectedVersionFile -NoNewline -Encoding Ascii

    & $smoke -Binary $fake -Fixture $fixture -ExpectedVersionFile $expectedVersionFile -ResultPath $result | Out-Null
    if ($env:CI -ne "true") {
        throw "candidate smoke mutated parent CI"
    }
    $parsed = Get-Content -LiteralPath $result -Raw | ConvertFrom-Json
    if ($parsed.schema_version -ne 1 -or
        $parsed.kind -ne "ctx-native-candidate-smoke" -or
        $parsed.status -ne "passed") {
        throw "unexpected candidate smoke result envelope"
    }
    $topKeys = @($parsed.PSObject.Properties.Name)
    if (($topKeys -join ",") -ne "schema_version,kind,status,steps") {
        throw "candidate smoke result contains unexpected top-level keys"
    }
    $stepKeys = @($parsed.steps.PSObject.Properties.Name)
    if (($stepKeys -join ",") -ne "version,setup,import,search,read_only,released_defaults,explicit_opt_outs,semantic_offline_fail_closed") {
        throw "candidate smoke result contains unexpected step keys"
    }
    foreach ($key in $stepKeys) {
        if ($parsed.steps.$key -ne "passed") {
            throw "candidate smoke step did not pass: $key"
        }
    }

    foreach ($case in @("legacy", "malformed-entries", "event-mismatch", "duplicate-delivery", "non-v4", "wrong-operation")) {
        $outbox = $outboxJson | ConvertFrom-Json
        $payload = $analyticsPayload | ConvertFrom-Json
        $expectedFailure = "analytics evidence does not contain exactly one status UUIDv4"
        switch ($case) {
            "legacy" {
                $outbox.schema_version = 2
                $outbox.entries[0].schema_version = 2
                $expectedFailure = "candidate produced malformed analytics evidence"
            }
            "malformed-entries" {
                $outbox.entries = @{}
                $expectedFailure = "candidate produced malformed analytics evidence"
            }
            "event-mismatch" {
                $payload.events[0].event_id = "33333333-3333-4333-8333-333333333333"
                $expectedFailure = "daemon did not deliver the queued status analytics UUID"
            }
            "duplicate-delivery" { $payload.events = @($payload.events[0], $payload.events[0]) }
            "non-v4" {
                $payload.events[0].event_id = "11111111-1111-1111-8111-111111111111"
                $outbox.entries[0].payload = $payload | ConvertTo-Json -Depth 10 -Compress
            }
            "wrong-operation" {
                $payload.events[0].operation = "search"
                $outbox.entries[0].payload = $payload | ConvertTo-Json -Depth 10 -Compress
            }
        }
        $negativeFake = Join-Path $root "ctx-$case.cmd"
        $negativeResult = Join-Path $root "$case-result.json"
        Write-AnalyticsFake $negativeFake ($outbox | ConvertTo-Json -Depth 10 -Compress) ($payload | ConvertTo-Json -Depth 10 -Compress)
        try {
            & $smoke -Binary $negativeFake -Fixture $fixture -ExpectedVersion 0.25.0 -ResultPath $negativeResult 2>$null | Out-Null
            throw "candidate smoke accepted invalid analytics: $case"
        } catch {
            if ($_.Exception.Message -notmatch [regex]::Escape($expectedFailure)) { throw }
        }
        if (Test-Path -LiteralPath $negativeResult) { throw "candidate smoke wrote evidence for $case" }
    }

    $noEmbedPolicyFake = Join-Path $root "ctx-no-embed-policy.cmd"
    Copy-Item -LiteralPath $fake -Destination $noEmbedPolicyFake
    $noEmbedPolicyResult = Join-Path $root "no-embed-policy-result.json"
    & $smoke -Binary $noEmbedPolicyFake -Fixture $fixture `
        -ExpectedVersion 0.25.0 -ResultPath $noEmbedPolicyResult | Out-Null
    if ((Get-Content -LiteralPath $noEmbedPolicyResult -Raw | ConvertFrom-Json).status -ne "passed") {
        throw "candidate smoke rejected an omitted optional semantic embed policy"
    }

    $foregroundAnalyticsFake = Join-Path $root "ctx-foreground-analytics.cmd"
    Copy-Item -LiteralPath $fake -Destination $foregroundAnalyticsFake
    $foregroundAnalyticsResult = Join-Path $root "foreground-analytics-result.json"
    try {
        & $smoke -Binary $foregroundAnalyticsFake -Fixture $fixture `
            -ExpectedVersion 0.25.0 -ResultPath $foregroundAnalyticsResult 2>$null | Out-Null
        throw "candidate smoke accepted foreground analytics delivery"
    } catch {
        if ($_.Exception.Message -notmatch "foreground CLI delivered analytics before daemon ownership") {
            throw
        }
    }
    if (Test-Path -LiteralPath $foregroundAnalyticsResult) {
        throw "candidate smoke wrote evidence after foreground analytics delivery"
    }

    $noAnalyticsDeliveryFake = Join-Path $root "ctx-no-analytics-delivery.cmd"
    Copy-Item -LiteralPath $fake -Destination $noAnalyticsDeliveryFake
    $noAnalyticsDeliveryResult = Join-Path $root "no-analytics-delivery-result.json"
    $analyticsDaemonPidPath = Join-Path $root "analytics-daemon.pid"
    $savedTimeout = $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = "3"
    $env:CTX_NATIVE_CANDIDATE_TEST_ANALYTICS_DAEMON_PID = $analyticsDaemonPidPath
    $started = Get-Date
    try {
        & $smoke -Binary $noAnalyticsDeliveryFake -Fixture $fixture `
            -ExpectedVersion 0.25.0 -ResultPath $noAnalyticsDeliveryResult 2>$null | Out-Null
        throw "candidate smoke accepted an analytics daemon that did not deliver"
    } catch {
        if ($_.Exception.Message -notmatch
            "exceeded 3 seconds during analytics delivery; owned tree termination completed; final drain completed") {
            throw
        }
    } finally {
        $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = $savedTimeout
        $env:CTX_NATIVE_CANDIDATE_TEST_ANALYTICS_DAEMON_PID = $null
    }
    if (((Get-Date) - $started).TotalSeconds -ge 10) {
        throw "candidate analytics delivery timeout was not bounded"
    }
    if (Test-Path -LiteralPath $noAnalyticsDeliveryResult) {
        throw "candidate smoke wrote evidence after analytics delivery timeout"
    }
    if (-not (Test-Path -LiteralPath $analyticsDaemonPidPath -PathType Leaf)) {
        throw "analytics daemon survivor fixture did not start"
    }
    $analyticsDaemonPid = [int](Get-Content -LiteralPath $analyticsDaemonPidPath -Raw)
    if ($null -ne (Get-Process -Id $analyticsDaemonPid -ErrorAction SilentlyContinue)) {
        throw "candidate smoke left the timed-out analytics daemon tree running"
    }

    $freshEpochFake = Join-Path $root "ctx-v1.cmd"
    Copy-Item -LiteralPath $fake -Destination $freshEpochFake
    $freshEpochResult = Join-Path $root "fresh-epoch-result.json"
    & $smoke -Binary $freshEpochFake -Fixture $fixture -ExpectedVersion 1.0.0 -ResultPath $freshEpochResult | Out-Null
    $freshEpochParsed = Get-Content -LiteralPath $freshEpochResult -Raw | ConvertFrom-Json
    if ($freshEpochParsed.status -ne "passed") {
        throw "fresh-epoch candidate smoke did not pass"
    }

    $pairFake = Join-Path $root "ctx-pair.exe"
    $pairFakeSource = @'
using System;
using System.IO;
using System.Text;
using System.Threading;

public static class CtxManagedPairFake {
    private const string AnalyticsPayload = "{\"events\":[{\"event_name\":\"operation_completed\",\"event_version\":1,\"surface\":\"cli\",\"operation\":\"status\",\"outcome\":\"success\",\"event_id\":\"11111111-1111-4111-8111-111111111111\"}]}";

    private static bool Has(string[] args, string value) {
        return Array.IndexOf(args, value) >= 0;
    }

    private static void WriteOutbox() {
        string root = Path.Combine(Environment.GetEnvironmentVariable("LOCALAPPDATA"), "ctx");
        Directory.CreateDirectory(root);
        File.WriteAllText(Path.Combine(root, "analytics-outbox-v1.json"), "@ANALYTICS_OUTBOX@");
    }

    public static int Main(string[] args) {
        if (args.Length == 7 && args[0] == "--ctx-core-managed-pair-apply-v1") {
            string bin = Path.Combine(args[1], "bin");
            string libexec = Path.Combine(args[1], "libexec");
            string share = Path.Combine(args[1], "share", "ctx");
            Directory.CreateDirectory(bin);
            Directory.CreateDirectory(libexec);
            Directory.CreateDirectory(share);
            File.Copy(args[4], Path.Combine(bin, "ctx.exe"), true);
            File.Copy(args[5], Path.Combine(libexec, "ctx-pro.exe"), true);
            File.Copy(args[3], Path.Combine(share, "managed-pair-envelope.json"), true);
            File.Copy(args[6], Path.Combine(bin, "ctx.exe.install.json"), true);
            byte[] receipt = Encoding.UTF8.GetBytes(
                "{\"schema_version\":1,\"command\":\"managed_pair_apply\",\"ok\":true,\"status\":\"committed\"}\n");
            Stream stdout = Console.OpenStandardOutput();
            stdout.Write(receipt, 0, receipt.Length);
            if (Environment.GetEnvironmentVariable("CTX_FAKE_MANAGED_PAIR_EXTRA_OUTPUT") == "1") {
                byte[] extra = Encoding.UTF8.GetBytes("unexpected output\n");
                stdout.Write(extra, 0, extra.Length);
            }
            return 0;
        }
        if (Has(args, "--backend") && Has(args, "semantic")) {
            Console.Error.WriteLine("semantic-only search will not initialize or download a model");
            return 1;
        }
        if (args.Length == 1 && args[0] == "--version") {
            Console.WriteLine("ctx 0.25.0");
            return 0;
        }
        if (args.Length == 2 && args[0] == "pro" && args[1] == "--help") return 0;
        if (args.Length > 0 && args[0] == "setup") return 0;
        if (args.Length > 0 && args[0] == "import") {
            Console.WriteLine("{\"totals\":{\"imported_events\":2}}");
            return 0;
        }
        if (args.Length > 0 && args[0] == "search") {
            Console.WriteLine("{\"retrieval\":{\"requested_mode\":\"lexical\",\"effective_mode\":\"lexical\"},\"results\":[{\"text\":\"Add a parser test.\"}]}");
            return 0;
        }
        if (args.Length > 1 && args[0] == "daemon" && args[1] == "run") {
            File.WriteAllText(new Uri(Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENDPOINT")).LocalPath, AnalyticsPayload + Environment.NewLine);
            Thread.Sleep(30000);
            return 0;
        }
        if (args.Length > 0 && args[0] == "status") {
            if (Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENABLED") == null) {
                if (Environment.GetEnvironmentVariable("CTX_DAEMON_AUTOSTART_OFF") != "1") return 97;
                WriteOutbox();
                Console.WriteLine("{\"read_only\":true,\"daemon\":{\"enabled\":true},\"upgrade\":{\"auto\":\"apply\",\"auto_enabled\":true},\"semantic\":{\"config_source\":\"default\",\"reason\":\"semantic_disabled\",\"embed_policy\":{\"source\":\"dynamic_quiet\"}}}");
            } else {
                Console.WriteLine("{\"read_only\":true,\"daemon\":{\"enabled\":false},\"upgrade\":{\"auto\":\"off\",\"auto_enabled\":false},\"semantic\":{\"config_source\":\"default\",\"reason\":\"semantic_disabled\",\"embed_policy\":{\"source\":\"dynamic_quiet\"}}}");
            }
            return 0;
        }
        return 99;
    }
}
'@
    $pairFakeSource = $pairFakeSource.Replace("@ANALYTICS_OUTBOX@", $outboxCSharp)
    Add-Type -TypeDefinition $pairFakeSource -OutputAssembly $pairFake -OutputType ConsoleApplication
    $pairCompanion = Join-Path $root "ctx-pro.exe"
    $pairEnvelope = Join-Path $root "managed-pair-envelope.json"
    [IO.File]::WriteAllText($pairCompanion, "companion", [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($pairEnvelope, "{}", [Text.UTF8Encoding]::new($false))
    $pairResult = Join-Path $root "pair-result.json"
    & $smoke -Binary $pairFake -Companion $pairCompanion -PairEnvelope $pairEnvelope `
        -Fixture $fixture -ExpectedVersion 0.25.0 -ResultPath $pairResult | Out-Null
    $pairParsed = Get-Content -LiteralPath $pairResult -Raw | ConvertFrom-Json
    if ($pairParsed.status -ne "passed" -or
        $pairParsed.steps.managed_pair_apply -ne "passed" -or
        $pairParsed.steps.companion_selection -ne "passed") {
        throw "managed-pair candidate smoke did not pass"
    }
    $extraPairResult = Join-Path $root "pair-extra-output-result.json"
    $env:CTX_FAKE_MANAGED_PAIR_EXTRA_OUTPUT = "1"
    try {
        & $smoke -Binary $pairFake -Companion $pairCompanion -PairEnvelope $pairEnvelope `
            -Fixture $fixture -ExpectedVersion 0.25.0 -ResultPath $extraPairResult 2>$null | Out-Null
        throw "candidate smoke accepted extra managed-pair receipt output"
    } catch {
        if ($_.Exception.Message -notmatch "invalid managed-pair apply receipt") { throw }
    } finally {
        $env:CTX_FAKE_MANAGED_PAIR_EXTRA_OUTPUT = $null
    }
    if (Test-Path -LiteralPath $extraPairResult) {
        throw "candidate smoke wrote evidence after extra managed-pair receipt output"
    }

    $hung = Join-Path $root "ctx-hang.cmd"
    "@echo off`r`nif defined CI exit /b 97`r`nping -n 30 127.0.0.1 >nul`r`n" |
        Set-Content -LiteralPath $hung -Encoding Ascii
    $hungResult = Join-Path $root "hung-result.json"
    $savedTimeout = $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = "1"
    $started = Get-Date
    try {
        & $smoke -Binary $hung -Fixture $fixture -ExpectedVersion 0.25.0 -ResultPath $hungResult 2>$null | Out-Null
        throw "candidate smoke accepted a hung command"
    } catch {
        if ($_.Exception.Message -notmatch
            "exceeded 1 seconds during process exit; owned tree termination completed; final drain completed") {
            throw
        }
    } finally {
        $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = $savedTimeout
    }
    if (((Get-Date) - $started).TotalSeconds -ge 10) {
        throw "candidate smoke timeout was not bounded"
    }
    if (Test-Path -LiteralPath $hungResult) {
        throw "candidate smoke wrote evidence after a hung command"
    }

    $pipeHolder = Join-Path $root "ctx-pipe-holder.exe"
    $pipeHolderSource = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Threading;

public static class CtxPipeHolder {
    public static int Main(string[] args) {
        string mode = args.Length == 0 ? "" : args[0];
        if (mode == "--hold") {
            string pidPath = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER_PID");
            File.WriteAllText(pidPath, Process.GetCurrentProcess().Id.ToString());
            Thread.Sleep(30000);
            return 0;
        }
        if (mode == "--launch-unrelated") {
            string readyPath = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_READY");
            DateTime deadline = DateTime.UtcNow.AddSeconds(10);
            while (!File.Exists(readyPath) && DateTime.UtcNow < deadline) {
                Thread.Sleep(10);
            }
            if (!File.Exists(readyPath)) {
                return 98;
            }
            string candidate = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_BINARY");
            string pidPath = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_UNRELATED_PID");
            ProcessStartInfo start = new ProcessStartInfo(candidate, "--unrelated");
            start.UseShellExecute = false;
            using (Process unrelated = Process.Start(start)) {
                File.WriteAllText(pidPath, unrelated.Id.ToString());
                unrelated.WaitForExit();
                return unrelated.ExitCode;
            }
        }
        return 99;
    }
}
'@
    Add-Type -TypeDefinition $pipeHolderSource -Language CSharp `
        -OutputAssembly $pipeHolder -OutputType ConsoleApplication

    $pipeOwner = Join-Path $root "ctx-pipe-owner.exe"
    $pipeOwnerSource = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Threading;

public static class CtxPipeOwner {
    private const string AnalyticsPayload = "{\"events\":[{\"event_name\":\"operation_completed\",\"event_version\":1,\"surface\":\"cli\",\"operation\":\"status\",\"outcome\":\"success\",\"event_id\":\"11111111-1111-4111-8111-111111111111\"}]}";

    private static bool HasArgument(string[] args, string expected) {
        foreach (string arg in args) {
            if (String.Equals(arg, expected, StringComparison.OrdinalIgnoreCase)) {
                return true;
            }
        }
        return false;
    }

    public static int Main(string[] args) {
        string mode = args.Length == 0 ? "" : args[0];
        if (mode == "--unrelated") {
            Thread.Sleep(30000);
            return 0;
        }
        if (mode != "status" && mode != "daemon" &&
            Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENABLED") != "false") return 91;
        if (mode != "status" && mode != "daemon" &&
            Environment.GetEnvironmentVariable("CTX_UPGRADE_AUTO") != "off") return 92;
        if (Environment.GetEnvironmentVariable("CTX_DAEMON_AUTOSTART_OFF") != "1") return 93;
        if (String.IsNullOrEmpty(Environment.GetEnvironmentVariable("HOME"))) return 94;
        if (String.IsNullOrEmpty(Environment.GetEnvironmentVariable("USERPROFILE"))) return 95;
        if (!String.IsNullOrEmpty(Environment.GetEnvironmentVariable("CI"))) return 96;
        if (mode == "--version") {
            string readyPath = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_READY");
            string unrelatedPidPath = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_UNRELATED_PID");
            File.WriteAllText(readyPath, "ready");
            DateTime deadline = DateTime.UtcNow.AddSeconds(10);
            while (!File.Exists(unrelatedPidPath) && DateTime.UtcNow < deadline) {
                Thread.Sleep(10);
            }
            if (!File.Exists(unrelatedPidPath)) {
                return 97;
            }

            string holder = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER");
            ProcessStartInfo start = new ProcessStartInfo(holder, "--hold");
            start.UseShellExecute = false;
            Process.Start(start);
            Console.WriteLine("ctx 0.25.0");
            string forcedExitText = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_ROOT_EXIT_CODE");
            int forcedExitCode;
            if (Int32.TryParse(forcedExitText, out forcedExitCode)) {
                return forcedExitCode;
            }
            return 0;
        }
        if (mode == "setup") {
            return 0;
        }
        if (mode == "import") {
            Console.WriteLine("{\"totals\":{\"imported_events\":2}}");
            return 0;
        }
        if (mode == "search" && HasArgument(args, "semantic")) {
            if (Environment.GetEnvironmentVariable("CTX_SEARCH_SEMANTIC") != "1") return 98;
            if (Environment.GetEnvironmentVariable("CTX_DAEMON_ENABLED") != "true") return 99;
            Console.Error.WriteLine("semantic-only search will not initialize or download a model during search");
            return 1;
        }
        if (mode == "search") {
            Console.WriteLine("{\"retrieval\":{\"requested_mode\":\"lexical\",\"effective_mode\":\"lexical\"},\"results\":[{\"text\":\"Add a parser test.\"}]}");
            return 0;
        }
        if (mode == "daemon") {
            File.WriteAllText(new Uri(Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENDPOINT")).LocalPath, AnalyticsPayload + Environment.NewLine);
            Thread.Sleep(30000);
            return 0;
        }
        if (mode == "status") {
            if (Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENABLED") == null) {
                string state = Path.Combine(Environment.GetEnvironmentVariable("LOCALAPPDATA"), "ctx");
                Directory.CreateDirectory(state);
                File.WriteAllText(Path.Combine(state, "analytics-outbox-v1.json"), "@ANALYTICS_OUTBOX@");
                Console.WriteLine("{\"read_only\":true,\"daemon\":{\"enabled\":true},\"upgrade\":{\"auto\":\"off\",\"auto_enabled\":false},\"semantic\":{\"config_source\":\"default\",\"reason\":\"semantic_disabled\",\"embed_policy\":{\"source\":\"dynamic_quiet\"}}}");
            } else {
                Console.WriteLine("{\"read_only\":true,\"daemon\":{\"enabled\":false},\"upgrade\":{\"auto\":\"off\",\"auto_enabled\":false},\"semantic\":{\"config_source\":\"default\",\"reason\":\"semantic_disabled\",\"embed_policy\":{\"source\":\"dynamic_quiet\"}}}");
            }
            return 0;
        }
        return 99;
    }
}
'@
    $pipeOwnerSource = $pipeOwnerSource.Replace("@ANALYTICS_OUTBOX@", $outboxCSharp)
    Add-Type -TypeDefinition $pipeOwnerSource -Language CSharp `
        -OutputAssembly $pipeOwner -OutputType ConsoleApplication

    $readyPath = Join-Path $root "pipe-owner-ready"
    $pipeHolderPidPath = Join-Path $root "pipe-holder.pid"
    $unrelatedPidPath = Join-Path $root "unrelated.pid"
    $pipeOwnerResult = Join-Path $root "pipe-owner-result.json"
    $savedTimeout = $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
    $env:CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER = $pipeHolder
    $env:CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER_PID = $pipeHolderPidPath
    $env:CTX_NATIVE_CANDIDATE_TEST_READY = $readyPath
    $env:CTX_NATIVE_CANDIDATE_TEST_BINARY = $pipeOwner
    $env:CTX_NATIVE_CANDIDATE_TEST_UNRELATED_PID = $unrelatedPidPath
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = "60"
    # This launcher is outside the candidate tree. It starts the same candidate
    # image only after the job-owned root signals that it is running.
    $unrelatedLauncher = Start-Process -FilePath $pipeHolder `
        -ArgumentList "--launch-unrelated" -PassThru
    if ($unrelatedLauncher.HasExited) {
        throw "unrelated candidate launcher exited before the pipe-drain test"
    }
    $started = Get-Date
    try {
        & $smoke -Binary $pipeOwner -Fixture $fixture -ExpectedVersion 0.25.0 `
            -ResultPath $pipeOwnerResult | Out-Null
    } finally {
        $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = $savedTimeout
    }
    if (((Get-Date) - $started).TotalSeconds -ge 15) {
        throw "candidate smoke waited too long to clean up a post-root-exit pipe holder"
    }
    if (-not (Test-Path -LiteralPath $pipeHolderPidPath -PathType Leaf)) {
        throw "candidate smoke fixture did not create the redirected pipe owner"
    }
    $pipeHolderPid = [int](Get-Content -LiteralPath $pipeHolderPidPath -Raw)
    if ($null -ne (Get-Process -Id $pipeHolderPid -ErrorAction SilentlyContinue)) {
        throw "candidate smoke left the redirected pipe owner running"
    }
    if (-not (Test-Path -LiteralPath $unrelatedPidPath -PathType Leaf)) {
        throw "unrelated same-image candidate fixture did not start"
    }
    $unrelatedPid = [int](Get-Content -LiteralPath $unrelatedPidPath -Raw)
    $unrelated = Get-Process -Id $unrelatedPid -ErrorAction SilentlyContinue
    if ($null -eq $unrelated -or $unrelated.HasExited) {
        throw "candidate smoke killed an unrelated same-image process"
    }
    if (-not (Test-Path -LiteralPath $pipeOwnerResult -PathType Leaf)) {
        throw "candidate smoke did not write evidence after owned pipe-holder cleanup"
    }
    $pipeOwnerParsed = Get-Content -LiteralPath $pipeOwnerResult -Raw | ConvertFrom-Json
    if ($pipeOwnerParsed.status -ne "passed") {
        throw "candidate smoke did not pass after owned pipe-holder cleanup"
    }

    $pipeHolderPidPath = Join-Path $root "failed-pipe-holder.pid"
    $failedPipeOwnerResult = Join-Path $root "failed-pipe-owner-result.json"
    $env:CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER_PID = $pipeHolderPidPath
    $env:CTX_NATIVE_CANDIDATE_TEST_ROOT_EXIT_CODE = "7"
    $started = Get-Date
    try {
        & $smoke -Binary $pipeOwner -Fixture $fixture -ExpectedVersion 0.25.0 `
            -ResultPath $failedPipeOwnerResult 2>$null | Out-Null
        throw "candidate smoke accepted a failed root after owned pipe-holder cleanup"
    } catch {
        if ($_.Exception.Message -notmatch "ctx --version failed: ctx 0.25.0") {
            throw
        }
    } finally {
        $env:CTX_NATIVE_CANDIDATE_TEST_ROOT_EXIT_CODE = $null
    }
    if (((Get-Date) - $started).TotalSeconds -ge 15) {
        throw "candidate smoke waited too long to preserve a failed root result"
    }
    if (-not (Test-Path -LiteralPath $pipeHolderPidPath -PathType Leaf)) {
        throw "failed-root fixture did not create the redirected pipe owner"
    }
    $pipeHolderPid = [int](Get-Content -LiteralPath $pipeHolderPidPath -Raw)
    if ($null -ne (Get-Process -Id $pipeHolderPid -ErrorAction SilentlyContinue)) {
        throw "candidate smoke left the failed root's redirected pipe owner running"
    }
    if (Test-Path -LiteralPath $failedPipeOwnerResult) {
        throw "candidate smoke wrote evidence after a failed root command"
    }
    if ($unrelated.HasExited) {
        throw "failed root cleanup killed an unrelated same-image process"
    }

    Write-Host "Windows native candidate smoke tests passed"
} finally {
    if ($null -eq $unrelated -and
        -not [string]::IsNullOrWhiteSpace($unrelatedPidPath) -and
        (Test-Path -LiteralPath $unrelatedPidPath -PathType Leaf)) {
        $unrelatedPid = [int](Get-Content -LiteralPath $unrelatedPidPath -Raw)
        $unrelated = Get-Process -Id $unrelatedPid -ErrorAction SilentlyContinue
    }
    if ($null -ne $unrelated -and -not $unrelated.HasExited) {
        Stop-Process -Id $unrelated.Id -Force -ErrorAction SilentlyContinue
        [void]$unrelated.WaitForExit(5000)
    }
    if ($null -ne $unrelated) {
        $unrelated.Dispose()
    }
    if ($null -ne $unrelatedLauncher -and -not $unrelatedLauncher.HasExited) {
        Stop-Process -Id $unrelatedLauncher.Id -Force -ErrorAction SilentlyContinue
        [void]$unrelatedLauncher.WaitForExit(5000)
    }
    if ($null -ne $unrelatedLauncher) {
        $unrelatedLauncher.Dispose()
    }
    if (-not [string]::IsNullOrWhiteSpace($pipeHolderPidPath) -and
        (Test-Path -LiteralPath $pipeHolderPidPath -PathType Leaf)) {
        $pipeHolderPid = [int](Get-Content -LiteralPath $pipeHolderPidPath -Raw)
        $pipeHolderProcess = Get-Process -Id $pipeHolderPid -ErrorAction SilentlyContinue
        if ($null -ne $pipeHolderProcess) {
            Stop-Process -InputObject $pipeHolderProcess -Force -ErrorAction SilentlyContinue
            [void]$pipeHolderProcess.WaitForExit(5000)
            $pipeHolderProcess.Dispose()
        }
    }
    foreach ($name in $testEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($name, $savedTestEnvironment[$name], "Process")
    }
    $env:CI = $savedCI
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
