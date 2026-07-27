//! Captured-record projection into normalized sessions and events.

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::Store;
use serde_json::json;

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::provider::importer::{
    provider_cursor_stream, provider_scoped_source_uuid, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ExistingSessionEventOutcome,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{native_event, provider_line_from_index, NativeEventDraft};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    DEEPAGENTS_SQLITE_SOURCE_FORMAT,
};

use super::cursor::deepagents_cursor_candidate;
use super::ledger::deepagents_message_identity;
use super::message::{deepagents_messages_from_blob, DeepAgentsMessage};
use super::record::{
    decode_deepagents_thread_values, decode_deepagents_write_values,
    deepagents_decode_event_indices, deepagents_decode_offsets, deepagents_required_text,
    deepagents_required_u64,
};
use super::source::DeepAgentsThread;
use super::{
    DEEPAGENTS_REJECTED_WRITE_RECORD_KIND, DEEPAGENTS_THREAD_RECORD_KIND,
    DEEPAGENTS_WRITE_RECORD_KIND,
};

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsEventDraft {
    pub(super) thread_id: String,
    pub(super) provider_event_index: u64,
    pub(super) cursor: String,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) message: DeepAgentsMessage,
    pub(super) checkpoint_id: String,
    pub(super) task_id: String,
    pub(super) write_idx: i64,
    pub(super) message_offset: usize,
    pub(super) provider_event_identity_index: Option<u64>,
    pub(super) provider_event_hash: String,
}

pub(super) struct DeepAgentsCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) raw_source_path: Option<String>,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: String,
    pub(super) source_revision: String,
    pub(super) committed_store: Option<Store>,
}

impl CapturedBatchProjector for DeepAgentsCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Deep Agents projector requires SQLite logical values",
            ));
        };
        match record.record_kind().as_str() {
            DEEPAGENTS_WRITE_RECORD_KIND => self.project_write(values, output),
            DEEPAGENTS_REJECTED_WRITE_RECORD_KIND => {
                let [row_number, reason] = values.as_slice() else {
                    return Err(ProviderProjectionFatal::system_invariant(
                        "Deep Agents rejected write has an invalid value shape",
                    ));
                };
                output.reject_record(
                    provider_line_from_index(
                        deepagents_required_u64(row_number, "row_number")
                            .map_err(ProviderProjectionFatal::new)?,
                    ),
                    deepagents_required_text(reason, "rejection reason")
                        .map_err(ProviderProjectionFatal::new)?,
                );
                Ok(())
            }
            DEEPAGENTS_THREAD_RECORD_KIND => self.project_thread(values, output),
            _ => Err(ProviderProjectionFatal::system_invariant(
                "Deep Agents projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        deepagents_cursor_candidate(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            deepagents_cursor_candidate(batch.source(), batch.range_end())?,
        ))
    }
}

impl DeepAgentsCapturedBatchProjector {
    fn project_write(
        &self,
        values: &[CapturedSqliteValue],
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let write = decode_deepagents_write_values(values).map_err(ProviderProjectionFatal::new)?;
        let line = provider_line_from_index(write.row_number);
        let Some(occurred_at) = write.occurred_at else {
            output.reject_record(
                line,
                format!(
                    "Deep Agents writes row references unknown thread_id {}",
                    write.key.thread_id
                ),
            );
            return Ok(());
        };
        let messages =
            match deepagents_messages_from_blob(write.value_type.as_deref(), &write.value) {
                Ok(messages) => messages,
                Err(error) => {
                    output.reject_record(line, error.to_string());
                    return Ok(());
                }
            };
        let accepted_offsets = deepagents_decode_offsets(&write.accepted_offsets)
            .map_err(ProviderProjectionFatal::new)?;
        let accepted_event_indices = deepagents_decode_event_indices(&write.accepted_event_indices)
            .map_err(ProviderProjectionFatal::new)?;
        if accepted_offsets.len() != accepted_event_indices.len() {
            return Err(ProviderProjectionFatal::system_invariant(
                "Deep Agents accepted offsets and event indices have different lengths",
            ));
        }
        let mut prior_offset = None;
        let mut prior_event_index = None;
        for (offset, provider_event_index) in
            accepted_offsets.into_iter().zip(accepted_event_indices)
        {
            if prior_offset.is_some_and(|prior| prior >= offset) {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Deep Agents accepted message offsets are not strictly increasing",
                ));
            }
            prior_offset = Some(offset);
            if prior_event_index.is_some_and(|prior| prior >= provider_event_index) {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Deep Agents accepted event indices are not strictly increasing",
                ));
            }
            prior_event_index = Some(provider_event_index);
            let offset_usize = usize::try_from(offset).map_err(|_| {
                ProviderProjectionFatal::system_invariant(
                    "Deep Agents message offset exceeds platform limits",
                )
            })?;
            let message = messages.get(offset_usize).ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Deep Agents accepted message offset is outside the decoded write",
                )
            })?;
            let cursor = format!(
                "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
                write.key.thread_id,
                write.key.checkpoint_id,
                write.key.task_id,
                write.key.idx,
                offset
            );
            let message_identity = message
                .message_id
                .as_deref()
                .map(|message_id| deepagents_message_identity(&write.key.thread_id, message_id));
            let event = DeepAgentsEventDraft {
                thread_id: write.key.thread_id.clone(),
                provider_event_index,
                provider_event_identity_index: message_identity
                    .as_ref()
                    .map(|identity| identity.provider_index),
                provider_event_hash: message_identity
                    .map(|identity| identity.payload_hash)
                    .unwrap_or_else(|| cursor.clone()),
                cursor,
                occurred_at,
                message: message.clone(),
                checkpoint_id: write.key.checkpoint_id.clone(),
                task_id: write.key.task_id.clone(),
                write_idx: write.key.idx,
                message_offset: offset_usize,
            };
            // The authoritative session record is emitted before the write phase. Child records
            // retain only their own source key, timestamp, raw write, and fixed-size acceptance
            // plan; the persistence seam resolves the exact source-scoped parent without
            // overwriting its aggregate metadata.
            let event_thread = DeepAgentsThread {
                thread_id: write.key.thread_id.clone(),
                agent_name: None,
                created_at: occurred_at,
                updated_at: occurred_at,
                latest_checkpoint_id: None,
                git_branch: None,
                cwd: None,
            };
            let outcome = output.emit_existing_session_event(
                line,
                deepagents_capture(
                    &event_thread,
                    Some(&event),
                    &self.context,
                    self.raw_source_path.clone(),
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.source_revision,
                ),
            )?;
            if outcome == ExistingSessionEventOutcome::Rejected {
                return Ok(());
            }
        }
        Ok(())
    }

    fn project_thread(
        &self,
        values: &[CapturedSqliteValue],
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let summary =
            decode_deepagents_thread_values(values).map_err(ProviderProjectionFatal::new)?;
        if let Some(store) = self.committed_store.as_ref() {
            let source_id = provider_scoped_source_uuid(
                CaptureProvider::DeepAgents,
                &summary.thread.thread_id,
                DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                self.raw_source_path.as_deref(),
            );
            if let Some(session) = store
                .session_by_capture_source_and_external_session(
                    source_id,
                    CaptureProvider::DeepAgents,
                    &summary.thread.thread_id,
                )
                .map_err(|error| ProviderProjectionFatal::new(CaptureError::Store(error)))?
            {
                if let Some(capture_source_id) = session.capture_source_id {
                    let capture_source =
                        store
                            .get_capture_source(capture_source_id)
                            .map_err(|error| {
                                ProviderProjectionFatal::new(CaptureError::Store(error))
                            })?;
                    if capture_source.sync.metadata["source_metadata"]
                        ["source_observation_revision"]
                        .as_str()
                        == Some(self.source_revision.as_str())
                    {
                        return Ok(());
                    }
                }
            }
        }
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                0,
                deepagents_capture(
                    &summary.thread,
                    None,
                    &self.context,
                    self.raw_source_path.clone(),
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.source_revision,
                ),
            )],
            ..ProviderNormalizationResult::default()
        })
    }
}

