use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Read, Write as _},
    path::Path,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_platform::platform_security::{restrict_private_directory, restrict_private_file};
#[cfg(unix)]
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

use super::super::{
    metadata::{SemanticAssetMetadata, SemanticFileMetadata},
    ReleaseProcessPort,
};
#[cfg(unix)]
use super::durability::sync_directory;

const MAX_RUNTIME_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;

#[cfg(windows)]
fn windows_powershell_path() -> Result<std::path::PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt as _};

    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 {
        return Err(anyhow!(
            "resolve Windows system directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length >= buffer.len() {
        return Err(anyhow!(
            "Windows system directory exceeds the supported path length"
        ));
    }
    let system_directory = std::path::PathBuf::from(OsString::from_wide(&buffer[..length]));
    if !system_directory.is_absolute() {
        return Err(anyhow!(
            "Windows system directory is not absolute: {}",
            system_directory.display()
        ));
    }
    Ok(system_directory
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe"))
}

/// Convert a path into the form Windows PowerShell can accept as a parameter.
///
/// Paths inside ctx are frequently canonicalised, which on Windows yields the
/// verbatim `\\?\` form. The provider cmdlets the extraction helper relies on
/// (`Join-Path`, `New-Item`) cannot resolve a drive for a verbatim path and fail
/// with a null `drive` binding, so the ordinary Win32 form is passed instead.
#[cfg(windows)]
fn powershell_path_argument(path: &Path) -> std::ffi::OsString {
    use std::ffi::OsString;

    let Some(text) = path.to_str() else {
        return path.as_os_str().to_owned();
    };
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.as_os_str().to_owned();
    };
    if let Some(share) = rest.strip_prefix(r"UNC\") {
        return OsString::from(format!(r"\\{share}"));
    }
    let mut characters = rest.chars();
    let drive = characters.next();
    let colon = characters.next();
    if drive.is_some_and(|drive| drive.is_ascii_alphabetic()) && colon == Some(':') {
        return OsString::from(rest.to_owned());
    }
    path.as_os_str().to_owned()
}

#[cfg(unix)]
pub(super) fn extract_runtime_archive(
    _process: &dyn ReleaseProcessPort,
    archive_path: &Path,
    destination: &Path,
    artifact_name: &str,
    platform: &str,
    version: &str,
) -> Result<()> {
    if !artifact_name.ends_with(".tar.gz") {
        return Err(anyhow!(
            "unsupported ONNX Runtime archive format for {platform}: {artifact_name}"
        ));
    }
    use std::os::unix::fs::PermissionsExt as _;

    let library = if platform.starts_with("macos-") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let expected_files = BTreeSet::from([
        "LICENSE".to_owned(),
        "ThirdPartyNotices.txt".to_owned(),
        "VERSION_NUMBER".to_owned(),
        "GIT_COMMIT_ID".to_owned(),
        format!("lib/{library}"),
    ]);
    let mut expected_entries = expected_files.clone();
    expected_entries.insert("lib".to_owned());
    let archive_file = fs::File::open(archive_path)
        .with_context(|| format!("open runtime archive {}", archive_path.display()))?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let mut seen = BTreeSet::new();
    let mut total_size = 0_u64;
    let lib_dir = destination.join("lib");

    for entry in archive.entries().context("read ONNX Runtime archive")? {
        let mut entry = entry.context("read ONNX Runtime archive entry")?;
        let raw = std::str::from_utf8(entry.path_bytes().as_ref())
            .context("runtime archive path is not UTF-8")?
            .to_owned();
        let is_directory_name = raw.ends_with('/');
        let name = raw.strip_suffix('/').unwrap_or(&raw);
        if name.is_empty()
            || raw.contains('\\')
            || raw.starts_with('/')
            || name == "."
            || name == ".."
            || name.starts_with("../")
            || name.contains("/./")
            || name.contains("//")
            || (is_directory_name && name != "lib")
        {
            return Err(anyhow!(
                "unsafe or non-canonical runtime archive path: {raw:?}"
            ));
        }
        if !expected_entries.contains(name) {
            return Err(anyhow!("unexpected runtime archive entry: {name}"));
        }
        if !seen.insert(name.to_owned()) {
            return Err(anyhow!("duplicate runtime archive entry: {name}"));
        }
        let mode = entry.header().mode().context("read runtime archive mode")?;
        if mode & 0o7000 != 0 {
            return Err(anyhow!(
                "unsafe permission bits on runtime archive entry: {name}"
            ));
        }
        let entry_type = entry.header().entry_type();
        if name == "lib" {
            if !is_directory_name || !entry_type.is_dir() {
                return Err(anyhow!("runtime lib entry is not a directory"));
            }
            fs::create_dir_all(&lib_dir)
                .with_context(|| format!("create runtime directory {}", lib_dir.display()))?;
            fs::set_permissions(&lib_dir, fs::Permissions::from_mode(0o755))?;
            continue;
        }
        if is_directory_name || !entry_type.is_file() {
            return Err(anyhow!(
                "runtime archive entry is not a regular file: {name}"
            ));
        }
        total_size = total_size
            .checked_add(entry.size())
            .ok_or_else(|| anyhow!("runtime archive expanded size overflow"))?;
        if total_size > runtime_expanded_size_limit() {
            return Err(anyhow!(
                "runtime archive expands beyond the 1 GiB safety limit"
            ));
        }
        let target = destination.join(name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create runtime directory {}", parent.display()))?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .with_context(|| format!("create runtime file {}", target.display()))?;
        let copied = std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("extract runtime file {name}"))?;
        if copied != entry.size() {
            return Err(anyhow!(
                "runtime archive entry size mismatch for {name}: expected {}, copied {copied}",
                entry.size()
            ));
        }
        fs::set_permissions(
            &target,
            fs::Permissions::from_mode(if name.starts_with("lib/") {
                0o755
            } else {
                0o644
            }),
        )?;
        use std::io::Write as _;
        output.flush()?;
        output.sync_all()?;
    }
    if seen != expected_entries {
        let missing = expected_entries
            .difference(&seen)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "runtime archive entries do not exactly match the expected layout; missing: {missing}"
        ));
    }
    let actual_version = fs::read(destination.join("VERSION_NUMBER"))?;
    if actual_version != format!("{version}\n").as_bytes() {
        return Err(anyhow!("runtime VERSION_NUMBER is not exactly {version}"));
    }
    sync_directory(&lib_dir)?;
    sync_directory(destination)?;
    Ok(())
}

