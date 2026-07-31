use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::FileChangeKind;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::file_touches::{
    inferred_file_change_kind, normalize_file_path, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
};
use crate::provider::normalization::{
    provider_json_text, provider_local_preview, provider_normalized_result_value,
    provider_timestamp_seconds, provider_value_text,
};
use crate::{OutputOutcome, OutputOutcomeMetadata, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::stream::{GooseRetainedContentClass, GooseRetainedMessage};

pub(super) struct GooseOutputProjection {
    pub(super) call_id: Option<String>,
    pub(super) outcome: OutputOutcomeMetadata,
}

fn goose_output_outcome_label(outcome: OutputOutcome) -> &'static str {
    match outcome {
        OutputOutcome::Success => "success",
        OutputOutcome::Failure => "failure",
        OutputOutcome::Timeout => "timeout",
        OutputOutcome::Unknown => "unknown",
    }
}

pub(super) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .or_else(|| {
            raw.parse::<f64>()
                .ok()
                .map(|timestamp| provider_timestamp_seconds(Some(timestamp), fallback))
        })
        .unwrap_or(fallback)
}

fn goose_content_text(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_collect_text(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Returns complete normalized Goose tool-response bodies in native array
/// order. Only direct `toolResponse` blocks and their documented result fields
/// are accepted; arbitrary object descendants are not searched. The caller
/// owns any byte bound.
pub(crate) fn goose_normalized_result_content(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_visit_tool_responses(content, &mut |object| {
        if let Some(value) = goose_tool_response_value(object) {
            parts.push(provider_normalized_result_value(value));
        }
    });
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn goose_complete_content_text(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_collect_complete_text(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn goose_collect_complete_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                goose_collect_complete_text(item, parts);
            }
        }
        Value::Object(object) => {
            let before = parts.len();
            goose_collect_text(value, parts);
            if parts.len() == before {
                for child in object.values() {
                    goose_collect_complete_text(child, parts);
                }
            }
        }
        _ => goose_collect_text(value, parts),
    }
}

fn goose_visit_tool_responses(
    value: &Value,
    visitor: &mut impl FnMut(&serde_json::Map<String, Value>),
) {
    match value {
        Value::Array(items) => {
            for item in items {
                goose_visit_tool_responses(item, visitor);
            }
        }
        Value::Object(object)
            if object.get("type").and_then(Value::as_str) == Some("toolResponse") =>
        {
            visitor(object);
        }
        _ => {}
    }
}

pub(super) fn goose_output_projection(content: &Value) -> GooseOutputProjection {
    let mut aggregate = GooseOutcomeAggregate::default();
    goose_visit_tool_responses(content, &mut |object| {
        if aggregate.call_id.is_none() {
            aggregate.call_id = goose_tool_response_string(
                object,
                &["toolCallId", "tool_call_id", "call_id", "id"],
            )
            .or_else(|| {
                object
                    .get("toolCall")
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        }
        goose_collect_outcome(object, &mut aggregate);
        if let Some(result) = goose_tool_response_value(object) {
            goose_collect_outcome_value(result, &mut aggregate);
        }
    });
    GooseOutputProjection {
        call_id: aggregate.call_id,
        outcome: OutputOutcomeMetadata {
            outcome: if aggregate.timeout {
                OutputOutcome::Timeout
            } else if aggregate.failure {
                OutputOutcome::Failure
            } else if aggregate.success {
                OutputOutcome::Success
            } else {
                OutputOutcome::Unknown
            },
            exit_code: aggregate.exit_code,
            duration_ms: aggregate.duration_ms,
        },
    }
}

#[derive(Default)]
struct GooseOutcomeAggregate {
    timeout: bool,
    failure: bool,
    success: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    call_id: Option<String>,
}

fn goose_collect_outcome(
    object: &serde_json::Map<String, Value>,
    aggregate: &mut GooseOutcomeAggregate,
) {
    aggregate.timeout |= ["timed_out", "timedOut", "timeout"]
        .iter()
        .any(|key| object.get(*key).and_then(Value::as_bool).unwrap_or(false));
    if let Some(success) = object.get("success").and_then(Value::as_bool) {
        aggregate.success |= success;
        aggregate.failure |= !success;
    }
    aggregate.failure |= object
        .get("isError")
        .or_else(|| object.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(code) = object
        .get("exit_code")
        .or_else(|| object.get("exitCode"))
        .and_then(Value::as_i64)
    {
        aggregate.exit_code = i32::try_from(code).ok();
        aggregate.success |= code == 0;
        aggregate.failure |= code != 0;
    }
    if aggregate.duration_ms.is_none() {
        aggregate.duration_ms = ["duration_ms", "durationMs"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_u64));
    }
    for key in ["status", "state", "outcome"] {
        if let Some(status) = object.get(key).and_then(Value::as_str) {
            let status = status.trim().to_ascii_lowercase();
            aggregate.timeout |= matches!(status.as_str(), "timeout" | "timed_out" | "timedout");
            aggregate.failure |= matches!(
                status.as_str(),
                "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
            );
            aggregate.success |= matches!(
                status.as_str(),
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            );
        }
    }
    aggregate.failure |= object.get("error").is_some_and(goose_error_is_nonempty);
}

fn goose_collect_outcome_value(value: &Value, aggregate: &mut GooseOutcomeAggregate) {
    match value {
        Value::Array(items) => {
            for item in items {
                goose_collect_outcome_value(item, aggregate);
            }
        }
        Value::Object(object) => {
            goose_collect_outcome(object, aggregate);
            for value in object.values() {
                goose_collect_outcome_value(value, aggregate);
            }
        }
        _ => {}
    }
}

fn goose_error_is_nonempty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn goose_tool_response_string(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn goose_collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                goose_collect_text(item, parts);
            }
        }
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            match kind {
                Some("text") => {
                    if let Some(text) = object.get("text").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    }
                }
                Some("thinking") => {
                    if let Some(text) = object.get("thinking").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    }
                }
                Some("redactedThinking") => {
                    parts.push("redacted thinking".to_owned());
                }
                Some("toolRequest") | Some("frontendToolRequest") => {
                    let call = object.get("toolCall").unwrap_or(value);
                    let name = call
                        .get("name")
                        .or_else(|| object.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    parts.push(format!("tool call: {name}"));
                    if let Some(input) = call
                        .get("arguments")
                        .or_else(|| call.get("input"))
                        .and_then(provider_value_text)
                    {
                        parts.push(format!("tool input: {input}"));
                    }
                }
                Some("toolResponse") => {
                    parts.push("tool response".to_owned());
                    if let Some(text) =
                        goose_tool_response_value(object).and_then(provider_value_text)
                    {
                        parts.push(text);
                    }
                }
                Some("toolConfirmationRequest") => {
                    parts.push("tool confirmation request".to_owned());
                }
                Some("systemNotification") | Some("actionRequired") => {
                    for key in ["message", "text", "content"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            break;
                        }
                    }
                }
                _ => {
                    for key in ["text", "content", "message"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            return;
                        }
                    }
                }
            }
        }
        Value::Number(_) | Value::Bool(_) => parts.push(value.to_string()),
        Value::Null => {}
    }
}

