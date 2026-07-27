use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Confidence, EventType, Fidelity, FileChangeKind,
    ProviderCaptureEnvelope, ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    emit_projected_normalization_units, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ExistingSessionEventOutcome,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_line_from_index, provider_role,
    provider_timestamp_millis, provider_timestamp_seconds, provider_value_text, text_id_index,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderNormalizationResult,
    Result, CRUSH_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

use super::capture::{decode_position, initial_position};
use super::{
    CRUSH_FILE_RECORD_KIND, CRUSH_MESSAGE_CHILD_RECORD_KIND, CRUSH_READ_FILE_RECORD_KIND,
    CRUSH_SESSION_RECORD_KIND,
};

#[derive(Debug, Clone)]
struct CrushSessionRow {
    id: String,
    parent_session_id: Option<String>,
    title: Option<String>,
    created_at: Option<i64>,
    updated_at: Option<i64>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cost: Option<f64>,
    summary_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CrushMessageRow {
    rowid: i64,
    pub(super) id: String,
    session_id: String,
    role: String,
    parts: String,
    created_at: Option<i64>,
    updated_at: Option<i64>,
    provider: Option<String>,
    model: Option<String>,
    is_summary_message: bool,
}

#[derive(Debug, Clone)]
struct CrushFileRow {
    rowid: i64,
    session_id: Option<String>,
    path: String,
    version: Option<String>,
    created_at: Option<i64>,
    updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
struct CrushReadFileRow {
    rowid: i64,
    session_id: String,
    path: String,
    read_at: Option<i64>,
}

pub(super) struct CrushCapturedBatchProjector {
    context: ProviderAdapterContext,
    raw_source_path: String,
    user_version: i64,
    schema_fingerprint: String,
}

impl CrushCapturedBatchProjector {
    pub(super) fn new(
        context: ProviderAdapterContext,
        raw_source_path: String,
        user_version: i64,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            raw_source_path,
            user_version,
            schema_fingerprint,
        }
    }
}

enum CrushRecordProjection {
    Normalization(ProviderNormalizationResult),
    Message(Box<CrushMessageProjection>),
    Rejection { line_number: usize, reason: String },
}

struct CrushMessageProjection {
    line_number: usize,
    provider_session_id: String,
    capture: ProviderCaptureEnvelope,
    event: ProviderEventEnvelope,
    raw_parts: Value,
    existing_session: bool,
}

pub(super) struct CrushChildMessageRow {
    pub(super) parent_rowid: Option<i64>,
    parent_created_at: Option<i64>,
    parent_updated_at: Option<i64>,
    pub(super) message: CrushMessageRow,
}

impl CapturedBatchProjector for CrushCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Crush projector requires SQLite logical values",
            ));
        };
        let projected = match record.record_kind().as_str() {
            CRUSH_SESSION_RECORD_KIND => decode_session(values).map(|session| {
                let started_at =
                    provider_timestamp_millis(session.created_at, self.context.imported_at);
                let ended_at = session
                    .updated_at
                    .map(|timestamp| provider_timestamp_millis(Some(timestamp), started_at));
                CrushRecordProjection::Normalization(ProviderNormalizationResult {
                    captures: vec![(
                        0,
                        capture(
                            &session,
                            CrushCaptureContext {
                                started_at,
                                ended_at,
                                raw_source_path: &self.raw_source_path,
                                user_version: self.user_version,
                                schema_fingerprint: &self.schema_fingerprint,
                                event: None,
                            },
                            &self.context,
                        ),
                    )],
                    ..ProviderNormalizationResult::default()
                })
            }),
            CRUSH_MESSAGE_CHILD_RECORD_KIND => {
                let child = decode_message_child(values).map_err(ProviderProjectionFatal::new)?;
                let parent = child.parent_rowid.map(|_| CrushSessionRow {
                    id: child.message.session_id.clone(),
                    parent_session_id: None,
                    title: None,
                    created_at: child.parent_created_at,
                    updated_at: child.parent_updated_at,
                    prompt_tokens: None,
                    completion_tokens: None,
                    cost: None,
                    summary_message_id: None,
                });
                Ok(project_message(
                    &child.message,
                    parent.as_ref(),
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.context,
                    true,
                ))
            }
            CRUSH_FILE_RECORD_KIND => decode_file(values).map(|row| {
                CrushRecordProjection::Normalization(ProviderNormalizationResult {
                    files_touched: file_touch(row, &self.raw_source_path, self.context.imported_at)
                        .into_iter()
                        .collect(),
                    ..ProviderNormalizationResult::default()
                })
            }),
            CRUSH_READ_FILE_RECORD_KIND => decode_read_file(values).map(|row| {
                CrushRecordProjection::Normalization(ProviderNormalizationResult {
                    files_touched: vec![read_file_touch(
                        row,
                        &self.raw_source_path,
                        self.context.imported_at,
                    )],
                    ..ProviderNormalizationResult::default()
                })
            }),
            _ => Err(CaptureError::SystemInvariant(
                "Crush projector received an unexpected record kind",
            )),
        };
        match projected {
            Ok(CrushRecordProjection::Normalization(normalization)) => {
                emit_projected_normalization_units(output, normalization)
            }
            Ok(CrushRecordProjection::Message(message)) => {
                let CrushMessageProjection {
                    line_number,
                    provider_session_id,
                    capture,
                    event,
                    raw_parts,
                    existing_session,
                } = *message;
                output.use_explicit_file_touches();
                if existing_session {
                    let event_outcome = output.emit_existing_session_event(
                        provider_line_from_index(record.ordinal().saturating_add(1)),
                        capture,
                    )?;
                    if event_outcome == ExistingSessionEventOutcome::Rejected {
                        return Ok(());
                    }
                } else {
                    emit_projected_normalization_units(
                        output,
                        ProviderNormalizationResult {
                            captures: vec![(line_number, capture)],
                            ..ProviderNormalizationResult::default()
                        },
                    )?;
                }
                let file_touch_outcome = visit_provider_file_touches_from_raw_value(
                    ProviderFileTouchSourceContext::new(
                        CaptureProvider::Crush,
                        &provider_session_id,
                        CRUSH_SQLITE_SOURCE_FORMAT,
                        Some(self.raw_source_path.as_str()),
                        Some(self.raw_source_path.as_str()),
                    ),
                    &raw_parts,
                    &event,
                    line_number,
                    |file_touch| {
                        output.emit_normalization(ProviderNormalizationResult {
                            files_touched: vec![file_touch],
                            ..ProviderNormalizationResult::default()
                        })
                    },
                )?;
                if file_touch_outcome.limit_exceeded() {
                    output
                        .reject_record(line_number, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
                }
                Ok(())
            }
            Ok(CrushRecordProjection::Rejection {
                line_number,
                reason,
            }) => {
                output.reject_record(line_number, reason);
                Ok(())
            }
            Err(error) => {
                output.reject_record(
                    provider_line_from_index(record.ordinal().saturating_add(1)),
                    error.to_string(),
                );
                Ok(())
            }
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_position()? {
            return Err(CaptureError::InvalidPayload(
                "Crush initial cursor candidate is not at the SQLite source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&())?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        decode_position(batch.range_end())?;
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

fn project_message(
    message: &CrushMessageRow,
    session: Option<&CrushSessionRow>,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
    existing_session: bool,
) -> CrushRecordProjection {
    let provider_event_index = event_index(message);
    let line = provider_line_from_index(provider_event_index);
    let Some(session) = session else {
        return CrushRecordProjection::Rejection {
            line_number: line,
            reason: format!(
                "Crush message {} references missing session {}",
                message.id, message.session_id
            ),
        };
    };
    let parts: Value = match serde_json::from_str(&message.parts) {
        Ok(parts) => parts,
        Err(error) => {
            return CrushRecordProjection::Rejection {
                line_number: line,
                reason: format!(
                    "invalid JSON in Crush message {} parts: {error}",
                    message.id
                ),
            };
        }
    };
    let started_at = provider_timestamp_millis(session.created_at, context.imported_at);
    let occurred_at = provider_timestamp_millis(message.created_at, started_at);
    let ended_at = session
        .updated_at
        .map(|timestamp| provider_timestamp_millis(Some(timestamp), occurred_at));
    let event_type = event_type(message, &parts);
    let text = parts_text(&parts).unwrap_or_else(|| format!("Crush {} message", message.role));
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Crush,
        source_format: CRUSH_SQLITE_SOURCE_FORMAT,
        provider_session_id: message.session_id.clone(),
        provider_event_index,
        provider_event_hash: Some(message.id.clone()),
        cursor: format!(
            "session:{}:message:{}:rowid:{}",
            message.session_id, message.id, message.rowid
        ),
        event_type,
        role: Some(provider_role(Some(&message.role))),
        occurred_at,
        text,
        body: json!({
            "message_id": message.id,
            "role": message.role,
            "parts": parts,
            "provider": message.provider,
            "model": message.model,
            "is_summary_message": message.is_summary_message,
            "created_at": message.created_at,
            "updated_at": message.updated_at,
        }),
        metadata: json!({
            "source": "crush_messages",
            "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
            "message_id": message.id,
            "session_id": message.session_id,
            "rowid": message.rowid,
            "provider": message.provider,
            "model": message.model,
        }),
    });
    let capture = capture(
        session,
        CrushCaptureContext {
            started_at,
            ended_at,
            raw_source_path,
            user_version,
            schema_fingerprint,
            event: Some(event.clone()),
        },
        context,
    );
    CrushRecordProjection::Message(Box::new(CrushMessageProjection {
        line_number: line,
        provider_session_id: session.id.clone(),
        capture,
        event,
        raw_parts: parts,
        existing_session,
    }))
}

fn decode_session(values: &[CapturedSqliteValue]) -> Result<CrushSessionRow> {
    if values.len() != 10 {
        return Err(CaptureError::SystemInvariant(
            "Crush session logical row has an invalid value count",
        ));
    }
    let _rowid = integer(values, 0)?;
    decode_session_at(values, 1)?.ok_or(CaptureError::SystemInvariant(
        "Crush session logical row is missing its required session",
    ))
}

pub(super) fn decode_message_child(values: &[CapturedSqliteValue]) -> Result<CrushChildMessageRow> {
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

fn decode_message_at(values: &[CapturedSqliteValue], offset: usize) -> Result<CrushMessageRow> {
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
    values: &[CapturedSqliteValue],
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

fn decode_file(values: &[CapturedSqliteValue]) -> Result<CrushFileRow> {
    if values.len() != 6 {
        return Err(CaptureError::SystemInvariant(
            "Crush file logical row has an invalid value count",
        ));
    }
    Ok(CrushFileRow {
        rowid: integer(values, 0)?,
        session_id: Some(text(values, 1)?.to_owned()),
        path: text(values, 2)?.to_owned(),
        version: optional_text(values, 3)?,
        created_at: optional_integer(values, 4)?,
        updated_at: optional_integer(values, 5)?,
    })
}

fn decode_read_file(values: &[CapturedSqliteValue]) -> Result<CrushReadFileRow> {
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

fn text(values: &[CapturedSqliteValue], index: usize) -> Result<&str> {
    match values.get(index) {
        Some(CapturedSqliteValue::Text(value)) => Ok(value),
        _ => Err(CaptureError::SystemInvariant(
            "Crush logical row has an invalid text value",
        )),
    }
}

fn optional_text(values: &[CapturedSqliteValue], index: usize) -> Result<Option<String>> {
    match values.get(index) {
        Some(CapturedSqliteValue::Null) => Ok(None),
        Some(CapturedSqliteValue::Text(value)) => Ok(Some(value.clone())),
        _ => Err(CaptureError::SystemInvariant(
            "Crush logical row has an invalid optional text value",
        )),
    }
}

fn integer(values: &[CapturedSqliteValue], index: usize) -> Result<i64> {
    match values.get(index) {
        Some(CapturedSqliteValue::Integer(value)) => Ok(*value),
        _ => Err(CaptureError::SystemInvariant(
            "Crush logical row has an invalid integer value",
        )),
    }
}

fn optional_integer(values: &[CapturedSqliteValue], index: usize) -> Result<Option<i64>> {
    match values.get(index) {
        Some(CapturedSqliteValue::Null) => Ok(None),
        Some(CapturedSqliteValue::Integer(value)) => Ok(Some(*value)),
        _ => Err(CaptureError::SystemInvariant(
            "Crush logical row has an invalid optional integer value",
        )),
    }
}

fn optional_real(values: &[CapturedSqliteValue], index: usize) -> Result<Option<f64>> {
    match values.get(index) {
        Some(CapturedSqliteValue::Null) => Ok(None),
        Some(value) => value
            .as_real()
            .map(Some)
            .ok_or(CaptureError::SystemInvariant(
                "Crush logical row has an invalid optional real value",
            )),
        None => Err(CaptureError::SystemInvariant(
            "Crush logical row is missing an optional real value",
        )),
    }
}

struct CrushCaptureContext<'a> {
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    raw_source_path: &'a str,
    user_version: i64,
    schema_fingerprint: &'a str,
    event: Option<ProviderEventEnvelope>,
}

fn capture(
    session: &CrushSessionRow,
    draft: CrushCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let is_subagent = session.parent_session_id.is_some();
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Crush,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT,
            provider_session_id: session.id.clone(),
            parent_provider_session_id: session.parent_session_id.clone(),
            root_provider_session_id: session.parent_session_id.clone(),
            external_agent_id: None,
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": CRUSH_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
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
        },
        context,
        draft.event,
    )
}

fn file_touch(
    row: CrushFileRow,
    raw_source_path: &str,
    fallback: DateTime<Utc>,
) -> Option<(usize, ProviderFileTouchedEnvelope)> {
    let session_id = row.session_id?;
    let occurred_at = provider_timestamp_millis(row.updated_at.or(row.created_at), fallback);
    let touch_index = 0x0100_0000_0000_u64.saturating_add(row.rowid.max(0) as u64);
    Some((
        provider_line_from_index(touch_index),
        ProviderFileTouchedEnvelope {
            provider: CaptureProvider::Crush,
            provider_session_id: session_id,
            provider_touch_index: touch_index,
            provider_event_index: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_root: Some(raw_source_path.to_owned()),
            path: row.path,
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            occurred_at,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
            metadata: json!({
                "source": "crush_files",
                "rowid": row.rowid,
                "version": row.version,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
            }),
        },
    ))
}

fn read_file_touch(
    row: CrushReadFileRow,
    raw_source_path: &str,
    fallback: DateTime<Utc>,
) -> (usize, ProviderFileTouchedEnvelope) {
    let occurred_at = provider_timestamp_seconds(row.read_at.map(|value| value as f64), fallback);
    let touch_index = 0x0200_0000_0000_u64.saturating_add(row.rowid.max(0) as u64);
    (
        provider_line_from_index(touch_index),
        ProviderFileTouchedEnvelope {
            provider: CaptureProvider::Crush,
            provider_session_id: row.session_id,
            provider_touch_index: touch_index,
            provider_event_index: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_root: Some(raw_source_path.to_owned()),
            path: row.path,
            change_kind: Some(FileChangeKind::Read),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            occurred_at,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
            metadata: json!({
                "source": "crush_read_files",
                "rowid": row.rowid,
                "read_at": row.read_at,
            }),
        },
    )
}

fn event_index(message: &CrushMessageRow) -> u64 {
    let base = message
        .created_at
        .or(message.updated_at)
        .unwrap_or(message.rowid)
        .max(0) as u64;
    base.saturating_mul(4_096)
        .saturating_add(text_id_index(&message.id, 0) % 4_096)
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
                    for key in ["content", "data", "output"] {
                        if let Some(value) = data.get(key).and_then(provider_value_text) {
                            text.push(value);
                            break;
                        }
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

fn push_json_text(parts: &mut Vec<String>, value: &Value) {
    if let Some(text) = provider_value_text(value).filter(|text| !text.trim().is_empty()) {
        parts.push(text);
    }
}
