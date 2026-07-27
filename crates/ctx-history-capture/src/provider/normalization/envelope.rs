use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use super::content_policy::{provider_policy_body, provider_policy_event_text};
use super::result_evidence::{
    provider_result_identifier_evidence, provider_result_outcome_evidence,
};
use super::value::provider_capped_json;
use crate::provider::importer::provider_cursor_stream;
use crate::{ProviderAdapterContext, PROVIDER_MAX_PREVIEW_CHARS};

#[derive(Clone)]
pub(crate) struct NativeSessionDraft {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: Option<String>,
    pub(crate) is_primary: bool,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) fidelity: Fidelity,
    pub(crate) raw_source_path: String,
    pub(crate) trust: ProviderSourceTrust,
    pub(crate) source_metadata: Value,
    pub(crate) session_metadata: Value,
}

pub(crate) fn native_provider_capture(
    draft: NativeSessionDraft,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: draft.provider,
        source: ProviderSourceEnvelope {
            source_format: draft.source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: Some(draft.raw_source_path),
            source_root: context.source_root_display(),
            trust: draft.trust,
            fidelity: draft.fidelity,
            cursor: event.as_ref().and_then(|event| {
                event.cursor.as_ref().map(|cursor| ProviderCursorRange {
                    before: None,
                    after: Some(ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(draft.provider, draft.source_format),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    }),
                })
            }),
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                draft.provider.as_str(),
                draft.source_format,
                draft.provider_session_id
            )),
            metadata: draft.source_metadata,
        },
        session: ProviderSessionEnvelope {
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id,
            root_provider_session_id: draft.root_provider_session_id,
            external_agent_id: draft.external_agent_id,
            agent_type: draft.agent_type,
            role_hint: draft.role_hint,
            is_primary: draft.is_primary,
            status: SessionStatus::Imported,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: draft.fidelity,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                draft.provider.as_str(),
                draft.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: draft.session_metadata,
        },
        event,
    }
}

pub(crate) struct NativeEventDraft {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) body: Value,
    pub(crate) metadata: Value,
}

pub(crate) fn native_event(draft: NativeEventDraft) -> ProviderEventEnvelope {
    let retained_text = provider_policy_event_text(draft.event_type, &draft.text, &draft.body);
    let body = provider_policy_body(draft.event_type, &draft.body);
    let result_evidence =
        provider_result_identifier_evidence(draft.event_type, &draft.text, &draft.body);
    let result_outcome = provider_result_outcome_evidence(draft.event_type, &draft.body);
    ProviderEventEnvelope {
        provider_event_index: draft.provider_event_index,
        provider_event_hash: draft.provider_event_hash,
        cursor: Some(draft.cursor),
        event_type: draft.event_type,
        role: draft.role,
        occurred_at: draft.occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            draft.provider.as_str(),
            draft.provider_session_id,
            draft.provider_event_index
        )),
        artifacts: Vec::new(),
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": draft.source_format,
            "body": provider_capped_json(&body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: draft.metadata,
    }
}
