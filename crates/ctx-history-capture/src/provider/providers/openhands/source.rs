use std::{
    fs::Metadata,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        open_provider_source_file, open_provider_source_path, path_has_component,
        OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceRoot,
    },
    fnv1a64,
    provider::importer::{provider_path_identity, provider_source_cursor_stream_for_path},
    CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

const OPENHANDS_DISCOVERY_MAX_DEPTH: usize = 16;
const OPENHANDS_DISCOVERY_MAX_ENTRIES: usize = 16_384;
const OPENHANDS_MAX_PATH_BYTES: usize = 7 * 1024;
const OPENHANDS_ROUTE_HASH_DOMAIN: &[u8] = b"ctx-openhands-nativepath-route-v1\0";
const OPENHANDS_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-openhands-nativepath-source-revision-v1\0";
const OPENHANDS_PHYSICAL_FINGERPRINT_DOMAIN: &[u8] =
    b"ctx-openhands-nativepath-physical-fingerprint-v1\0";

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

    pub(super) fn physical_identity(&self) -> (Option<u64>, Option<u64>) {
        (self.device, self.inode)
    }
}

#[derive(Debug)]
pub(super) struct OpenHandsObservedFile {
    pub(super) canonical_path: PathBuf,
    pub(super) canonical_path_text: String,
    pub(super) conversation_dir: PathBuf,
    pub(super) session_id: String,
    pub(super) user_id: Option<String>,
    pub(super) path_identity: String,
    pub(super) route_sha256: [u8; 32],
    pub(super) cursor_stream: String,
    pub(super) observation: OpenHandsFileObservation,
    pub(super) raw_bytes: Option<Vec<u8>>,
    pub(super) content_sha256: Option<[u8; 32]>,
    opened: Arc<OpenedProviderSourceFile>,
}

impl OpenHandsObservedFile {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let canonical_path = normalized_openhands_authority_path(path)?;
        let opened = open_openhands_source_file(&canonical_path)?;
        Self::from_opened(canonical_path, opened)
    }

    fn from_opened(canonical_path: PathBuf, opened: OpenedProviderSourceFile) -> Result<Self> {
        let canonical_path_text = openhands_checked_path_text(&canonical_path)?;
        let session_id = openhands_conversation_id_from_path(&canonical_path)
            .ok_or_else(|| openhands_missing_event_files(&canonical_path))
            .and_then(|value| openhands_bounded_derived_text(value, "conversation id"))?;
        let user_id = openhands_user_id_from_path(&canonical_path)
            .map(|value| openhands_bounded_derived_text(value, "user id"))
            .transpose()?;
        let conversation_dir = canonical_path
            .parent()
            .ok_or_else(|| openhands_missing_event_files(&canonical_path))?
            .to_path_buf();
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
        let path_identity = provider_path_identity(&canonical_path)?;
        let route_sha256 = route_sha256(&path_identity);
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenHands,
            OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            &path_identity,
        );
        Ok(Self {
            canonical_path,
            canonical_path_text,
            conversation_dir,
            session_id,
            user_id,
            path_identity,
            route_sha256,
            cursor_stream,
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

    /// Cursor revision for this exact path. The route hash deliberately keeps a
    /// cursor from being resumed after the file moves.
    pub(super) fn cursor_revision(&self, inventory_token: Option<&str>) -> String {
        let mut digest = Sha256::new();
        digest.update(OPENHANDS_SOURCE_REVISION_DOMAIN);
        digest.update(self.route_sha256);
        digest.update(self.observation.length.to_be_bytes());
        digest.update([u8::from(self.observation.modified.before_epoch)]);
        digest.update(self.observation.modified.seconds.to_be_bytes());
        digest.update(self.observation.modified.nanos.to_be_bytes());
        digest.update([u8::from(self.observation.readonly)]);
        hash_optional_u64(&mut digest, self.observation.device);
        hash_optional_u64(&mut digest, self.observation.inode);
        match self.content_sha256 {
            Some(content_sha256) => {
                digest.update([1]);
                digest.update(content_sha256);
            }
            None => digest.update([0]),
        }
        if let Some(token) = inventory_token {
            digest.update([1]);
            digest.update((token.len() as u64).to_be_bytes());
            digest.update(token.as_bytes());
        } else {
            digest.update([0]);
        }
        format!("openhands-nativepath-source-v1:{}", hex(&digest.finalize()))
    }

    /// Bounded physical-source evidence used only to reconcile a missing path
    /// with one uniquely matching new path.
    pub(super) fn physical_fingerprint(&self) -> String {
        openhands_physical_fingerprint(&self.observation, self.content_sha256)
    }

    pub(super) fn current_prefix_matches(
        &self,
        prior_length: u64,
        prior_content_sha256: [u8; 32],
    ) -> bool {
        let Some(bytes) = self.raw_bytes.as_deref() else {
            return false;
        };
        let Ok(prior_length) = usize::try_from(prior_length) else {
            return false;
        };
        bytes
            .get(..prior_length)
            .is_some_and(|prefix| <[u8; 32]>::from(Sha256::digest(prefix)) == prior_content_sha256)
    }
}

pub(super) fn openhands_physical_fingerprint(
    observation: &OpenHandsFileObservation,
    content_sha256: Option<[u8; 32]>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_PHYSICAL_FINGERPRINT_DOMAIN);
    digest.update(observation.length.to_be_bytes());
    digest.update([u8::from(observation.modified.before_epoch)]);
    digest.update(observation.modified.seconds.to_be_bytes());
    digest.update(observation.modified.nanos.to_be_bytes());
    digest.update([u8::from(observation.readonly)]);
    hash_optional_u64(&mut digest, observation.device);
    hash_optional_u64(&mut digest, observation.inode);
    match content_sha256 {
        Some(content_sha256) => {
            digest.update([1]);
            digest.update(content_sha256);
        }
        None => digest.update([0]),
    }
    format!(
        "openhands-nativepath-physical-v1:{}",
        hex(&digest.finalize())
    )
}

