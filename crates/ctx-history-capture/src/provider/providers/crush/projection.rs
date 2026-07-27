use chrono::{DateTime, Utc};
use ctx_history_core::{Confidence, EventRole, EventType, FileChangeKind};
use serde_json::{json, Value};

use crate::native_source::NativeSqliteValue;
use crate::provider::importer::compact_provider_result_payload;
use crate::provider::normalization::{
    provider_capped_json, provider_line_from_index, provider_normalized_result_value,
    provider_policy_body, provider_policy_event_text, provider_role, provider_timestamp_millis,
    provider_timestamp_seconds, provider_value_text,
};
use crate::{
    compute_payload_hash, fnv1a64, CaptureError, OutputCommandContext, OutputObservationKind,
    OutputOutcome, OutputOutcomeMetadata, ProviderAdapterContext, Result,
    CRUSH_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

#[derive(Debug, Clone)]
pub(super) struct CrushSessionRow {
    pub(super) id: String,
    pub(super) parent_session_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
    pub(super) prompt_tokens: Option<i64>,
    pub(super) completion_tokens: Option<i64>,
    pub(super) cost: Option<f64>,
    pub(super) summary_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CrushMessageRow {
    pub(super) rowid: i64,
    pub(super) id: String,
    pub(super) session_id: String,
    pub(super) role: String,
    pub(super) parts: String,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
    pub(super) provider: Option<String>,
    pub(super) model: Option<String>,
    pub(super) is_summary_message: bool,
}

#[derive(Debug, Clone)]
pub(super) struct CrushFileRow {
    pub(super) rowid: i64,
    pub(super) session_id: Option<String>,
    pub(super) path: String,
    pub(super) version: Option<String>,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) struct CrushReadFileRow {
    pub(super) rowid: i64,
    pub(super) session_id: String,
    pub(super) path: String,
    pub(super) read_at: Option<i64>,
}

pub(super) enum CrushRecordProjection {
    Message(Box<CrushMessageProjection>),
    Rejection { line_number: usize, reason: String },
}

pub(super) struct CrushSessionDraft {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) source_metadata: Value,
    pub(super) session_metadata: Value,
}

pub(super) struct CrushEventDraft {
    pub(super) provider_event_index: u64,
    pub(super) legacy_provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

pub(super) struct CrushFileTouchDraft {
    pub(super) provider_session_id: String,
    pub(super) provider_touch_index: u64,
    pub(super) provider_event_index: Option<u64>,
    pub(super) path: String,
    pub(super) change_kind: Option<FileChangeKind>,
    pub(super) old_path: Option<String>,
    pub(super) line_count_delta: Option<i64>,
    pub(super) confidence: Confidence,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) metadata: Value,
}

pub(super) struct CrushMessageProjection {
    pub(super) line_number: usize,
    pub(super) provider_session_id: String,
    pub(super) event: Option<CrushEventDraft>,
    pub(super) event_type: EventType,
    pub(super) raw_parts: Value,
    pub(super) complete_text: Option<String>,
    pub(super) output: Option<CrushOutputProjection>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) provider_event_index: u64,
    pub(super) native_record_id: String,
    pub(super) parent_session_id: Option<String>,
}

pub(super) struct CrushOutputProjection {
    pub(super) kind: OutputObservationKind,
    pub(super) call_id: Option<String>,
    pub(super) command: Option<OutputCommandContext>,
    pub(super) outcome: OutputOutcomeMetadata,
}

impl CrushOutputProjection {
    pub(super) fn retain_in_core(&self) -> bool {
        matches!(
            self.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    }
}

fn crush_output_outcome_label(outcome: OutputOutcome) -> &'static str {
    match outcome {
        OutputOutcome::Success => "success",
        OutputOutcome::Failure => "failure",
        OutputOutcome::Timeout => "timeout",
        OutputOutcome::Unknown => "unknown",
    }
}

