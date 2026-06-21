Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail($Message) {
  Write-Error $Message
  exit 1
}

function Require-Command($Name) {
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    Fail "$Name is required"
  }
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptDir
$jobId = if ($env:BUILDKITE_JOB_ID) { $env:BUILDKITE_JOB_ID } else { "manual" }
$tempRoot = if ($env:TEMP) { $env:TEMP } else { [System.IO.Path]::GetTempPath() }
$downloadRoot = Join-Path $tempRoot "ctx-cli-windows-x64-smoke-$jobId"
$summaryPath = Join-Path $repoRoot "ctx-cli-windows-x64-smoke.json"

Set-Location $repoRoot
Require-Command "buildkite-agent"

Remove-Item -Recurse -Force $downloadRoot -ErrorAction SilentlyContinue
Remove-Item -Force $summaryPath -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $downloadRoot | Out-Null

& buildkite-agent artifact download "ctx-cli-windows-x64/ctx.exe" $downloadRoot
if ($LASTEXITCODE -ne 0) {
  Fail "failed to download ctx-cli-windows-x64/ctx.exe"
}
& buildkite-agent artifact download "ctx-cli-windows-x64/ctx.exe.sha256" $downloadRoot
if ($LASTEXITCODE -ne 0) {
  Fail "failed to download ctx-cli-windows-x64/ctx.exe.sha256"
}

$ctxExe = Get-ChildItem -Path $downloadRoot -Filter "ctx.exe" -File -Recurse | Select-Object -First 1
if (-not $ctxExe) {
  Fail "artifact download did not produce ctx.exe"
}
$shaFile = Get-ChildItem -Path $downloadRoot -Filter "ctx.exe.sha256" -File -Recurse | Select-Object -First 1
if (-not $shaFile) {
  Fail "artifact download did not produce ctx.exe.sha256"
}

$actualSha = (Get-FileHash -Algorithm SHA256 -Path $ctxExe.FullName).Hash.ToLowerInvariant()
$expectedSha = ((Get-Content -Raw -Path $shaFile.FullName) -split "\s+")[0].ToLowerInvariant()
if ($actualSha -ne $expectedSha) {
  Fail "ctx.exe sha256 $actualSha did not match artifact manifest $expectedSha"
}

$rootHelp = & $ctxExe.FullName --help
if ($LASTEXITCODE -ne 0) {
  Fail "ctx.exe --help failed with exit code $LASTEXITCODE"
}
if (-not ($rootHelp -match "Usage")) {
  Fail "ctx.exe --help did not look like the root CLI help"
}
$workHelp = & $ctxExe.FullName work --help
if ($LASTEXITCODE -ne 0) {
  Fail "ctx.exe work --help failed with exit code $LASTEXITCODE"
}
if (-not ($workHelp -match "Work")) {
  Fail "ctx.exe work --help did not look like the Work CLI"
}

$osCaption = "windows"
try {
  $osCaption = (Get-CimInstance Win32_OperatingSystem).Caption
} catch {
  $osCaption = "windows"
}

$summary = [ordered]@{
  platform = "windows-x64"
  sha256 = $actualSha
  root_help_checked = $true
  work_help_checked = $true
  runtime = [ordered]@{
    os_family = "windows"
    os_caption = $osCaption
    architecture = [string]$env:PROCESSOR_ARCHITECTURE
    lane = "cli-windows-x64-smoke"
  }
}
$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding UTF8
& buildkite-agent artifact upload $summaryPath
if ($LASTEXITCODE -ne 0) {
  Fail "failed to upload $summaryPath"
}

Write-Output "validated windows-x64 ctx.exe help and Work CLI"
