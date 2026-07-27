use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};
use uuid::Uuid;

pub(super) fn atomic_write_output(path: &Path, body: &[u8]) -> Result<()> {
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
    let result = (|| -> Result<()> {
        let mut file =
            file.with_context(|| format!("create temporary output for {}", path.display()))?;
        file.write_all(body)
            .with_context(|| format!("write temporary output for {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary output for {}", path.display()))?;
        drop(file);
        replace_file(&temporary, path)
            .with_context(|| format!("install rendered output {}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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
}