fn goose_tool_response_value(object: &serde_json::Map<String, Value>) -> Option<&Value> {
    ["toolResult", "content", "result"]
        .iter()
        .find_map(|key| object.get(*key))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GooseNativeEventKind {
    Message,
    ToolCall,
    ToolOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct GooseNativeFileTouch {
    pub(super) ordinal: u32,
    pub(super) path: String,
    pub(super) old_path: Option<String>,
    pub(super) change_kind: FileChangeKind,
    pub(super) evidence: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GooseNativeEvent {
    pub(super) sqlite_rowid: i64,
    pub(super) native_order: i64,
    pub(super) native_identity: String,
    pub(super) provider_message_identity: String,
    pub(super) identity_degraded: bool,
    pub(super) session_identity: String,
    pub(super) kind: GooseNativeEventKind,
    pub(super) role: String,
    pub(super) content: Value,
    pub(super) searchable_text: String,
    pub(super) created_timestamp: Option<i64>,
    pub(super) timestamp: Option<String>,
    pub(super) tokens_json: Option<String>,
    pub(super) metadata_json: Option<String>,
    pub(super) retained_content_bytes: u64,
    pub(super) logical_row_digest: Option<[u8; 32]>,
    pub(super) file_touches: Vec<GooseNativeFileTouch>,
}

pub(super) fn goose_event_payload_hash(event: &GooseNativeEvent) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-goose-nativepath-canonical-event-v1\0");
    digest.update(event.native_order.to_le_bytes());
    digest.update(event.native_identity.as_bytes());
    digest.update(event.provider_message_identity.as_bytes());
    digest.update(event.session_identity.as_bytes());
    digest.update(event.role.as_bytes());
    digest.update(event.content.to_string().as_bytes());
    digest.update(event.searchable_text.as_bytes());
    digest.update(event.created_timestamp.unwrap_or_default().to_le_bytes());
    if let Some(timestamp) = &event.timestamp {
        digest.update(timestamp.as_bytes());
    }
    if let Some(tokens) = &event.tokens_json {
        digest.update(tokens.as_bytes());
    }
    if let Some(metadata) = &event.metadata_json {
        digest.update(metadata.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn normalize_goose_native_message(
    message: GooseRetainedMessage,
) -> Result<GooseNativeEvent> {
    let content: Value = serde_json::from_str(&message.content_json).map_err(|error| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose retained message {} changed classification while parsing: {error}",
            message.native_identity
        ))
    })?;
    content.as_array().ok_or_else(|| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose retained message {} is no longer an array",
            message.native_identity
        ))
    })?;
    let kind = match message.retained_class {
        GooseRetainedContentClass::Message => GooseNativeEventKind::Message,
        GooseRetainedContentClass::ToolCall => GooseNativeEventKind::ToolCall,
    };
    let searchable_text = goose_complete_content_text(&content)
        .unwrap_or_else(|| format!("Goose {} message", message.role));
    let file_touches = if kind == GooseNativeEventKind::ToolCall {
        goose_native_file_touches(&content)?
    } else {
        Vec::new()
    };
    Ok(GooseNativeEvent {
        sqlite_rowid: message.sqlite_rowid,
        native_order: message.native_order,
        native_identity: message.native_identity,
        provider_message_identity: message.provider_message_identity,
        identity_degraded: message.identity_degraded,
        session_identity: message.session_identity,
        kind,
        role: message.role,
        content,
        searchable_text,
        created_timestamp: message.created_timestamp,
        timestamp: message.timestamp,
        tokens_json: message.tokens_json,
        metadata_json: message.metadata_json,
        retained_content_bytes: message.content_bytes,
        logical_row_digest: Some(message.logical_row_digest),
        file_touches,
    })
}

