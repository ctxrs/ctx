use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::compute_payload_hash;
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_capped_json_value, provider_value_text,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{ProviderAdapterContext, FORGECODE_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

use super::event::{
    forgecode_event_role, forgecode_event_type, forgecode_message_parts, forgecode_message_text,
    forgecode_role_text, forgecode_text_body,
};
use super::source::ForgeCodeConversationRow;

pub(super) struct ForgeCodeCaptureContext<'a> {
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) raw_source_path: &'a str,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: &'a str,
    pub(super) context_value: Option<&'a Value>,
    pub(super) metrics_value: Option<&'a Value>,
    pub(super) event: Option<ProviderEventEnvelope>,
}

pub(super) fn forgecode_capture(
    row: &ForgeCodeConversationRow,
    draft: ForgeCodeCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let context_message_count = draft
        .context_value
        .and_then(|value| value.get("messages"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::ForgeCode,
            source_format: FORGECODE_SQLITE_SOURCE_FORMAT,
            provider_session_id: row.conversation_id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: draft
                .context_value
                .and_then(|value| value.get("initiator"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": FORGECODE_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "upstream_tables": ["conversations"],
                "upstream_schema_anchor": "crates/forge_repo/src/database/migrations/2025-09-12-065405_create_conversations_table/up.sql",
                "upstream_dto_anchor": "crates/forge_repo/src/conversation/conversation_record.rs",
            }),
            session_metadata: json!({
                "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
                "conversation_id": row.conversation_id,
                "title": row.title,
                "workspace_id": row.workspace_id,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
                "context_conversation_id": draft.context_value
                    .and_then(|value| value.get("conversation_id"))
                    .and_then(Value::as_str),
                "initiator": draft.context_value
                    .and_then(|value| value.get("initiator"))
                    .and_then(Value::as_str),
                "context_message_count": context_message_count,
                "tools_count": draft.context_value
                    .and_then(|value| value.get("tools"))
                    .and_then(Value::as_array)
                    .map(Vec::len),
                "tool_choice": draft.context_value
                    .and_then(|value| value.get("tool_choice"))
                    .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "context": draft.context_value
                    .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "metrics": draft.metrics_value
                    .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "limitations": [
                    "ForgeCode stores conversation messages as a context JSON snapshot; message cursors use array index because the DTO does not expose stable message ids",
                    "recognized text, tool call, tool result, image, usage, and metrics fields are normalized; unrecognized DTO fields are retained as capped raw JSON metadata",
                    "workspace_id is retained, but the current Forge schema does not keep a workspace path after the workspace table was dropped"
                ],
            }),
        },
        context,
        draft.event,
    )
}

pub(super) fn forgecode_event(
    row: &ForgeCodeConversationRow,
    entry: &Value,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let parts = forgecode_message_parts(entry);
    let event_type = forgecode_event_type(parts);
    let role = forgecode_event_role(parts);
    let text = forgecode_message_text(parts, event_type);
    let message_hash = compute_payload_hash(entry).ok();
    native_event(NativeEventDraft {
        provider: CaptureProvider::ForgeCode,
        source_format: FORGECODE_SQLITE_SOURCE_FORMAT,
        provider_session_id: row.conversation_id.clone(),
        provider_event_index,
        provider_event_hash: message_hash,
        cursor: format!(
            "conversation:{}:message:{}",
            row.conversation_id, provider_event_index
        ),
        event_type,
        role,
        occurred_at,
        text,
        body: json!({
            "message_index": provider_event_index,
            "message_variant": parts.variant,
            "message": entry,
            "usage": parts.usage,
        }),
        metadata: json!({
            "source": "forgecode_conversations",
            "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
            "conversation_id": row.conversation_id,
            "message_index": provider_event_index,
            "message_variant": parts.variant,
            "role": forgecode_role_text(parts),
            "model": forgecode_text_body(parts)
                .and_then(|body| body.get("model"))
                .and_then(provider_value_text),
            "usage": parts.usage
                .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
        }),
    })
}
