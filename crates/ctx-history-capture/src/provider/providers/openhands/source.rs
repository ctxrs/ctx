use std::{
    fs::Metadata,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        open_provider_source_path, path_has_component, OpenedProviderSourceFile,
        OpenedProviderSourcePath, ProviderSourceRoot,
    },
    CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const OPENHANDS_DISCOVERY_MAX_DEPTH: usize = 16;
const OPENHANDS_DISCOVERY_MAX_ENTRIES: usize = 16_384;
const OPENHANDS_MAX_PATH_BYTES: usize = 7 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OpenHandsObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl OpenHandsObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenHandsFileObservation {
    pub(super) length: u64,
    modified: OpenHandsObservedTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl OpenHandsFileObservation {
    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: OpenHandsObservedTime::from_system_time(metadata.modified()?),
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }
}

#[derive(Debug)]
pub(super) struct OpenHandsObservedFile {
    pub(super) canonical_path_text: String,
    pub(super) observation: OpenHandsFileObservation,
    pub(super) raw_bytes: Option<Vec<u8>>,
    pub(super) content_sha256: Option<[u8; 32]>,
    opened: Arc<OpenedProviderSourceFile>,
}

impl OpenHandsObservedFile {
    fn from_opened(canonical_path: PathBuf, opened: OpenedProviderSourceFile) -> Result<Self> {
        let canonical_path_text = openhands_checked_path_text(&canonical_path)?;
        let descriptor_observation = OpenHandsFileObservation::from_metadata(opened.metadata())?;

        let limit = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("OpenHands record byte limit exceeds u64")
        })?;
        let raw_bytes = if descriptor_observation.length > limit {
            None
        } else {
            let bytes = opened.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
            if u64::try_from(bytes.len()).ok() != Some(descriptor_observation.length)
                || bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Some(bytes)
        };
        opened.revalidate()?;
        let content_sha256 = raw_bytes
            .as_deref()
            .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
        Ok(Self {
            canonical_path_text,
            observation: descriptor_observation,
            raw_bytes,
            content_sha256,
            opened: Arc::new(opened),
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
}

#[derive(Debug)]
pub(super) struct OpenHandsInventory {
    pub(super) paths: Vec<PathBuf>,
    pub(super) root_missing: bool,
    pub(super) authority: Option<ProviderSourceRoot>,
    selected_path: PathBuf,
    selected_file: bool,
}

pub(super) fn discover_openhands_event_paths(root: &Path) -> Result<OpenHandsInventory> {
    let root = normalized_openhands_authority_path(root)?;
    let opened = match open_provider_source_path(&root) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(OpenHandsInventory {
                paths: Vec::new(),
                root_missing: true,
                authority: None,
                selected_path: root,
                selected_file: false,
            });
        }
        Err(error) => return Err(error),
    };
    let (authority, selected_path, selected_file) = match opened {
        OpenedProviderSourcePath::Directory(directory) => {
            let authority = directory.authority_root();
            (
                authority.clone(),
                authority.named_path().to_path_buf(),
                false,
            )
        }
        OpenedProviderSourcePath::File(file) => {
            let name = root
                .file_name()
                .ok_or_else(|| openhands_missing_event_files(&root))?;
            let parent = root
                .parent()
                .ok_or_else(|| openhands_missing_event_files(&root))?;
            let authority = ProviderSourceRoot::open(parent)?;
            let selected_path = authority.named_path().join(name);
            authority.open_file(Path::new(name))?.revalidate()?;
            file.revalidate()?;
            (authority, selected_path, true)
        }
    };
    let paths = discover_with_openhands_authority(&authority, &selected_path, selected_file)?;
    authority.revalidate()?;
    Ok(OpenHandsInventory {
        paths,
        root_missing: false,
        authority: Some(authority),
        selected_path,
        selected_file,
    })
}

impl OpenHandsInventory {
    pub(super) fn selected_path(&self) -> &Path {
        &self.selected_path
    }

    pub(super) fn open_source(&self, path: &Path) -> Result<OpenHandsObservedFile> {
        let authority = self
            .authority
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands inventory has no retained authority",
            ))?;
        let relative = path.strip_prefix(authority.named_path()).map_err(|_| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "OpenHands source escaped its retained authority root",
            }
        })?;
        OpenHandsObservedFile::from_opened(path.to_path_buf(), authority.open_file(relative)?)
    }

    pub(super) fn refresh_paths(&self) -> Result<Vec<PathBuf>> {
        let authority = self
            .authority
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands inventory has no retained authority",
            ))?;
        let paths =
            discover_with_openhands_authority(authority, &self.selected_path, self.selected_file)?;
        authority.revalidate()?;
        Ok(paths)
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        match self.authority.as_ref() {
            Some(authority) => authority.revalidate(),
            None if self.root_missing => Ok(()),
            None => Err(CaptureError::SystemInvariant(
                "OpenHands complete inventory has no retained authority",
            )),
        }
    }
}

fn discover_with_openhands_authority(
    authority: &ProviderSourceRoot,
    selected_path: &Path,
    selected_file: bool,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut visited_entries = 0_usize;
    let relative = selected_path
        .strip_prefix(authority.named_path())
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: selected_path.to_path_buf(),
            reason: "OpenHands selected source escaped its retained authority root",
        })?;
    if selected_file {
        let file = authority.open_file(relative)?;
        if openhands_json_path_is_event(selected_path) {
            file.revalidate()?;
            paths.push(selected_path.to_path_buf());
        }
    } else {
        discover_at(authority, relative, 0, &mut visited_entries, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn discover_at(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
    depth: usize,
    visited_entries: &mut usize,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let path = authority.named_path().join(relative_path);
    if depth > OPENHANDS_DISCOVERY_MAX_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands event directory nesting exceeds the supported limit",
        });
    }
    match authority.open_path(relative_path)? {
        OpenedProviderSourcePath::File(file) => {
            if openhands_json_path_is_event(&path) {
                file.revalidate()?;
                paths.push(path);
            }
            return Ok(());
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let remaining = OPENHANDS_DISCOVERY_MAX_ENTRIES.saturating_sub(*visited_entries);
            let children = directory.entries(remaining.saturating_add(1))?;
            *visited_entries = visited_entries.checked_add(children.len()).ok_or(
                CaptureError::SystemInvariant("OpenHands discovery entry count overflowed"),
            )?;
            if *visited_entries > OPENHANDS_DISCOVERY_MAX_ENTRIES {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path,
                    reason: "OpenHands event discovery exceeds the supported entry limit",
                });
            }
            for child in children {
                discover_at(
                    authority,
                    &relative_path.join(child),
                    depth.saturating_add(1),
                    visited_entries,
                    paths,
                )?;
            }
            directory.revalidate()?;
        }
    }
    Ok(())
}

pub(super) fn normalized_openhands_authority_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(CaptureError::InvalidProviderTranscriptPath {
                        path: path.to_path_buf(),
                        reason: "OpenHands roots cannot escape the filesystem root",
                    });
                }
            }
        }
    }
    Ok(normalized)
}

pub(super) fn openhands_missing_event_files(path: &Path) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "no OpenHands event JSON files found under v1_conversations",
    }
}

pub(super) fn openhands_checked_path_text(path: &Path) -> Result<String> {
    let Some(text) = path.to_str() else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands selected paths must be valid UTF-8",
        });
    };
    if text.len() > OPENHANDS_MAX_PATH_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands selected path exceeds the provider identity byte limit",
        });
    }
    Ok(text.to_owned())
}

pub(crate) fn openhands_json_path_is_event(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path_has_component(path, "v1_conversations")
}