#[cfg(test)]
std::thread_local! {
    static OPENHANDS_SOURCE_FILE_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn open_openhands_source_file(path: &Path) -> Result<OpenedProviderSourceFile> {
    #[cfg(test)]
    OPENHANDS_SOURCE_FILE_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    open_provider_source_file(path)
}

#[cfg(test)]
pub(crate) fn count_openhands_source_file_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    OPENHANDS_SOURCE_FILE_OPEN_COUNT.with(|count| {
        assert_eq!(count.replace(Some(0)), None);
    });
    let output = operation();
    let opens = OPENHANDS_SOURCE_FILE_OPEN_COUNT.with(|count| count.replace(None).unwrap());
    (output, opens)
}

fn hash_optional_u64(digest: &mut Sha256, value: Option<u64>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        digest.update(value.to_be_bytes());
    }
}

fn route_sha256(path_identity: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_ROUTE_HASH_DOMAIN);
    digest.update((path_identity.len() as u64).to_be_bytes());
    digest.update(path_identity.as_bytes());
    digest.finalize().into()
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

pub(super) fn openhands_conversation_id_from_path(path: &Path) -> Option<String> {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    while let Some(component) = components.next() {
        if component == "v1_conversations" {
            return components
                .next()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
        }
    }
    None
}

pub(super) fn openhands_user_id_from_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[1] == "v1_conversations" && !window[0].trim().is_empty())
            .then(|| window[0].to_owned())
    })
}

pub(super) fn openhands_line_number(path: &Path) -> usize {
    fnv1a64(path.display().to_string().as_bytes()) as usize
}

pub(super) fn openhands_legacy_filename_index_candidate(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    let (ordinal, suffix) = stem.split_once('-')?;
    if ordinal.is_empty() || suffix.is_empty() || !ordinal.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    ordinal
        .parse::<u64>()
        .ok()
        .and_then(|ordinal| ordinal.checked_sub(1))
}

fn openhands_bounded_derived_text(value: String, field: &str) -> Result<String> {
    const MAX_DERIVED_TEXT_BYTES: usize = 16 * 1024;
    if value.len() > MAX_DERIVED_TEXT_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenHands {field} exceeds {MAX_DERIVED_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
