Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$smoke = Join-Path $repoRoot "scripts\run-native-candidate-smoke.ps1"
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-native-smoke-test-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
$savedCI = $env:CI
$env:CI = "true"
$unrelated = $null
$unrelatedLauncher = $null
$pipeHolderPidPath = $null
$unrelatedPidPath = $null
$testEnvironmentNames = @(
    "CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER",
    "CTX_NATIVE_CANDIDATE_TEST_PIPE_HOLDER_PID",
    "CTX_NATIVE_CANDIDATE_TEST_READY",
    "CTX_NATIVE_CANDIDATE_TEST_BINARY",
    "CTX_NATIVE_CANDIDATE_TEST_UNRELATED_PID",
    "CTX_NATIVE_CANDIDATE_TEST_ROOT_EXIT_CODE",
    "CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS"
)
$savedTestEnvironment = @{}
foreach ($name in $testEnvironmentNames) {
    $savedTestEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}

try {
    $fake = Join-Path $root "ctx.cmd"
    @'
@echo off
if not "%CTX_ANALYTICS_ENABLED%"=="false" exit /b 91
if not "%CTX_UPGRADE_AUTO%"=="off" exit /b 92
if not "%CTX_DAEMON_AUTOSTART_OFF%"=="1" exit /b 93
if "%HOME%"=="" exit /b 94
if "%USERPROFILE%"=="" exit /b 95
if not "%CI%"=="" exit /b 97
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
if "%1"=="status" (
  if not "%CTX_SEARCH_SEMANTIC%"=="" exit /b 89
  if not "%CTX_DAEMON_ENABLED%"=="" exit /b 90
  echo {"read_only":true,"semantic":{"config_source":"default","enabled":false,"reason":"semantic_disabled","embed_policy":{"source":"dynamic_quiet"}}}
  exit /b 0
)
exit /b 99
'@ | Set-Content -LiteralPath $fake -Encoding Ascii

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
    if (($stepKeys -join ",") -ne "version,setup,import,search,read_only,semantic_offline_fail_closed") {
        throw "candidate smoke result contains unexpected step keys"
    }
    foreach ($key in $stepKeys) {
        if ($parsed.steps.$key -ne "passed") {
            throw "candidate smoke step did not pass: $key"
        }
    }

    $freshEpochFake = Join-Path $root "ctx-v1.cmd"
    Copy-Item -LiteralPath $fake -Destination $freshEpochFake
    $freshEpochResult = Join-Path $root "fresh-epoch-result.json"
    & $smoke -Binary $freshEpochFake -Fixture $fixture -ExpectedVersion 1.0.0 -ResultPath $freshEpochResult | Out-Null
    $freshEpochParsed = Get-Content -LiteralPath $freshEpochResult -Raw | ConvertFrom-Json
    if ($freshEpochParsed.status -ne "passed") {
        throw "fresh-epoch candidate smoke did not pass"
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
        if (Environment.GetEnvironmentVariable("CTX_ANALYTICS_ENABLED") != "false") return 91;
        if (Environment.GetEnvironmentVariable("CTX_UPGRADE_AUTO") != "off") return 92;
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
        if (mode == "status") {
            if (Environment.GetEnvironmentVariable("CTX_SEARCH_SEMANTIC") != null) return 89;
            if (Environment.GetEnvironmentVariable("CTX_DAEMON_ENABLED") != null) return 90;
            Console.WriteLine("{\"read_only\":true,\"semantic\":{\"config_source\":\"default\",\"enabled\":false,\"reason\":\"semantic_disabled\",\"embed_policy\":{\"source\":\"dynamic_quiet\"}}}");
            return 0;
        }
        return 99;
    }
}
'@
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