pub(super) fn normalize_goose_native_output_diagnostic(
    message: &super::stream::GooseScannedMessage,
) -> Result<GooseNativeEvent> {
    let outcome = message.output_outcome.ok_or_else(|| {
        crate::CaptureError::SystemInvariant(
            "Goose output diagnostic omitted its SQL-classified outcome",
        )
    })?;
    if !matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
        return Err(crate::CaptureError::SystemInvariant(
            "Goose attempted to retain a successful or unknown output in Core",
        ));
    }
    let (diagnostic, call_id, exit_code, duration_ms) =
        if let Some(raw_content) = message.content_json.as_deref() {
            let content: Value = serde_json::from_str(raw_content).map_err(|error| {
                crate::CaptureError::InvalidPayload(format!(
                    "Goose SQL-classified output {} changed while building its diagnostic: {error}",
                    message.native_identity
                ))
            })?;
            let projection = goose_output_projection(&content);
            (
                goose_normalized_result_content(&content)
                    .unwrap_or_else(|| "Goose tool response failed".to_owned()),
                projection.call_id,
                projection.outcome.exit_code,
                projection.outcome.duration_ms,
            )
        } else {
            ("Goose tool response failed".to_owned(), None, None, None)
        };
    let output_preview = provider_local_preview(&diagnostic, PROVIDER_MAX_PREVIEW_CHARS).0;
    let body = json!({
        "message_id": message.provider_message_identity,
        "row_id": message.native_order,
        "role": message.role,
        "output_preview": output_preview,
        "result_outcome": goose_output_outcome_label(outcome),
        "exit_code": exit_code,
        "duration_ms": duration_ms,
        "timed_out": outcome == OutputOutcome::Timeout,
        "call_id": call_id,
        "output_retention": "bounded_diagnostic",
        "metadata": message.metadata_json.as_deref().map(provider_json_text),
        "tokens": message.tokens_json.as_deref().map(provider_json_text),
        "created_timestamp": message.created_timestamp,
        "timestamp": message.timestamp,
    });
    let retained_content_bytes = u64::try_from(body.to_string().len()).map_err(|_| {
        crate::CaptureError::SystemInvariant("Goose diagnostic body length exceeds u64")
    })?;
    Ok(GooseNativeEvent {
        sqlite_rowid: message.sqlite_rowid,
        native_order: message.native_order,
        native_identity: message.native_identity.clone(),
        provider_message_identity: message.provider_message_identity.clone(),
        identity_degraded: message.identity_degraded,
        session_identity: message.session_identity.clone(),
        kind: GooseNativeEventKind::ToolOutput,
        role: message.role.clone(),
        content: body,
        searchable_text: diagnostic,
        created_timestamp: message.created_timestamp,
        timestamp: message.timestamp.clone(),
        tokens_json: None,
        metadata_json: None,
        retained_content_bytes,
        logical_row_digest: message.logical_row_digest,
        file_touches: Vec::new(),
    })
}

