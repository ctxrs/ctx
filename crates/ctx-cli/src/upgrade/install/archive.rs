use std::{env, fs, path::Path};

#[cfg(unix)]
use std::collections::BTreeSet;

use anyhow::{anyhow, Context, Result};
#[cfg(unix)]
use flate2::read::GzDecoder;

#[cfg(unix)]
use super::durability::sync_directory;

const MAX_RUNTIME_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;

#[cfg(unix)]
pub(super) fn extract_runtime_archive(
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
    let script_path = archive_path.with_extension("extract.ps1");
    fs::write(&script_path, super::windows_runtime_extract_script())
        .with_context(|| format!("write runtime extraction helper {}", script_path.display()))?;
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg("-ArchivePath")
        .arg(archive_path)
        .arg("-Destination")
        .arg(destination)
        .arg("-ExpectedVersion")
        .arg(version)
        .arg("-MaxExpandedBytes")
        .arg(runtime_expanded_size_limit().to_string())
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
    if cfg!(debug_assertions) {
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