pub(super) struct CrushChildMessageRow {
    pub(super) parent_rowid: Option<i64>,
    pub(super) parent_created_at: Option<i64>,
    pub(super) parent_updated_at: Option<i64>,
    pub(super) message: CrushMessageRow,
}

pub(super) fn project_message(
    message: &CrushMessageRow,
    session: Option<&CrushSessionRow>,
    context: &ProviderAdapterContext,
) -> Result<CrushRecordProjection> {
    let Some(session) = session else {
        return Ok(CrushRecordProjection::Rejection {
            line_number: provider_line_from_index(event_index(message)),
            reason: format!(
                "Crush message {} references missing session {}",
                message.id, message.session_id
            ),
        });
    };
    let parts: Value = match serde_json::from_str(&message.parts) {
        Ok(parts) => parts,
        Err(error) => {
            return Ok(CrushRecordProjection::Rejection {
                line_number: provider_line_from_index(event_index(message)),
                reason: format!(
                    "invalid JSON in Crush message {} parts: {error}",
                    message.id
                ),
            });
        }
    };
    let started_at = provider_timestamp_millis(session.created_at, context.imported_at);
    let occurred_at = provider_timestamp_millis(message.created_at, started_at);
    let event_type = event_type(message, &parts);
    let output = matches!(event_type, EventType::ToolOutput | EventType::CommandOutput)
        .then(|| crush_output_projection(event_type, &parts));
    let provider_event_index = event_index(message);
    let legacy_provider_event_index = legacy_event_index(message);
    let line = provider_line_from_index(provider_event_index);
    let retain_core_event = output
        .as_ref()
        .is_none_or(CrushOutputProjection::retain_in_core);
    let complete_text = output
        .is_none()
        .then(|| parts_text(&parts).unwrap_or_else(|| format!("Crush {} message", message.role)));
    let event = if retain_core_event {
        let payload = crush_core_payload(
            message,
            &parts,
            event_type,
            output.as_ref(),
            complete_text.as_deref(),
        );
        let provider_event_hash = crush_normalized_payload_hash(event_type, &payload)?;
        Some(CrushEventDraft {
            provider_event_index,
            legacy_provider_event_index,
            provider_event_hash,
            cursor: format!(
                "session:{}:message:{}:rowid:{}",
                message.session_id, message.id, message.rowid
            ),
            event_type,
            role: Some(provider_role(Some(&message.role))),
            occurred_at,
            payload,
            metadata: json!({
                "source": "crush_messages",
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "message_id": message.id.clone(),
                "session_id": message.session_id.clone(),
                "rowid": message.rowid,
                "provider": message.provider.clone(),
                "model": message.model.clone(),
            }),
        })
    } else {
        None
    };
    Ok(CrushRecordProjection::Message(Box::new(
        CrushMessageProjection {
            line_number: line,
            provider_session_id: session.id.clone(),
            event,
            event_type,
            raw_parts: parts,
            complete_text,
            output,
            occurred_at,
            provider_event_index,
            native_record_id: message.id.clone(),
            parent_session_id: session.parent_session_id.clone(),
        },
    )))
}

fn crush_core_payload(
    message: &CrushMessageRow,
    parts: &Value,
    event_type: EventType,
    output: Option<&CrushOutputProjection>,
    complete_text: Option<&str>,
) -> Value {
    if let Some(output) = output {
        let result_evidence = output.call_id.as_deref().map_or_else(Vec::new, |call_id| {
            vec![json!({
                "kind": "call_id",
                "value": call_id,
            })]
        });
        return json!({
            "tool": output.command.as_ref().map(|command| command.tool_name.as_str()),
            "call_id": output.call_id,
            "result_evidence": result_evidence,
            "result_outcome": crush_output_outcome_label(output.outcome.outcome),
            "exit_code": output.outcome.exit_code,
            "duration_ms": output.outcome.duration_ms,
            "timed_out": output.outcome.outcome == OutputOutcome::Timeout,
            "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
        });
    }

    let text = complete_text
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Crush {} message", message.role));
    let body = json!({
        "message_id": message.id,
        "role": message.role,
        "parts": parts,
        "provider": message.provider,
        "model": message.model,
        "is_summary_message": message.is_summary_message,
        "created_at": message.created_at,
        "updated_at": message.updated_at,
    });
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    json!({
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": [],
        "result_outcome": Value::Null,
        "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
        "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
    })
}

