param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("platform-smoke", "release-dry-run", "release-artifact-smoke")]
  [string]$Mode
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Set-StrictMode -Version Latest

function Join-PathSafe {
  param([string]$Base, [string]$Child)
  return [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($Base, $Child))
}

function Positive-Int {
  param([string]$Value)
  return $Value -match '^[0-9]+$' -and [int]$Value -gt 0
}

function Json-Escape {
  param([string]$Value)
  return ($Value | ConvertTo-Json -Compress)
}

function Run-Step {
  param(
    [string]$Name,
    [scriptblock]$Body
  )

  Write-Host "==> $Name"
  $started = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  try {
    & $Body
    $exitCode = 0
    $status = "passed"
  } catch {
    $exitCode = if ($LASTEXITCODE -ne $null -and $LASTEXITCODE -ne 0) { $LASTEXITCODE } else { 1 }
    $status = "failed"
    throw
  } finally {
    $ended = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $duration = $ended - $started
    Add-Content -Path $script:TimingEvents -Value (@{
      name = $Name
      status = $status
      started_at_unix_s = $started
      ended_at_unix_s = $ended
      duration_s = $duration
      exit_code = $exitCode
      note = $Name
    } | ConvertTo-Json -Compress)
  }
}

function Finish-Timings {
  if (Test-Path $script:TimingEvents) {
    $events = Get-Content -Path $script:TimingEvents -Raw
    if ($events.Trim().Length -eq 0) {
      "[]" | Set-Content -Path $script:TimingFile -Encoding utf8
    } else {
      $json = @()
      foreach ($line in (Get-Content -Path $script:TimingEvents)) {
        if ($line.Trim().Length -gt 0) {
          $json += ($line | ConvertFrom-Json)
        }
      }
      $json | ConvertTo-Json -Depth 8 | Set-Content -Path $script:TimingFile -Encoding utf8
    }
    Remove-Item -Force $script:TimingEvents
    Write-Host "timing artifact: $script:TimingFile"
  }
}

function Require-Command {
  param([string]$Name)
  $cmd = Get-Command $Name -ErrorAction SilentlyContinue
  if (-not $cmd) {
    throw "$Name is required but was not found on PATH=$env:PATH"
  }
  return $cmd.Source
}

function Download-File {
  param(
    [string]$Uri,
    [string]$OutFile
  )

  $tmp = "$OutFile.download"
  if (Test-Path $tmp) {
    Remove-Item -Force $tmp
  }

  $curl = Get-Command "curl.exe" -ErrorAction SilentlyContinue
  if ($curl) {
    Write-Host "Downloading $Uri with curl.exe"
    & $curl.Source --fail --location --show-error --retry 5 --retry-delay 5 --connect-timeout 30 --max-time 900 --output $tmp $Uri
    if ($LASTEXITCODE -ne 0) {
      throw "curl.exe download failed with exit code $LASTEXITCODE for $Uri"
    }
  } else {
    Write-Host "Downloading $Uri with Invoke-WebRequest"
    Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $tmp
  }

  if (-not (Test-Path $tmp)) {
    throw "download did not produce expected file: $tmp"
  }
  $bytes = (Get-Item $tmp).Length
  if ($bytes -le 0) {
    throw "download produced an empty file: $tmp"
  }
  Move-Item -Force $tmp $OutFile
  Write-Host "Downloaded $Uri to $OutFile ($bytes bytes)"
}

function Rust-Tools-Available {
  try {
    Require-Command "cargo" | Out-Null
    Require-Command "rustc" | Out-Null
    & cargo fmt --version | Out-Null
    if ($LASTEXITCODE -ne 0) { return $false }
    & cargo clippy --version | Out-Null
    if ($LASTEXITCODE -ne 0) { return $false }
    return $true
  } catch {
    return $false
  }
}

function Current-Rust-Host-Triple {
  try {
    $lines = & rustc -vV
    if ($LASTEXITCODE -ne 0) { return $null }
    foreach ($line in $lines) {
      if ($line -match '^host:\s+(.+)$') {
        return $Matches[1]
      }
    }
    return $null
  } catch {
    return $null
  }
}

function Default-Windows-Tool-Cache-Root {
  if ($env:CTX_WINDOWS_TOOL_CACHE_ROOT) {
    return $env:CTX_WINDOWS_TOOL_CACHE_ROOT
  }
  if ($env:BUILDKITE_AGENT_HOME) {
    return Join-PathSafe $env:BUILDKITE_AGENT_HOME "tool-cache\ctx-records"
  }
  if ($env:ProgramData) {
    return Join-PathSafe $env:ProgramData "ctx-buildkite\tool-cache\ctx-records"
  }
  return Join-PathSafe $script:RepoRoot "target\tool-cache\windows"
}

