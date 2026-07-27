use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::position::warp_content_locator;
use super::proto::{warp_decode_task, WarpMessageProto};
use super::sqlite::{
    WARP_CONVERSATION_OVERSIZE_RECORD_KIND, WARP_CONVERSATION_START_RECORD_KIND,
    WARP_ORDERING_KEY_MAX_BYTES, WARP_TASK_INVALID_KEY_RECORD_KIND, WARP_TASK_RECORD_KIND,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::common::time::parse_rfc3339_utc;
use crate::complete_content::sqlite::{
    attach_sqlite_complete_content_locator, attach_sqlite_result_content_locator,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ExistingSessionEventOutcome, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_provider_capture, provider_capped_json, provider_line_from_index,
    provider_local_preview, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    PROVIDER_MAX_PREVIEW_CHARS, WARP_SQLITE_SOURCE_FORMAT,
};

const WARP_RUNTIME_METADATA_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct WarpConversationRow {
    rowid: i64,
    conversation_id: String,
    conversation_data: String,
    last_modified_at: String,
}

#[derive(Debug, Clone)]
struct WarpTaskRow {
    rowid: i64,
    conversation_id: String,
    task_id: String,
    task: Vec<u8>,
    last_modified_at: String,
}

#[derive(Debug, Clone)]
struct WarpConversationCheckpoint {
    conversation_id: String,
    parent_conversation_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct WarpParserCheckpoint {
    pub(super) next_event_index: u64,
}

pub(super) struct WarpCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) raw_source_path: String,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: String,
    pub(super) checkpoint: WarpParserCheckpoint,
}

