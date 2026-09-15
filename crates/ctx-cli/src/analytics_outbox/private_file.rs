//! Existing private, atomic outbox writes, shared with optional metadata.
use super::*;

pub(super) fn write_private_file_durably(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("analytics outbox path has no parent")?;
    let temp = parent.join(format!(
        "{OUTBOX_TEMP_PREFIX}{}{OUTBOX_TEMP_SUFFIX}",
        uuid::Uuid::new_v4()
    ));
    write_private_file_via(path, body, &temp)
}

// The optional sidecar uses its own fixed temp name, outside authoritative cleanup.
pub(super) fn write_private_file_via(path: &Path, body: &[u8], temp: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("analytics outbox path has no parent")?;
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, READ_CONTROL, WRITE_DAC,
                },
            };

            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options
            .open(temp)
            .context("create analytics outbox temporary file")?;
        restrict_private_file_handle(&file).context("protect analytics outbox temporary file")?;
        file.write_all(body)
            .context("write analytics outbox temporary file")?;
        file.sync_all()
            .context("sync analytics outbox temporary file")?;
        drop(file);
        replace_file(temp, path).context("publish analytics outbox")?;
        verify_private_file(path).context("verify analytics outbox permissions")?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
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
}

#[cfg(unix)]
pub(super) fn sync_parent(path: &Path) -> Result<()> {
    fs::File::open(path)
        .context("open analytics outbox directory")?
        .sync_all()
        .context("sync analytics outbox directory")
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}
