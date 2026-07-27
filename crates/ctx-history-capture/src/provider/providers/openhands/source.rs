use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
        path_has_component,
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
}

impl OpenHandsObservedFile {
    pub(super) fn open(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let canonical_path = fs::canonicalize(path)?;
        let canonical_path_text = openhands_checked_path_text(&canonical_path)?;
        let session_id = openhands_conversation_id_from_path(&canonical_path)
            .ok_or_else(|| openhands_missing_event_files(path))
            .and_then(|value| openhands_bounded_derived_text(value, "conversation id"))?;
        let user_id = openhands_user_id_from_path(&canonical_path)
            .map(|value| openhands_bounded_derived_text(value, "user id"))
            .transpose()?;
        let conversation_dir = canonical_path
            .parent()
            .ok_or_else(|| openhands_missing_event_files(path))?
            .to_path_buf();
        let mut file = open_openhands_source_file(&canonical_path)?;
        let descriptor_observation = OpenHandsFileObservation::from_metadata(&file.metadata()?)?;
        let path_observation =
            OpenHandsFileObservation::from_metadata(&fs::metadata(&canonical_path)?)?;
        if descriptor_observation != path_observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }

        let limit = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).map_err(|_| {
            CaptureError::SystemInvariant("OpenHands record byte limit exceeds u64")
        })?;
        let raw_bytes = if descriptor_observation.length > limit {
            None
        } else {
            let capacity = usize::try_from(descriptor_observation.length).map_err(|_| {
                CaptureError::SystemInvariant("OpenHands file length exceeds platform limits")
            })?;
            let mut bytes = Vec::with_capacity(capacity);
            file.by_ref()
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if u64::try_from(bytes.len()).ok() != Some(descriptor_observation.length)
                || bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Some(bytes)
        };
        let after_descriptor = OpenHandsFileObservation::from_metadata(&file.metadata()?)?;
        let after_path = OpenHandsFileObservation::from_metadata(&fs::metadata(&canonical_path)?)?;
        if after_descriptor != descriptor_observation || after_path != descriptor_observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
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
        })
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        match fs::metadata(&self.canonical_path) {
            Ok(metadata) => {
                Ok(OpenHandsFileObservation::from_metadata(&metadata)? == self.observation)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn source_revision(&self, inventory_token: Option<&str>) -> String {
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

#[cfg(test)]
std::thread_local! {
    static OPENHANDS_SOURCE_FILE_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn open_openhands_source_file(path: &Path) -> Result<File> {
    #[cfg(test)]
    OPENHANDS_SOURCE_FILE_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    Ok(File::open(path)?)
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
}

pub(super) fn discover_openhands_event_paths(root: &Path) -> Result<OpenHandsInventory> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "symlinked provider transcript roots are rejected",
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(OpenHandsInventory {
                paths: Vec::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    let mut paths = Vec::new();
    let mut visited_entries = 0_usize;
    discover_at(root, 0, &mut visited_entries, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(OpenHandsInventory {
        paths,
        root_missing: false,
    })
}

fn discover_at(
    path: &Path,
    depth: usize,
    visited_entries: &mut usize,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > OPENHANDS_DISCOVERY_MAX_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands event directory nesting exceeds the supported limit",
        });
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked OpenHands event entries are rejected",
        });
    }
    if metadata.is_file() {
        if openhands_json_path_is_event(path) {
            ensure_regular_provider_transcript_file(path)?;
            paths.push(fs::canonicalize(path)?);
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut children = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    *visited_entries =
        visited_entries
            .checked_add(children.len())
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands discovery entry count overflowed",
            ))?;
    if *visited_entries > OPENHANDS_DISCOVERY_MAX_ENTRIES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands event discovery exceeds the supported entry limit",
        });
    }
    children.sort();
    for child in children {
        discover_at(&child, depth.saturating_add(1), visited_entries, paths)?;
    }
    Ok(())
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
