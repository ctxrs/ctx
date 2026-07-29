use std::{
    fs, io,
    io::Write as _,
    path::Path,
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};

static CONFIG_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn write_config_durably(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
    let sequence = CONFIG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".config.{}.{}.tmp", process::id(), sequence));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("create temporary config {}", temp.display()))?;
        file.write_all(body)
            .with_context(|| format!("write temporary config {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary config {}", temp.display()))?;
        drop(file);
        replace_config_file(&temp, path)
            .with_context(|| format!("publish config {}", path.display()))?;
        sync_config_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_config_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_config_file(source: &Path, target: &Path) -> io::Result<()> {
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
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_config_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open config directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync config directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_config_directory(_path: &Path) -> Result<()> {
    Ok(())
}
