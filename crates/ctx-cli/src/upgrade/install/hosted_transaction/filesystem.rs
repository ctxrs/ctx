use std::{
    env, fs,
    io::Write as _,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use uuid::Uuid;

use super::{Journal, JOURNAL_SUFFIX, MAX_BINARY_BYTES};
use crate::upgrade::sha256_hex;

pub(super) fn validate_install_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path.file_name().is_none()
    {
        bail!("hosted transaction install path is not a safe absolute leaf");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("hosted transaction install path has no parent"))?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalize hosted install directory {}", parent.display()))?;
    validate_private_directory(&canonical_parent)?;
    let canonical = canonical_parent.join(
        path.file_name()
            .ok_or_else(|| anyhow!("hosted transaction install path has no file name"))?,
    );
    if canonical != path {
        bail!("hosted transaction install path is not canonical");
    }
    Ok(canonical)
}

#[cfg(unix)]
pub(super) fn validate_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("hosted transaction install directory is not owner-private");
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn validate_private_directory(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::verify_private_directory(path)
        .context("verify hosted transaction install directory")
}

#[cfg(not(any(unix, windows)))]
pub(super) fn validate_private_directory(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

pub(super) fn current_executable() -> Result<PathBuf> {
    fs::canonicalize(env::current_exe().context("resolve hosted transaction executable")?)
        .context("canonicalize hosted transaction executable")
}

pub(super) fn journal_path(install_path: &Path) -> PathBuf {
    install_path.with_file_name(format!(
        ".{}.{}",
        install_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("ctx"),
        JOURNAL_SUFFIX
    ))
}

pub(super) fn staged_binary_path(journal: &Journal) -> PathBuf {
    sibling(journal, "binary.new")
}

pub(super) fn staged_marker_path(journal: &Journal) -> PathBuf {
    sibling(journal, "marker.new")
}

pub(super) fn staged_ownership_path(journal: &Journal) -> PathBuf {
    sibling(journal, "ownership.new")
}

pub(super) fn sibling(journal: &Journal, suffix: &str) -> PathBuf {
    journal.install_path.with_file_name(format!(
        ".{}.hosted-{}.{}",
        journal
            .install_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("ctx"),
        journal.attempt_id,
        suffix
    ))
}

pub(super) fn ownership_path(install_path: &Path) -> PathBuf {
    let mut name = install_path.file_name().unwrap_or_default().to_os_string();
    name.push(".install-integrations");
    install_path.with_file_name(name)
}

pub(in crate::upgrade) fn uninstall_helper_path(install_path: &Path) -> PathBuf {
    let name = install_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    #[cfg(windows)]
    let suffix = "hosted-uninstall-helper.exe";
    #[cfg(not(windows))]
    let suffix = "hosted-uninstall-helper";
    install_path.with_file_name(format!(".{name}.{suffix}"))
}

pub(super) fn read_journal(path: &Path) -> Result<Option<Journal>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    verify_private_file(path)?;
    let journal = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse hosted transaction {}", path.display()))?;
    Ok(Some(journal))
}

pub(super) fn write_initial_journal(path: &Path, journal: &Journal) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("hosted journal has no parent"))?;
    let temporary = parent.join(format!(
        ".{JOURNAL_SUFFIX}.{}.initial",
        Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        restrict_private_file(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)
            .with_context(|| format!("claim hosted transaction {}", path.display()))?;
        remove_if_present(&temporary)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn write_journal(path: &Path, journal: &Journal) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("hosted journal has no parent"))?;
    let temporary = parent.join(format!(".{JOURNAL_SUFFIX}.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        restrict_private_file(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        atomic_publish(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn remove_journal(path: &Path) -> Result<()> {
    remove_durable(path)
}

pub(super) fn stage_file(source: &Path, target: &Path, executable: bool) -> Result<()> {
    let bytes = read_bounded(source, MAX_BINARY_BYTES, "hosted transaction source")?;
    stage_bytes(&bytes, target, executable)
}

pub(super) fn stage_bytes(bytes: &[u8], target: &Path, executable: bool) -> Result<()> {
    remove_if_present(target)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if executable {
        restrict_private_executable(target)?;
    } else {
        restrict_private_file(target)?;
    }
    sync_parent(target)
}

#[cfg(unix)]
pub(super) fn atomic_publish(source: &Path, target: &Path) -> Result<()> {
    fs::rename(source, target).with_context(|| {
        format!(
            "atomically publish {} to {}",
            source.display(),
            target.display()
        )
    })?;
    sync_parent(target)
}

#[cfg(windows)]
pub(super) fn atomic_publish(source: &Path, target: &Path) -> Result<()> {
    super::super::transaction::durable_replace_file(source, target)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn atomic_publish(_source: &Path, _target: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

pub(super) fn remove_durable(path: &Path) -> Result<()> {
    remove_if_present(path)?;
    sync_parent(path)
}

pub(super) fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
pub(super) fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn read_bounded(path: &Path, max: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.len() > max
    {
        bail!("{label} is not a bounded regular file");
    }
    fs::read(path).with_context(|| format!("read {label}"))
}

pub(super) fn sha256_path(path: &Path, max: u64, label: &str) -> Result<String> {
    Ok(sha256_hex(&read_bounded(path, max, label)?))
}

pub(super) fn verify_file_digest(path: &Path, expected: &str, max: u64, label: &str) -> Result<()> {
    let actual = sha256_path(path, max, label)?;
    if actual != expected {
        bail!("{label} digest does not match its hosted transaction");
    }
    Ok(())
}

pub(super) fn file_has_digest(path: &Path, expected: &str, max: u64) -> Result<bool> {
    if !path.try_exists()? {
        return Ok(false);
    }
    Ok(sha256_path(path, max, "hosted transaction path")? == expected)
}

pub(super) fn normalized_sha256(value: &str) -> Result<String> {
    let normalized = value.to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("hosted transaction SHA-256 identity is invalid");
    }
    Ok(normalized)
}

pub(super) fn is_normalized_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(unix)]
pub(super) fn restrict_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(windows)]
pub(super) fn restrict_private_file(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::restrict_private_file(path).map_err(Into::into)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn restrict_private_file(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

#[cfg(unix)]
pub(super) fn restrict_private_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn set_installed_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    sync_parent(path)
}

#[cfg(not(unix))]
pub(super) fn set_installed_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub(super) fn restrict_private_executable(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::restrict_private_executable(path).map_err(Into::into)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn restrict_private_executable(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

#[cfg(unix)]
pub(super) fn verify_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("hosted transaction journal is not owner-private");
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn verify_private_file(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::verify_private_file(path).map_err(Into::into)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn verify_private_file(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}