function Invoke-Batch-Environment {
  param(
    [string]$BatchFile,
    [string[]]$BatchArgs
  )

  if (-not (Test-Path $BatchFile)) {
    throw "MSVC environment script not found: $BatchFile"
  }

  $escapedArgs = @()
  foreach ($arg in $BatchArgs) {
    $escapedArgs += '"' + ($arg -replace '"', '\"') + '"'
  }
  $command = 'call "' + $BatchFile + '" ' + ($escapedArgs -join ' ') + ' >nul && set'
  $output = & cmd.exe /d /s /c $command
  if ($LASTEXITCODE -ne 0) {
    throw "MSVC environment script failed: $BatchFile"
  }

  foreach ($line in $output) {
    $separator = $line.IndexOf("=")
    if ($separator -gt 0) {
      $name = $line.Substring(0, $separator)
      $value = $line.Substring($separator + 1)
      Set-Item -Path "Env:$name" -Value $value
    }
  }
}

function Find-MSVC-Environment-Script {
  $candidates = @()
  $programFilesX86 = [Environment]::GetEnvironmentVariable("ProgramFiles(x86)")
  $programFiles = [Environment]::GetEnvironmentVariable("ProgramFiles")

  if ($programFilesX86) {
    $vswhere = Join-PathSafe $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
      $installPath = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null | Select-Object -First 1)
      if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($installPath)) {
        $candidates += Join-PathSafe $installPath "Common7\Tools\VsDevCmd.bat"
        $candidates += Join-PathSafe $installPath "VC\Auxiliary\Build\vcvars64.bat"
      }
    }
  }

  foreach ($root in @($programFilesX86, $programFiles)) {
    if ([string]::IsNullOrWhiteSpace($root)) {
      continue
    }
    foreach ($year in @("2022", "2019", "2017")) {
      foreach ($edition in @("BuildTools", "Community", "Professional", "Enterprise")) {
        $base = Join-PathSafe $root "Microsoft Visual Studio\$year\$edition"
        $candidates += Join-PathSafe $base "Common7\Tools\VsDevCmd.bat"
        $candidates += Join-PathSafe $base "VC\Auxiliary\Build\vcvars64.bat"
      }
    }
  }

  foreach ($candidate in $candidates) {
    if (Test-Path $candidate) {
      return $candidate
    }
  }

  return $null
}