fn goose_native_file_touches(content: &Value) -> Result<Vec<GooseNativeFileTouch>> {
    let mut touches = Vec::new();
    let mut seen = BTreeSet::new();
    let mut found_patch = false;
    goose_visit_patch_values(content, &mut |path, old_path, change_kind, evidence| {
        found_patch = true;
        goose_push_native_touch(
            &mut touches,
            &mut seen,
            path,
            old_path,
            change_kind,
            evidence,
        )
    })?;
    if !found_patch {
        goose_visit_structured_touches(
            content,
            None,
            &mut |path, old_path, change_kind, evidence| {
                goose_push_native_touch(
                    &mut touches,
                    &mut seen,
                    path,
                    old_path,
                    change_kind,
                    evidence,
                )
            },
        )?;
    }
    Ok(touches)
}

fn goose_push_native_touch(
    touches: &mut Vec<GooseNativeFileTouch>,
    seen: &mut BTreeSet<(String, Option<String>, String)>,
    path: String,
    old_path: Option<String>,
    change_kind: FileChangeKind,
    evidence: &'static str,
) -> Result<()> {
    let key = (path.clone(), old_path.clone(), format!("{change_kind:?}"));
    if !seen.insert(key) {
        return Ok(());
    }
    if touches.len() == MAX_PROVIDER_FILE_TOUCHES_PER_EVENT {
        return Err(crate::CaptureError::InvalidPayload(
            "Goose retained event exceeds the safe file-touch limit".to_owned(),
        ));
    }
    touches.push(GooseNativeFileTouch {
        ordinal: u32::try_from(touches.len()).map_err(|_| {
            crate::CaptureError::SystemInvariant("Goose file-touch ordinal exceeds u32")
        })?,
        path,
        old_path,
        change_kind,
        evidence,
    });
    Ok(())
}