#[cfg(windows)]
pub(super) fn extract_runtime_archive(
    process: &dyn ReleaseProcessPort,
    archive_path: &Path,
    destination: &Path,
    artifact_name: &str,
    platform: &str,
    version: &str,
) -> Result<()> {
    if !artifact_name.to_ascii_lowercase().ends_with(".zip") {
        return Err(anyhow!(
            "unsupported ONNX Runtime archive format for {platform}: {artifact_name}"
        ));
    }
    let powershell = windows_powershell_path()?;
    let script_path = archive_path.with_extension("extract.ps1");
    fs::write(&script_path, super::windows_runtime_extract_script())
        .with_context(|| format!("write runtime extraction helper {}", script_path.display()))?;
    let mut command = std::process::Command::new(powershell);
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg("-ArchivePath")
        .arg(powershell_path_argument(archive_path))
        .arg("-Destination")
        .arg(powershell_path_argument(destination))
        .arg("-ExpectedVersion")
        .arg(version)
        .arg("-MaxExpandedBytes")
        .arg(runtime_expanded_size_limit().to_string());
    process.sanitize_release_authority_env(&mut command);
    let output = command
        .output()
        .context("run Windows ONNX Runtime extraction helper");
    let _ = fs::remove_file(&script_path);
    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(anyhow!("extract ONNX Runtime sidecar: {stderr}"));
    }
    Ok(())
}