function Test-Process-Is-Elevated {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = [Security.Principal.WindowsPrincipal]::new($identity)
  return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Install-Visual-Studio-Build-Tools {
  if ($env:CTX_WINDOWS_BOOTSTRAP_MSVC -ne "1") {
    return
  }

  if (-not (Test-Process-Is-Elevated)) {
    throw "CTX_WINDOWS_BOOTSTRAP_MSVC=1 but this Buildkite job is not elevated; install Visual Studio Build Tools on the windows-x64 agent or run the lane on a prepared Windows worker"
  }

  $toolCache = Default-Windows-Tool-Cache-Root
  $installerDir = Join-PathSafe $toolCache "vs-buildtools-installer"
  New-Item -ItemType Directory -Force -Path $installerDir | Out-Null
  $installer = Join-PathSafe $installerDir "vs_BuildTools.exe"
  if (-not (Test-Path $installer)) {
    $url = "https://aka.ms/vs/17/release/vs_BuildTools.exe"
    Write-Host "Visual Studio Build Tools not found; downloading installer from $url"
    Download-File -Uri $url -OutFile $installer
  }

  $installArgs = @(
    "--quiet",
    "--wait",
    "--norestart",
    "--nocache",
    "--add", "Microsoft.VisualStudio.Workload.VCTools",
    "--add", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    "--add", "Microsoft.VisualStudio.Component.Windows11SDK.26100",
    "--includeRecommended"
  )
  Write-Host "Installing Visual Studio Build Tools for Windows release verification"
  $process = Start-Process -FilePath $installer -ArgumentList $installArgs -Wait -PassThru
  if ($process.ExitCode -ne 0 -and $process.ExitCode -ne 3010) {
    throw "Visual Studio Build Tools installer failed with exit code $($process.ExitCode)"
  }
  if ($process.ExitCode -eq 3010) {
    Write-Host "Visual Studio Build Tools installer requested reboot; continuing to probe toolchain availability"
  }
}

function Write-Tool-Wrapper {
  param(
    [string]$Path,
    [string]$Command
  )
  Set-Content -Path $Path -Value $Command -Encoding ascii
}

function Ensure-MinGW-LibgccEh-Compatibility {
  param(
    [string]$MingwRoot,
    [string]$Gcc
  )

  $libgccOutput = & $Gcc -print-libgcc-file-name
  if ($LASTEXITCODE -ne 0) {
    throw "failed to locate libgcc with $Gcc -print-libgcc-file-name"
  }

  $libgccPath = [string]($libgccOutput | Select-Object -First 1)
  if (-not [string]::IsNullOrWhiteSpace($libgccPath)) {
    $libgccPath = $libgccPath.Trim()
  }

  if ([string]::IsNullOrWhiteSpace($libgccPath) -or -not (Test-Path $libgccPath)) {
    $libgcc = Get-ChildItem -Path $MingwRoot -Recurse -File -Filter "libgcc.a" |
      Select-Object -First 1
    if (-not $libgcc) {
      throw "w64devkit did not provide libgcc.a under $MingwRoot"
    }
    $libgccPath = $libgcc.FullName
  }

  $libgccDir = Split-Path -Parent $libgccPath
  $libgccEh = Join-PathSafe $libgccDir "libgcc_eh.a"
  if (-not (Test-Path $libgccEh)) {
    Copy-Item -Force -Path $libgccPath -Destination $libgccEh
    Write-Host "Provisioned missing Rust GNU compatibility archive: $libgccEh"
  }
}

function Ensure-MinGW-GNU-Build-Environment {
  if ($env:CTX_EXPECT_HOST_TRIPLE -ne "x86_64-pc-windows-gnu") {
    return
  }

  $toolCache = Default-Windows-Tool-Cache-Root
  $mingwVersion = if ($env:CTX_W64DEVKIT_VERSION) { $env:CTX_W64DEVKIT_VERSION } else { "2.8.0" }
  $mingwName = "w64devkit-x64-$mingwVersion"
  $mingwCache = Join-PathSafe $toolCache "w64devkit"
  $mingwRoot = Join-PathSafe $mingwCache $mingwName
  $mingwBin = Join-PathSafe $mingwRoot "bin"
  $mingwGcc = Join-PathSafe $mingwBin "gcc.exe"
  $mingwGxx = Join-PathSafe $mingwBin "g++.exe"
  $mingwAr = Join-PathSafe $mingwBin "ar.exe"
  $archive = Join-PathSafe $mingwCache "$mingwName.7z.exe"
  $sevenZip = Join-PathSafe $mingwCache "7zr.exe"
  New-Item -ItemType Directory -Force -Path $mingwCache | Out-Null

  if (-not (Test-Path $mingwGcc)) {
    if (-not (Test-Path $archive)) {
      $url = "https://github.com/skeeto/w64devkit/releases/download/v$mingwVersion/$mingwName.7z.exe"
      Write-Host "w64devkit not found; downloading $url"
      Download-File -Uri $url -OutFile $archive
    }
    if (-not (Test-Path $sevenZip)) {
      $url = "https://www.7-zip.org/a/7zr.exe"
      Write-Host "7zr extractor not found; downloading $url"
      Download-File -Uri $url -OutFile $sevenZip
    }
    $extractDir = Join-PathSafe $mingwCache "extract-$mingwName"
    if (Test-Path $extractDir) {
      Remove-Item -Recurse -Force $extractDir
    }
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
    & $sevenZip x $archive "-o$extractDir" -y
    if ($LASTEXITCODE -ne 0) {
      throw "w64devkit extraction failed with exit code $LASTEXITCODE"
    }
    $candidateRoots = @((Get-Item -Path $extractDir)) + @(Get-ChildItem -Path $extractDir -Directory -Recurse)
    $extractedRoot = $candidateRoots |
      Where-Object { Test-Path (Join-PathSafe $_.FullName "bin\gcc.exe") } |
      Select-Object -First 1
    if (-not $extractedRoot) {
      throw "w64devkit archive did not contain expected bin\gcc.exe under $extractDir"
    }
    if (Test-Path $mingwRoot) {
      Remove-Item -Recurse -Force $mingwRoot
    }
    if ([System.IO.Path]::GetFullPath($extractedRoot.FullName) -eq [System.IO.Path]::GetFullPath($extractDir)) {
      Move-Item -Force $extractDir $mingwRoot
    } else {
      Move-Item -Force $extractedRoot.FullName $mingwRoot
      Remove-Item -Recurse -Force $extractDir
    }
  }

  Ensure-MinGW-LibgccEh-Compatibility -MingwRoot $mingwRoot -Gcc $mingwGcc

  $env:CC_x86_64_pc_windows_gnu = $mingwGcc
  $env:CXX_x86_64_pc_windows_gnu = $mingwGxx
  $env:AR_x86_64_pc_windows_gnu = $mingwAr
  $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = $mingwGcc
  $env:PATH = "$mingwBin;$env:PATH"
  Write-Host "Windows GNU build tools: w64devkit=$mingwRoot linker=$mingwGcc"
}

function Ensure-MSVC-Build-Environment {
  if ($env:CTX_EXPECT_HOST_TRIPLE -ne "x86_64-pc-windows-msvc") {
    return
  }

  if (Get-Command "link.exe" -ErrorAction SilentlyContinue) {
    return
  }

  $script = Find-MSVC-Environment-Script
  if ([string]::IsNullOrWhiteSpace($script)) {
    Install-Visual-Studio-Build-Tools
    $script = Find-MSVC-Environment-Script
  }
  if ([string]::IsNullOrWhiteSpace($script)) {
    throw "MSVC linker link.exe is required for x86_64-pc-windows-msvc but was not found on PATH, and no Visual Studio Build Tools environment script was found"
  }

  Write-Host "link.exe not found on PATH; importing MSVC environment from $script"
  if ($script.EndsWith("VsDevCmd.bat", [StringComparison]::OrdinalIgnoreCase)) {
    Invoke-Batch-Environment -BatchFile $script -BatchArgs @("-arch=amd64", "-host_arch=amd64")
  } else {
    Invoke-Batch-Environment -BatchFile $script -BatchArgs @()
  }

  if (-not (Get-Command "link.exe" -ErrorAction SilentlyContinue)) {
    throw "MSVC environment loaded from $script but link.exe is still unavailable on PATH"
  }
}

function Ensure-Rust-Toolchain {
  $toolCache = Default-Windows-Tool-Cache-Root
  $env:CARGO_HOME = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-PathSafe $toolCache "cargo" }
  $env:RUSTUP_HOME = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-PathSafe $toolCache "rustup" }
  New-Item -ItemType Directory -Force -Path $env:CARGO_HOME, $env:RUSTUP_HOME | Out-Null
  $cargoBin = Join-PathSafe $env:CARGO_HOME "bin"
  $env:PATH = "$cargoBin;$env:PATH"
  $expectedHost = $env:CTX_EXPECT_HOST_TRIPLE

  if (Rust-Tools-Available) {
    $actualHost = Current-Rust-Host-Triple
    if ([string]::IsNullOrWhiteSpace($expectedHost) -or $actualHost -eq $expectedHost) {
      return
    }
    Write-Host "Rust host triple is $actualHost; installing expected host $expectedHost"
  }

  $rustup = Get-Command "rustup" -ErrorAction SilentlyContinue
  $toolchain = if ([string]::IsNullOrWhiteSpace($expectedHost)) { "stable" } else { "stable-$expectedHost" }
  if (-not $rustup) {
    $installerDir = Join-PathSafe $toolCache "rustup-init"
    New-Item -ItemType Directory -Force -Path $installerDir | Out-Null
    $installer = Join-PathSafe $installerDir "rustup-init.exe"
    if (-not (Test-Path $installer)) {
      $url = "https://win.rustup.rs/x86_64"
      Write-Host "cargo/rustup not found; downloading rustup-init from $url"
      Download-File -Uri $url -OutFile $installer
    }
    $installArgs = @("-y", "--profile", "minimal", "--default-toolchain", "stable", "--component", "rustfmt", "--component", "clippy")
    if (-not [string]::IsNullOrWhiteSpace($expectedHost)) {
      $installArgs += @("--default-host", $expectedHost)
    }
    & $installer @installArgs
    if ($LASTEXITCODE -ne 0) {
      throw "rustup-init failed with exit code $LASTEXITCODE"
    }
  } else {
    Write-Host "Rust toolchain found but expected host/components are missing; installing $toolchain"
    & rustup toolchain install $toolchain --profile minimal --component rustfmt --component clippy
    if ($LASTEXITCODE -ne 0) {
      throw "rustup toolchain install failed for $toolchain"
    }
    & rustup default $toolchain
    if ($LASTEXITCODE -ne 0) {
      throw "rustup default failed for $toolchain"
    }
  }

  if (-not (Rust-Tools-Available)) {
    throw "Rust toolchain is incomplete after bootstrap; cargo, rustc, rustfmt, and clippy are required"
  }
}

