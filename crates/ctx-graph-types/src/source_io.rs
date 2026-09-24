use anyhow::{Context, Result, bail, ensure};
use std::{fs::OpenOptions, io::Read, path::Path};

pub fn read_source(path: &Path, maximum: u64) -> Result<(String, Option<Vec<u8>>)> {
    for _ in 0..2 {
        if !std::fs::symlink_metadata(path)?.is_file() {
            bail!("source is no longer a regular file: {}", path.display());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let before = file.metadata()?;
        ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "source is no longer a regular file: {}",
            path.display()
        );
        if before.len() > maximum {
            if file.metadata()?.len() > maximum {
                let size = if maximum == crate::MAX_SOURCE_BYTES as u64 {
                    "4MiB".to_owned()
                } else {
                    maximum.to_string()
                };
                return Ok((format!("oversized:{size}"), None));
            }
            continue;
        }
        let modified = before.modified()?;
        let mut hasher = blake3::Hasher::new();
        let mut content = Some(Vec::new());
        let mut buffer = [0; 64 * 1024];
        let mut count = 0u64;
        // One extra byte detects growth without chasing an indefinitely growing file.
        let mut bounded = (&mut file).take(before.len().saturating_add(1));
        loop {
            let size = bounded
                .read(&mut buffer)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if size == 0 {
                break;
            }
            count += size as u64;
            hasher.update(&buffer[..size]);
            if let Some(bytes) = &mut content {
                if bytes.len() + size <= maximum as usize {
                    bytes.extend_from_slice(&buffer[..size]);
                } else {
                    content = None;
                }
            }
        }
        let after = file.metadata()?;
        if count == before.len() && before.len() == after.len() && modified == after.modified()? {
            return Ok((hasher.finalize().to_hex().to_string(), content));
        }
    }
    bail!(
        "source changed during both read attempts: {}",
        path.display()
    )
}
