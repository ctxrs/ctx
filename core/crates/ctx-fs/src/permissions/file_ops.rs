use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

use super::platform::harden_private_open_file_sync;
use super::{
    chain, ensure_private_dir_sync, harden_private_file_sync, private_parent, PRIVATE_FILE_MODE,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn read_private_file_to_string_sync(path: &Path) -> Result<Option<String>> {
    if let Some(parent) = private_parent(path) {
        if !chain::ensure_private_dir_chain_sync(parent, false)? {
            return Ok(None);
        }
    }
    if !chain::reject_symlink_sync(path)? {
        return Ok(None);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("opening private file {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("reading private file metadata {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("private path must be a regular file: {}", path.display());
    }
    harden_private_open_file_sync(&file, path)?;

    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .with_context(|| format!("reading private file {}", path.display()))?;
    Ok(Some(contents))
}

pub(super) fn write_private_file_atomic_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("missing parent directory for {}", path.display()))?;
    ensure_private_dir_sync(parent)?;

    let mut last_error = None;
    for _ in 0..32 {
        let tmp_path = private_temp_path(parent);
        match create_private_new_file(&tmp_path) {
            Ok(mut file) => {
                let result = (|| -> Result<()> {
                    harden_private_file_sync(&tmp_path)?;
                    file.write_all(bytes)
                        .with_context(|| format!("writing {}", tmp_path.display()))?;
                    file.sync_data()
                        .with_context(|| format!("syncing {}", tmp_path.display()))?;
                    drop(file);
                    fs::rename(&tmp_path, path).with_context(|| {
                        format!("renaming {} to {}", tmp_path.display(), path.display())
                    })?;
                    harden_private_file_sync(path)?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = fs::remove_file(&tmp_path);
                }
                return result;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(err);
                continue;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("creating private temp file {}", tmp_path.display()));
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "private temp file collision limit exceeded",
            )
        })
        .into())
}

pub(super) fn open_private_append_sync(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .with_context(|| format!("missing parent directory for {}", path.display()))?;
    ensure_private_dir_sync(parent)?;
    chain::reject_symlink_sync(path)?;
    let file = open_private_append_options()
        .open(path)
        .with_context(|| format!("opening private append file {}", path.display()))?;
    harden_private_open_file_sync(&file, path)?;
    Ok(file)
}

fn private_temp_path(parent: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    parent.join(format!(
        ".ctx-private-{}-{nanos}-{counter}.tmp",
        std::process::id()
    ))
}

#[cfg(unix)]
fn create_private_new_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_new_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create_new(true).write(true).open(path)
}

#[cfg(unix)]
fn open_private_append_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options
        .create(true)
        .append(true)
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW);
    options
}

#[cfg(not(unix))]
fn open_private_append_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}
