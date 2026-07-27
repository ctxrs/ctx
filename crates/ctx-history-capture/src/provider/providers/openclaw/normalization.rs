use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::{
    provider::normalization::{
        native_event, native_provider_capture, provider_capped_json, provider_role,
        provider_value_text, NativeEventDraft, NativeSessionDraft,
    },
    ProviderAdapterContext, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(super) struct OpenClawCaptureDraft<'a> {
    pub(super) provider_session_id: &'a str,
    pub(super) agent_id: Option<&'a str>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) path: &'a Path,
    pub(super) index: Value,
    pub(super) header_raw: Value,
    pub(super) event: Option<ProviderEventEnvelope>,
}

pub(super) fn capture(
    draft: OpenClawCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let OpenClawCaptureDraft {
        provider_session_id,
        agent_id,
        started_at,
        ended_at,
        cwd,
        path,
        index,
        header_raw,
        event,
    } = draft;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::OpenClaw,
            source_format: OPENCLAW_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: index
                .get("parentSessionId")
                .or_else(|| index.get("parent_session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            root_provider_session_id: None,
            external_agent_id: agent_id.map(str::to_owned),
            agent_type: AgentType::Primary,
            role_hint: Some("personal-agent".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": OPENCLAW_SOURCE_FORMAT,
                "index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "header": provider_capped_json(&header_raw, PROVIDER_MAX_PREVIEW_CHARS),
                "support_level": "beta",
            }),
            session_metadata: json!({
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "agent_id": agent_id,
                "session_index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "fidelity_gap": "OpenClaw session JSONL is current native storage, but upstream keeps a storage-neutral accessor for future schema changes",
            }),
        },
        context,
        event,
    )
}

pub(crate) fn event(
    provider_session_id: &str,
    event_index: u64,
    line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let row_type = row.get("type").and_then(Value::as_str).unwrap_or("message");
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" => match role {
            Some(EventRole::Tool) => EventType::ToolOutput,
            _ => EventType::Message,
        },
        "leaf" | "compaction" | "custom" => EventType::Notice,
        _ => EventType::Notice,
    };
    let text = message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_value_text)
        .unwrap_or_default();
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenClaw,
        source_format: OPENCLAW_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        cursor: format!("line:{line_number}"),
        event_type,
        role,
        occurred_at,
        text,
        body: row.clone(),
        metadata: json!({
            "source": "openclaw_jsonl",
            "source_format": OPENCLAW_SOURCE_FORMAT,
            "row_type": row_type,
            "message_id": row.get("id").and_then(Value::as_str),
            "parent_id": row.get("parentId").or_else(|| row.get("parent_id")).cloned(),
        }),
    })
}
