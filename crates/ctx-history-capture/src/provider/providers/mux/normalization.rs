use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::provider::normalization::{
    provider_capped_json, provider_capped_json_value, provider_local_preview, provider_policy_body,
    provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_role, provider_value_text,
};
use crate::{MUX_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

use super::metadata::{mux_string_pointer, mux_value_timestamp};

#[derive(Debug, Clone)]
pub(super) struct MuxMessageRow {
    pub(super) line_number: usize,
    pub(super) source_path: PathBuf,
    pub(super) value: Value,
    pub(super) is_partial: bool,
}

#[derive(Debug)]
pub(super) struct MuxCoreEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MuxOutputOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MuxOutputProjection {
    pub(super) body_available: bool,
    pub(super) call_ids: Vec<String>,
    pub(super) tool_names: Vec<String>,
    pub(super) outcome: MuxOutputOutcome,
    pub(super) exit_code: Option<i32>,
}

pub(super) fn mux_core_event(
    event_index: u64,
    row: &MuxMessageRow,
    occurred_at: DateTime<Utc>,
    model: Option<&str>,
) -> MuxCoreEvent {
    let role = row
        .value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = mux_event_type(&row.value);
    let model_value = model
        .map(str::to_owned)
        .or_else(|| mux_message_model(&row.value));
    let provider_event_hash = mux_event_id(&row.value, row.line_number, role, row.is_partial);
    let cursor = format!("{}:line:{}", row.source_path.display(), row.line_number);
    let text = mux_event_text(&row.value, event_type);
    let body = row.value.clone();
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    MuxCoreEvent {
        provider_event_index: event_index,
        provider_event_hash,
        cursor,
        event_type,
        role: Some(provider_role(Some(role))),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": MUX_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": MUX_SOURCE_FORMAT,
            "source_format": MUX_SOURCE_FORMAT,
            "line": row.line_number,
            "is_partial": row.is_partial,
            "role": role,
            "message_id": row.value.get("id").and_then(Value::as_str),
            "workspace_id": row.value.get("workspaceId").and_then(Value::as_str),
            "history_sequence": mux_history_sequence(&row.value),
            "model": model_value,
            "usage": row.value.pointer("/metadata/usage").map(|usage| provider_capped_json_value(usage, PROVIDER_MAX_PREVIEW_CHARS)),
            "provider_metadata": row.value.pointer("/metadata/providerMetadata").map(|metadata| provider_capped_json_value(metadata, PROVIDER_MAX_PREVIEW_CHARS)),
            "mux_metadata": row.value.pointer("/metadata/muxMetadata").map(|metadata| provider_capped_json_value(metadata, PROVIDER_MAX_PREVIEW_CHARS)),
            "partial": row.value.pointer("/metadata/partial").and_then(Value::as_bool),
        }),
    }
}

