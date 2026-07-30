use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    OpenedProviderSourceFile,
};
use crate::provider::normalization::provider_optional_regular_file;
use crate::{CaptureError, Result};

const MAX_ROVODEV_DISCOVERY_DIRECTORIES: usize = 65_536;
const MAX_ROVODEV_DISCOVERY_ENTRIES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RovoDevFrozenFile {
    path: PathBuf,
    length: u64,
    ordinary_file_token: [u8; 32],
}

impl RovoDevFrozenFile {
    fn from_opened(path: PathBuf, source: &OpenedProviderSourceFile) -> Self {
        Self {
            path,
            length: source.len(),
            ordinary_file_token: source.ordinary_file_token(),
        }
    }

    fn hash_revision_authority(&self, digest: &mut Sha256) {
        let path = format!("{:?}", self.path.as_os_str());
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        digest.update(self.length.to_be_bytes());
        digest.update(self.ordinary_file_token);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RovoDevSessionObservation {
    canonical_path: PathBuf,
    context_file: RovoDevFrozenFile,
    metadata_file: Option<RovoDevFrozenFile>,
}

impl RovoDevSessionObservation {
    pub(super) fn from_opened(
        canonical_path: PathBuf,
        context_path: PathBuf,
        context_file: &OpenedProviderSourceFile,
        metadata: Option<(PathBuf, &OpenedProviderSourceFile)>,
    ) -> Self {
        Self {
            canonical_path,
            context_file: RovoDevFrozenFile::from_opened(context_path, context_file),
            metadata_file: metadata
                .map(|(path, source)| RovoDevFrozenFile::from_opened(path, source)),
        }
    }

    pub(super) fn context_length(&self) -> u64 {
        self.context_file.length
    }

    pub(super) fn metadata_length(&self) -> Option<u64> {
        self.metadata_file.as_ref().map(|file| file.length)
    }

    pub(super) fn revision_authority(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx-rovodev-frozen-source-revision-v2\0");
        let canonical_path = self.canonical_path.as_os_str().as_encoded_bytes();
        digest.update((canonical_path.len() as u64).to_be_bytes());
        digest.update(canonical_path);
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