fn runtime_expanded_size_limit() -> u64 {
    if crate::upgrade::test_harness_enabled() {
        if let Ok(value) = env::var("CTX_UPGRADE_RUNTIME_MAX_EXPANDED_BYTES_FOR_TESTS") {
            if let Ok(value) = value.parse::<u64>() {
                if value > 0 {
                    return value;
                }
            }
        }
    }
    MAX_RUNTIME_EXPANDED_BYTES
}

pub(super) fn extract_semantic_archive(
    process: &dyn ReleaseProcessPort,
    archive_path: &Path,
    destination: &Path,
    asset: &SemanticAssetMetadata,
) -> Result<()> {
    match asset.archive_format.as_str() {
        "tar.zst" | "tar.xz" => extract_semantic_tar(archive_path, destination, asset),
        "zip" => extract_semantic_zip(process, archive_path, destination, asset),
        format => Err(anyhow!(
            "unsupported signed Semantic archive format {format:?}"
        )),
    }
}

fn extract_semantic_tar(
    archive_path: &Path,
    destination: &Path,
    asset: &SemanticAssetMetadata,
) -> Result<()> {
    let archive_file = fs::File::open(archive_path)
        .with_context(|| format!("open Semantic archive {}", archive_path.display()))?;
    let decoder: Box<dyn Read> = match asset.archive_format.as_str() {
        "tar.zst" => Box::new(
            zstd::stream::read::Decoder::new(archive_file).context("open Semantic zstd archive")?,
        ),
        "tar.xz" => Box::new(xz2::read::XzDecoder::new(archive_file)),
        _ => return Err(anyhow!("Semantic tar archive has the wrong format")),
    };
    let mut archive = tar::Archive::new(decoder);
    let expected = expected_archive_files(asset);
    let expected_directories = expected_archive_directories(expected.keys());
    let mut seen_entries = BTreeSet::new();
    let mut seen_files = BTreeSet::new();
    let mut total_size = 0_u64;

    for entry in archive.entries().context("read signed Semantic archive")? {
        let mut entry = entry.context("read signed Semantic archive entry")?;
        let raw = std::str::from_utf8(entry.path_bytes().as_ref())
            .context("Semantic archive path is not UTF-8")?
            .to_owned();
        let is_directory_name = raw.ends_with('/');
        let name = raw.strip_suffix('/').unwrap_or(&raw);
        validate_archive_path(name)?;
        if !seen_entries.insert(name.to_ascii_lowercase()) {
            return Err(anyhow!(
                "duplicate or case-colliding Semantic archive entry: {raw}"
            ));
        }
        let mode = entry
            .header()
            .mode()
            .context("read Semantic archive entry mode")?;
        if mode & 0o7000 != 0 {
            return Err(anyhow!(
                "unsafe permission bits on Semantic archive entry: {raw}"
            ));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            if !expected_directories.contains(name) {
                return Err(anyhow!("unexpected Semantic archive directory: {raw}"));
            }
            continue;
        }
        if is_directory_name || !entry_type.is_file() {
            return Err(anyhow!(
                "Semantic archive entry is not a regular file: {raw}"
            ));
        }
        let expected_file = expected
            .get(name)
            .ok_or_else(|| anyhow!("unexpected Semantic archive file: {raw}"))?;
        if entry.size() != expected_file.size {
            return Err(anyhow!(
                "Semantic archive file {raw} size {} does not match signed size {}",
                entry.size(),
                expected_file.size
            ));
        }
        total_size = total_size
            .checked_add(entry.size())
            .ok_or_else(|| anyhow!("Semantic archive expanded size overflow"))?;
        if total_size > asset.max_expanded_bytes {
            return Err(anyhow!(
                "Semantic archive expands beyond its signed safety limit"
            ));
        }
        write_verified_entry(&mut entry, destination, expected_file)?;
        seen_files.insert(name.to_owned());
    }
    let expected_files = expected.keys().cloned().collect::<BTreeSet<_>>();
    if seen_files != expected_files {
        return Err(anyhow!(
            "Semantic archive file set does not exactly match signed metadata"
        ));
    }
    #[cfg(unix)]
    sync_tree(destination)?;
    Ok(())
}

