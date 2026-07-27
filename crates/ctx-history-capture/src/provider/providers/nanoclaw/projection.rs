use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_json_text, provider_nonnegative_i64_to_u64,
    provider_timestamp_millis, provider_value_text, text_id_index, NativeEventDraft,
    NativeSessionDraft,
};
use crate::provider::provider_safe_path_segment;
use crate::{
    fnv1a64, CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    NANOCLAW_SOURCE_FORMAT,
};

use super::position::decode_nanoclaw_position;
use super::rows::{
    decode_nanoclaw_message_record, decode_nanoclaw_session, NanoClawMessageRow, NanoClawSessionRow,
};
use super::{NANOCLAW_MESSAGE_RECORD_KIND, NANOCLAW_SESSION_RECORD_KIND};

pub(super) struct NanoClawCapturedBatchProjector {
    context: ProviderAdapterContext,
    raw_source_path: String,
    central_path: String,
    user_version: i64,
    schema_fingerprint: String,
}

impl NanoClawCapturedBatchProjector {
    pub(super) fn new(
        context: ProviderAdapterContext,
        raw_source_path: String,
        central_path: String,
        user_version: i64,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            raw_source_path,
            central_path,
            user_version,
            schema_fingerprint,
        }
    }
}

impl CapturedBatchProjector for NanoClawCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "NanoClaw projector requires SQLite logical values",
            ));
        };
        match record.record_kind().as_str() {
            NANOCLAW_SESSION_RECORD_KIND => {
                let session =
                    decode_nanoclaw_session(values).map_err(ProviderProjectionFatal::new)?;
                if !provider_safe_path_segment(&session.agent_group_id)
                    || !provider_safe_path_segment(&session.id)
                {
                    output.reject_record(
                        nanoclaw_record_line(record.ordinal()),
                        "NanoClaw session identifiers are not safe path segments".to_owned(),
                    );
                }
                Ok(())
            }
            NANOCLAW_MESSAGE_RECORD_KIND => {
                let (message, session) =
                    decode_nanoclaw_message_record(values).map_err(ProviderProjectionFatal::new)?;
                let seq = match message
                    .seq
                    .map(|seq| provider_nonnegative_i64_to_u64(seq, "NanoClaw message seq"))
                    .transpose()
                {
                    Ok(seq) => seq,
                    Err(error) => {
                        output.reject_record(
                            nanoclaw_record_line(record.ordinal()),
                            error.to_string(),
                        );
                        return Ok(());
                    }
                };
                output.emit_normalization(nanoclaw_message_normalization(
                    &session,
                    message,
                    seq,
                    NanoClawNormalizationContext {
                        project_root: &self.raw_source_path,
                        central_path: &self.central_path,
                        user_version: self.user_version,
                        schema_fingerprint: &self.schema_fingerprint,
                        adapter: &self.context,
                    },
                ))
            }
            _ => Err(ProviderProjectionFatal::system_invariant(
                "NanoClaw projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_nanoclaw_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "NanoClaw initial cursor candidate is not at the SQLite source start".to_owned(),
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

struct NanoClawNormalizationContext<'a> {
    project_root: &'a str,
    central_path: &'a str,
    user_version: i64,
    schema_fingerprint: &'a str,
    adapter: &'a ProviderAdapterContext,
}

fn nanoclaw_message_normalization(
    session: &NanoClawSessionRow,
    message: NanoClawMessageRow,
    seq: Option<u64>,
    normalization: NanoClawNormalizationContext<'_>,
) -> ProviderNormalizationResult {
    let NanoClawNormalizationContext {
        project_root,
        central_path,
        user_version,
        schema_fingerprint,
        adapter: context,
    } = normalization;
    let provider_session_id = format!("{}/{}", session.agent_group_id, session.id);
    let occurred_at = provider_timestamp_millis(message.timestamp, context.imported_at);
    let started_at = provider_timestamp_millis(session.created_at, occurred_at);
    let content = message
        .content
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let text = provider_value_text(&content).unwrap_or_else(|| {
        format!(
            "NanoClaw {}",
            message.kind.as_deref().unwrap_or(message.source)
        )
    });
    let event_index = nanoclaw_event_index(&message, seq);
    let role = if message.source == "inbound" {
        Some(EventRole::User)
    } else {
        Some(EventRole::Assistant)
    };
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT,
        provider_session_id: provider_session_id.clone(),
        provider_event_index: event_index,
        provider_event_hash: Some(format!("{}:{}", message.source, message.id)),
        cursor: format!(
            "{}:{}:{}",
            message.source,
            session.id,
            message.seq.unwrap_or_default()
        ),
        event_type: EventType::Message,
        role,
        occurred_at,
        text,
        body: json!({
            "message_id": message.id,
            "seq": message.seq,
            "kind": message.kind,
            "content": content,
            "status": message.status,
            "in_reply_to": message.in_reply_to,
            "platform_id": message.platform_id,
            "channel_type": message.channel_type,
            "thread_id": message.thread_id,
            "trigger": message.trigger,
            "source_session_id": message.source_session_id,
            "on_wake": message.on_wake,
        }),
        metadata: json!({
            "source": format!("nanoclaw_{}", message.source),
            "source_format": NANOCLAW_SOURCE_FORMAT,
            "message_id": message.id,
            "seq": message.seq,
        }),
    });
    ProviderNormalizationResult {
        captures: vec![(
            event_index.min(usize::MAX as u64) as usize,
            native_provider_capture(
                NativeSessionDraft {
                    provider: CaptureProvider::NanoClaw,
                    source_format: NANOCLAW_SOURCE_FORMAT,
                    provider_session_id,
                    parent_provider_session_id: None,
                    root_provider_session_id: None,
                    external_agent_id: session.agent_provider.clone(),
                    agent_type: AgentType::Primary,
                    role_hint: Some("container-session".to_owned()),
                    is_primary: true,
                    started_at,
                    ended_at: session.last_active.map(|timestamp| {
                        provider_timestamp_millis(Some(timestamp), context.imported_at)
                    }),
                    cwd: session.agent_group_folder.clone(),
                    fidelity: Fidelity::Partial,
                    raw_source_path: project_root.to_owned(),
                    trust: ProviderSourceTrust::ProviderNative,
                    source_metadata: json!({
                        "adapter": NANOCLAW_SOURCE_FORMAT,
                        "central_db": central_path,
                        "sqlite_user_version": user_version,
                        "schema_fingerprint": schema_fingerprint,
                        "support_level": "explicit",
                    }),
                    session_metadata: json!({
                        "source_format": NANOCLAW_SOURCE_FORMAT,
                        "session_id": session.id,
                        "agent_group_id": session.agent_group_id,
                        "agent_group_name": session.agent_group_name,
                        "agent_provider": session.agent_provider,
                        "status": session.status,
                        "container_status": session.container_status,
                        "messaging_group_id": session.messaging_group_id,
                        "messaging": {
                            "channel_type": session.messaging_channel_type,
                            "platform_id": session.messaging_platform_id,
                            "instance": session.messaging_instance,
                            "name": session.messaging_name,
                            "thread_id": session.thread_id,
                        },
                    }),
                },
                context,
                Some(event),
            ),
        )],
        ..ProviderNormalizationResult::default()
    }
}

pub(super) fn nanoclaw_event_index(message: &NanoClawMessageRow, seq: Option<u64>) -> u64 {
    if let Some(seq) = seq {
        let source_bucket = if message.source == "outbound" {
            500_000
        } else {
            0
        };
        let row_bucket = fnv1a64(format!("{}:{}", message.source, message.id).as_bytes()) % 500_000;
        return seq
            .saturating_mul(1_000_000)
            .saturating_add(source_bucket)
            .saturating_add(row_bucket);
    }
    text_id_index(&format!("{}:{}", message.source, message.id), 2_000_000_000)
}

fn nanoclaw_record_line(ordinal: u64) -> usize {
    ordinal
        .min(usize::MAX.saturating_sub(1) as u64)
        .saturating_add(1) as usize
}