pub(super) fn deepagents_capture(
    thread: &DeepAgentsThread,
    event: Option<&DeepAgentsEventDraft>,
    context: &ProviderAdapterContext,
    raw_source_path: Option<String>,
    sqlite_user_version: i64,
    schema_fingerprint: &str,
    source_observation_revision: &str,
) -> ProviderCaptureEnvelope {
    let observed_at = event
        .map(|event| event.occurred_at)
        .unwrap_or(thread.updated_at);
    let cursor = event.map(|event| event.cursor.clone()).or_else(|| {
        thread
            .latest_checkpoint_id
            .as_ref()
            .map(|checkpoint_id| format!("thread:{}:checkpoint:{checkpoint_id}", thread.thread_id))
    });
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::DeepAgents,
        source: ProviderSourceEnvelope {
            source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at,
            raw_source_path: raw_source_path.clone(),
            source_root: context.source_root_display().or(raw_source_path.clone()),
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Imported,
            cursor: cursor.clone().map(|cursor| ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(
                        CaptureProvider::DeepAgents,
                        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    ),
                    cursor,
                    observed_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:deepagents:{DEEPAGENTS_SQLITE_SOURCE_FORMAT}:{}",
                thread.thread_id
            )),
            metadata: json!({
                "adapter": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": sqlite_user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_observation_revision": source_observation_revision,
                "message_import_policy": "root writes.messages only; checkpoint state blobs are not indexed",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: thread.thread_id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: thread.agent_name.clone(),
            agent_type: AgentType::Primary,
            role_hint: thread
                .agent_name
                .clone()
                .or_else(|| Some("agent".to_owned())),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: thread.created_at,
            ended_at: Some(thread.updated_at),
            cwd: thread.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:deepagents:{}", thread.thread_id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "agent_name": thread.agent_name,
                "git_branch": thread.git_branch,
                "latest_checkpoint_id": thread.latest_checkpoint_id,
                "storage": "LangGraph AsyncSqliteSaver checkpoints/writes",
            }),
        },
        event: event.map(deepagents_event),
    }
}

pub(super) fn deepagents_event(event: &DeepAgentsEventDraft) -> ProviderEventEnvelope {
    let event_type = if event.message.role == EventRole::Tool {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::DeepAgents,
        source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        provider_session_id: event.thread_id.clone(),
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event.provider_event_hash.clone()),
        cursor: event.cursor.clone(),
        event_type,
        role: Some(event.message.role),
        occurred_at: event.occurred_at,
        text: event.message.text.clone(),
        body: json!({
            "message_type": event.message.message_type,
            "message_class": event.message.message_class,
            "message_id": event.message.message_id,
            "checkpoint_id": event.checkpoint_id,
            "task_id": event.task_id,
            "write_idx": event.write_idx,
            "message_offset": event.message_offset,
        }),
        metadata: json!({
            "source": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "checkpoint_id": event.checkpoint_id,
            "task_id": event.task_id,
            "write_idx": event.write_idx,
            "message_offset": event.message_offset,
            "message_type": event.message.message_type,
            "message_class": event.message.message_class,
            "message_id": event.message.message_id,
            "provider_event_identity_index": event.provider_event_identity_index,
            "privacy": "decoded from writes.messages only",
        }),
    })
}
