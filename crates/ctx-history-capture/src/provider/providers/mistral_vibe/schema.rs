use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::common::io::read_text_file_limited;
use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::normalization::{
    provider_capped_json, provider_explicit_result_value_text, provider_local_preview,
    provider_value_text,
};
use crate::{
    CaptureError, ProviderImportSummary, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
    PROVIDER_MAX_PREVIEW_CHARS,
};

use super::source::MistralVibeSessionSource;
use super::MISTRAL_VIBE_MAX_ID_BYTES;

pub(super) fn mistral_vibe_bounded_metadata(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
) -> Result<(Value, Option<String>)> {
    let mut summary = ProviderImportSummary::default();
    let metadata = read_mistral_vibe_metadata(&source.metadata_path, &mut summary);
    mistral_vibe_bounded_metadata_from_value(source, imported_at, metadata, summary)
}

pub(super) fn mistral_vibe_bounded_metadata_from_bytes(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
    bytes: &[u8],
) -> Result<(Value, Option<String>)> {
    let mut summary = ProviderImportSummary::default();
    let metadata = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) if value.is_object() => value,
        Ok(_) => {
            push_provider_import_failure(
                &mut summary,
                0,
                "Mistral Vibe meta.json must contain a JSON object".to_owned(),
            );
            Value::Null
        }
        Err(error) => {
            push_provider_import_failure(
                &mut summary,
                0,
                format!("invalid Mistral Vibe meta.json: {error}"),
            );
            Value::Null
        }
    };
    mistral_vibe_bounded_metadata_from_value(source, imported_at, metadata, summary)
}

fn mistral_vibe_bounded_metadata_from_value(
    source: &MistralVibeSessionSource,
    imported_at: DateTime<Utc>,
    metadata: Value,
    summary: ProviderImportSummary,
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
    let ended_at = mistral_vibe_metadata_timestamp(&metadata, "end_time")
        .map(|timestamp| timestamp.to_rfc3339());
    let bounded_text = |value: Option<String>| {
        value.map(|text| provider_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS).0)
    };
    let external_agent_id = bounded_text(mistral_vibe_metadata_pointer_string(
        &metadata,
        &["/agent_profile/name"],
    ));
    let metadata_failure = summary
        .failures
        .first()
        .map(|failure| provider_local_preview(&failure.error, PROVIDER_MAX_PREVIEW_CHARS).0);
    Ok((
        json!({
            "session_id": provider_session_id,
            "parent_session_id": parent_provider_session_id,
            "start_time": started_at,
            "end_time": ended_at,
            "title": bounded_text(mistral_vibe_metadata_string(&metadata, "title")),
            "title_source": bounded_text(mistral_vibe_metadata_string(&metadata, "title_source")),
            "git_branch": bounded_text(mistral_vibe_metadata_string(&metadata, "git_branch")),
            "git_commit": bounded_text(mistral_vibe_metadata_string(&metadata, "git_commit")),
            "total_messages": metadata.get("total_messages").and_then(Value::as_u64),
            "environment": {
                "working_directory": bounded_text(mistral_vibe_metadata_pointer_string(
                    &metadata,
                    &["/environment/working_directory"],
                )),
            },
            "agent_profile": {
                "name": external_agent_id,
                "preview": metadata.get("agent_profile").map(|value| {
                    provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
                }),
            },
            "stats": metadata.get("stats").map(|value| {
                provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
            }),
            "loops": metadata.get("loops").map(|value| {
                provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
            }),
            "experiments": metadata.get("experiments").map(|value| {
                provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
            }),
        }),
        metadata_failure,
    ))
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

/// Returns only fields that explicitly carry a Mistral Vibe tool result.
pub(crate) fn mistral_vibe_result_content(value: &Value) -> Option<String> {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if mistral_vibe_event_type(role, value) != EventType::ToolOutput {
        return None;
    }
    let mut parts = Vec::new();
    for field in ["content", "reasoning_content", "images"] {
        if let Some(text) = value
            .get(field)
            .and_then(provider_explicit_result_value_text)
        {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(super) fn mistral_vibe_tool_calls_text(value: &Value) -> Option<String> {
    let calls = value.as_array()?;
    let names = calls
        .iter()
        .filter_map(|call| {
            call.pointer("/function/name")
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
        })
        .collect::<Vec<_>>();
    if names.is_empty() {
        Some(provider_value_text(value)?)
    } else {
        Some(format!("tool calls: {}", names.join(", ")))
    }
}

pub(super) fn mistral_vibe_event_id(value: &Value, line_number: usize, role: &str) -> String {
    value
        .get("message_id")
        .or_else(|| value.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{role}:line-{line_number}"))
}

pub(super) fn read_mistral_vibe_metadata(
    path: &Path,
    summary: &mut ProviderImportSummary,
) -> Value {
    match read_text_file_limited(
        path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Mistral Vibe meta.json",
    ) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                push_provider_import_failure(
                    summary,
                    0,
                    "Mistral Vibe meta.json must contain a JSON object".to_owned(),
                );
                Value::Null
            }
            Err(err) => {
                push_provider_import_failure(
                    summary,
                    0,
                    format!("invalid Mistral Vibe meta.json: {err}"),
                );
                Value::Null
            }
        },
        Err(err) => {
            push_provider_import_failure(
                summary,
                0,
                format!("could not read Mistral Vibe meta.json: {err}"),
            );
            Value::Null
        }
    }
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
