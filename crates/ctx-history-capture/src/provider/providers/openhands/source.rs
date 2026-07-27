use std::{
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::CaptureProvider;
use sha2::{Digest, Sha256};

use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, CapturedRecord, NativeLocator, NativePosition,
    ProviderRecordKind, SourceObservation, StructuralRejectionKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    path_has_component,
};
use crate::provider::importer::{provider_path_identity, provider_source_cursor_stream_for_path};
use crate::{fnv1a64, CaptureError, Result, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT};

use super::{
    openhands_bounded_derived_text, OPENHANDS_CAPTURE_REVISION, OPENHANDS_INVENTORY_MIN_INTERVAL,
    OPENHANDS_INVENTORY_PAGE_RECORDS, OPENHANDS_LOCATOR_KIND, OPENHANDS_MAX_PATH_BYTES,
    OPENHANDS_POLICY_REVISION, OPENHANDS_POSITION_KIND,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenHandsFrozenFile {
    pub(super) length: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
}

impl OpenHandsFrozenFile {
    pub(super) fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    pub(super) fn source_revision(&self, content_hash: Option<&[u8; 32]>) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "openhands-event-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={};sha256={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            content_hash.map_or_else(
                || "oversize".to_owned(),
                |hash| openhands_hex(hash),
            ),
        )
    }

    pub(super) fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(super) struct OpenHandsEventSource {
    pub(super) canonical_path: PathBuf,
    pub(super) canonical_path_text: String,
    pub(super) conversation_dir: PathBuf,
    pub(super) session_id: String,
    pub(super) frozen: OpenHandsFrozenFile,
    pub(super) raw_bytes: Option<Vec<u8>>,
    pub(super) path_identity: String,
    pub(super) observation: SourceObservation,
}

impl OpenHandsEventSource {
    pub(super) fn observe(path: &Path, inventory_observation_token: Option<&str>) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let canonical_path = fs::canonicalize(path)?;
        let canonical_path_text = openhands_checked_path_text(&canonical_path)?;
        let session_id = openhands_conversation_id_from_path(&canonical_path)
            .ok_or_else(|| openhands_missing_event_files(path))
            .and_then(|value| openhands_bounded_derived_text(value, "conversation id"))?;
        let conversation_dir = canonical_path
            .parent()
            .ok_or_else(|| openhands_missing_event_files(path))?
            .to_path_buf();
        let frozen = OpenHandsFrozenFile::read(&canonical_path)?;
        let raw_bytes = if frozen.length > openhands_oversize_limit()? {
            None
        } else {
            Some(read_openhands_frozen_bytes(&canonical_path, &frozen)?)
        };
        let content_hash = raw_bytes
            .as_deref()
            .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
        let path_identity = provider_path_identity(&canonical_path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenHands,
            OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            &path_identity,
        );
        let observation = SourceObservation::new(
            CaptureProvider::OpenHands,
            OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            format!("openhands-event-file:{path_identity}"),
            frozen.source_revision(content_hash.as_ref()),
            cursor_stream,
            OPENHANDS_CAPTURE_REVISION,
            OPENHANDS_POLICY_REVISION,
            inventory_observation_token,
        )
        .map_err(openhands_captured_error)?;
        Ok(Self {
            canonical_path,
            canonical_path_text,
            conversation_dir,
            session_id,
            frozen,
            raw_bytes,
            path_identity,
            observation,
        })
    }
}

struct OpenHandsInventoryPacer {
    entries: usize,
    window_started: Instant,
}

impl OpenHandsInventoryPacer {
    fn new() -> Self {
        Self {
            entries: 0,
            window_started: Instant::now(),
        }
    }