pub(crate) fn mux_event_type(value: &Value) -> EventType {
    if mux_is_summary_message(value) {
        return EventType::Summary;
    }
    if value.get("role").and_then(Value::as_str) == Some("system") {
        return EventType::Notice;
    }
    let mut saw_tool_call = false;
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("dynamic-tool") {
                continue;
            }
            let state = part.get("state").and_then(Value::as_str);
            if matches!(state, Some("output-available" | "output-redacted"))
                || part.get("output").is_some()
            {
                return EventType::ToolOutput;
            }
            saw_tool_call = true;
        }
    }
    if saw_tool_call {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

fn mux_is_summary_message(value: &Value) -> bool {
    value
        .pointer("/metadata/compacted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .pointer("/metadata/compactionBoundary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || value.pointer("/metadata/contextBoundaryKind").is_some()
        || value
            .pointer("/metadata/muxMetadata/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.contains("compaction") || kind.contains("summary"))
}

pub(crate) fn mux_event_text(value: &Value, event_type: EventType) -> String {
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(mux_tool_part_text(part)),
                Some("file") => {
                    if let Some(text) = mux_file_part_text(part) {
                        rendered.push(text);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return rendered.join("\n");
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return text;
    }
    match event_type {
        EventType::ToolOutput => "Mux tool output".to_owned(),
        EventType::ToolCall => "Mux tool call".to_owned(),
        EventType::Summary => "Mux summary".to_owned(),
        EventType::Notice => "Mux notice".to_owned(),
        _ => "Mux message".to_owned(),
    }
}

fn mux_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push('\n');
        text.push_str("input: ");
        text.push_str(&mux_value_preview(input));
    }
    if let Some(output) = part.get("output") {
        text.push('\n');
        text.push_str("output: ");
        text.push_str(&mux_value_preview(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push('\n');
            text.push_str("nested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn mux_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

fn mux_value_preview(value: &Value) -> String {
    let raw = provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string());
    provider_local_preview(&raw, PROVIDER_MAX_PREVIEW_CHARS).0
}

pub(crate) fn mux_event_id(
    value: &Value,
    line_number: usize,
    role: &str,
    is_partial: bool,
) -> String {
    let prefix = if is_partial { "partial:" } else { "" };
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("{prefix}{id}"))
        .or_else(|| {
            mux_history_sequence(value)
                .map(|sequence| format!("{prefix}historySequence:{sequence}"))
        })
        .unwrap_or_else(|| format!("{prefix}{role}:line-{line_number}"))
}

/// Exact normalized result body for a Mux dynamic-tool record.
///
/// A record containing any redacted output is deliberately ineligible. A
/// single result preserves its string bytes or canonical JSON serialization;
/// multiple results use a JSON array so their boundaries cannot be confused.
pub(crate) fn mux_result_content(value: &Value) -> Option<String> {
    let parts = value.get("parts")?.as_array()?;
    let mut outputs = Vec::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("dynamic-tool") {
            continue;
        }
        if part.get("state").and_then(Value::as_str) == Some("output-redacted") {
            return None;
        }
        let Some(output) = part.get("output").filter(|output| !output.is_null()) else {
            continue;
        };
        outputs.push(output);
    }
    match outputs.as_slice() {
        [] => None,
        [Value::String(text)] => Some((*text).clone()),
        [output] => serde_json::to_string(output).ok(),
        outputs => serde_json::to_string(outputs).ok(),
    }
}

/// Cheap aggregate output classification performed before event identity, result hashing, or
/// verified-content locator construction.
///
/// Mux stores several dynamic-tool parts in one message. Keep that message cardinality and retain
/// every native call association in source order; the transient output contract must not force the
/// aggregate body to be duplicated once per call.
pub(super) fn mux_output_projection(value: &Value) -> Option<MuxOutputProjection> {
    let parts = value.get("parts")?.as_array()?;
    let mut output_parts = 0_usize;
    let mut available_parts = 0_usize;
    let mut saw_redacted = false;
    let mut saw_success = false;
    let mut saw_failure = false;
    let mut saw_timeout = false;
    let mut saw_unknown = false;
    let mut call_ids = Vec::new();
    let mut tool_names = Vec::new();
    let mut exit_codes = Vec::new();

    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("dynamic-tool")
            || !mux_part_is_output(part)
        {
            continue;
        }
        output_parts = output_parts.saturating_add(1);
        saw_redacted |= part.get("state").and_then(Value::as_str) == Some("output-redacted");
        available_parts = available_parts.saturating_add(usize::from(
            part.get("output").is_some_and(|value| !value.is_null()),
        ));
        push_mux_part_string(
            &mut call_ids,
            part,
            &[
                "toolCallId",
                "tool_call_id",
                "callId",
                "call_id",
                "toolUseId",
                "tool_use_id",
                "id",
            ],
        );
        push_mux_part_string(&mut tool_names, part, &["toolName", "tool_name", "name"]);

        let (outcome, exit_code) = mux_part_outcome(part);
        match outcome {
            MuxOutputOutcome::Success => saw_success = true,
            MuxOutputOutcome::Failure => saw_failure = true,
            MuxOutputOutcome::Timeout => saw_timeout = true,
            MuxOutputOutcome::Unknown => saw_unknown = true,
        }
        if let Some(exit_code) = exit_code {
            exit_codes.push(exit_code);
        }
    }
    if output_parts == 0 {
        return None;
    }
    let outcome = if saw_redacted {
        MuxOutputOutcome::Unknown
    } else if saw_timeout {
        MuxOutputOutcome::Timeout
    } else if saw_failure {
        MuxOutputOutcome::Failure
    } else if saw_success && !saw_unknown {
        MuxOutputOutcome::Success
    } else {
        MuxOutputOutcome::Unknown
    };
    let exit_code = (output_parts == 1)
        .then(|| exit_codes.first().copied())
        .flatten();
    Some(MuxOutputProjection {
        body_available: !saw_redacted && available_parts != 0,
        call_ids,
        tool_names,
        outcome,
        exit_code,
    })
}

pub(super) fn apply_mux_core_output_diagnostic(
    event: &mut MuxCoreEvent,
    value: &Value,
    projection: &MuxOutputProjection,
) {
    if projection.body_available {
        if let Some(content) = mux_result_content(value) {
            let (preview, truncated) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
            event.payload["text"] = Value::String(preview);
            event.payload["truncated"] = Value::Bool(truncated);
        }
    }
    event.payload["exit_code"] = projection
        .exit_code
        .map_or(Value::Null, |code| Value::from(i64::from(code)));
    event.payload["timed_out"] = Value::Bool(projection.outcome == MuxOutputOutcome::Timeout);
}

fn mux_part_is_output(part: &Value) -> bool {
    matches!(
        part.get("state").and_then(Value::as_str),
        Some("output-available" | "output-redacted")
    ) || part.get("output").is_some()
}

fn push_mux_part_string(output: &mut Vec<String>, part: &Value, keys: &[&str]) {
    let value = keys
        .iter()
        .find_map(|key| part.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty());
    if let Some(value) = value {
        let value = value.to_owned();
        if !output.contains(&value) {
            output.push(value);
        }
    }
}

fn mux_part_outcome(part: &Value) -> (MuxOutputOutcome, Option<i32>) {
    let timeout = ["timedOut", "timed_out", "timeout"]
        .iter()
        .any(|key| part.get(*key).and_then(Value::as_bool).unwrap_or(false));
    let exit_code = ["exitCode", "exit_code"]
        .iter()
        .find_map(|key| part.get(*key).and_then(Value::as_i64))
        .and_then(|code| i32::try_from(code).ok());
    let explicit_failure = part
        .get("success")
        .and_then(Value::as_bool)
        .is_some_and(|success| !success)
        || part
            .get("ok")
            .and_then(Value::as_bool)
            .is_some_and(|success| !success)
        || part
            .get("isError")
            .or_else(|| part.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || part.get("error").is_some_and(mux_error_is_nonempty)
        || exit_code.is_some_and(|code| code != 0);
    let explicit_success = part.get("success").and_then(Value::as_bool) == Some(true)
        || part.get("ok").and_then(Value::as_bool) == Some(true)
        || exit_code == Some(0);
    let status = ["status", "outcome", "state"]
        .iter()
        .find_map(|key| part.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let status_timeout = status
        .as_deref()
        .is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"));
    let status_failure = status.as_deref().is_some_and(|status| {
        matches!(
            status,
            "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
        )
    });
    let status_success = status.as_deref().is_some_and(|status| {
        matches!(
            status,
            "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
        )
    });
    if timeout || status_timeout {
        (MuxOutputOutcome::Timeout, exit_code)
    } else if explicit_failure || status_failure {
        (MuxOutputOutcome::Failure, exit_code)
    } else if explicit_success || status_success {
        (MuxOutputOutcome::Success, exit_code)
    } else {
        (MuxOutputOutcome::Unknown, exit_code)
    }
}

fn mux_error_is_nonempty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

pub(super) fn mux_partial_event_index(bytes: &[u8]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-mux-partial-event-index-sha256-v1\0");
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix) | (1_u64 << 63)
}

pub(super) fn mux_history_sequence(value: &Value) -> Option<i64> {
    match value.pointer("/metadata/historySequence") {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        Some(Value::String(raw)) => raw.parse::<i64>().ok(),
        _ => None,
    }
}

pub(super) fn mux_message_model(value: &Value) -> Option<String> {
    mux_string_pointer(value, &["/metadata/model", "/model"])
}

pub(super) fn mux_message_timestamp_opt(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("createdAt")
        .and_then(mux_value_timestamp)
        .or_else(|| {
            value
                .pointer("/metadata/timestamp")
                .and_then(mux_value_timestamp)
        })
        .or_else(|| {
            value
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| {
                    parts
                        .iter()
                        .find_map(|part| part.get("timestamp").and_then(mux_value_timestamp))
                })
        })
}