fn goose_visit_patch_values<E>(
    value: &Value,
    visit: &mut impl FnMut(
        String,
        Option<String>,
        FileChangeKind,
        &'static str,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    match value {
        Value::String(text) if text.contains("*** Begin Patch") => {
            goose_visit_patch_text(text, visit)?;
        }
        Value::Array(values) => {
            for value in values {
                goose_visit_patch_values(value, visit)?;
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                goose_visit_patch_values(value, visit)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn goose_visit_patch_text<E>(
    patch: &str,
    visit: &mut impl FnMut(
        String,
        Option<String>,
        FileChangeKind,
        &'static str,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    let mut pending_update = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            goose_visit_pending_update(&mut pending_update, visit)?;
            if let Some(path) = normalize_file_path(path) {
                visit(path, None, FileChangeKind::Created, "apply_patch_add")?;
            }
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            goose_visit_pending_update(&mut pending_update, visit)?;
            pending_update = normalize_file_path(path);
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            goose_visit_pending_update(&mut pending_update, visit)?;
            if let Some(path) = normalize_file_path(path) {
                visit(path, None, FileChangeKind::Deleted, "apply_patch_delete")?;
            }
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            let old_path = pending_update.take();
            if let Some(path) = normalize_file_path(path) {
                visit(path, old_path, FileChangeKind::Renamed, "apply_patch_move")?;
            }
        }
    }
    goose_visit_pending_update(&mut pending_update, visit)
}

fn goose_visit_pending_update<E>(
    pending_update: &mut Option<String>,
    visit: &mut impl FnMut(
        String,
        Option<String>,
        FileChangeKind,
        &'static str,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    if let Some(path) = pending_update.take() {
        visit(path, None, FileChangeKind::Modified, "apply_patch_update")?;
    }
    Ok(())
}

fn goose_visit_structured_touches<E>(
    value: &Value,
    inherited_kind: Option<FileChangeKind>,
    visit: &mut impl FnMut(
        String,
        Option<String>,
        FileChangeKind,
        &'static str,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    match value {
        Value::Array(values) => {
            for value in values {
                goose_visit_structured_touches(value, inherited_kind, visit)?;
            }
        }
        Value::Object(object) => {
            let object_kind = {
                let inferred = inferred_file_change_kind(object);
                (inferred != FileChangeKind::Unknown)
                    .then_some(inferred)
                    .or(inherited_kind)
                    .unwrap_or(FileChangeKind::Unknown)
            };
            let old_path = object.iter().find_map(|(key, value)| {
                goose_is_old_path_key(key)
                    .then(|| value.as_str())
                    .flatten()
                    .and_then(normalize_file_path)
            });
            for (key, value) in object {
                if !goose_is_path_key(key) {
                    continue;
                }
                let Some(raw_path) = value.as_str() else {
                    continue;
                };
                if goose_normalized_key(key) == "uri" && !raw_path.trim().starts_with("file://") {
                    continue;
                }
                if let Some(path) = normalize_file_path(raw_path) {
                    visit(
                        path,
                        old_path.clone(),
                        object_kind,
                        "structured_provider_payload",
                    )?;
                }
            }
            for value in object.values() {
                goose_visit_structured_touches(value, Some(object_kind), visit)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn goose_is_path_key(key: &str) -> bool {
    matches!(
        goose_normalized_key(key).as_str(),
        "path"
            | "file"
            | "filepath"
            | "filename"
            | "targetfile"
            | "targetpath"
            | "relativepath"
            | "absolutepath"
            | "uri"
            | "destinationfile"
            | "destinationpath"
    )
}

fn goose_is_old_path_key(key: &str) -> bool {
    matches!(
        goose_normalized_key(key).as_str(),
        "oldpath" | "frompath" | "sourcepath" | "originalpath" | "previouspath"
    )
}

fn goose_normalized_key(key: &str) -> String {
    key.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| char::from(byte.to_ascii_lowercase()))
        .take(256)
        .collect()
}
