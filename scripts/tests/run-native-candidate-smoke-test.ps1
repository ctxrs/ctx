Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$smoke = Join-Path $repoRoot "scripts\run-native-candidate-smoke.ps1"
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-native-smoke-test-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null

try {
    $fake = Join-Path $root "ctx.cmd"
    @'
@echo off
if not "%CTX_ANALYTICS_ENABLED%"=="false" exit /b 91
if not "%CTX_UPGRADE_AUTO%"=="off" exit /b 92
if not "%CTX_DAEMON_AUTOSTART_OFF%"=="1" exit /b 93
if "%HOME%"=="" exit /b 94
if "%USERPROFILE%"=="" exit /b 95
echo %* | findstr /c:"--backend semantic" >nul
if not errorlevel 1 (
  if not "%CTX_SEARCH_SEMANTIC%"=="1" exit /b 96
  if not "%CTX_DAEMON_ENABLED%"=="true" exit /b 98
  1>&2 echo semantic-only search will not initialize or download intfloat/multilingual-e5-small during search
  exit /b 1
)
if "%1"=="--version" (
  echo ctx 0.25.0
  exit /b 0
)
if "%1"=="setup" exit /b 0
if "%1"=="import" (
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
    '{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}' |
        Set-Content -LiteralPath $fixture -Encoding Ascii
    $result = Join-Path $root "result.json"
    $expectedVersionFile = Join-Path $root "expected-version"
    "0.25.0`n" | Set-Content -LiteralPath $expectedVersionFile -NoNewline -Encoding Ascii

    & $smoke -Binary $fake -Fixture $fixture -ExpectedVersionFile $expectedVersionFile -ResultPath $result | Out-Null
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

    $hung = Join-Path $root "ctx-hang.cmd"
    "@echo off`r`nping -n 30 127.0.0.1 >nul`r`n" |
        Set-Content -LiteralPath $hung -Encoding Ascii
    $hungResult = Join-Path $root "hung-result.json"
    $savedTimeout = $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = "1"
    $started = Get-Date
    try {
        & $smoke -Binary $hung -Fixture $fixture -ExpectedVersion 0.25.0 -ResultPath $hungResult 2>$null | Out-Null
        throw "candidate smoke accepted a hung command"
    } catch {
        if ($_.Exception.Message -notmatch "exceeded 1 seconds") {
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

    Write-Host "Windows native candidate smoke tests passed"
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