fn crush_normalized_payload_hash(event_type: EventType, payload: &Value) -> Result<String> {
    compute_payload_hash(&compact_provider_result_payload(event_type, payload))
}

pub(super) fn project_session(
    session: &CrushSessionRow,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    fallback: DateTime<Utc>,
) -> CrushSessionDraft {
    let started_at = provider_timestamp_millis(session.created_at, fallback);
    let ended_at = session
        .updated_at
        .map(|timestamp| provider_timestamp_millis(Some(timestamp), started_at));
    CrushSessionDraft {
        provider_session_id: session.id.clone(),
        parent_provider_session_id: session.parent_session_id.clone(),
        started_at,
        ended_at,
        source_metadata: json!({
            "adapter": CRUSH_SQLITE_SOURCE_FORMAT,
            "sqlite_user_version": user_version,
            "schema_fingerprint": schema_fingerprint,
            "source_path": raw_source_path,
            "upstream_tables": ["sessions", "messages", "files", "read_files"],
        }),
        session_metadata: json!({
            "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
            "session_id": session.id,
            "title": session.title,
            "parent_session_id": session.parent_session_id,
            "summary_message_id": session.summary_message_id,
            "tokens": {
                "prompt": session.prompt_tokens,
                "completion": session.completion_tokens,
            },
            "cost": session.cost,
            "created_at": session.created_at,
            "updated_at": session.updated_at,
        }),
    }
}

pub(super) fn decode_session(values: &[NativeSqliteValue]) -> Result<CrushSessionRow> {
    if values.len() != 10 {
        return Err(CaptureError::SystemInvariant(
            "Crush session logical row has an invalid value count",
        ));
    }
    let _rowid = integer(values, 0)?;
    decode_session_at(values, 1)?.ok_or_else(|| {
        CaptureError::InvalidPayload("Crush session row has a NULL required session id".to_owned())
    })
}

pub(super) fn decode_message_child(values: &[NativeSqliteValue]) -> Result<CrushChildMessageRow> {
    if values.len() != 13 {
        return Err(CaptureError::SystemInvariant(
            "Crush child message logical row has an invalid value count",
        ));
    }
    Ok(CrushChildMessageRow {
        parent_rowid: optional_integer(values, 0)?,
        parent_created_at: optional_integer(values, 1)?,
        parent_updated_at: optional_integer(values, 2)?,
        message: decode_message_at(values, 3)?,
    })
}

fn decode_message_at(values: &[NativeSqliteValue], offset: usize) -> Result<CrushMessageRow> {
    Ok(CrushMessageRow {
        rowid: integer(values, offset)?,
        id: text(values, offset + 1)?.to_owned(),
        session_id: text(values, offset + 2)?.to_owned(),
        role: text(values, offset + 3)?.to_owned(),
        parts: text(values, offset + 4)?.to_owned(),
        created_at: optional_integer(values, offset + 5)?,
        updated_at: optional_integer(values, offset + 6)?,
        provider: optional_text(values, offset + 7)?,
        model: optional_text(values, offset + 8)?,
        is_summary_message: integer(values, offset + 9)? != 0,
    })
}