function Host-Triple {
  $lines = & rustc -vV
  if ($LASTEXITCODE -ne 0) {
    throw "rustc -vV failed"
  }
  foreach ($line in $lines) {
    if ($line -match '^host:\s+(.+)$') {
      return $Matches[1]
    }
  }
  throw "rustc -vV did not report a host triple"
}

function Git-Current-Branch {
  if (-not [string]::IsNullOrWhiteSpace($env:BUILDKITE_BRANCH)) {
    return $env:BUILDKITE_BRANCH
  }

  $branchOutput = & git branch --show-current
  if ($LASTEXITCODE -eq 0) {
    $branch = [string]($branchOutput | Select-Object -First 1)
    if (-not [string]::IsNullOrWhiteSpace($branch)) {
      return $branch.Trim()
    }
  }

  $abbrevOutput = & git rev-parse --abbrev-ref HEAD
  if ($LASTEXITCODE -eq 0) {
    $abbrev = [string]($abbrevOutput | Select-Object -First 1)
    if (-not [string]::IsNullOrWhiteSpace($abbrev)) {
      $abbrev = $abbrev.Trim()
      if ($abbrev -ne "HEAD") {
        return $abbrev
      }
    }
  }

  return "detached"
}

function Require-Host-Triple {
  param([string]$Expected)
  if ([string]::IsNullOrWhiteSpace($Expected)) {
    return
  }
  Ensure-Rust-Toolchain
  $actual = Host-Triple
  if ($actual -ne $Expected) {
    throw "host triple mismatch: expected $Expected, got $actual"
  }
  if ($actual -match '-msvc$') {
    Ensure-MSVC-Build-Environment
  } elseif ($actual -eq "x86_64-pc-windows-gnu") {
    Ensure-MinGW-GNU-Build-Environment
  }
}