impl WarpCapturedBatchProjector {
    fn project_conversation(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() == WARP_CONVERSATION_OVERSIZE_RECORD_KIND {
            let (rowid, observed_bytes) = decode_warp_oversize_conversation(record.payload())
                .map_err(ProviderProjectionFatal::new)?;
            output.reject_record(
                warp_line_number(rowid, 0),
                format!(
                    "Warp conversation row exceeds the captured-record limit ({observed_bytes} bytes)"
                ),
            );
            return Ok(());
        }
        let row =
            decode_warp_conversation(record.payload()).map_err(ProviderProjectionFatal::new)?;
        let conversation_modified = match warp_sqlite_timestamp(
            &row.last_modified_at,
            "Warp agent_conversations.last_modified_at",
        ) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                output.reject_record(warp_line_number(row.rowid, 0), error.to_string());
                return Ok(());
            }
        };
        let conversation_data = warp_conversation_data(&row.conversation_data);
        let conversation = warp_conversation_checkpoint(&row, &conversation_data);
        let parent_conversation_id = conversation.parent_conversation_id.clone();
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                provider_line_from_index(record.ordinal().saturating_add(1)),
                warp_capture(
                    &conversation.conversation_id,
                    parent_conversation_id.clone(),
                    parent_conversation_id.is_some(),
                    conversation_modified,
                    conversation_modified,
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    warp_runtime_session_metadata(&conversation_data),
                    None,
                    &self.context,
                ),
            )],
            ..ProviderNormalizationResult::default()
        })
    }

    fn project_task(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() == WARP_TASK_INVALID_KEY_RECORD_KIND {
            let (rowid, observed_bytes) = decode_warp_invalid_task_key(record.payload())
                .map_err(ProviderProjectionFatal::new)?;
            output.reject_record(
                warp_line_number(rowid, 0),
                format!(
                    "Warp task ordering key is not supported or exceeds the \
                     {WARP_ORDERING_KEY_MAX_BYTES}-byte limit ({observed_bytes} bytes)"
                ),
            );
            return Ok(());
        }
        let task_row =
            decode_warp_task_record(record.payload()).map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::SqliteValues(task_values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Warp task record requires SQLite logical values",
            ));
        };
        let task = match warp_decode_task(&task_row.task) {
            Ok(task) => task,
            Err(error) => {
                output.reject_record(
                    warp_line_number(task_row.rowid, 0),
                    format!(
                        "failed to decode Warp agent_tasks.task {}: {error}",
                        task_row.task_id
                    ),
                );
                return Ok(());
            }
        };
        let task_modified = match warp_sqlite_timestamp(
            &task_row.last_modified_at,
            "Warp agent_tasks.last_modified_at",
        ) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                output.reject_record(warp_line_number(task_row.rowid, 0), error.to_string());
                return Ok(());
            }
        };
        let task_id = if task.id.is_empty() {
            task_row.task_id.clone()
        } else {
            task.id.clone()
        };
        let line = provider_line_from_index(record.ordinal().saturating_add(1));
        for (message_index, message) in task.messages.iter().enumerate() {
            if message.text.trim().is_empty()
                && message.complete_text.as_deref().is_none_or(str::is_empty)
            {
                continue;
            }
            let message_index = u64::try_from(message_index).map_err(|_| {
                ProviderProjectionFatal::system_invariant(
                    "Warp task message index exceeds the provider event range",
                )
            })?;
            let message_time = message.timestamp.unwrap_or(task_modified);
            let provider_event_index = self.checkpoint.next_event_index;
            self.checkpoint.next_event_index = self
                .checkpoint
                .next_event_index
                .checked_add(1)
                .ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "Warp global provider event index overflowed",
                    )
                })?;
            let locator_index = u32::try_from(message_index).map_err(|_| {
                ProviderProjectionFatal::system_invariant(
                    "Warp task message index exceeds the locator range",
                )
            })?;
            let content_locator = warp_content_locator(task_row.rowid, locator_index)
                .map_err(ProviderProjectionFatal::new)?;
            let mut event = warp_message_event(
                &task_row.conversation_id,
                &task_id,
                message,
                message_index,
                provider_event_index,
                message_time,
            );
            if let Some(complete_text) = message.complete_text.as_deref() {
                match event.event_type {
                    EventType::Message => attach_sqlite_complete_content_locator(
                        &mut event,
                        CaptureProvider::Warp,
                        WARP_SQLITE_SOURCE_FORMAT,
                        &content_locator,
                        task_values,
                        || complete_text.to_owned(),
                    ),
                    EventType::ToolOutput => attach_sqlite_result_content_locator(
                        &mut event,
                        CaptureProvider::Warp,
                        WARP_SQLITE_SOURCE_FORMAT,
                        &content_locator,
                        task_values,
                        Some(complete_text.to_owned()),
                    ),
                    _ => Ok(()),
                }
                .map_err(ProviderProjectionFatal::new)?;
            }
            let outcome = output.emit_existing_session_event(
                line,
                warp_capture(
                    &task_row.conversation_id,
                    None,
                    false,
                    message_time,
                    task_modified,
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    Value::Null,
                    Some(event),
                    &self.context,
                ),
            )?;
            if outcome == ExistingSessionEventOutcome::Rejected {
                return Ok(());
            }
        }
        Ok(())
    }
}

impl CapturedBatchProjector for WarpCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        match record.record_kind().as_str() {
            WARP_CONVERSATION_START_RECORD_KIND | WARP_CONVERSATION_OVERSIZE_RECORD_KIND => {
                self.project_conversation(record, output)
            }
            WARP_TASK_RECORD_KIND | WARP_TASK_INVALID_KEY_RECORD_KIND => {
                self.project_task(record, output)
            }
            _ => Err(ProviderProjectionFatal::system_invariant(
                "Warp projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&WarpParserCheckpoint::default())?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&self.checkpoint)?,
            )?,
        ))
    }
}

fn decode_warp_conversation(payload: &CapturedRecordPayload) -> Result<WarpConversationRow> {
    let CapturedRecordPayload::SqliteValues(values) = payload else {
        return Err(CaptureError::SystemInvariant(
            "Warp conversation record requires SQLite logical values",
        ));
    };
    let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Text(conversation_id), CapturedSqliteValue::Text(conversation_data), CapturedSqliteValue::Text(last_modified_at)] =
        values.as_slice()
    else {
        return Err(CaptureError::SystemInvariant(
            "Warp conversation logical row has an invalid value shape",
        ));
    };
    Ok(WarpConversationRow {
        rowid: *rowid,
        conversation_id: conversation_id.clone(),
        conversation_data: conversation_data.clone(),
        last_modified_at: last_modified_at.clone(),
    })
}

