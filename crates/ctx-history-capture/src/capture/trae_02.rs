#[allow(unused_imports)]
use super::*;

pub(crate) fn trae_event(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    event: &TraeEventInput,
) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&event.text, PROVIDER_MAX_TEXT_CHARS);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    ProviderEventEnvelope {
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(format!("{chat_key}:{event_id}")),
        event_type: EventType::Message,
        role: Some(provider_role(event.role.as_deref())),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Partial,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{event_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "event_id": event_id,
            "native_workspace_id": workspace_id,
            "native_message_id": event.native_message_id,
            "text": text,
            "truncated": truncated,
            "body": provider_capped_json(&event.raw_message, PROVIDER_MAX_PREVIEW_CHARS),
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
