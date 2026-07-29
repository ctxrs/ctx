use std::{
    fs::{self, File, Metadata},
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

use crate::common::io::{
    ensure_regular_provider_transcript_file, read_provider_jsonl_line_or_skip_oversized,
    read_text_file_limited, ProviderJsonlLineRead,
};
use crate::provider::normalization::provider_local_preview;
use crate::{CaptureError, Result, PROVIDER_MAX_TEXT_CHARS};

pub(super) const KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES: usize = 16 * 1024 * 1024;
pub(super) const KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES: usize = 65_536;
const KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES_U64: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KimiSessionIndexEntry {
    pub(super) session_id: String,
    pub(super) session_dir: Option<String>,
    pub(super) work_dir: Option<String>,
}

impl KimiSessionIndexEntry {
    pub(super) fn metadata(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "session_dir": self.session_dir,
            "work_dir": self.work_dir,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KimiFrozenFileMetadata {
    pub(super) length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl KimiFrozenFileMetadata {
    // Direct reads remain a safety oracle for the admitted-handle path and its
    // cross-platform metadata revalidation tests.
    #[allow(dead_code)]
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    #[allow(dead_code)]
    fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                ensure_regular_provider_transcript_file(path)?;
                Self::from_metadata(&metadata).map(Some)
            }
            Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Kimi Code CLI auxiliary paths must be regular files",
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
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

    pub(super) fn revision_component(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        )
    }

    #[allow(dead_code)]
    fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    #[allow(dead_code)]
    fn revalidate_optional(expected: &Option<Self>, path: &Path) -> Result<bool> {
        match Self::read_optional(path) {
            Ok(current) => Ok(current == *expected),
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KimiWireRoute {
    source_root: PathBuf,
    state_path: PathBuf,
    index_path: PathBuf,
    agent_id: String,
    session_id: String,
}

impl KimiWireRoute {
    pub(super) fn parse(path: &Path) -> Result<Self> {
        if path.file_name().and_then(|name| name.to_str()) != Some("wire.jsonl") {
            return Err(invalid_kimi_wire_path(path));
        }
        let agent_dir = path.parent().ok_or_else(|| invalid_kimi_wire_path(path))?;
        let agent_id = agent_dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|agent_id| !agent_id.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid_kimi_wire_path(path))?;
        let agents_dir = agent_dir
            .parent()
            .ok_or_else(|| invalid_kimi_wire_path(path))?;
        if agents_dir.file_name().and_then(|name| name.to_str()) != Some("agents") {
            return Err(invalid_kimi_wire_path(path));
        }
        let session_dir = agents_dir
            .parent()
            .ok_or_else(|| invalid_kimi_wire_path(path))?
            .to_path_buf();
        let session_id = session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|session_id| !session_id.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid_kimi_wire_path(path))?;
        let work_dir_key = session_dir
            .parent()
            .ok_or_else(|| invalid_kimi_wire_path(path))?;
        if work_dir_key.file_name().is_none() {
            return Err(invalid_kimi_wire_path(path));
        }
        let sessions_dir = work_dir_key
            .parent()
            .ok_or_else(|| invalid_kimi_wire_path(path))?;
        if sessions_dir.file_name().and_then(|name| name.to_str()) != Some("sessions") {
            return Err(invalid_kimi_wire_path(path));
        }
        let root_dir = sessions_dir
            .parent()
            .ok_or_else(|| invalid_kimi_wire_path(path))?;
        Ok(Self {
            source_root: root_dir.to_path_buf(),
            state_path: session_dir.join("state.json"),
            index_path: root_dir.join("session_index.jsonl"),
            agent_id,
            session_id,
        })
    }

    pub(super) fn source_root(&self) -> &Path {
        &self.source_root
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KimiWireLayout {
    route: KimiWireRoute,
    canonical_wire_path: PathBuf,
    wire: KimiFrozenFileMetadata,
    state_file: Option<KimiFrozenFileMetadata>,
    state: Value,
    index_file: Option<KimiFrozenFileMetadata>,
    index_entry: Option<KimiSessionIndexEntry>,
}

impl KimiWireLayout {
    // Retain the path-based oracle for parity checks against production's
    // already-admitted file-handle construction.
    #[allow(dead_code)]
    pub(super) fn read(path: &Path) -> Result<Self> {
        let wire = KimiFrozenFileMetadata::read(path)?;
        let canonical_wire_path = fs::canonicalize(path)?;
        let route = KimiWireRoute::parse(&canonical_wire_path)?;
        let state_file = KimiFrozenFileMetadata::read_optional(&route.state_path)?;
        let state = match state_file.as_ref() {
            Some(file) => read_kimi_state(&route.state_path, file)?,
            None => Value::Null,
        };
        let index_file = KimiFrozenFileMetadata::read_optional(&route.index_path)?;
        let index_entry = match index_file.as_ref() {
            Some(file) => {
                read_kimi_session_index_entry(&route.index_path, file, &route.session_id)?
            }
            None => None,
        };
        Ok(Self {
            route,
            canonical_wire_path,
            wire,
            state_file,
            state,
            index_file,
            index_entry,
        })
    }

    pub(super) fn read_from_admitted(
        path: &Path,
        canonical_wire_path: PathBuf,
        wire_metadata: &Metadata,
        state: Option<(&Metadata, &[u8])>,
        index: Option<(&Metadata, &[u8])>,
    ) -> Result<Self> {
        KimiWireRoute::parse(path)?;
        let route = KimiWireRoute::parse(&canonical_wire_path)?;
        let wire = KimiFrozenFileMetadata::from_metadata(wire_metadata)?;
        let (state_file, state) = match state {
            Some((metadata, bytes)) => {
                let frozen = KimiFrozenFileMetadata::from_metadata(metadata)?;
                if frozen.length > KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES_U64 {
                    return Err(CaptureError::InvalidPayload(format!(
                        "Kimi Code CLI state.json exceeds the {KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES}-byte layout limit (observed {} bytes)",
                        frozen.length
                    )));
                }
                (
                    Some(frozen),
                    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null),
                )
            }
            None => (None, Value::Null),
        };
        let (index_file, index_entry) = match index {
            Some((metadata, bytes)) => {
                let frozen = KimiFrozenFileMetadata::from_metadata(metadata)?;
                if frozen.length > KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES_U64 {
                    return Err(kimi_index_bytes_error(frozen.length));
                }
                let entry = read_kimi_session_index_entry_from_reader(
                    BufReader::new(Cursor::new(bytes)),
                    &route.session_id,
                )?;
                (Some(frozen), entry)
            }
            None => (None, None),
        };
        Ok(Self {
            route,
            canonical_wire_path,
            wire,
            state_file,
            state,
            index_file,
            index_entry,
        })
    }

    pub(super) fn canonical_wire_path(&self) -> &Path {
        &self.canonical_wire_path
    }

    pub(super) fn wire(&self) -> &KimiFrozenFileMetadata {
        &self.wire
    }

    pub(super) fn agent_id(&self) -> &str {
        &self.route.agent_id
    }

    pub(super) fn session_id(&self) -> &str {
        &self.route.session_id
    }

    pub(super) fn take_state(&mut self) -> Value {
        std::mem::take(&mut self.state)
    }

    pub(super) fn take_index_entry(&mut self) -> Option<KimiSessionIndexEntry> {
        self.index_entry.take()
    }

    #[allow(dead_code)]
    pub(super) fn revalidate(&self, path: &Path) -> Result<bool> {
        let canonical_path = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let current_route = match KimiWireRoute::parse(&canonical_path) {
            Ok(route) => route,
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        if current_route != self.route
            || !self.wire.revalidate(path)?
            || canonical_path != self.canonical_wire_path
        {
            return Ok(false);
        }
        if !KimiFrozenFileMetadata::revalidate_optional(&self.state_file, &self.route.state_path)? {
            return Ok(false);
        }
        KimiFrozenFileMetadata::revalidate_optional(&self.index_file, &self.route.index_path)
    }
}

pub(super) fn complete_content_auxiliary_paths(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let route = KimiWireRoute::parse(path)?;
    Ok((route.state_path, route.index_path))
}

pub(super) fn canonical_source_root_for_wire(path: &Path) -> Result<PathBuf> {
    let routed_path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::path::absolute(path)?,
        Err(error) => return Err(error.into()),
    };
    let route = KimiWireRoute::parse(&routed_path)?;
    match fs::canonicalize(route.source_root()) {
        Ok(root) => Ok(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::path::absolute(route.source_root())?)
        }
        Err(error) => Err(error.into()),
    }
}

#[allow(dead_code)]
fn read_kimi_state(path: &Path, file: &KimiFrozenFileMetadata) -> Result<Value> {
    if file.length > KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES_U64 {
        return Err(CaptureError::InvalidPayload(format!(
            "Kimi Code CLI state.json exceeds the {KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES}-byte layout limit (observed {} bytes)",
            file.length
        )));
    }
    let raw = read_text_file_limited(
        path,
        KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES,
        "Kimi Code CLI state.json",
    )?;
    Ok(serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null))
}

#[allow(dead_code)]
fn read_kimi_session_index_entry(
    path: &Path,
    file: &KimiFrozenFileMetadata,
    expected_session_id: &str,
) -> Result<Option<KimiSessionIndexEntry>> {
    if file.length > KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES_U64 {
        return Err(kimi_index_bytes_error(file.length));
    }
    let file = File::open(path)?;
    let limited = file.take(KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES_U64.saturating_add(1));
    read_kimi_session_index_entry_from_reader(BufReader::new(limited), expected_session_id)
}

fn read_kimi_session_index_entry_from_reader<R: Read>(
    mut reader: BufReader<R>,
    expected_session_id: &str,
) -> Result<Option<KimiSessionIndexEntry>> {
    let mut line = Vec::new();
    let mut aggregate_bytes = 0_usize;
    let mut entries = 0_usize;
    let mut matching_entry = None;
    loop {
        let read = read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)?;
        let bytes = match read {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Line { bytes } | ProviderJsonlLineRead::Oversized { bytes } => {
                bytes
            }
        };
        aggregate_bytes = aggregate_bytes.saturating_add(bytes);
        if aggregate_bytes > KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES {
            return Err(kimi_index_bytes_error(aggregate_bytes as u64));
        }
        entries = entries.saturating_add(1);
        if entries > KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES {
            return Err(CaptureError::InvalidPayload(format!(
                "Kimi Code CLI session_index.jsonl exceeds the {KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES}-entry layout limit"
            )));
        }
        if matches!(read, ProviderJsonlLineRead::Oversized { .. })
            || line.iter().all(u8::is_ascii_whitespace)
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some(session_id) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(Value::as_str)
            .filter(|session_id| *session_id == expected_session_id)
        else {
            continue;
        };
        if matching_entry.is_none() {
            // The index is first-wins, but the scan must continue so a later global budget
            // violation rejects the entire read instead of returning a partial positive.
            matching_entry = Some(KimiSessionIndexEntry {
                session_id: session_id.to_owned(),
                session_dir: value
                    .get("sessionDir")
                    .or_else(|| value.get("session_dir"))
                    .and_then(Value::as_str)
                    .map(capped_kimi_text),
                work_dir: value
                    .get("workDir")
                    .or_else(|| value.get("work_dir"))
                    .and_then(Value::as_str)
                    .map(capped_kimi_text),
            });
        }
    }
    Ok(matching_entry)
}

fn invalid_kimi_wire_path(path: &Path) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Kimi Code CLI wire path must be <root>/sessions/<workDirKey>/<sessionId>/agents/<agentId>/wire.jsonl",
    }
}

fn kimi_index_bytes_error(observed_bytes: u64) -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "Kimi Code CLI session_index.jsonl exceeds the {KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES}-byte layout limit (observed {observed_bytes} bytes)"
    ))
}

fn capped_kimi_text(value: &str) -> String {
    provider_local_preview(value, PROVIDER_MAX_TEXT_CHARS).0
}