fn decode_warp_oversize_conversation(payload: &CapturedRecordPayload) -> Result<(i64, i64)> {
    let CapturedRecordPayload::SqliteValues(values) = payload else {
        return Err(CaptureError::SystemInvariant(
            "Warp oversize conversation requires SQLite logical values",
        ));
    };
    let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Integer(observed_bytes)] =
        values.as_slice()
    else {
        return Err(CaptureError::SystemInvariant(
            "Warp oversize conversation has an invalid value shape",
        ));
    };
    Ok((*rowid, *observed_bytes))
}

pub(super) fn decode_warp_invalid_task_key(payload: &CapturedRecordPayload) -> Result<(i64, i64)> {
    let CapturedRecordPayload::SqliteValues(values) = payload else {
        return Err(CaptureError::SystemInvariant(
            "Warp invalid task key requires SQLite logical values",
        ));
    };
    let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Integer(observed_bytes)] =
        values.as_slice()
    else {
        return Err(CaptureError::SystemInvariant(
            "Warp invalid task key has an invalid value shape",
        ));
    };
    Ok((*rowid, *observed_bytes))
}

fn decode_warp_task_record(payload: &CapturedRecordPayload) -> Result<WarpTaskRow> {
    let CapturedRecordPayload::SqliteValues(values) = payload else {
        return Err(CaptureError::SystemInvariant(
            "Warp task record requires SQLite logical values",
        ));
    };
    let [CapturedSqliteValue::Integer(task_rowid), CapturedSqliteValue::Text(conversation_id), CapturedSqliteValue::Text(task_id), CapturedSqliteValue::Blob(task), CapturedSqliteValue::Text(task_modified)] =
        values.as_slice()
    else {
        return Err(CaptureError::SystemInvariant(
            "Warp task logical row has an invalid value shape",
        ));
    };
    Ok(WarpTaskRow {
        rowid: *task_rowid,
        conversation_id: conversation_id.clone(),
        task_id: task_id.clone(),
        task: task.clone(),
        last_modified_at: task_modified.clone(),
    })
}

fn warp_conversation_data(raw: &str) -> Value {
    serde_json::from_str(raw)
        .unwrap_or_else(|_| json!({ "parse_error": "invalid conversation_data JSON" }))
}

fn warp_conversation_checkpoint(
    row: &WarpConversationRow,
    conversation_data: &Value,
) -> WarpConversationCheckpoint {
    let parent_conversation_id = conversation_data
        .get("parent_conversation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    WarpConversationCheckpoint {
        conversation_id: row.conversation_id.clone(),
        parent_conversation_id,
    }
}

fn warp_runtime_session_metadata(conversation_data: &Value) -> Value {
    let bounded_value = |field: &str| {
        conversation_data.get(field).map_or(Value::Null, |value| {
            if serde_json::to_vec(value)
                .is_ok_and(|encoded| encoded.len() <= PROVIDER_MAX_PREVIEW_CHARS)
            {
                value.clone()
            } else {
                provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
            }
        })
    };
    let agent_name = conversation_data
        .get("agent_name")
        .and_then(Value::as_str)
        .map(|value| provider_local_preview(value, PROVIDER_MAX_PREVIEW_CHARS).0);
    let mut metadata = json!({
        "source_format": WARP_SQLITE_SOURCE_FORMAT,
        "title": agent_name.clone().unwrap_or_else(|| "Warp conversation".to_owned()),
        "agent_name": agent_name,
        "parent_conversation_id": bounded_value("parent_conversation_id"),
        "run_id": bounded_value("run_id"),
        "server_conversation_token_present": warp_nonempty_string_field(
            conversation_data,
            "server_conversation_token",
        ),
        "forked_from_server_conversation_token_present": warp_nonempty_string_field(
            conversation_data,
            "forked_from_server_conversation_token",
        ),
        "conversation_usage_metadata": bounded_value("conversation_usage_metadata"),
        "task_summaries": [],
    });
    if serde_json::to_vec(&metadata)
        .is_ok_and(|encoded| encoded.len() > WARP_RUNTIME_METADATA_MAX_BYTES)
    {
        metadata["conversation_usage_metadata"] =
            json!({ "truncated": true, "reason": "bounded_runtime_metadata" });
    }
    metadata
}