function Cargo-Locked-Args {
  if (($env:CTX_CARGO_LOCKED -ne "0") -and (Test-Path (Join-PathSafe $script:RepoRoot "Cargo.lock"))) {
    return @("--locked")
  }
  return @()
}

function Init-Resource-Env {
  $cpu = if (Positive-Int $env:CTX_CPU_COUNT) { [int]$env:CTX_CPU_COUNT } else { [Environment]::ProcessorCount }
  if ($cpu -lt 1) { $cpu = 2 }
  $memoryGb = if (Positive-Int $env:CTX_TOTAL_MEMORY_GB) { [int]$env:CTX_TOTAL_MEMORY_GB } else { 4 }
  $memoryJobs = [Math]::Max([int][Math]::Floor($memoryGb / 3), 1)
  $defaultJobs = [Math]::Max([Math]::Min($cpu, $memoryJobs), 1)

  $env:CTX_CPU_COUNT = "$cpu"
  $env:CTX_TOTAL_MEMORY_GB = "$memoryGb"
  $env:CARGO_BUILD_JOBS = if ($env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS } elseif ($env:CTX_CARGO_JOBS) { $env:CTX_CARGO_JOBS } else { "$defaultJobs" }
  $env:RUST_TEST_THREADS = if ($env:RUST_TEST_THREADS) { $env:RUST_TEST_THREADS } elseif ($env:CTX_TEST_THREADS) { $env:CTX_TEST_THREADS } else { $env:CARGO_BUILD_JOBS }
  $env:CARGO_TERM_COLOR = if ($env:CARGO_TERM_COLOR) { $env:CARGO_TERM_COLOR } else { "always" }
  $env:TMPDIR = if ($env:TMPDIR) { $env:TMPDIR } else { Join-PathSafe $script:RepoRoot "target\tmp" }
  $env:TMP = $env:TMPDIR
  $env:TEMP = $env:TMPDIR
  New-Item -ItemType Directory -Force -Path $env:TMPDIR | Out-Null

  if ($env:CTX_USE_SCCACHE -ne "1" -and $env:RUSTC_WRAPPER -like "*sccache*") {
    Remove-Item Env:RUSTC_WRAPPER
  }
}

function Print-Resource-Env {
  Write-Host "resource limits: cpu=$env:CTX_CPU_COUNT memory_gb=$env:CTX_TOTAL_MEMORY_GB cargo_jobs=$env:CARGO_BUILD_JOBS test_threads=$env:RUST_TEST_THREADS tmpdir=$env:TMPDIR"
}

function Run-Cargo {
  param([string[]]$CargoArgs)
  & cargo @CargoArgs
  if ($LASTEXITCODE -ne 0) {
    throw "cargo $($CargoArgs -join ' ') failed with exit code $LASTEXITCODE"
  }
}

function Run-Ctx {
  param([string]$Binary, [string[]]$CtxArgs)
  & $Binary @CtxArgs
  if ($LASTEXITCODE -ne 0) {
    throw "$Binary $($CtxArgs -join ' ') failed with exit code $LASTEXITCODE"
  }
}

function Run-Ctx-Capture {
  param(
    [string]$Name,
    [string]$Binary,
    [string[]]$CtxArgs,
    [string]$OutDir
  )

  $stdout = Join-PathSafe $OutDir "$Name.stdout"
  $stderr = Join-PathSafe $OutDir "$Name.stderr"
  & $Binary @CtxArgs > $stdout 2> $stderr
  if ($LASTEXITCODE -ne 0) {
    throw "$Binary $($CtxArgs -join ' ') failed with exit code $LASTEXITCODE; stdout=$stdout stderr=$stderr"
  }
}

function Env-Value {
  param(
    [string]$Path,
    [string]$Key
  )

  foreach ($line in Get-Content -Path $Path) {
    if ($line -match '^\s*#') {
      continue
    }
    if ($line.StartsWith("$Key=")) {
      return $line.Substring($Key.Length + 1).TrimEnd("`r")
    }
  }
  throw "$Path does not set $Key"
}

