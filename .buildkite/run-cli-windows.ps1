$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptDir
$coreRoot = Join-Path $repoRoot "core"

Set-Location $coreRoot

function Find-ExistingPath {
  param([string[]]$Candidates)

  foreach ($candidate in $Candidates) {
    if ([string]::IsNullOrWhiteSpace($candidate)) {
      continue
    }
    if (Test-Path $candidate) {
      return (Resolve-Path $candidate).Path
    }
  }

  return $null
}

function Resolve-Cargo {
  $cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
  if ($null -ne $cargoCommand) {
    return $cargoCommand.Source
  }

  $candidates = @()
  if (-not [string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
    $candidates += Join-Path $env:CARGO_HOME "bin/cargo.exe"
  }
  if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
    $candidates += Join-Path $env:USERPROFILE ".cargo/bin/cargo.exe"
  }
  if (-not [string]::IsNullOrWhiteSpace($env:SystemDrive)) {
    $candidates += Join-Path $env:SystemDrive "Users/buildkite/.cargo/bin/cargo.exe"
  }
  $candidates += "C:\Users\buildkite\.cargo\bin\cargo.exe"
  $candidates += "C:\Rust\.cargo\bin\cargo.exe"

  $cargoPath = Find-ExistingPath $candidates
  if ($null -ne $cargoPath) {
    $cargoBin = Split-Path -Parent $cargoPath
    $env:PATH = "${cargoBin};${env:PATH}"
    return $cargoPath
  }

  $rustupCommand = Get-Command rustup -ErrorAction SilentlyContinue
  if ($null -ne $rustupCommand) {
    $rustupCargo = & $rustupCommand.Source which cargo 2>$null | Select-Object -First 1
    if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($rustupCargo) -and (Test-Path $rustupCargo)) {
      $cargoBin = Split-Path -Parent $rustupCargo
      $env:PATH = "${cargoBin};${env:PATH}"
      return (Resolve-Path $rustupCargo).Path
    }
  }

  $checked = $candidates -join ", "
  throw "Windows Buildkite worker is missing Cargo. Install Rust/Cargo for the windows-x64 queue or add the existing Rustup .cargo\bin directory to PATH. Checked: ${checked}"
}

$cargo = Resolve-Cargo
Write-Host "Using Cargo at ${cargo}"
& $cargo --version

& $cargo test --manifest-path Cargo.toml -p ctx-http --bin ctx agent_work_cli::tests --locked
& $cargo build --manifest-path Cargo.toml -p ctx-http --bin ctx --locked

$ctxExe = Join-Path $coreRoot "target/debug/ctx.exe"
if (-not (Test-Path $ctxExe)) {
  throw "expected Windows ctx CLI artifact at $ctxExe"
}
