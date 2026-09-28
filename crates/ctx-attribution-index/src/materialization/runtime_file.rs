//! Disposable spill storage. It never participates in a published generation.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::MaterializationIndexError;

#[derive(Debug)]
pub struct RuntimeFile {
    file: Mutex<File>,
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl RuntimeFile {
    pub fn new(root: &Path) -> Result<Self, MaterializationIndexError> {
        let directory = tempfile::Builder::new()
            .prefix("ctx-attribution-")
            .tempdir_in(root)
            .map_err(|source| MaterializationIndexError::Io {
                operation: "create runtime directory",
                path: root.to_owned(),
                source,
            })?;
        let path = directory.path().to_owned();
        ctx_history_platform::platform_security::restrict_private_directory(&path).map_err(
            |source| MaterializationIndexError::Io {
                operation: "restrict runtime directory",
                path: path.clone(),
                source,
            },
        )?;
        // The existing tempfile primitive uses an unlinked file on Unix and
        // FILE_FLAG_DELETE_ON_CLOSE on Windows. The private directory shields
        // the file from creation onward; process exit reclaims its payload
        // even when Rust destructors cannot run.
        let file =
            tempfile::tempfile_in(&path).map_err(|source| MaterializationIndexError::Io {
                operation: "create runtime spill",
                path: path.clone(),
                source,
            })?;
        #[cfg(unix)]
        ctx_history_platform::platform_security::restrict_private_file_handle(&file).map_err(
            |source| MaterializationIndexError::Io {
                operation: "restrict runtime spill",
                path: path.clone(),
                source,
            },
        )?;
        Ok(Self {
            file: Mutex::new(file),
            _directory: directory,
            path,
        })
    }

    pub fn byte_len(&self) -> Result<u64, MaterializationIndexError> {
        self.with_file(|file| Ok(file.metadata()?.len()))
    }

    pub fn append(&self, bytes: &[u8]) -> Result<u64, MaterializationIndexError> {
        self.with_file(|file| {
            let offset = file.seek(SeekFrom::End(0))?;
            file.write_all(bytes)?;
            Ok(offset)
        })
    }

    pub fn read_at(&self, offset: u64, bytes: &mut [u8]) -> Result<(), MaterializationIndexError> {
        self.with_file(|file| {
            let end = offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| io::Error::other("spill offset overflow"))?;
            if end > file.metadata()?.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated runtime spill",
                ));
            }
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(bytes)
        })
    }

    fn with_file<T>(
        &self,
        f: impl FnOnce(&mut File) -> io::Result<T>,
    ) -> Result<T, MaterializationIndexError> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| MaterializationIndexError::Corrupt("runtime spill lock poisoned"))?;
        f(&mut file).map_err(|source| MaterializationIndexError::Io {
            operation: "access runtime spill",
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests;
