use std::{
    fs,
    fs::{File, Metadata},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::{cell::Cell, io::BufReader};

#[cfg(test)]
use serde_json::Value;

use crate::common::io::ensure_regular_provider_transcript_file;
#[cfg(test)]
use crate::common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead};
#[cfg(test)]
use crate::provider::codex::session::filter::should_parse_codex_session_line;
#[cfg(test)]
use crate::provider::codex::session::header::codex_session_header;
use crate::provider_sources::open_ordinary_file_without_following;
use crate::{CaptureError, Result};

use super::{CODEX_CAPTURE_REVISION, CODEX_POLICY_REVISION};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodexFrozenFileMetadata {
    pub(super) length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl CodexFrozenFileMetadata {
    pub(super) fn read(path: &Path) -> Result<Self> {
        let file = open_codex_file_without_symlinks(path)?;
        Self::from_metadata(&file.metadata()?)
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

    pub(super) fn source_revision(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "codex-jsonl-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={};capture={CODEX_CAPTURE_REVISION};policy={CODEX_POLICY_REVISION}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
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

fn open_codex_file_without_symlinks(path: &Path) -> Result<File> {
    match open_ordinary_file_without_following(path) {
        Ok(file) => Ok(file),
        Err(original @ CaptureError::InvalidProviderTranscriptPath { .. }) => {
            ensure_regular_provider_transcript_file(path)?;
            Err(original)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
std::thread_local! {
    static CODEX_SOURCE_FILE_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(super) fn open_codex_source_file(path: &Path) -> Result<File> {
    #[cfg(test)]
    CODEX_SOURCE_FILE_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    open_codex_file_without_symlinks(path)
}

pub(super) fn canonical_codex_source_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => return Ok(fs::canonicalize(path)?),
        }
    }
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Ok(fs::canonicalize(path)?)
    }
}

#[cfg(test)]
pub(super) fn count_codex_source_file_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    CODEX_SOURCE_FILE_OPEN_COUNT.with(|count| {
        assert_eq!(count.replace(Some(0)), None);
    });
    let output = operation();
    let opens = CODEX_SOURCE_FILE_OPEN_COUNT.with(|count| count.replace(None).unwrap());
    (output, opens)
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexSessionConversationScan {
    pub(crate) has_real_conversation: bool,
    pub(crate) has_malformed_header: bool,
    pub(crate) has_malformed_relevant_line: bool,
    pub(crate) oversized_required_header: bool,
    pub(crate) oversized_events: usize,
}

#[cfg(test)]
pub(crate) fn codex_session_file_conversation_scan(
    path: &Path,
) -> Result<CodexSessionConversationScan> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut scan = CodexSessionConversationScan::default();
    let mut header_seen = false;
    loop {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Line { .. } => {}
            ProviderJsonlLineRead::Oversized { .. } => {
                if header_seen {
                    scan.oversized_events = scan.oversized_events.saturating_add(1);
                    continue;
                }
                scan.oversized_required_header = true;
                return Ok(scan);
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => value,
            Err(_) if should_parse_codex_session_line(&line) => {
                scan.has_malformed_relevant_line = true;
                return Ok(scan);
            }
            Err(_) => continue,
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if codex_session_header(value.clone()).is_ok() {
                header_seen = true;
            } else {
                scan.has_malformed_header = true;
                return Ok(scan);
            }
        }
        let Some(payload) = value
            .get("payload")
            .filter(|_| value.get("type").and_then(Value::as_str) == Some("response_item"))
        else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(role, "user" | "assistant" | "system" | "developer") {
            continue;
        }
        if payload
            .get("content")
            .and_then(crate::provider::codex::events::codex_content_text)
            .is_some_and(|text| !text.trim().is_empty())
        {
            scan.has_real_conversation = true;
            return Ok(scan);
        }
    }
    Ok(scan)
}
