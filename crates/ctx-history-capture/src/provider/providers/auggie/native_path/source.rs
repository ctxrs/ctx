use std::{
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{common::io::OpenedProviderSourceFile, CaptureError, Result};

/// Short-lived handle for one actively read Auggie document.
///
/// Complete inventories retain only closed observations and reopen through
/// their bounded tree authority; this value must never be stored per leaf.
#[derive(Debug)]
pub(super) struct AuggieFileStamp {
    pub(super) canonical_path: PathBuf,
    pub(super) len: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
    authority_fingerprint: [u8; 32],
    opened: OpenedProviderSourceFile,
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
            authority_fingerprint: opened.authority_fingerprint(),
            opened,
        })
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

    pub(super) fn authority_fingerprint(&self) -> [u8; 32] {
        self.authority_fingerprint
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
