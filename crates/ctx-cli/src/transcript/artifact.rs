use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use uuid::Uuid;

pub(super) struct AtomicOutputFile {
    destination: PathBuf,
    temporary: PathBuf,
    file: Option<fs::File>,
    installed: bool,
}

impl AtomicOutputFile {
    pub(super) fn create(path: &Path) -> Result<Self> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = parent.join(format!(".ctx-output-{}.tmp", Uuid::new_v4()));
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
        };
        #[cfg(not(unix))]
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary);
        let file =
            file.with_context(|| format!("create temporary output for {}", path.display()))?;
        Ok(Self {
            destination: path.to_path_buf(),
            temporary,
            file: Some(file),
            installed: false,
        })
    }

    pub(super) fn commit(mut self) -> Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush().with_context(|| {
                format!("flush temporary output for {}", self.destination.display())
            })?;
            file.sync_all().with_context(|| {
                format!("sync temporary output for {}", self.destination.display())
            })?;
        }
        drop(self.file.take());
        replace_file(&self.temporary, &self.destination)
            .with_context(|| format!("install rendered output {}", self.destination.display()))?;
        self.installed = true;
        Ok(())
    }
}

impl Write for AtomicOutputFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("atomic output is already closed"))?
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("atomic output is already closed"))?
            .flush()
    }
}

impl Drop for AtomicOutputFile {
    fn drop(&mut self) {
        if !self.installed {
            drop(self.file.take());
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

pub(super) fn atomic_write_output(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut output = AtomicOutputFile::create(path)?;
    output
        .write_all(body)
        .with_context(|| format!("write temporary output for {}", path.display()))?;
    output.commit()
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
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
    // SAFETY: both buffers are NUL-terminated and live for the duration of the call.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_output_replaces_the_destination_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("transcript.txt");
        fs::write(&output, "old").unwrap();

        atomic_write_output(&output, b"complete transcript").unwrap();

        assert_eq!(fs::read_to_string(&output).unwrap(), "complete transcript");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn abandoned_stream_keeps_destination_and_removes_staging_file() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("transcript.json");
        fs::write(&output, "old").unwrap();

        {
            let mut staged = AtomicOutputFile::create(&output).unwrap();
            staged.write_all(b"partial").unwrap();
        }

        assert_eq!(fs::read_to_string(&output).unwrap(), "old");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
