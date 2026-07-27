use std::{
    fs::{self, File, Metadata},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{
        ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    },
    provider::importer::provider_path_identity,
    CaptureError,
};

const PI_DISCOVERY_MAX_DEPTH: usize = 16;
const PI_DISCOVERY_MAX_ENTRIES: usize = 16_384;
const PI_ROUTE_DOMAIN: &[u8] = b"ctx-pi-nativepath-route-v1\0";

#[derive(Debug, Error)]
pub(crate) enum PiNativePathError {
    #[error("Pi NativePath I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid Pi NativePath source {path}: {reason}")]
    InvalidSource { path: PathBuf, reason: String },
    #[error("Pi NativePath source changed while a page was being prepared")]
    SourceChanged,
    #[error("Pi NativePath position or accounting overflowed")]
    PositionOverflow,
    #[error("Pi NativePath page is invalid: {0}")]
    Page(String),
    #[error("Pi NativePath normalized row could not be encoded: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("Pi NativePath canonical normalization failed: {0}")]
    Normalization(#[from] CaptureError),
}

impl PiNativePathError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    fn invalid(path: &Path, reason: impl Into<String>) -> Self {
        Self::InvalidSource {
            path: path.to_path_buf(),
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PiPhysicalFileId {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PiFrozenSource {
    pub(super) path: PathBuf,
    pub(super) canonical_path: PathBuf,
    pub(super) route_identity: String,
    pub(super) route_sha256: [u8; 32],
    pub(super) physical_file_id: Option<PiPhysicalFileId>,
    pub(super) len: u64,
    modified: SystemTime,
    readonly: bool,
}

impl PiFrozenSource {
    pub(super) fn open(path: &Path) -> Result<(File, Self), PiNativePathError> {
        ensure_regular_provider_transcript_file(path).map_err(|error| match error {
            CaptureError::Io(source) => PiNativePathError::io(path, source),
            other => PiNativePathError::invalid(path, other.to_string()),
        })?;
        let canonical_path =
            fs::canonicalize(path).map_err(|source| PiNativePathError::io(path, source))?;
        let file = File::open(path).map_err(|source| PiNativePathError::io(path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| PiNativePathError::io(path, source))?;
        let route_identity = provider_path_identity(path)?;
        let route_sha256 = route_sha256(&route_identity);
        let frozen = Self::from_metadata(
            path.to_path_buf(),
            canonical_path,
            route_identity,
            route_sha256,
            &metadata,
        )?;
        Ok((file, frozen))
    }

    fn from_metadata(
        path: PathBuf,
        canonical_path: PathBuf,
        route_identity: String,
        route_sha256: [u8; 32],
        metadata: &Metadata,
    ) -> Result<Self, PiNativePathError> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let physical_file_id = Some(PiPhysicalFileId {
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        #[cfg(not(unix))]
        let physical_file_id = None;

        Ok(Self {
            path,
            canonical_path,
            route_identity,
            route_sha256,
            physical_file_id,
            len: metadata.len(),
            modified: metadata
                .modified()
                .map_err(|source| PiNativePathError::io(Path::new("<metadata>"), source))?,
            readonly: metadata.permissions().readonly(),
        })
    }

    pub(super) fn source_revision(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        let physical = self.physical_file_id.map_or_else(
            || "none".to_owned(),
            |value| format!("{}:{}", value.device, value.inode),
        );
        format!(
            "pi-nativepath-source-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};physical={physical}",
            self.len, self.readonly
        )
    }

    pub(super) fn fence(&self, file: &File) -> Result<(), PiNativePathError> {
        let descriptor = file
            .metadata()
            .map_err(|source| PiNativePathError::io(&self.path, source))?;
        if !self.matches_metadata(&descriptor) {
            return Err(PiNativePathError::SourceChanged);
        }
        let path_metadata =
            fs::metadata(&self.path).map_err(|source| PiNativePathError::io(&self.path, source))?;
        if !self.matches_metadata(&path_metadata) {
            return Err(PiNativePathError::SourceChanged);
        }
        Ok(())
    }

    fn matches_metadata(&self, metadata: &Metadata) -> bool {
        if metadata.len() != self.len
            || metadata.permissions().readonly() != self.readonly
            || metadata.modified().ok() != Some(self.modified)
        {
            return false;
        }
        physical_file_id(metadata) == self.physical_file_id
    }
}

pub(crate) fn revalidate_pi_source_revision(
    path: &Path,
    expected_revision: &str,
) -> Result<bool, PiNativePathError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| PiNativePathError::io(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    ensure_provider_path_parents_are_not_symlinks(path)
        .map_err(|error| PiNativePathError::invalid(path, error.to_string()))?;
    let canonical_path =
        fs::canonicalize(path).map_err(|source| PiNativePathError::io(path, source))?;
    let route_identity = provider_path_identity(path)?;
    let observed = PiFrozenSource::from_metadata(
        path.to_path_buf(),
        canonical_path,
        route_identity.clone(),
        route_sha256(&route_identity),
        &metadata,
    )?;
    Ok(observed.source_revision() == expected_revision)
}

fn physical_file_id(metadata: &Metadata) -> Option<PiPhysicalFileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(PiPhysicalFileId {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn route_sha256(route_identity: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PI_ROUTE_DOMAIN);
    hasher.update((route_identity.len() as u64).to_be_bytes());
    hasher.update(route_identity.as_bytes());
    hasher.finalize().into()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PiDiscoveryStats {
    pub(crate) visited_entries: usize,
    pub(crate) selected_files: usize,
    pub(crate) peak_directory_entries: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PiDiscovery {
    pub(crate) sessions: Vec<PathBuf>,
    pub(crate) stats: PiDiscoveryStats,
}

pub(crate) fn discover_pi_sessions(root: &Path) -> Result<PiDiscovery, PiNativePathError> {
    let metadata =
        fs::symlink_metadata(root).map_err(|source| PiNativePathError::io(root, source))?;
    if metadata.file_type().is_symlink() {
        return Err(PiNativePathError::invalid(
            root,
            "symlinked Pi session roots are rejected",
        ));
    }
    ensure_provider_path_parents_are_not_symlinks(root)
        .map_err(|error| PiNativePathError::invalid(root, error.to_string()))?;
    let mut discovery = PiDiscovery {
        sessions: Vec::new(),
        stats: PiDiscoveryStats::default(),
    };
    discover_at(root, 0, &mut discovery)?;
    discovery.sessions.sort();
    Ok(discovery)
}

fn discover_at(
    path: &Path,
    depth: usize,
    discovery: &mut PiDiscovery,
) -> Result<(), PiNativePathError> {
    if depth > PI_DISCOVERY_MAX_DEPTH {
        return Err(PiNativePathError::invalid(
            path,
            "Pi session directory nesting exceeds the supported limit",
        ));
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|source| PiNativePathError::io(path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(PiNativePathError::invalid(
            path,
            "symlinked Pi session entries are rejected",
        ));
    }
    if metadata.is_file() {
        if is_jsonl(path) {
            ensure_regular_provider_transcript_file(path)
                .map_err(|error| PiNativePathError::invalid(path, error.to_string()))?;
            discovery.sessions.push(path.to_path_buf());
            discovery.stats.selected_files = discovery.stats.selected_files.saturating_add(1);
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut children = fs::read_dir(path)
        .map_err(|source| PiNativePathError::io(path, source))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| PiNativePathError::io(path, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    discovery.stats.visited_entries = discovery
        .stats
        .visited_entries
        .checked_add(children.len())
        .ok_or(PiNativePathError::PositionOverflow)?;
    if discovery.stats.visited_entries > PI_DISCOVERY_MAX_ENTRIES {
        return Err(PiNativePathError::invalid(
            path,
            "Pi session discovery exceeds the supported entry limit",
        ));
    }
    discovery.stats.peak_directory_entries =
        discovery.stats.peak_directory_entries.max(children.len());
    children.sort();
    for child in children {
        discover_at(&child, depth.saturating_add(1), discovery)?;
    }
    Ok(())
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("jsonl")
}
