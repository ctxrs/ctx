use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::provider_value_text;
use crate::{CaptureError, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::source::MistralVibeSessionSource;
use super::MISTRAL_VIBE_MAX_ID_BYTES;

pub(super) fn mistral_vibe_bounded_metadata_from_bytes(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
    bytes: &[u8],
) -> Result<(Value, Option<String>)> {
    let (metadata, failure) = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) if value.is_object() => (value, None),
        Ok(_) => (
            Value::Null,
            Some("Mistral Vibe meta.json must contain a JSON object".to_owned()),
        ),
        Err(error) => (
            Value::Null,
            Some(bounded_metadata_text(format!(
                "invalid Mistral Vibe meta.json: {error}"
            ))),
        ),
    };
    mistral_vibe_bounded_metadata_from_value(source, imported_at, metadata, failure)
}

fn mistral_vibe_bounded_metadata_from_value(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
    metadata: Value,
    failure: Option<String>,
) -> Result<(Value, Option<String>)> {
    let provider_session_id = bounded_mistral_vibe_identity(
        mistral_vibe_metadata_string(&metadata, "session_id").or_else(|| {
            source
                .session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        }),
        &source.session_dir,
        "session id",
    )?
    .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
        path: source.session_dir.clone(),
        reason: "Mistral Vibe session directory is missing a session id",
    })?;
    let parent_provider_session_id = bounded_mistral_vibe_identity(
        mistral_vibe_metadata_string(&metadata, "parent_session_id"),
        &source.metadata_path,
        "parent session id",
    )?;
    let started_at = mistral_vibe_metadata_timestamp(&metadata, "start_time")
        .unwrap_or(imported_at)
        .to_rfc3339();
    Ok((
        json!({
            "session_id": provider_session_id,
            "parent_session_id": parent_provider_session_id,
            "start_time": started_at,
            "git_branch": mistral_vibe_metadata_string(&metadata, "git_branch")
                .map(bounded_metadata_text),
            "environment": {
                "working_directory": mistral_vibe_metadata_pointer_string(
                    &metadata,
                    &["/environment/working_directory"],
                )
                .map(bounded_metadata_text),
            },
        }),
        failure,
    ))
}

fn bounded_metadata_text(text: impl Into<String>) -> String {
    text.into()
        .chars()
        .take(PROVIDER_MAX_PREVIEW_CHARS)
        .collect()
}

fn bounded_mistral_vibe_identity(
    value: Option<String>,
    path: &Path,
    label: &'static str,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > MISTRAL_VIBE_MAX_ID_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: match label {
                "session id" => "Mistral Vibe session id exceeds the supported size",
                _ => "Mistral Vibe parent session id exceeds the supported size",
            },
        });
    }
    Ok(Some(value))
}

pub(super) fn mistral_vibe_event_type(role: &str, value: &Value) -> EventType {
    if role == "tool" || value.get("tool_call_id").is_some() {
        EventType::ToolOutput
    } else if value
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
    {
        EventType::ToolCall
    } else if role == "system" {
        EventType::Notice
    } else {
        EventType::Message
    }
}

pub(super) fn mistral_vibe_event_text(role: &str, value: &Value, event_type: EventType) -> String {
    let mut parts = Vec::new();
    if let Some(content) = value.get("content").and_then(provider_value_text) {
        parts.push(content);
    }
    if let Some(reasoning) = value.get("reasoning_content").and_then(provider_value_text) {
        parts.push(reasoning);
    }
    if let Some(tool_calls) = value
        .get("tool_calls")
        .and_then(mistral_vibe_tool_calls_text)
    {
        parts.push(tool_calls);
    }
    if let Some(images) = value.get("images").and_then(provider_value_text) {
        parts.push(images);
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }
    match event_type {
        EventType::ToolOutput => format!("Mistral Vibe {role} output"),
        EventType::ToolCall => format!("Mistral Vibe {role} tool call"),
        _ => format!("Mistral Vibe {role} message"),
    }
}

pub(super) fn mistral_vibe_tool_calls_text(value: &Value) -> Option<String> {
    value.as_array()?;
    serde_json::to_string(value).ok()
}

pub(super) fn mistral_vibe_metadata_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn mistral_vibe_metadata_pointer_string(
    value: &Value,
    pointers: &[&str],
) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(super) fn mistral_vibe_metadata_timestamp(value: &Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}