#[allow(clippy::too_many_arguments)]
pub(super) fn warp_capture(
    conversation_id: &str,
    parent_conversation_id: Option<String>,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    session_metadata: Value,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Warp,
            source_format: WARP_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation_id.to_owned(),
            parent_provider_session_id: parent_conversation_id.clone(),
            root_provider_session_id: parent_conversation_id,
            external_agent_id: Some("warp-agent".to_owned()),
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at,
            ended_at: Some(ended_at),
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": WARP_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
                "upstream_schema_anchor": {
                    "repository": "warpdotdev/warp",
                    "files": [
                        "crates/persistence/src/schema.rs",
                        "crates/persistence/src/model.rs",
                        "app/src/persistence/agent.rs"
                    ],
                    "proto_repository": "warpdotdev/warp-proto-apis",
                    "proto_files": ["apis/multi_agent/v1/task.proto"]
                },
            }),
            session_metadata,
        },
        context,
        event,
    )
}

fn warp_nonempty_string_field(value: &Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

pub(super) fn warp_sqlite_timestamp(raw: &str, field: &'static str) -> Result<DateTime<Utc>> {
    if let Some(timestamp) = parse_rfc3339_utc(raw) {
        return Ok(timestamp);
    }
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f").map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} is not a supported timestamp: {raw:?}"))
    })?;
    Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

pub(super) fn warp_line_number(rowid: i64, index: u64) -> usize {
    let row = u64::try_from(rowid.max(0)).unwrap_or(0);
    provider_line_from_index(row.saturating_mul(100_000).saturating_add(index))
}

pub(super) fn warp_message_event(
    conversation_id: &str,
    task_id: &str,
    message: &WarpMessageProto,
    message_index: u64,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let body = json!({
        "text": message.text,
        "message_index": message_index,
    });
    let retained_text = provider_policy_event_text(message.event_type, &message.text, &body);
    let result_evidence =
        provider_result_identifier_evidence(message.event_type, &message.text, &body);
    let result_outcome = provider_result_outcome_evidence(message.event_type, &body);
    let message_id = if message.id.is_empty() {
        format!("{task_id}:{message_index}")
    } else {
        message.id.clone()
    };
    let provider_event_identity_index =
        warp_message_identity_index(conversation_id, task_id, &message_id);
    ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: Some(message_id.clone()),
        cursor: Some(format!("agent_task:{task_id}:message:{message_index}")),
        event_type: message.event_type,
        role: message.role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!(
            "provider-event:warp:{conversation_id}:{message_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "kind": message.kind,
            "message_id": message_id,
            "task_id": task_id,
            "request_id": if message.request_id.is_empty() { Value::Null } else { json!(message.request_id) },
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_policy_body(message.event_type, &body),
        }),
        metadata: json!({
            "source": WARP_SQLITE_SOURCE_FORMAT,
            "source_format": WARP_SQLITE_SOURCE_FORMAT,
            "message_kind": message.kind,
            "task_id": task_id,
            "proto_task_id": if message.task_id.is_empty() { Value::Null } else { json!(message.task_id) },
            "request_id": if message.request_id.is_empty() { Value::Null } else { json!(message.request_id) },
            "provider_event_identity_index": provider_event_identity_index,
        }),
    }
}

pub(super) fn warp_message_identity_index(
    conversation_id: &str,
    task_id: &str,
    message_id: &str,
) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        b"ctx-warp-message-v1".as_slice(),
        conversation_id.as_bytes(),
        task_id.as_bytes(),
        message_id.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
