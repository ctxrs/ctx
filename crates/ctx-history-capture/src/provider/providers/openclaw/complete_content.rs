use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use serde_json::Value;

use crate::{
    captured_batch::CapturedRecord,
    complete_content::jsonl::ExactJsonlSourceBinding,
    provider::normalization::{
        provider_capped_json, provider_explicit_result_value_text, provider_role,
        provider_value_text,
    },
    Result, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    normalization, openclaw_index_revision, openclaw_session_index_for_file,
    OpenClawFrozenFileMetadata, OpenClawSessionObservation,
};

pub(crate) fn source_from_admitted(
    path: &Path,
    transcript_metadata: &Metadata,
    index: Option<(&Metadata, &[u8])>,
    path_identity: String,
) -> Result<(String, String)> {
    let transcript = OpenClawFrozenFileMetadata::from_metadata(transcript_metadata)?;
    let (index_file, index) = match index {
        Some((metadata, bytes)) => {
            let parsed = std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .map(|value| openclaw_session_index_for_file(path, &value))
                .unwrap_or(Value::Null);
            (
                Some(OpenClawFrozenFileMetadata::from_metadata(metadata)?),
                provider_capped_json(&parsed, PROVIDER_MAX_PREVIEW_CHARS),
            )
        }
        None => (None, Value::Null),
    };
    let observation = OpenClawSessionObservation {
        canonical_path: PathBuf::new(),
        transcript,
        index_file,
        index_revision: openclaw_index_revision(&index)?,
        index,
    };
    Ok((observation.source_revision(), path_identity))
}

pub(super) fn event_with_locators(
    provider_session_id: &str,
    event_index: u64,
    line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
    record: &CapturedRecord,
    binding: &ExactJsonlSourceBinding,
) -> Result<ProviderEventEnvelope> {
    let mut event = normalization::event(
        provider_session_id,
        event_index,
        line_number,
        row,
        occurred_at,
    );
    crate::complete_content::jsonl::attach_exact_jsonl_complete_content_locator(
        &mut event,
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        row,
        record,
        line_number,
        binding,
    )?;
    if let Some((content, native_record_id)) = crate::complete_content::jsonl::result_content_and_id(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        row,
        line_number,
    ) {
        crate::complete_content::jsonl::attach_exact_jsonl_result_content_locator(
            &mut event,
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &content,
            &native_record_id,
            record,
            binding,
        )?;
    }
    Ok(event)
}

pub(crate) fn message_record(value: &Value, line_number: usize) -> Option<(String, String)> {
    let row_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let message = value.get("message").unwrap_or(value);
    let role = message
        .get("role")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" if role != Some(EventRole::Tool) => EventType::Message,
        "message" => EventType::ToolOutput,
        _ => EventType::Notice,
    };
    (event_type == EventType::Message).then(|| {
        let native_record_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{line_number}"));
        let text = message
            .get("content")
            .or_else(|| message.get("text"))
            .or_else(|| message.get("output"))
            .and_then(provider_value_text)
            .unwrap_or_default();
        (text, native_record_id)
    })
}

/// Extracts explicit output from an OpenClaw legacy JSONL tool message.
pub(crate) fn result_content(row: &Value) -> Option<String> {
    if row.get("type").and_then(Value::as_str).unwrap_or("message") != "message" {
        return None;
    }
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    if role != Some(EventRole::Tool) {
        return None;
    }
    message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_explicit_result_value_text)
}