function To-Repo-Relative {
  param([string]$Path)

  $full = [System.IO.Path]::GetFullPath($Path)
  $prefix = $script:RepoRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
  if ($full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
    return $full.Substring($prefix.Length).Replace("\", "/")
  }
  return $Path.Replace("\", "/")
}

function Run-Platform-Smoke {
  Require-Host-Triple $env:CTX_EXPECT_HOST_TRIPLE
  Ensure-Rust-Toolchain
  Ensure-MSVC-Build-Environment
  Ensure-MinGW-GNU-Build-Environment
  $locked = Cargo-Locked-Args
  Run-Cargo -CargoArgs (@("build", "-p", "ctx", "--bin", "ctx") + $locked)

  $bin = Join-PathSafe $script:RepoRoot "target\debug\ctx.exe"
  if (-not (Test-Path $bin)) {
    throw "expected smoke binary missing: $bin"
  }

  $dataRoot = Join-PathSafe $env:TMPDIR ("ctx-records-smoke-" + [Guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $dataRoot | Out-Null
  $env:CTX_DATA_ROOT = $dataRoot

  Run-Ctx $bin @("setup")
  Run-Ctx $bin @("sources", "--json")
  $fixture = Join-PathSafe $script:RepoRoot "tests\fixtures\provider-history\codex-sessions"
  Run-Ctx $bin @("import", "--provider", "codex", "--path", $fixture, "--json")
  $listOutput = & $bin list --json
  if ($LASTEXITCODE -ne 0) {
    throw "platform smoke failed to list imported history"
  }
  try {
    $recordJson = ($listOutput -join "`n") | ConvertFrom-Json
    $recordId = $null
    if (($recordJson.PSObject.Properties.Name -contains "items") -and $recordJson.items -and $recordJson.items.Count -gt 0) {
      $recordId = $recordJson.items[0].id
    }
    if ([string]::IsNullOrWhiteSpace([string]$recordId)) {
      throw "missing id"
    }
    Write-Host "platform smoke item: $recordId"
  } catch {
    throw "platform smoke failed to parse item id from: $listOutput"
  }
  Run-Ctx $bin @("search", "onboarding", "--json")
  Run-Ctx $bin @("show", $recordId, "--json")
  Run-Ctx $bin @("status", "--json")
  Run-Ctx $bin @("doctor", "--json")
  Run-Ctx $bin @("validate", "--json")
}

function Sha256-File {
  param([string]$Path)
  return (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
}

function Cargo-Version {
  $cargoToml = Join-PathSafe $script:RepoRoot "crates\ctx-cli\Cargo.toml"
  foreach ($line in Get-Content $cargoToml) {
    if ($line -match '^version\s*=\s*"([^"]+)"') {
      return $Matches[1]
    }
  }
  throw "could not read ctx-cli version from $cargoToml"
}

function Run-Release-Dry-Run {
  Require-Host-Triple $env:CTX_EXPECT_HOST_TRIPLE
  Ensure-Rust-Toolchain
  Ensure-MSVC-Build-Environment
  Ensure-MinGW-GNU-Build-Environment
  $locked = Cargo-Locked-Args
  Run-Cargo -CargoArgs (@("build", "--workspace", "--release", "--bins") + $locked)

  $version = Cargo-Version
  $hostTriple = Host-Triple
  $targetTriple = if ($env:CTX_RELEASE_TARGET_TRIPLE) { $env:CTX_RELEASE_TARGET_TRIPLE } else { $hostTriple }
  $platform = if ($env:CTX_RELEASE_PLATFORM) { $env:CTX_RELEASE_PLATFORM } else { "host-$hostTriple" }
  $commit = (& git rev-parse HEAD).Trim()
  $branch = Git-Current-Branch
  $sourceBin = Join-PathSafe $script:RepoRoot "target\release\ctx.exe"
  if (-not (Test-Path $sourceBin)) {
    throw "expected host binary missing: $sourceBin"
  }

  $artifact = "ctx-$version-$targetTriple.exe"
  $artifactPath = Join-PathSafe $script:ArtifactDir $artifact
  $artifactManifestPath = Join-Path -Path $script:ArtifactDir -ChildPath $artifact
  if ([System.IO.Path]::IsPathRooted($artifactManifestPath)) {
    $repoRootPrefix = $script:RepoRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    $artifactFullPath = [System.IO.Path]::GetFullPath($artifactManifestPath)
    if (-not $artifactFullPath.StartsWith($repoRootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
      throw "release artifact manifest path must be relative to the repo root: $artifactManifestPath"
    }
    $artifactManifestPath = $artifactFullPath.Substring($repoRootPrefix.Length)
  }
  $artifactManifestPath = $artifactManifestPath -replace "\\", "/"
  if ([System.IO.Path]::IsPathRooted($artifactManifestPath) -or $artifactManifestPath -match '(^|/)\.\.(/|$)') {
    throw "release artifact manifest path must be a safe relative path: $artifactManifestPath"
  }
  Copy-Item -Force $sourceBin $artifactPath
  $checksum = Sha256-File $artifactPath
  $bytes = (Get-Item $artifactPath).Length
  $generatedAt = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  $platformKey = $platform.Replace("-", "_")

  "$checksum  $artifact" | Set-Content -Path (Join-PathSafe $script:ArtifactDir "checksums.sha256") -Encoding ascii
  @(
    "# ctx release installer metadata, schema v1."
    "# Generated by Windows release dry-run; publishing remains disabled."
    "CTX_RELEASE_SCHEMA_VERSION=1"
    "CTX_RELEASE_VERSION=$version"
    "CTX_RELEASE_CHANNEL=dry-run"
    "CTX_RELEASE_BASE_URL=https://github.com/ctxrs/ctx/releases/download/v$version"
    "CTX_RELEASE_ARTIFACT_$platformKey=$artifact"
    "CTX_RELEASE_SHA256_$platformKey=$checksum"
  ) | Set-Content -Path (Join-PathSafe $script:ArtifactDir "ctx-release-metadata.env") -Encoding ascii
  @{
    schema_version = 1
    dry_run = $true
    upload = $false
    package = "ctx"
    version = $version
    platform = $platform
    target_triple = $targetTriple
    host_triple = $hostTriple
    expected_host_triple = $env:CTX_EXPECT_HOST_TRIPLE
    git_commit = $commit
    git_branch = $branch
    generated_at_unix_s = $generatedAt
    artifacts = @(@{
      path = $artifactManifestPath
      sha256 = $checksum
      bytes = $bytes
    })
  } | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-PathSafe $script:ArtifactDir "manifest.json") -Encoding utf8

  Write-Host "release dry-run manifest: $(Join-PathSafe $script:ArtifactDir "manifest.json")"
  Write-Host "release dry-run checksums: $(Join-PathSafe $script:ArtifactDir "checksums.sha256")"
  Write-Host "release dry-run install metadata: $(Join-PathSafe $script:ArtifactDir "ctx-release-metadata.env")"
}

function Run-Release-Artifact-Smoke {
  $platform = if ($env:CTX_RELEASE_PLATFORM) { $env:CTX_RELEASE_PLATFORM } else { "windows-x64" }
  if ($platform -ne "windows-x64") {
    throw "Windows release artifact smoke only supports windows-x64, got $platform"
  }
  $targetTriple = if ($env:CTX_RELEASE_TARGET_TRIPLE) { $env:CTX_RELEASE_TARGET_TRIPLE } else { "x86_64-pc-windows-gnu" }
  $platformKey = "windows_x64"
  Require-Host-Triple $env:CTX_EXPECT_HOST_TRIPLE

  $releaseDir = if ($env:CTX_RELEASE_DRY_RUN_DIR) { $env:CTX_RELEASE_DRY_RUN_DIR } else { Join-PathSafe $script:RepoRoot "artifacts\buildkite\release-dry-run\windows-x64" }
  $metadata = Join-PathSafe $releaseDir "ctx-release-metadata.env"
  $manifest = Join-PathSafe $releaseDir "manifest.json"
  if (-not (Test-Path $metadata)) {
    throw "release artifact smoke metadata is missing: $metadata"
  }
  if (-not (Test-Path $manifest)) {
    throw "release artifact smoke manifest is missing: $manifest"
  }

  $version = Env-Value -Path $metadata -Key "CTX_RELEASE_VERSION"
  $artifact = Env-Value -Path $metadata -Key "CTX_RELEASE_ARTIFACT_$platformKey"
  $checksum = Env-Value -Path $metadata -Key "CTX_RELEASE_SHA256_$platformKey"
  $artifactPath = Join-PathSafe $releaseDir $artifact
  if (-not (Test-Path $artifactPath)) {
    throw "release artifact smoke artifact is missing: $artifactPath"
  }
  $actualChecksum = Sha256-File $artifactPath
  if ($actualChecksum -ne $checksum) {
    throw "release artifact smoke checksum mismatch for ${artifactPath}: metadata $checksum, file $actualChecksum"
  }
  $artifactBytes = (Get-Item $artifactPath).Length

  $tempRoot = Join-PathSafe $env:TMPDIR ("ctx-release-artifact-smoke-" + [Guid]::NewGuid().ToString("N"))
  $binDir = Join-PathSafe $tempRoot "bin"
  $homeDir = Join-PathSafe $tempRoot "home"
  $dataRoot = Join-PathSafe $tempRoot "data-root"
  $commandDir = Join-PathSafe $script:ArtifactDir "commands"
  New-Item -ItemType Directory -Force -Path $binDir, $homeDir, $dataRoot, $commandDir | Out-Null

  $installedBin = Join-PathSafe $binDir "ctx.exe"
  Copy-Item -Force $artifactPath $installedBin
  $oldDataRoot = $env:CTX_DATA_ROOT
  $oldHome = $env:HOME
  $env:CTX_DATA_ROOT = $dataRoot
  $env:HOME = $homeDir
  foreach ($secretName in @("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY", "AZURE_OPENAI_API_KEY")) {
    if (Test-Path "Env:$secretName") {
      Remove-Item "Env:$secretName"
    }
  }

  try {
    Run-Ctx-Capture -Name "version" -Binary $installedBin -CtxArgs @("--version") -OutDir $commandDir
    $versionOutput = ((Get-Content -Path (Join-PathSafe $commandDir "version.stdout") -Raw).Trim() -split "`n" | Select-Object -First 1).Trim()
    if ($versionOutput -notlike "*$version*") {
      throw "ctx --version output does not contain release version $version`: $versionOutput"
    }
    $fixture = "tests/fixtures/provider-history/codex-sessions"
    $fixturePath = Join-PathSafe $script:RepoRoot "tests\fixtures\provider-history\codex-sessions"
    Run-Ctx-Capture -Name "setup" -Binary $installedBin -CtxArgs @("setup") -OutDir $commandDir
    Run-Ctx-Capture -Name "import" -Binary $installedBin -CtxArgs @("import", "--provider", "codex", "--path", $fixturePath, "--json") -OutDir $commandDir
    Run-Ctx-Capture -Name "search" -Binary $installedBin -CtxArgs @("search", "onboarding", "--json") -OutDir $commandDir
    Run-Ctx-Capture -Name "doctor" -Binary $installedBin -CtxArgs @("doctor", "--json") -OutDir $commandDir
    Run-Ctx-Capture -Name "validate" -Binary $installedBin -CtxArgs @("validate", "--json") -OutDir $commandDir
  } finally {
    if ($null -eq $oldDataRoot) {
      Remove-Item Env:CTX_DATA_ROOT -ErrorAction SilentlyContinue
    } else {
      $env:CTX_DATA_ROOT = $oldDataRoot
    }
    if ($null -eq $oldHome) {
      Remove-Item Env:HOME -ErrorAction SilentlyContinue
    } else {
      $env:HOME = $oldHome
    }
  }

  $commit = (& git rev-parse HEAD).Trim()
  $branch = Git-Current-Branch
  $generatedAt = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  $hostTriple = Host-Triple
  $smokeJson = Join-PathSafe $script:ArtifactDir "artifact-smoke.json"
  $smokeMd = Join-PathSafe $script:ArtifactDir "artifact-smoke.md"

  @{
    schema_version = 1
    kind = "ctx_release_artifact_smoke"
    mode = "release-artifact-smoke"
    status = "passed"
    publishing = $false
    platform = $platform
    platform_key = $platformKey
    target_triple = $targetTriple
    host_triple = $hostTriple
    release_dry_run_dir = To-Repo-Relative $releaseDir
    release_manifest = To-Repo-Relative $manifest
    release_metadata = To-Repo-Relative $metadata
    release_artifact = To-Repo-Relative $artifactPath
    release_artifact_name = $artifact
    release_artifact_sha256 = $checksum
    release_artifact_bytes = $artifactBytes
    install_method = "direct-binary-copy"
    installed_artifact_runtime = $true
    fixture = "tests/fixtures/provider-history/codex-sessions"
    command_output_dir = To-Repo-Relative $commandDir
    version_output = $versionOutput
    version_status = "passed"
    setup_status = "passed"
    import_status = "passed"
    search_status = "passed"
    doctor_status = "passed"
    validate_status = "passed"
    git_commit = $commit
    git_branch = $branch
    buildkite = @{
      build_url = if ($env:BUILDKITE_BUILD_URL) { $env:BUILDKITE_BUILD_URL } else { "local" }
      build_id = if ($env:BUILDKITE_BUILD_ID) { $env:BUILDKITE_BUILD_ID } else { "" }
      job_id = if ($env:BUILDKITE_JOB_ID) { $env:BUILDKITE_JOB_ID } else { "" }
    }
    generated_at_unix_s = $generatedAt
  } | ConvertTo-Json -Depth 8 | Set-Content -Path $smokeJson -Encoding utf8

  @(
    "# ctx Release Artifact Smoke"
    ""
    "- Publishing: false"
    "- Platform: ``$platform``"
    "- Target triple: ``$targetTriple``"
    "- Release artifact: ``$(To-Repo-Relative $artifactPath)``"
    "- SHA-256: ``$checksum``"
    "- Install method: ``direct-binary-copy``"
    "- Fixture: ``tests/fixtures/provider-history/codex-sessions``"
    "- Commands: ``ctx --version``, ``ctx setup``, ``ctx import``, ``ctx search``, ``ctx doctor``, ``ctx validate``"
    "- Status: passed"
  ) | Set-Content -Path $smokeMd -Encoding utf8

  Write-Host "release artifact smoke: $smokeJson"
  Write-Host "release artifact smoke notes: $smokeMd"
}

$script:ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$script:RepoRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($script:ScriptDir, ".."))
Set-Location $script:RepoRoot

Init-Resource-Env
$script:ArtifactDir = if ($env:CTX_ARTIFACT_DIR) { $env:CTX_ARTIFACT_DIR } else { Join-PathSafe $script:RepoRoot "target\ctx-artifacts\windows-ci" }
New-Item -ItemType Directory -Force -Path $script:ArtifactDir | Out-Null
$script:TimingFile = Join-PathSafe $script:ArtifactDir "timings.json"
$script:TimingEvents = "$($script:TimingFile).events"
Set-Content -Path $script:TimingEvents -Value "" -Encoding utf8

try {
  Print-Resource-Env
  if ($Mode -eq "platform-smoke") {
    Run-Step "platform-smoke" { Run-Platform-Smoke }
  } elseif ($Mode -eq "release-dry-run") {
    Run-Step "release-dry-run" { Run-Release-Dry-Run }
  } elseif ($Mode -eq "release-artifact-smoke") {
    Run-Step "release-artifact-smoke" { Run-Release-Artifact-Smoke }
  } else {
    throw "unknown Windows CI mode: $Mode"
  }
} finally {
  Finish-Timings
}