    fn observe(&mut self) {
        self.entries = self.entries.saturating_add(1);
        if self.entries < OPENHANDS_INVENTORY_PAGE_RECORDS {
            return;
        }
        let elapsed = self.window_started.elapsed();
        if elapsed < OPENHANDS_INVENTORY_MIN_INTERVAL {
            thread::sleep(OPENHANDS_INVENTORY_MIN_INTERVAL - elapsed);
        }
        self.entries = 0;
        self.window_started = Instant::now();
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

pub(super) fn capture_openhands_event_batch(
    path: &Path,
    path_text: &str,
    frozen: &OpenHandsFrozenFile,
    raw_bytes: Option<Vec<u8>>,
    source: SourceObservation,
    record_kind: ProviderRecordKind,
) -> Result<CapturedBatch> {
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let locator = NativeLocator::new(OPENHANDS_LOCATOR_KIND, path_text.as_bytes().to_vec())
        .map_err(openhands_captured_error)?;
    let line_number = openhands_line_number(path);
    let ordinal = u64::try_from(line_number.saturating_sub(1))
        .map_err(|_| CaptureError::SystemInvariant("OpenHands source citation exceeds u64"))?;
    let record = if frozen.length > openhands_oversize_limit()? {
        CapturedRecord::structural_rejection(
            ordinal,
            locator,
            record_kind,
            StructuralRejectionKind::OversizeRecord,
            frozen.length,
        )
    } else {
        let bytes = raw_bytes.ok_or(CaptureError::SystemInvariant(
            "OpenHands bounded event bytes were not retained for projection",
        ))?;
        CapturedRecord::content(ordinal, locator, record_kind, bytes)
            .map_err(openhands_captured_error)?
    };
    let mut builder = CapturedBatchBuilder::new(source, openhands_position(0)?);
    builder.push(record).map_err(openhands_captured_error)?;
    builder.mark_source_exhausted();
    builder
        .finish(openhands_position(1)?)
        .map_err(openhands_captured_error)
}

pub(super) fn openhands_position(position: u64) -> Result<NativePosition> {
    NativePosition::new(OPENHANDS_POSITION_KIND, position.to_be_bytes().to_vec())
        .map_err(openhands_captured_error)
}

pub(super) fn decode_openhands_position(position: &NativePosition) -> Result<u64> {
    if position.kind() != OPENHANDS_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "OpenHands cursor has an unexpected position kind".to_owned(),
        ));
    }
    let bytes: [u8; 8] = position.value().try_into().map_err(|_| {
        CaptureError::InvalidPayload("OpenHands cursor position is malformed".to_owned())
    })?;
    let decoded = u64::from_be_bytes(bytes);
    if decoded > 1 {
        return Err(CaptureError::InvalidPayload(
            "OpenHands cursor position is outside its event-file boundary".to_owned(),
        ));
    }
    Ok(decoded)
}

pub(super) fn visit_openhands_event_paths(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<()> {
    visit_openhands_event_paths_with_pacer(root, &mut OpenHandsInventoryPacer::new(), visit)
}

fn visit_openhands_event_paths_with_pacer(
    root: &Path,
    pacer: &mut OpenHandsInventoryPacer,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        if openhands_json_path_is_event(root) {
            ensure_regular_provider_transcript_file(root)?;
            visit(root)?;
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        pacer.observe();
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visit_openhands_event_paths_with_pacer(&path, pacer, visit)?;
        } else if file_type.is_file() && openhands_json_path_is_event(&path) {
            ensure_regular_provider_transcript_file(&path)?;
            visit(&path)?;
        }
    }
    Ok(())
}

pub(super) fn read_openhands_frozen_bytes(
    path: &Path,
    frozen: &OpenHandsFrozenFile,
) -> Result<Vec<u8>> {
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let maximum = openhands_oversize_limit()?;
    let read_limit = maximum.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "OpenHands record read limit overflowed",
    ))?;
    let capacity = usize::try_from(frozen.length.min(read_limit)).map_err(|_| {
        CaptureError::SystemInvariant("OpenHands file length exceeds platform limits")
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    open_openhands_source_file(path)?
        .take(read_limit)
        .read_to_end(&mut bytes)?;
    if !frozen.revalidate(path)? || u64::try_from(bytes.len()).ok() != Some(frozen.length) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if bytes.len() > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(bytes)
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

fn openhands_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("OpenHands byte limit exceeds u64"))
}

pub(super) fn openhands_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn openhands_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
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