fn decode_session_at(
    values: &[NativeSqliteValue],
    offset: usize,
) -> Result<Option<CrushSessionRow>> {
    if values.len() < offset.saturating_add(9) {
        return Err(CaptureError::SystemInvariant(
            "Crush joined session logical row has an invalid value count",
        ));
    }
    let Some(id) = optional_text(values, offset)? else {
        return Ok(None);
    };
    if id.trim().is_empty() {
        return Err(CaptureError::InvalidPayload(
            "Crush session id is empty".to_owned(),
        ));
    }
    Ok(Some(CrushSessionRow {
        id,
        parent_session_id: optional_text(values, offset + 1)?,
        title: optional_text(values, offset + 2)?,
        created_at: optional_integer(values, offset + 3)?,
        updated_at: optional_integer(values, offset + 4)?,
        prompt_tokens: optional_integer(values, offset + 5)?,
        completion_tokens: optional_integer(values, offset + 6)?,
        cost: optional_real(values, offset + 7)?,
        summary_message_id: optional_text(values, offset + 8)?,
    }))
}

pub(super) fn decode_file(values: &[NativeSqliteValue]) -> Result<CrushFileRow> {
    if values.len() != 6 {
        return Err(CaptureError::SystemInvariant(
            "Crush file logical row has an invalid value count",
        ));
    }
    Ok(CrushFileRow {
        rowid: integer(values, 0)?,
        session_id: optional_text(values, 1)?,
        path: text(values, 2)?.to_owned(),
        version: optional_text(values, 3)?,
        created_at: optional_integer(values, 4)?,
        updated_at: optional_integer(values, 5)?,
    })
}

pub(super) fn decode_read_file(values: &[NativeSqliteValue]) -> Result<CrushReadFileRow> {
    if values.len() != 4 {
        return Err(CaptureError::SystemInvariant(
            "Crush read-file logical row has an invalid value count",
        ));
    }
    Ok(CrushReadFileRow {
        rowid: integer(values, 0)?,
        session_id: text(values, 1)?.to_owned(),
        path: text(values, 2)?.to_owned(),
        read_at: optional_integer(values, 3)?,
    })
}

fn text(values: &[NativeSqliteValue], index: usize) -> Result<&str> {
    match values.get(index) {
        Some(NativeSqliteValue::Text(value)) if !value.trim().is_empty() => Ok(value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Crush provider row field {index} is not valid nonempty text"
        ))),
    }
}

pub(super) fn optional_text(values: &[NativeSqliteValue], index: usize) -> Result<Option<String>> {
    match values.get(index) {
        Some(NativeSqliteValue::Null) => Ok(None),
        Some(NativeSqliteValue::Text(value)) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Crush provider row field {index} is not valid optional text"
        ))),
    }
}

fn integer(values: &[NativeSqliteValue], index: usize) -> Result<i64> {
    match values.get(index) {
        Some(NativeSqliteValue::Integer(value)) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Crush provider row field {index} is not a valid integer"
        ))),
    }
}

fn optional_integer(values: &[NativeSqliteValue], index: usize) -> Result<Option<i64>> {
    match values.get(index) {
        Some(NativeSqliteValue::Null) => Ok(None),
        Some(NativeSqliteValue::Integer(value)) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Crush provider row field {index} is not a valid optional integer"
        ))),
    }
}

fn optional_real(values: &[NativeSqliteValue], index: usize) -> Result<Option<f64>> {
    match values.get(index) {
        Some(NativeSqliteValue::Null) => Ok(None),
        Some(value) => value.as_real().map(Some).ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Crush provider row field {index} is not a valid optional real"
            ))
        }),
        None => Err(CaptureError::SystemInvariant(
            "Crush logical row is missing a projected optional real value",
        )),
    }
}

pub(super) fn file_touch(
    row: CrushFileRow,
    fallback: DateTime<Utc>,
) -> Option<CrushFileTouchDraft> {
    let session_id = row.session_id?;
    let occurred_at = provider_timestamp_millis(row.updated_at.or(row.created_at), fallback);
    let touch_index = 0x0100_0000_0000_u64.saturating_add(row.rowid.max(0) as u64);
    Some(CrushFileTouchDraft {
        provider_session_id: session_id,
        provider_touch_index: touch_index,
        provider_event_index: None,
        path: row.path,
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: None,
        confidence: Confidence::Explicit,
        occurred_at,
        metadata: json!({
            "source": "crush_files",
            "rowid": row.rowid,
            "version": row.version,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        }),
    })
}

