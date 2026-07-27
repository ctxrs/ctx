mod archive;
mod durability;
mod marker;
mod runtime;
mod transaction;

pub(crate) use marker::is_valid_install_attempt_id;
pub(super) use marker::{
    current_install_path, install_marker_for_plan, read_verified_install_marker_for_current_exe,
    InstallMarker,
};
pub(super) use transaction::{apply_artifact, recover_interrupted_install, ApplyResult};

// The Windows runtime contract test reads this constant from this exact source path.
#[cfg(windows)]
const EXTRACT_SCRIPT: &str = r#"
param(
  [string]$ArchivePath,
  [string]$Destination,
  [string]$ExpectedVersion,
  [long]$MaxExpandedBytes
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
  [string[]]@(
    'LICENSE',
    'MICROSOFT_VC_RUNTIME_LICENSE.rtf',
    'ThirdPartyNotices.txt',
    'VERSION_NUMBER',
    'GIT_COMMIT_ID',
    'lib/onnxruntime.dll',
    'lib/msvcp140.dll',
    'lib/msvcp140_1.dll',
    'lib/vcruntime140.dll',
    'lib/vcruntime140_1.dll'
  ),
  [System.StringComparer]::Ordinal
)
$expectedEntries = [System.Collections.Generic.HashSet[string]]::new($expectedFiles, [System.StringComparer]::Ordinal)
[void]$expectedEntries.Add('lib')
$seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$entries = @{}
[long]$totalLength = 0
$archive = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
try {
  foreach ($entry in $archive.Entries) {
    $rawName = $entry.FullName
    if (
      [string]::IsNullOrEmpty($rawName) -or
      $rawName.Contains('\') -or
      $rawName.StartsWith('/', [System.StringComparison]::Ordinal) -or
      $rawName -match '^[A-Za-z]:'
    ) {
      throw "unsafe runtime archive entry path: '$rawName'"
    }
    $isDirectory = $rawName.EndsWith('/', [System.StringComparison]::Ordinal)
    $name = if ($isDirectory) { $rawName.Substring(0, $rawName.Length - 1) } else { $rawName }
    $expectedRawName = if ($name -ceq 'lib') { 'lib/' } else { $name }
    if (
      $rawName -cne $expectedRawName -or
      -not $expectedEntries.Contains($name) -or
      -not $seen.Add($name)
    ) {
      throw "unexpected, duplicate, or non-canonical runtime archive entry: '$rawName'"
    }
    $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF
    $fileType = $unixMode -band 0xF000
    if (($unixMode -band 0x0E00) -ne 0) {
      throw "unsafe permission bits on runtime archive entry: '$rawName'"
    }
    if ($name -ceq 'lib') {
      if (-not $isDirectory -or $fileType -ne 0x4000) {
        throw 'runtime lib entry is not a directory'
      }
    } elseif ($isDirectory -or $fileType -ne 0x8000) {
      throw "runtime archive entry is not a regular file: '$rawName'"
    }
    $totalLength += $entry.Length
    if ($totalLength -gt $MaxExpandedBytes) {
      throw 'runtime archive expands beyond the 1 GiB safety limit'
    }
    $entries[$name] = $entry
  }
  if ($seen.Count -ne $expectedEntries.Count) {
    $missing = @($expectedEntries | Where-Object { -not $seen.Contains($_) })
    throw "runtime archive entries do not exactly match the expected layout; missing: $($missing -join ', ')"
  }
  $versionStream = $entries['VERSION_NUMBER'].Open()
  try {
    $reader = [System.IO.StreamReader]::new($versionStream, [System.Text.UTF8Encoding]::new($false, $true))
    try {
      $versionText = $reader.ReadToEnd()
    } finally {
      $reader.Dispose()
    }
  } finally {
    $versionStream.Dispose()
  }
  if ($versionText -cne ($ExpectedVersion + [char]10)) {
    throw "runtime VERSION_NUMBER is not exactly $ExpectedVersion"
  }
  New-Item -ItemType Directory -Path (Join-Path $Destination 'lib') -Force | Out-Null
  foreach ($name in $expectedFiles) {
    $target = Join-Path $Destination ($name.Replace('/', '\'))
    $sourceStream = $entries[$name].Open()
    try {
      $targetStream = [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
      try {
        $sourceStream.CopyTo($targetStream)
        $targetStream.Flush($true)
      } finally {
        $targetStream.Dispose()
      }
    } finally {
      $sourceStream.Dispose()
    }
  }
} finally {
  $archive.Dispose()
}
"#;

#[cfg(windows)]
fn windows_runtime_extract_script() -> &'static str {
    EXTRACT_SCRIPT
}