fn expected_archive_files(
    asset: &SemanticAssetMetadata,
) -> BTreeMap<String, &SemanticFileMetadata> {
    asset
        .files
        .iter()
        .map(|file| {
            let path = if asset.archive_path_prefix.is_empty() {
                file.path.clone()
            } else {
                format!("{}/{}", asset.archive_path_prefix, file.path)
            };
            (path, file)
        })
        .collect()
}

fn expected_archive_directories<'a>(files: impl Iterator<Item = &'a String>) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for file in files {
        let mut path = file.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            directories.insert(parent.to_owned());
            path = parent;
        }
    }
    directories
}

fn validate_archive_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains("//")
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(anyhow!(
            "unsafe or non-canonical Semantic archive path: {path:?}"
        ));
    }
    Ok(())
}

fn write_verified_entry(
    input: &mut dyn Read,
    destination: &Path,
    expected: &SemanticFileMetadata,
) -> Result<()> {
    let target = destination.join(&expected.path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create Semantic directory {}", parent.display()))?;
        restrict_private_directory(parent)
            .with_context(|| format!("protect Semantic directory {}", parent.display()))?;
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .with_context(|| format!("create Semantic file {}", target.display()))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .with_context(|| format!("read Semantic file {}", expected.path))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("Semantic file size overflow"))?;
        if total > expected.size {
            return Err(anyhow!(
                "Semantic file {} exceeds signed size",
                expected.path
            ));
        }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if total != expected.size {
        return Err(anyhow!(
            "Semantic file {} size {total} does not match signed size {}",
            expected.path,
            expected.size
        ));
    }
    if format!("{:x}", hasher.finalize()) != expected.sha256 {
        return Err(anyhow!("Semantic file {} checksum mismatch", expected.path));
    }
    restrict_private_file(&target)
        .with_context(|| format!("protect Semantic file {}", target.display()))?;
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn sync_tree(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
            }
        }
        index += 1;
    }
    for directory in directories.into_iter().rev() {
        sync_directory(&directory)?;
    }
    Ok(())
}