pub(super) fn read_file_touch(
    row: CrushReadFileRow,
    fallback: DateTime<Utc>,
) -> CrushFileTouchDraft {
    let occurred_at = provider_timestamp_seconds(row.read_at.map(|value| value as f64), fallback);
    let touch_index = 0x0200_0000_0000_u64.saturating_add(row.rowid.max(0) as u64);
    CrushFileTouchDraft {
        provider_session_id: row.session_id,
        provider_touch_index: touch_index,
        provider_event_index: None,
        path: row.path,
        change_kind: Some(FileChangeKind::Read),
        old_path: None,
        line_count_delta: None,
        confidence: Confidence::Explicit,
        occurred_at,
        metadata: json!({
            "source": "crush_read_files",
            "rowid": row.rowid,
            "read_at": row.read_at,
        }),
    }
}

pub(super) fn event_index(message: &CrushMessageRow) -> u64 {
    fnv1a64(message.id.as_bytes())
}

pub(super) fn legacy_event_index(message: &CrushMessageRow) -> u64 {
    let base = message
        .created_at
        .or(message.updated_at)
        .unwrap_or(message.rowid)
        .max(0) as u64;
    base.saturating_mul(4_096)
        .saturating_add((fnv1a64(message.id.as_bytes()) & 0x0fff_ffff) % 4_096)
}

fn event_type(message: &CrushMessageRow, parts: &Value) -> EventType {
    if message.is_summary_message {
        return EventType::Summary;
    }
    if parts_have_type(parts, "shell_command") {
        EventType::CommandOutput
    } else if parts_have_type(parts, "tool_result") || message.role == "tool" {
        EventType::ToolOutput
    } else if parts_have_type(parts, "tool_call") {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

fn parts_have_type(parts: &Value, expected: &str) -> bool {
    parts.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some(expected))
    })
}

