//! Small optional analytics state. Unlike the outbox, counters are best effort:
//! interrupted writes may lose a window, and never justify waiting on a hook.
use anyhow::{Context, Result};
use ctx_history_platform::platform_security::{restrict_private_file_handle, verify_private_file};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::Path,
};

const MAX_BYTES: u64 = 128 * 1024;

pub struct StateFile(File);

impl StateFile {
    pub fn try_open(path: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(path.parent().context("analytics state has no parent")?)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
                    WRITE_DAC,
                },
            };
            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_BYTES {
            anyhow::bail!("optional analytics state is unavailable");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let named = fs::symlink_metadata(path)?;
            if metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
                || named.dev() != metadata.dev()
                || named.ino() != metadata.ino()
            {
                anyhow::bail!("optional analytics state is unavailable");
            }
        }
        restrict_private_file_handle(&file)?;
        verify_private_file(path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self(file))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error).context("lock optional analytics state"),
        }
    }

    pub fn read<T: serde::de::DeserializeOwned>(&mut self) -> Result<Option<T>> {
        self.0.rewind()?;
        let mut body = Vec::new();
        (&mut self.0).take(MAX_BYTES + 1).read_to_end(&mut body)?;
        if body.len() as u64 > MAX_BYTES {
            anyhow::bail!("optional analytics state is unavailable");
        }
        if body.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&body)
            .map(Some)
            .context("read optional analytics state")
    }

    pub fn write<T: serde::Serialize>(&mut self, value: &T) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        if body.len() as u64 > MAX_BYTES {
            anyhow::bail!("optional analytics state is unavailable");
        }
        self.0.rewind()?;
        self.0.set_len(0)?;
        self.0.write_all(&body)?;
        // No fsync or journal on the tool-output path. The queued event, once
        // materialized, uses the existing durable outbox instead.
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        self.0.rewind()?;
        self.0.set_len(0)?;
        Ok(())
    }
}

impl Drop for StateFile {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
mod tests;
