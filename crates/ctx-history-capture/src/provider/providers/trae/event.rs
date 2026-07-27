use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::complete_content::CompleteContentBodyDigest;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
};
use crate::provider::providers::task_json::{task_json_string_field, task_json_time_field};
use crate::{
    captured_batch::NativeLocator, ProviderAdapterContext, Result, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{TRAE_CN_INPUT_HISTORY_KEY, TRAE_STATE_VSCDB_SOURCE_FORMAT};

#[derive(Debug, Clone)]
pub(super) struct TraeEventInput {
    pub(super) line_number: usize,
    pub(super) provider_event_index: u64,
    pub(super) native_message_id: String,
    pub(super) role: Option<String>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) raw_message: Value,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trae_event_from_owned_message(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    message: Value,
    message_index: usize,
    fallback_time: DateTime<Utc>,
    line_base: usize,
) -> Option<TraeEventInput> {
    let text = trae_message_text(&message)?;
    if text.trim().is_empty() {
        return None;
    }
    let native_message_id = task_json_string_field(
        &message,
        &[
            "id",
            "messageId",
            "message_id",
            "uuid",
            "requestId",
            "responseId",
        ],
    )
    .unwrap_or_else(|| format!("{workspace_id}:{provider_session_id}:{chat_key}:{message_index}"));
    let occurred_at = task_json_time_field(
        &message,
        &["createdAt", "created_at", "timestamp", "time", "date"],
    )
    .unwrap_or(fallback_time);
    let mut role = task_json_string_field(&message, &["role", "type", "sender"]);
    if chat_key == TRAE_CN_INPUT_HISTORY_KEY && role.is_none() {
        role = Some("user".to_owned());
    }
    Some(TraeEventInput {
        line_number: line_base.saturating_add(message_index).saturating_add(1),
        provider_event_index: message_index as u64,
        native_message_id,
        role,
        occurred_at,
        text,
        raw_message: message,
    })
}

pub(super) fn trae_message_text(message: &Value) -> Option<String> {
    for field in [
        "content",
        "inputText",
        "text",
        "message",
        "summary",
        "answer",
        "query",
        "parsedQuery",
    ] {
        if let Some(text) = message.get(field).and_then(trae_content_text) {
            return Some(text);
        }
    }
    message
        .pointer("/data/summary")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(super) fn trae_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_owned()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(trae_content_text)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(map) => {
            for field in ["text", "content", "value", "summary"] {
                if let Some(text) = map.get(field).and_then(trae_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

pub(super) struct TraeCaptureInput<'a> {
    pub(super) provider_session_id: &'a str,
    pub(super) native_session_id: &'a str,
    pub(super) workspace_id: &'a str,
    pub(super) workspace_folder: Option<&'a str>,
    pub(super) raw_source_path: &'a str,
    pub(super) chat_key: &'a str,
    pub(super) session: &'a Value,
    pub(super) context: &'a ProviderAdapterContext,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) title: Option<String>,
    pub(super) event: TraeEventInput,
    pub(super) complete_content_locator: Option<NativeLocator>,
    pub(super) complete_content_record_digest: Option<CompleteContentBodyDigest>,
}

pub(super) fn trae_capture(input: TraeCaptureInput<'_>) -> Result<ProviderCaptureEnvelope> {
    let mut event_envelope = trae_event(
        input.provider_session_id,
        input.workspace_id,
        input.chat_key,
        &input.event,
    );
    if let (Some(locator), Some(record_digest)) = (
        input.complete_content_locator.as_ref(),
        input.complete_content_record_digest.as_ref(),
    ) {
        crate::complete_content::sqlite::attach_sqlite_native_content_locator(
            &mut event_envelope,
            CaptureProvider::Trae,
            TRAE_STATE_VSCDB_SOURCE_FORMAT,
            locator,
            record_digest,
            &input.event.text,
        )?;
    }
    Ok(ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Trae,
        source: ProviderSourceEnvelope {
            source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned(),
            machine_id: input.context.machine_id.clone(),
            observed_at: input.context.imported_at,
            raw_source_path: Some(input.raw_source_path.to_owned()),
            source_root: input
                .context
                .source_root_display()
                .or_else(|| Some(input.raw_source_path.to_owned())),
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Partial,
            cursor: Some(ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(
                        CaptureProvider::Trae,
                        TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    ),
                    cursor: event_envelope
                        .cursor
                        .clone()
                        .unwrap_or_else(|| input.provider_session_id.to_owned()),
                    observed_at: event_envelope.occurred_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{}",
                input.provider_session_id
            )),
            metadata: json!({
                "adapter": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                "chat_key": input.chat_key,
                "native_workspace_id": input.workspace_id,
                "schema_proof": "yuanjing001/trae-chats-exporter src/extension.ts and src/utils.ts read Trae User/workspaceStorage/*/state.vscdb ItemTable keys",
                "native_auto_scope": "Trae and Trae CN User/workspaceStorage roots with known ItemTable chat keys",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: input.provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: input.started_at,
            ended_at: input.ended_at,
            cwd: input.workspace_folder.map(str::to_owned),
            fidelity: Fidelity::Partial,
            idempotency_key: Some(format!(
                "provider-session:trae:{}",
                input.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                "provider": CaptureProvider::Trae.as_str(),
                "display_name": "Trae",
                "title": input.title,
                "native_workspace_id": input.workspace_id,
                "native_session_id": input.native_session_id,
                "workspace_folder": input.workspace_folder,
                "chat_key": input.chat_key,
                "session": provider_capped_json(
                    &trae_session_metadata_preview(input.session),
                    PROVIDER_MAX_PREVIEW_CHARS,
                ),
                "limitations": [
                    "Importer is based on public exporter source and synthetic fixture; no real local Trae run fixture is bundled",
                    "Only known Trae and Trae CN ItemTable chat keys and direct message arrays are imported",
                    "Trae CN input-history rows are usually user prompts only and may not include assistant replies"
                ],
            }),
        },
        event: Some(event_envelope),
    })
}

fn trae_session_metadata_preview(session: &Value) -> Value {
    provider_policy_body(EventType::Notice, &trae_session_metadata_source(session))
}

fn trae_session_metadata_source(session: &Value) -> Value {
    let Value::Object(object) = session else {
        return session.clone();
    };
    let mut preview = serde_json::Map::new();
    for (key, value) in object {
        if !["messages", "chatMessages", "bubbles", "items"].contains(&key.as_str()) {
            preview.insert(key.clone(), value.clone());
        }
    }
    Value::Object(preview)
}

pub(super) fn trae_event(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    event: &TraeEventInput,
) -> ProviderEventEnvelope {
    let event_type = EventType::Message;
    let retained_text = provider_policy_event_text(event_type, &event.text, &event.raw_message);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &event.text, &event.raw_message);
    let result_outcome = provider_result_outcome_evidence(event_type, &event.raw_message);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    ProviderEventEnvelope {
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(format!("{chat_key}:{event_id}")),
        event_type,
        role: Some(provider_role(event.role.as_deref())),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Partial,
        idempotency_key: Some(format!(
            "provider-event:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{event_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "event_id": event_id,
            "native_workspace_id": workspace_id,
            "native_message_id": event.native_message_id,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &event.raw_message), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "trae_state_vscdb_itemtable",
            "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
            "chat_key": chat_key,
            "native_message_id": event.native_message_id,
            "role": event.role,
            "model": task_json_string_field(&event.raw_message, &["model", "modelType", "model_id"]),
        }),
    }
}
