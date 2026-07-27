//! Provider-private captured-record and locator wire formats.

use chrono::{DateTime, Utc};

use crate::captured_batch::{CapturedSqliteValue, NativeLocator};
use crate::common::time::parse_rfc3339_utc;
use crate::{CaptureError, Result};

use super::cursor::deepagents_decode_u64;
use super::deepagents_captured_error;
use super::source::{
    DeepAgentsThread, DeepAgentsThreadSummary, DeepAgentsWriteCandidate, DeepAgentsWriteKey,
};

pub(super) struct DeepAgentsDecodedWrite {
    pub(super) row_number: u64,
    pub(super) key: DeepAgentsWriteKey,
    pub(super) occurred_at: Option<DateTime<Utc>>,
    pub(super) value_type: Option<String>,
    pub(super) value: Vec<u8>,
    pub(super) accepted_event_indices: Vec<u8>,
    pub(super) accepted_offsets: Vec<u8>,
}

pub(super) fn decode_deepagents_write_values(
    values: &[CapturedSqliteValue],
) -> Result<DeepAgentsDecodedWrite> {
    let [row_number, unknown_thread, thread_id, occurred_at, checkpoint_id, task_id, CapturedSqliteValue::Integer(idx), value_type, CapturedSqliteValue::Blob(value), CapturedSqliteValue::Blob(accepted_event_indices), CapturedSqliteValue::Blob(accepted_offsets)] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "Deep Agents write logical row has an invalid value shape",
        ));
    };
    let thread_id = deepagents_required_text(thread_id, "thread_id")?;
    let checkpoint_id = deepagents_required_text(checkpoint_id, "checkpoint_id")?;
    let task_id = deepagents_required_text(task_id, "task_id")?;
    let unknown_thread = deepagents_required_bool(unknown_thread, "unknown_thread")?;
    let occurred_at = if unknown_thread {
        None
    } else {
        Some(deepagents_required_time(occurred_at, "occurred_at")?)
    };
    Ok(DeepAgentsDecodedWrite {
        row_number: deepagents_required_u64(row_number, "row_number")?,
        key: DeepAgentsWriteKey {
            thread_id,
            checkpoint_id,
            task_id,
            idx: *idx,
        },
        occurred_at,
        value_type: deepagents_optional_text(value_type, "value_type")?,
        value: value.clone(),
        accepted_event_indices: accepted_event_indices.clone(),
        accepted_offsets: accepted_offsets.clone(),
    })
}

pub(super) fn decode_deepagents_thread_values(
    values: &[CapturedSqliteValue],
) -> Result<DeepAgentsThreadSummary> {
    let [thread_id, agent_name, created_at, updated_at, latest_checkpoint_id, git_branch, cwd] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "Deep Agents thread logical row has an invalid value shape",
        ));
    };
    let updated_at = deepagents_required_time(updated_at, "updated_at")?;
    Ok(DeepAgentsThreadSummary {
        thread: DeepAgentsThread {
            thread_id: deepagents_required_text(thread_id, "thread_id")?,
            agent_name: deepagents_optional_text(agent_name, "agent_name")?,
            created_at: deepagents_required_time(created_at, "created_at")?,
            updated_at,
            latest_checkpoint_id: deepagents_optional_text(
                latest_checkpoint_id,
                "latest_checkpoint_id",
            )?,
            git_branch: deepagents_optional_text(git_branch, "git_branch")?,
            cwd: deepagents_optional_text(cwd, "cwd")?,
        },
    })
}

pub(super) fn deepagents_write_values(
    ordinal: u64,
    candidate: &DeepAgentsWriteCandidate,
    occurred_at: Option<DateTime<Utc>>,
    value_type: Option<String>,
    value: Vec<u8>,
    accepted_event_indices: Vec<u8>,
    accepted_offsets: Vec<u8>,
) -> Result<Vec<CapturedSqliteValue>> {
    let key = candidate.key.as_ref().ok_or(CaptureError::SystemInvariant(
        "Deep Agents hydrated write is missing its preflighted key",
    ))?;
    let unknown_thread = occurred_at.is_none();
    let occurred_at = occurred_at.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    Ok(vec![
        CapturedSqliteValue::Blob(ordinal.saturating_add(1).to_be_bytes().to_vec()),
        CapturedSqliteValue::Integer(if unknown_thread { 1 } else { 0 }),
        CapturedSqliteValue::Text(key.thread_id.clone()),
        CapturedSqliteValue::Text(occurred_at.to_rfc3339()),
        CapturedSqliteValue::Text(key.checkpoint_id.clone()),
        CapturedSqliteValue::Text(key.task_id.clone()),
        CapturedSqliteValue::Integer(key.idx),
        deepagents_captured_optional_text(value_type),
        CapturedSqliteValue::Blob(value),
        CapturedSqliteValue::Blob(accepted_event_indices),
        CapturedSqliteValue::Blob(accepted_offsets),
    ])
}