#[cfg(windows)]
fn extract_semantic_zip(
    process: &dyn ReleaseProcessPort,
    archive_path: &Path,
    destination: &Path,
    asset: &SemanticAssetMetadata,
) -> Result<()> {
    let powershell = windows_powershell_path()?;
    let contract_path = archive_path.with_extension("extract.json");
    let script_path = archive_path.with_extension("extract.ps1");
    let contract = serde_json::json!({
        "prefix": asset.archive_path_prefix,
        "max_expanded_bytes": asset.max_expanded_bytes,
        "files": asset.files,
    });
    fs::write(&contract_path, serde_json::to_vec(&contract)?)
        .with_context(|| format!("write extraction contract {}", contract_path.display()))?;
    fs::write(&script_path, WINDOWS_SEMANTIC_ZIP_EXTRACT_SCRIPT)
        .with_context(|| format!("write extraction helper {}", script_path.display()))?;
    let mut command = std::process::Command::new(powershell);
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg("-ArchivePath")
        .arg(archive_path)
        .arg("-Destination")
        .arg(destination)
        .arg("-ContractPath")
        .arg(&contract_path);
    process.sanitize_release_authority_env(&mut command);
    let output = command
        .output()
        .context("run signed Semantic zip extraction helper");
    let _ = fs::remove_file(&script_path);
    let _ = fs::remove_file(&contract_path);
    let output = output?;
    if !output.status.success() {
        return Err(anyhow!(
            "extract signed Semantic zip: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    protect_extracted_windows_tree(destination)
}

#[cfg(not(windows))]
fn extract_semantic_zip(
    _process: &dyn ReleaseProcessPort,
    _archive_path: &Path,
    _destination: &Path,
    _asset: &SemanticAssetMetadata,
) -> Result<()> {
    Err(anyhow!(
        "zip Semantic archives are supported only on Windows"
    ))
}

#[cfg(windows)]
fn protect_extracted_windows_tree(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        restrict_private_directory(&directory)
            .with_context(|| format!("protect Semantic directory {}", directory.display()))?;
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read Semantic directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(anyhow!(
                    "Semantic installation contains a link: {}",
                    path.display()
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                restrict_private_file(&path)
                    .with_context(|| format!("protect Semantic file {}", path.display()))?;
            } else {
                return Err(anyhow!(
                    "Semantic installation contains a special file: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
const WINDOWS_SEMANTIC_ZIP_EXTRACT_SCRIPT: &str = r#"
param(
  [string]$ArchivePath,
  [string]$Destination,
  [string]$ContractPath
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$contract = Get-Content -LiteralPath $ContractPath -Raw | ConvertFrom-Json
$expectedArchive = @{}
$directories = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
foreach ($file in $contract.files) {
  $archiveName = if ([string]::IsNullOrEmpty($contract.prefix)) {
    [string]$file.path
  } else {
    "$($contract.prefix)/$($file.path)"
  }
  $expectedArchive[$archiveName] = $file
  $cursor = $archiveName
  while ($cursor.Contains('/')) {
    $cursor = $cursor.Substring(0, $cursor.LastIndexOf('/'))
    [void]$directories.Add($cursor)
  }
}
$seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$seenFiles = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
[long]$total = 0
$archiveStream = [System.IO.FileStream]::new(
  $ArchivePath,
  [System.IO.FileMode]::Open,
  [System.IO.FileAccess]::Read,
  [System.IO.FileShare]::ReadWrite
)
$archive = $null
try {
  $archive = [System.IO.Compression.ZipArchive]::new(
    $archiveStream,
    [System.IO.Compression.ZipArchiveMode]::Read,
    $true
  )
  foreach ($entry in $archive.Entries) {
    $raw = $entry.FullName
    if (
      [string]::IsNullOrEmpty($raw) -or
      $raw.Contains('\') -or
      $raw.StartsWith('/', [System.StringComparison]::Ordinal) -or
      $raw -match '^[A-Za-z]:' -or
      $raw.Contains('//')
    ) { throw "unsafe Semantic zip entry path: '$raw'" }
    $isDirectory = $raw.EndsWith('/', [System.StringComparison]::Ordinal)
    $name = if ($isDirectory) { $raw.Substring(0, $raw.Length - 1) } else { $raw }
    if (@($name.Split('/') | Where-Object { $_ -eq '' -or $_ -eq '.' -or $_ -eq '..' }).Count -ne 0) {
      throw "unsafe Semantic zip entry path: '$raw'"
    }
    if (-not $seen.Add($name)) { throw "duplicate or case-colliding Semantic zip entry: '$raw'" }
    $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF
    $fileType = $unixMode -band 0xF000
    if (($unixMode -band 0x0E00) -ne 0) { throw "unsafe permission bits on Semantic zip entry: '$raw'" }
    if ($isDirectory) {
      if (-not $directories.Contains($name) -or ($fileType -ne 0 -and $fileType -ne 0x4000)) {
        throw "unexpected Semantic zip directory: '$raw'"
      }
      continue
    }
    if (($fileType -ne 0 -and $fileType -ne 0x8000) -or -not $expectedArchive.ContainsKey($name)) {
      throw "unexpected or non-regular Semantic zip file: '$raw'"
    }
    $record = $expectedArchive[$name]
    if ([long]$entry.Length -ne [long]$record.size) { throw "Semantic zip file size mismatch: '$raw'" }
    $target = Join-Path $Destination ([string]$record.path).Replace('/', '\')
    $parent = Split-Path -Parent $target
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    $source = $entry.Open()
    try {
      $output = [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
      $sha = [System.Security.Cryptography.SHA256]::Create()
      try {
        $buffer = New-Object byte[] 131072
        [long]$written = 0
        while (($count = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
          [long]$fileRemaining = [long]$record.size - $written
          [long]$totalRemaining = [long]$contract.max_expanded_bytes - $total
          if ([long]$count -gt $fileRemaining) { throw "Semantic zip file exceeds signed size: '$raw'" }
          if ([long]$count -gt $totalRemaining) { throw 'Semantic zip exceeds signed expanded-size limit' }
          $output.Write($buffer, 0, $count)
          [void]$sha.TransformBlock($buffer, 0, $count, $null, 0)
          $written += [long]$count
          $total += [long]$count
        }
        [void]$sha.TransformFinalBlock([byte[]]::new(0), 0, 0)
        $actual = ([BitConverter]::ToString($sha.Hash)).Replace('-', '').ToLowerInvariant()
        if ($written -ne [long]$record.size -or $actual -cne [string]$record.sha256) {
          throw "Semantic zip file verification failed: '$raw'"
        }
        $output.Flush($true)
      } finally {
        $sha.Dispose()
        $output.Dispose()
      }
    } finally {
      $source.Dispose()
    }
    [void]$seenFiles.Add($name)
  }
  if ($seenFiles.Count -ne $expectedArchive.Count) {
    throw 'Semantic zip file set does not exactly match signed metadata'
  }
} finally {
  try {
    if ($null -ne $archive) {
      $archive.Dispose()
    }
  } finally {
    $archiveStream.Dispose()
  }
}
"#;

#[cfg(all(test, windows))]
mod powershell_path_argument_tests {
    use std::path::Path;

    use super::powershell_path_argument;

    #[test]
    fn verbatim_drive_paths_lose_the_prefix() {
        assert_eq!(
            powershell_path_argument(Path::new(r"\\?\C:\Users\me\.ctx\runtime")),
            r"C:\Users\me\.ctx\runtime"
        );
    }

    #[test]
    fn verbatim_unc_paths_become_ordinary_unc_paths() {
        assert_eq!(
            powershell_path_argument(Path::new(r"\\?\UNC\server\share\runtime")),
            r"\\server\share\runtime"
        );
    }

    #[test]
    fn ordinary_paths_are_unchanged() {
        for path in [
            r"C:\Users\me\.ctx\runtime",
            r"\\server\share\runtime",
            r"relative\runtime",
        ] {
            assert_eq!(powershell_path_argument(Path::new(path)), path);
        }
    }

    #[test]
    fn device_paths_that_are_not_drives_are_left_alone() {
        assert_eq!(
            powershell_path_argument(Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x")),
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x"
        );
    }
}

#[cfg(test)]
mod semantic_zip_script_tests {
    use super::WINDOWS_SEMANTIC_ZIP_EXTRACT_SCRIPT;

    #[test]
    fn streamed_zip_bytes_are_bounded_before_each_write() {
        let read_loop = WINDOWS_SEMANTIC_ZIP_EXTRACT_SCRIPT
            .split_once("while (($count = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {")
            .unwrap()
            .1
            .split_once("[void]$sha.TransformFinalBlock")
            .unwrap()
            .0;
        let file_guard = read_loop
            .find("if ([long]$count -gt $fileRemaining)")
            .unwrap();
        let total_guard = read_loop
            .find("if ([long]$count -gt $totalRemaining)")
            .unwrap();
        let write = read_loop.find("$output.Write($buffer, 0, $count)").unwrap();
        assert!(file_guard < write);
        assert!(total_guard < write);
        assert!(!WINDOWS_SEMANTIC_ZIP_EXTRACT_SCRIPT.contains("$total += [long]$entry.Length"));
    }
}

#[cfg(all(test, windows))]
mod windows_powershell_tests {
    use std::{ffi::OsStr, fs, process::Command};

    use tempfile::TempDir;

    use super::windows_powershell_path;

    #[test]
    fn powershell_resolves_to_an_absolute_system_path() {
        let powershell = windows_powershell_path().unwrap();

        assert!(powershell.is_absolute());
        assert_eq!(powershell.file_name(), Some(OsStr::new("powershell.exe")));
        assert!(powershell.ends_with(r"WindowsPowerShell\v1.0\powershell.exe"));
    }

    #[test]
    fn hostile_path_cannot_override_system_powershell() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("powershell.exe"),
            b"not a trusted executable",
        )
        .unwrap();

        let output = Command::new(windows_powershell_path().unwrap())
            .env("PATH", temp.path())
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.Write('trusted-system-powershell')",
            ])
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "system PowerShell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"trusted-system-powershell");
    }
}

#[cfg(test)]
mod semantic_tar_tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn asset(bytes: &[u8], max_expanded_bytes: u64) -> SemanticAssetMetadata {
        SemanticAssetMetadata {
            role: "model".to_owned(),
            backend: "onnx".to_owned(),
            version: "1.0.0".to_owned(),
            platform: "any".to_owned(),
            artifact: "model.tar.xz".to_owned(),
            archive_format: "tar.xz".to_owned(),
            archive_path_prefix: "model".to_owned(),
            archive_sha256: "0".repeat(64),
            max_expanded_bytes,
            max_files: 1,
            files: vec![SemanticFileMetadata {
                path: "onnx/model.onnx".to_owned(),
                size: bytes.len() as u64,
                sha256: sha256(bytes),
            }],
        }
    }

    fn write_archive(temp: &TempDir, bytes: &[u8]) -> std::path::PathBuf {
        let path = temp.path().join("model.tar.xz");
        let file = fs::File::create(&path).unwrap();
        let encoder = xz2::write::XzEncoder::new(file, 6);
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "model/onnx/model.onnx", Cursor::new(bytes))
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
        path
    }

    #[test]
    fn semantic_tar_extracts_only_the_exact_signed_file() {
        let temp = TempDir::new().unwrap();
        let bytes = b"signed-model";
        let archive = write_archive(&temp, bytes);
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();

        extract_semantic_archive(
            &crate::upgrade::TEST_RELEASE_PROCESS,
            &archive,
            &destination,
            &asset(bytes, bytes.len() as u64),
        )
        .unwrap();

        assert_eq!(
            fs::read(destination.join("onnx/model.onnx")).unwrap(),
            bytes
        );
    }

    #[test]
    fn semantic_tar_rejects_hash_mismatch() {
        let temp = TempDir::new().unwrap();
        let bytes = b"signed-model";
        let archive = write_archive(&temp, bytes);
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();
        let mut metadata = asset(bytes, bytes.len() as u64);
        metadata.files[0].sha256 = "f".repeat(64);

        let error = extract_semantic_archive(
            &crate::upgrade::TEST_RELEASE_PROCESS,
            &archive,
            &destination,
            &metadata,
        )
        .unwrap_err();

        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn semantic_tar_checks_expanded_limit_before_creating_the_file() {
        let temp = TempDir::new().unwrap();
        let bytes = b"signed-model";
        let archive = write_archive(&temp, bytes);
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();

        let error = extract_semantic_archive(
            &crate::upgrade::TEST_RELEASE_PROCESS,
            &archive,
            &destination,
            &asset(bytes, 1),
        )
        .unwrap_err();

        assert!(error.to_string().contains("signed safety limit"));
        assert!(!destination.join("onnx/model.onnx").exists());
    }
}