fn crush_output_projection(event_type: EventType, parts: &Value) -> CrushOutputProjection {
    let kind = if event_type == EventType::CommandOutput {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let mut aggregate = CrushOutcomeAggregate::default();
    let items = parts.as_array().map(Vec::as_slice).unwrap_or_default();
    let preferred_kind = if kind == OutputObservationKind::Command {
        "shell_command"
    } else {
        "tool_result"
    };
    for item in items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some(preferred_kind))
    {
        let data = item.get("data").unwrap_or(item);
        if aggregate.call_id.is_none() {
            aggregate.call_id =
                crush_string_field(data, &["call_id", "callId", "tool_call_id", "id"]);
        }
        if aggregate.command.is_none() && kind == OutputObservationKind::Command {
            let command =
                crush_string_field(data, &["command", "cmd"]).unwrap_or_else(|| "shell".to_owned());
            let tool_name =
                crush_string_field(data, &["name", "tool"]).unwrap_or_else(|| "shell".to_owned());
            let working_directory =
                crush_string_field(data, &["working_directory", "workingDirectory", "cwd"]);
            aggregate.command = Some(OutputCommandContext {
                tool_name,
                command,
                working_directory,
            });
        }
    }
    for item in items {
        let kind = item.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("tool_result" | "shell_command")) {
            continue;
        }
        crush_collect_outcome_value(item.get("data").unwrap_or(item), &mut aggregate);
    }
    CrushOutputProjection {
        kind,
        call_id: aggregate.call_id,
        command: aggregate.command,
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
struct CrushOutcomeAggregate {
    timeout: bool,
    failure: bool,
    success: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    call_id: Option<String>,
    command: Option<OutputCommandContext>,
}

fn crush_collect_outcome_value(value: &Value, aggregate: &mut CrushOutcomeAggregate) {
    let Some(object) = value.as_object() else {
        return;
    };
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
        .and_then(|code| i32::try_from(code).ok())
    {
        aggregate.exit_code = Some(code);
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
}

fn crush_string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parts_text(parts: &Value) -> Option<String> {
    let mut text = Vec::new();
    if let Some(items) = parts.as_array() {
        for item in items {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("part");
            let data = item.get("data").unwrap_or(item);
            match kind {
                "text" => push_json_text(&mut text, data.get("text").unwrap_or(data)),
                "reasoning" => {
                    push_json_text(
                        &mut text,
                        data.get("thinking")
                            .or_else(|| data.get("text"))
                            .unwrap_or(data),
                    );
                }
                "tool_call" => {
                    let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                    text.push(format!("tool call: {name}"));
                    if let Some(input) = data.get("input").and_then(provider_value_text) {
                        text.push(format!("tool input: {input}"));
                    }
                }
                "tool_result" => {
                    let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                    text.push(format!("tool result: {name}"));
                    if let Some(value) = crush_tool_result_value(data).and_then(provider_value_text)
                    {
                        text.push(value);
                    }
                }
                "shell_command" => {
                    if let Some(command) = data.get("command").and_then(Value::as_str) {
                        text.push(command.to_owned());
                    }
                    if let Some(output) = data.get("output").and_then(Value::as_str) {
                        text.push(output.to_owned());
                    }
                }
                "finish" => {
                    if let Some(reason) = data.get("reason").and_then(Value::as_str) {
                        text.push(format!("finish: {reason}"));
                    }
                }
                _ => push_json_text(&mut text, data),
            }
            if text.iter().map(|part| part.chars().count()).sum::<usize>()
                >= PROVIDER_MAX_TEXT_CHARS
            {
                break;
            }
        }
    } else {
        push_json_text(&mut text, parts);
    }
    (!text.is_empty()).then(|| text.join("\n"))
}

/// Returns complete normalized result bodies from Crush result parts in their
/// native order. Only schema-owned part kinds and fields are considered; the
/// function never searches arbitrary descendants. The caller owns any bound.
pub(crate) fn crush_normalized_result_content(parts: &Value) -> Option<String> {
    let items = parts.as_array()?;
    let mut results = Vec::new();
    for item in items {
        let Some(kind) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        let data = item.get("data").unwrap_or(item);
        let value = match kind {
            "tool_result" => crush_tool_result_value(data),
            "shell_command" => ["output", "stdout", "stderr"]
                .iter()
                .find_map(|key| data.get(*key)),
            _ => None,
        };
        if let Some(value) = value {
            results.push(provider_normalized_result_value(value));
        }
    }
    (!results.is_empty()).then(|| results.join("\n"))
}

fn crush_tool_result_value(data: &Value) -> Option<&Value> {
    ["content", "data", "output"]
        .iter()
        .find_map(|key| data.get(*key))
}

pub(crate) fn crush_complete_message(
    values: &[NativeSqliteValue],
) -> Result<(String, String, String, String)> {
    let child = decode_message_child(values)?;
    if child.parent_rowid.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Crush message parent is missing".into(),
        ));
    }
    let message = child.message;
    let parts: Value = serde_json::from_str(&message.parts)?;
    let event_type = event_type(&message, &parts);
    if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return Err(CaptureError::InvalidPayload(
            "Crush output rows are not complete-message content".to_owned(),
        ));
    }
    let text = parts_text(&parts).unwrap_or_else(|| format!("Crush {} message", message.role));
    let payload = crush_core_payload(&message, &parts, event_type, None, Some(&text));
    let normalized_hash = crush_normalized_payload_hash(event_type, &payload)?;
    Ok((message.session_id, message.id, normalized_hash, text))
}

fn push_json_text(parts: &mut Vec<String>, value: &Value) {
    if let Some(text) = provider_value_text(value).filter(|text| !text.trim().is_empty()) {
        parts.push(text);
    }
}
