use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::provider::normalization::provider_optional_regular_file;
use crate::{CaptureError, Result};

const MAX_ROVODEV_DISCOVERY_DIRECTORIES: usize = 65_536;
const MAX_ROVODEV_DISCOVERY_ENTRIES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RovoDevFrozenFile {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl RovoDevFrozenFile {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            path: path.to_path_buf(),
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn hash_revision_authority(&self, digest: &mut Sha256) {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (b'+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (b'-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        let path = format!("{:?}", self.path.as_os_str());
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        digest.update(self.length.to_be_bytes());
        digest.update([side]);
        digest.update(seconds.to_be_bytes());
        digest.update(nanos.to_be_bytes());
        digest.update([u8::from(self.readonly)]);
        digest.update(self.device.unwrap_or(u64::MAX).to_be_bytes());
        digest.update(self.inode.unwrap_or(u64::MAX).to_be_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RovoDevSessionObservation {
    canonical_path: PathBuf,
    context_file: RovoDevFrozenFile,
    metadata_file: Option<RovoDevFrozenFile>,
}

impl RovoDevSessionObservation {
    pub(super) fn read(source: &RovoDevSessionSource) -> Result<Self> {
        Ok(Self {
            canonical_path: fs::canonicalize(&source.context_path)?,
            context_file: RovoDevFrozenFile::read(&source.context_path)?,
            metadata_file: source
                .metadata_path
                .as_deref()
                .map(RovoDevFrozenFile::read)
                .transpose()?,
        })
    }

    pub(super) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(super) fn context_length(&self) -> u64 {
        self.context_file.length
    }

    pub(super) fn metadata_length(&self) -> Option<u64> {
        self.metadata_file.as_ref().map(|file| file.length)
    }

    pub(super) fn revision_authority(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx-rovodev-frozen-source-revision-v1\0");
        self.context_file.hash_revision_authority(&mut digest);
        match &self.metadata_file {
            Some(file) => {
                digest.update([1]);
                file.hash_revision_authority(&mut digest);
            }
            None => digest.update([0]),
        }
        digest.finalize().into()
    }

    pub(super) fn physical_identity(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"ctx-rovodev-physical-source-v1\0");
        let canonical = format!("{:?}", self.canonical_path.as_os_str());
        digest.update((canonical.len() as u64).to_be_bytes());
        digest.update(canonical.as_bytes());
        digest.update(self.context_file.device.unwrap_or(u64::MAX).to_be_bytes());
        digest.update(self.context_file.inode.unwrap_or(u64::MAX).to_be_bytes());
        if self.context_file.device.is_none() || self.context_file.inode.is_none() {
            let (side, seconds, nanos) = match self.context_file.modified.duration_since(UNIX_EPOCH)
            {
                Ok(duration) => (b'+', duration.as_secs(), duration.subsec_nanos()),
                Err(error) => {
                    let duration = error.duration();
                    (b'-', duration.as_secs(), duration.subsec_nanos())
                }
            };
            digest.update([side]);
            digest.update(seconds.to_be_bytes());
            digest.update(nanos.to_be_bytes());
        }
        format!("sha256:{:x}", digest.finalize())
    }

    pub(super) fn revalidate(&self, source: &RovoDevSessionSource) -> Result<bool> {
        let context_file = match RovoDevFrozenFile::read(&source.context_path) {
            Ok(file) => file,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let current_metadata_path =
            match provider_optional_regular_file(&source.session_dir.join("metadata.json")) {
                Ok(path) => path,
                Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
                Err(error) => return Err(error),
            };
        if current_metadata_path != source.metadata_path {
            return Ok(false);
        }
        let metadata_file = match current_metadata_path.as_deref() {
            Some(path) => match RovoDevFrozenFile::read(path) {
                Ok(file) => Some(file),
                Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(false);
                }
                Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
                Err(error) => return Err(error),
            },
            None => None,
        };
        Ok(context_file == self.context_file
            && metadata_file == self.metadata_file
            && fs::canonicalize(&source.context_path)? == self.canonical_path)
    }
}

#[derive(Debug, Clone)]
pub(super) struct RovoDevSessionSource {
    pub(super) session_dir: PathBuf,
    pub(super) context_path: PathBuf,
    pub(super) metadata_path: Option<PathBuf>,
    pub(super) provider_session_id: String,
}

fn rovodev_session_source_from_dir(dir: &Path) -> Result<Option<RovoDevSessionSource>> {
    let context_path = dir.join("session_context.json");
    if !context_path.is_file() {
        return Ok(None);
    }
    ensure_regular_provider_transcript_file(&context_path)?;
    let metadata_path = provider_optional_regular_file(&dir.join("metadata.json"))?;
    let provider_session_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: dir.to_path_buf(),
            reason: "Rovo Dev session directory is missing a session id",
        })?;
    Ok(Some(RovoDevSessionSource {
        session_dir: dir.to_path_buf(),
        context_path,
        metadata_path,
        provider_session_id,
    }))
}

#[derive(Debug, Clone)]
pub(super) struct RovoDevDiscovery {
    root_exists: bool,
    sources: Vec<RovoDevSessionSource>,
}

impl RovoDevDiscovery {
    pub(super) fn root_exists(&self) -> bool {
        self.root_exists
    }

    pub(super) fn sources(&self) -> &[RovoDevSessionSource] {
        &self.sources
    }

    pub(super) fn canonical_context_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = self
            .sources
            .iter()
            .map(|source| fs::canonicalize(&source.context_path))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        Ok(paths)
    }
}

pub(super) fn discover_rovodev_session_sources(root: &Path) -> Result<RovoDevDiscovery> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RovoDevDiscovery {
                root_exists: false,
                sources: Vec::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.file_name().and_then(|name| name.to_str()) == Some("session_context.json") {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = rovodev_session_source_from_dir(session_dir)? {
                    return Ok(RovoDevDiscovery {
                        root_exists: true,
                        sources: vec![source],
                    });
                }
            }
        }
        return Ok(RovoDevDiscovery {
            root_exists: true,
            sources: Vec::new(),
        });
    }
    if !file_type.is_dir() {
        return Ok(RovoDevDiscovery {
            root_exists: true,
            sources: Vec::new(),
        });
    }
    if let Some(source) = rovodev_session_source_from_dir(root)? {
        return Ok(RovoDevDiscovery {
            root_exists: true,
            sources: vec![source],
        });
    }

    let mut sources = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut visited_directories = 0_usize;
    let mut visited_entries = 0_usize;
    while let Some(directory) = pending.pop() {
        visited_directories = visited_directories.saturating_add(1);
        if visited_directories > MAX_ROVODEV_DISCOVERY_DIRECTORIES {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Rovo Dev session discovery exceeds its directory bound",
            });
        }
        let mut children = Vec::new();
        for entry in fs::read_dir(&directory)? {
            visited_entries = visited_entries.saturating_add(1);
            if visited_entries > MAX_ROVODEV_DISCOVERY_ENTRIES {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: root.to_path_buf(),
                    reason: "Rovo Dev session discovery exceeds its entry bound",
                });
            }
            children.push(entry?);
        }
        children.sort_by_key(std::fs::DirEntry::file_name);
        for entry in children {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            if let Some(source) = rovodev_session_source_from_dir(&path)? {
                sources.push(source);
            } else {
                pending.push(path);
            }
        }
    }
    sources.sort_by(|left, right| left.context_path.cmp(&right.context_path));
    Ok(RovoDevDiscovery {
        root_exists: true,
        sources,
    })
}