pub(super) fn deepagents_thread_values(
    summary: &DeepAgentsThreadSummary,
) -> Vec<CapturedSqliteValue> {
    vec![
        CapturedSqliteValue::Text(summary.thread.thread_id.clone()),
        deepagents_captured_optional_text(summary.thread.agent_name.clone()),
        CapturedSqliteValue::Text(summary.thread.created_at.to_rfc3339()),
        CapturedSqliteValue::Text(summary.thread.updated_at.to_rfc3339()),
        deepagents_captured_optional_text(summary.thread.latest_checkpoint_id.clone()),
        deepagents_captured_optional_text(summary.thread.git_branch.clone()),
        deepagents_captured_optional_text(summary.thread.cwd.clone()),
    ]
}

pub(super) fn deepagents_locator(
    kind: &str,
    value: &impl serde::Serialize,
) -> Result<NativeLocator> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        CaptureError::InvalidPayload(format!("Deep Agents locator encoding failed: {error}"))
    })?;
    NativeLocator::new(kind, encoded).map_err(deepagents_captured_error)
}

pub(super) fn deepagents_encode_offsets(offsets: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(offsets.len().saturating_mul(4));
    for offset in offsets {
        bytes.extend_from_slice(&offset.to_be_bytes());
    }
    bytes
}

pub(super) fn deepagents_encode_event_indices(indices: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(indices.len().saturating_mul(8));
    for index in indices {
        bytes.extend_from_slice(&index.to_be_bytes());
    }
    bytes
}

pub(super) fn deepagents_decode_offsets(bytes: &[u8]) -> Result<Vec<u32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents accepted-offset payload has an invalid width".to_owned(),
        ));
    }
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let chunk: [u8; 4] = chunk.try_into().map_err(|_| {
                CaptureError::InvalidPayload(
                    "Deep Agents accepted-offset entry is invalid".to_owned(),
                )
            })?;
            Ok(u32::from_be_bytes(chunk))
        })
        .collect()
}

pub(super) fn deepagents_decode_event_indices(bytes: &[u8]) -> Result<Vec<u64>> {
    if !bytes.len().is_multiple_of(8) {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents accepted-event-index payload has an invalid width".to_owned(),
        ));
    }
    bytes
        .chunks_exact(8)
        .map(|chunk| {
            deepagents_decode_u64(chunk).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Deep Agents accepted-event-index entry is invalid".to_owned(),
                )
            })
        })
        .collect()
}

pub(super) fn deepagents_required_u64(value: &CapturedSqliteValue, field: &str) -> Result<u64> {
    let CapturedSqliteValue::Blob(bytes) = value else {
        return Err(CaptureError::InvalidPayload(format!(
            "Deep Agents logical {field} must be an eight-byte integer"
        )));
    };
    deepagents_decode_u64(bytes)
}

pub(super) fn deepagents_required_bool(value: &CapturedSqliteValue, field: &str) -> Result<bool> {
    match value {
        CapturedSqliteValue::Integer(0) => Ok(false),
        CapturedSqliteValue::Integer(1) => Ok(true),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Deep Agents logical {field} must be zero or one"
        ))),
    }
}

pub(super) fn deepagents_required_text(value: &CapturedSqliteValue, field: &str) -> Result<String> {
    match value {
        CapturedSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Deep Agents logical {field} must be text"
        ))),
    }
}

pub(super) fn deepagents_optional_text(
    value: &CapturedSqliteValue,
    field: &str,
) -> Result<Option<String>> {
    match value {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Deep Agents logical {field} must be text or null"
        ))),
    }
}

pub(super) fn deepagents_required_time(
    value: &CapturedSqliteValue,
    field: &str,
) -> Result<DateTime<Utc>> {
    let value = deepagents_required_text(value, field)?;
    parse_rfc3339_utc(&value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "Deep Agents logical {field} must be an RFC3339 timestamp"
        ))
    })
}

pub(super) fn deepagents_captured_optional_text(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}
