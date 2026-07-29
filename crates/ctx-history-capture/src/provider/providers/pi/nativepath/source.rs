use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    provider::provider_path_identity,
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

#[derive(Clone, Debug)]
pub(super) struct PiFrozenSource {
    pub(super) path: PathBuf,
    pub(super) canonical_path: PathBuf,
    pub(super) route_identity: String,
    pub(super) route_sha256: [u8; 32],
    pub(super) physical_file_id: Option<PiPhysicalFileId>,
    pub(super) len: u64,
    modified: SystemTime,
    readonly: bool,
    opened: Arc<OpenedProviderSourceFile>,
}

impl PartialEq for PiFrozenSource {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.canonical_path == other.canonical_path
            && self.route_identity == other.route_identity
            && self.route_sha256 == other.route_sha256
            && self.physical_file_id == other.physical_file_id
            && self.len == other.len
            && self.modified == other.modified
            && self.readonly == other.readonly
    }
}

impl Eq for PiFrozenSource {}

impl PiFrozenSource {
    pub(super) fn from_opened(
        path: &Path,
        opened: Arc<OpenedProviderSourceFile>,
    ) -> Result<(File, Self), PiNativePathError> {
        let file = opened
            .file()
            .try_clone()
            .map_err(|source| PiNativePathError::io(path, source))?;
        let metadata = opened.metadata().clone();
        let route_identity = provider_path_identity(path)?;
        let route_sha256 = route_sha256(&route_identity);
        let frozen = Self::from_metadata(
            path.to_path_buf(),
            path.to_path_buf(),
            route_identity,
            route_sha256,
            &metadata,
            opened,
        )?;
        Ok((file, frozen))
    }

    fn from_metadata(
        path: PathBuf,
        canonical_path: PathBuf,
        route_identity: String,
        route_sha256: [u8; 32],
        metadata: &Metadata,
        opened: Arc<OpenedProviderSourceFile>,
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
            opened,
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
        if self.opened.revalidate().is_err() {
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

    pub(super) fn opened(&self) -> Arc<OpenedProviderSourceFile> {
        Arc::clone(&self.opened)
    }
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

#[derive(Clone, Debug)]
pub(crate) struct PiDiscovery {
    pub(crate) sessions: Vec<PathBuf>,
    pub(crate) stats: PiDiscoveryStats,
    authority: Option<ProviderSourceRoot>,
    opened: BTreeMap<PathBuf, Arc<OpenedProviderSourceFile>>,
}

impl PartialEq for PiDiscovery {
    fn eq(&self, other: &Self) -> bool {
        self.sessions == other.sessions && self.stats == other.stats
    }
}

impl Eq for PiDiscovery {}

impl PiDiscovery {
    pub(crate) fn opened(
        &self,
        path: &Path,
    ) -> Result<Arc<OpenedProviderSourceFile>, PiNativePathError> {
        self.opened
            .get(path)
            .cloned()
            .ok_or_else(|| PiNativePathError::invalid(path, "Pi discovery lost its source handle"))
    }

    pub(crate) fn revalidate(&self) -> Result<(), PiNativePathError> {
        for (path, opened) in &self.opened {
            opened
                .revalidate()
                .map_err(|_| PiNativePathError::SourceChanged)?;
            if !self.sessions.contains(path) {
                return Err(PiNativePathError::SourceChanged);
            }
        }
        if let Some(authority) = &self.authority {
            authority
                .revalidate()
                .map_err(|_| PiNativePathError::SourceChanged)?;
        }
        Ok(())
    }

    pub(crate) fn rediscover(&self) -> Result<Self, PiNativePathError> {
        let Some(authority) = &self.authority else {
            self.revalidate()?;
            return Ok(self.clone());
        };
        let root = authority.named_path().to_path_buf();
        let mut discovery = PiDiscovery {
            sessions: Vec::new(),
            stats: PiDiscoveryStats::default(),
            authority: Some(authority.clone()),
            opened: BTreeMap::new(),
        };
        let directory = authority
            .directory()
            .map_err(|error| map_capture_error(&root, error))?;
        discover_at(&root, directory, 0, &mut discovery)?;
        discovery.sessions.sort();
        discovery.revalidate()?;
        Ok(discovery)
    }
}

pub(crate) fn discover_pi_sessions(root: &Path) -> Result<PiDiscovery, PiNativePathError> {
    let root = absolute_lexical_path(root)?;
    let mut discovery = PiDiscovery {
        sessions: Vec::new(),
        stats: PiDiscoveryStats::default(),
        authority: None,
        opened: BTreeMap::new(),
    };
    match open_provider_source_path(&root).map_err(|error| map_capture_error(&root, error))? {
        OpenedProviderSourcePath::File(file) => {
            discovery.sessions.push(root.clone());
            discovery.opened.insert(root, Arc::new(file));
            discovery.stats.selected_files = 1;
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let authority = directory.authority_root();
            discover_at(&root, directory, 0, &mut discovery)?;
            discovery.authority = Some(authority);
        }
    }
    discovery.sessions.sort();
    discovery.revalidate()?;
    Ok(discovery)
}

fn absolute_lexical_path(path: &Path) -> Result<PathBuf, PiNativePathError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| PiNativePathError::io(path, source))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn discover_at(
    display_path: &Path,
    directory: ProviderSourceDirectory,
    depth: usize,
    discovery: &mut PiDiscovery,
) -> Result<(), PiNativePathError> {
    if depth > PI_DISCOVERY_MAX_DEPTH {
        return Err(PiNativePathError::invalid(
            display_path,
            "Pi session directory nesting exceeds the supported limit",
        ));
    }
    let children = directory
        .entries(PI_DISCOVERY_MAX_ENTRIES.saturating_add(1))
        .map_err(|error| map_capture_error(display_path, error))?;
    discovery.stats.visited_entries = discovery
        .stats
        .visited_entries
        .checked_add(children.len())
        .ok_or(PiNativePathError::PositionOverflow)?;
    if discovery.stats.visited_entries > PI_DISCOVERY_MAX_ENTRIES {
        return Err(PiNativePathError::invalid(
            display_path,
            "Pi session discovery exceeds the supported entry limit",
        ));
    }
    discovery.stats.peak_directory_entries =
        discovery.stats.peak_directory_entries.max(children.len());
    for name in children {
        let child_path = display_path.join(&name);
        match directory
            .open_child(&name)
            .map_err(|error| map_capture_error(&child_path, error))?
        {
            OpenedProviderSourcePath::File(file) if is_jsonl(&child_path) => {
                discovery.sessions.push(child_path.clone());
                discovery.opened.insert(child_path, Arc::new(file));
                discovery.stats.selected_files = discovery.stats.selected_files.saturating_add(1);
            }
            OpenedProviderSourcePath::Directory(child) => {
                discover_at(&child_path, child, depth.saturating_add(1), discovery)?;
            }
            OpenedProviderSourcePath::File(_) => {}
        }
    }
    directory
        .revalidate()
        .map_err(|error| map_capture_error(display_path, error))?;
    Ok(())
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("jsonl")
}

fn map_capture_error(path: &Path, error: CaptureError) -> PiNativePathError {
    match error {
        CaptureError::Io(source) => PiNativePathError::io(path, source),
        other => PiNativePathError::invalid(path, other.to_string()),
    }
}
