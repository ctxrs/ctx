use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use crate::{
    common::io::{open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath},
    CaptureError, Result,
};

use super::normalized_auggie_authority_path;

#[derive(Debug, Clone)]
pub(super) struct AuggieFileStamp {
    pub(super) canonical_path: PathBuf,
    pub(super) len: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
    opened: Arc<OpenedProviderSourceFile>,
}

impl AuggieFileStamp {
    pub(super) fn from_opened(path: PathBuf, opened: OpenedProviderSourceFile) -> Result<Self> {
        let metadata = opened.metadata();
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            canonical_path: path,
            len: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
            opened: Arc::new(opened),
        })
    }

    pub(super) fn observe(path: &Path) -> Result<Self> {
        let path = normalized_auggie_authority_path(path)?;
        let opened = match open_provider_source_path(&path)? {
            OpenedProviderSourcePath::File(opened) => opened,
            OpenedProviderSourcePath::Directory(_) => {
                return Err(invalid_source_path(
                    &path,
                    "Auggie transcript paths must be regular files",
                ));
            }
        };
        Self::from_opened(path, opened)
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        match self.opened.revalidate() {
            Ok(()) => Ok(true),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(CaptureError::InvalidProviderTranscriptPath { .. })
            | Err(CaptureError::SourceChangedDuringCapture) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn read_all_bounded(&self, maximum: usize) -> Result<Vec<u8>> {
        self.opened.read_all_bounded(maximum)
    }
}

pub(super) fn invalid_source_path(path: &Path, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}
