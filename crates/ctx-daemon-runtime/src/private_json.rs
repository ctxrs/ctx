use std::{
    fs,
    io::Write,
    path::Path,
    process,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(windows)]
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    create_private_dir_all, daemon_status_path, private_create_new_file,
    secure_private_file_permissions,
};

const PRIVATE_JSON_TEMP_ATTEMPTS: usize = 16;
static PRIVATE_JSON_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub const PRIVATE_FILE_REPLACE_ATTEMPTS: usize = 40;
#[cfg(windows)]
const PRIVATE_FILE_REPLACE_RETRY: Duration = Duration::from_millis(50);
pub const WINDOWS_ERROR_ACCESS_DENIED: i32 = 5;
pub const WINDOWS_ERROR_SHARING_VIOLATION: i32 = 32;
pub const WINDOWS_ERROR_LOCK_VIOLATION: i32 = 33;

#[derive(Debug, thiserror::Error)]
#[error("private status replacement is visible or indeterminate")]
pub struct PrivateJsonReplacementError;

pub fn write_private_json_file(path: &Path, value: &Value) -> Result<()> {
    write_private_json_file_with_permissions(path, value, secure_private_file_permissions)
}

fn write_private_json_file_with_permissions(
    path: &Path,
    value: &Value,
    secure: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let (tmp_path, mut file) = (0..PRIVATE_JSON_TEMP_ATTEMPTS)
        .find_map(|_| {
            let sequence = PRIVATE_JSON_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tmp_path = path.with_extension(format!("json.{}.{}.tmp", process::id(), sequence));
            match private_create_new_file(&tmp_path) {
                Ok(file) => Some(Ok((tmp_path, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .with_context(|| format!("allocate private status file beside {}", path.display()))?;
    let write_result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec_pretty(value)?)
            .with_context(|| format!("write private status file {}", tmp_path.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("write private status file {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync private status file {}", tmp_path.display()))?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }
    if let Err(error) = replace_private_file(&tmp_path, path)
        .with_context(|| format!("replace private status file {}", path.display()))
    {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }
    secure(path).map_err(|error| {
        error
            .context(format!("secure private status file {}", path.display()))
            .context(PrivateJsonReplacementError)
    })?;
    Ok(())
}

#[cfg(not(windows))]
pub fn sync_private_file_parent(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)
        .with_context(|| format!("open private status directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("sync private status directory {}", parent.display()))
}

#[cfg(windows)]
pub fn sync_private_file_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn replace_private_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
pub fn replace_private_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    retry_windows_private_file_replacement(
        || {
            let moved = unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    target.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if moved == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        },
        || std::thread::sleep(PRIVATE_FILE_REPLACE_RETRY),
    )
}

pub fn retry_windows_private_file_replacement(
    mut replace: impl FnMut() -> std::io::Result<()>,
    mut wait: impl FnMut(),
) -> std::io::Result<()> {
    for attempt in 1..=PRIVATE_FILE_REPLACE_ATTEMPTS {
        match replace() {
            Ok(()) => return Ok(()),
            Err(error)
                if windows_file_replacement_error_is_retryable(&error)
                    && attempt < PRIVATE_FILE_REPLACE_ATTEMPTS =>
            {
                // Virus scanners and indexers can briefly open a newly
                // published status file without delete sharing. Keep atomic
                // replacement semantics while allowing that handle to close.
                wait();
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded replacement loop always returns")
}

fn windows_file_replacement_error_is_retryable(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(
            WINDOWS_ERROR_ACCESS_DENIED
                | WINDOWS_ERROR_SHARING_VIOLATION
                | WINDOWS_ERROR_LOCK_VIOLATION
        )
    )
}

pub fn write_daemon_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&daemon_status_path(data_root), value)
}

pub fn read_daemon_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_daemon_job_status(path: &Path, value: &Value) -> Result<()> {
    write_private_json_file(path, value)
}

pub fn read_daemon_job_status(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Reads durable job authority without collapsing corruption or I/O failure
/// into the ordinary absent-file state used by best-effort status reporting.
pub fn read_daemon_job_status_strict(path: &Path) -> Result<Option<Value>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read durable daemon job status {}", path.display()))
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode durable daemon job status {}", path.display()))
        .map(Some)
}

#[cfg(test)]
mod tests;
