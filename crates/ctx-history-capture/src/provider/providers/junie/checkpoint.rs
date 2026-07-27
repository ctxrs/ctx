use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::captured_batch::NativeLocator;
use crate::common::io::ensure_regular_provider_transcript_file;
use crate::provider::importer::BoundedParserCheckpoint;
use crate::{CaptureError, Result};

use super::{JUNIE_JSONL_LOCATOR_KIND, MAX_JUNIE_FAILURE_BYTES, MAX_JUNIE_PARSER_STATE_BYTES};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JunieCheckpointFailure {
    pub(super) line: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JunieParserCheckpoint {
    pub(super) next_ordinal: u64,
    pub(super) next_line_number: u64,
    pub(super) provider_event_index: u64,
    pub(super) started_at: DateTime<Utc>,
    pub(super) last_ts: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) title_anchor: Option<JunieMetadataAnchor>,
    pub(super) cwd_anchor: Option<JunieMetadataAnchor>,
    pub(super) saw_supported_event: bool,
    pub(super) metadata_dirty: bool,
    pub(super) source_ended: bool,
    pub(super) auxiliary_revision: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) structural_rejections: u64,
    pub(super) rejected_records: u64,
    pub(super) failures: Vec<JunieCheckpointFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JunieMetadataAnchor {
    pub(super) start: u64,
    pub(super) end: u64,
    sha256: [u8; 32],
}

pub(super) fn junie_metadata_anchor(
    locator: &NativeLocator,
    payload: &[u8],
) -> Option<JunieMetadataAnchor> {
    if locator.kind() != JUNIE_JSONL_LOCATOR_KIND {
        return None;
    }
    let value = locator.value();
    let source_len = value
        .get(..std::mem::size_of::<u32>())?
        .try_into()
        .ok()
        .map(u32::from_be_bytes)? as usize;
    let range_offset = std::mem::size_of::<u32>().checked_add(source_len)?;
    let expected_len = range_offset.checked_add(2 * std::mem::size_of::<u64>())?;
    if value.len() != expected_len {
        return None;
    }
    let start = u64::from_be_bytes(
        value
            .get(range_offset..range_offset + std::mem::size_of::<u64>())?
            .try_into()
            .ok()?,
    );
    let end = u64::from_be_bytes(
        value
            .get(range_offset + std::mem::size_of::<u64>()..expected_len)?
            .try_into()
            .ok()?,
    );
    let raw_len = end.checked_sub(start)?;
    let payload_len = u64::try_from(payload.len()).ok()?;
    if raw_len < payload_len || raw_len > payload_len.saturating_add(2) {
        return None;
    }
    Some(JunieMetadataAnchor {
        start,
        end,
        sha256: Sha256::digest(payload).into(),
    })
}

pub(super) fn junie_read_anchored_metadata(
    path: &Path,
    anchor: &JunieMetadataAnchor,
    expected_kind: &'static str,
    field: &'static str,
) -> Result<Option<String>> {
    ensure_regular_provider_transcript_file(path)?;
    let length = anchor.end.checked_sub(anchor.start).ok_or_else(|| {
        CaptureError::InvalidPayload("Junie metadata anchor range moved backwards".to_owned())
    })?;
    if length > crate::MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64 {
        return Err(CaptureError::InvalidPayload(
            "Junie metadata anchor exceeds the provider record limit".to_owned(),
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        CaptureError::InvalidPayload("Junie metadata anchor exceeds platform limits".to_owned())
    })?;
    let mut file = File::open(path)?;
    if anchor.end > file.metadata()?.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if <[u8; 32]>::from(Sha256::digest(&bytes)) != anchor.sha256 {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    let agent_event = value
        .get("event")
        .and_then(|event| event.get("agentEvent"))
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Junie metadata anchor does not contain an agent event".to_owned(),
            )
        })?;
    if agent_event.get("kind").and_then(Value::as_str) != Some(expected_kind) {
        return Err(CaptureError::InvalidPayload(
            "Junie metadata anchor has an unexpected event kind".to_owned(),
        ));
    }
    let value = agent_event
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Junie metadata anchor does not contain its expected value".to_owned(),
            )
        })?;
    Ok(Some(value.to_owned()))
}

pub(super) fn bounded_junie_failure(mut error: String) -> String {
    if error.len() <= MAX_JUNIE_FAILURE_BYTES {
        return error;
    }
    let mut boundary = MAX_JUNIE_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

pub(super) fn junie_parser_state_is_bounded(state: &JunieParserCheckpoint) -> bool {
    BoundedParserCheckpoint::from_serializable(state)
        .map(|checkpoint| checkpoint.as_bytes().len() <= MAX_JUNIE_PARSER_STATE_BYTES)
        .unwrap_or(false)
}
