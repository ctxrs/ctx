#[allow(unused_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn codebuddy_capture(
    provider_session_id: &str,
    native_session_id: &str,
    project_hash: &str,
    raw_source_path: &str,
    context: &ProviderAdapterContext,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    title: Option<String>,
    project_index: Option<&Value>,
    conversation: Option<&Value>,
    session_index: &Value,
    file_names: &[&str],
    event: CodeBuddyEventInput,
) -> ProviderCaptureEnvelope {
    let event_envelope = codebuddy_event(provider_session_id, project_hash, &event);
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::CodeBuddy,
        source: ProviderSourceEnvelope {
            source_format: CODEBUDDY_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: Some(raw_source_path.to_owned()),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Imported,
            cursor: Some(ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(
                        CaptureProvider::CodeBuddy,
                        CODEBUDDY_SOURCE_FORMAT,
                    ),
                    cursor: event_envelope
                        .cursor
                        .clone()
                        .unwrap_or_else(|| provider_session_id.to_owned()),
                    observed_at: event_envelope.occurred_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:codebuddy:{CODEBUDDY_SOURCE_FORMAT}:{provider_session_id}"
            )),
            metadata: json!({
                "adapter": CODEBUDDY_SOURCE_FORMAT,
                "native_project_hash": project_hash,
                "native_session_id": native_session_id,
                "files": file_names,
                "schema_proof": "WayLog shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09 src/services/readers/codebuddy-reader.ts",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at,
            ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:codebuddy:{provider_session_id}")),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": CODEBUDDY_SOURCE_FORMAT,
                "provider": CaptureProvider::CodeBuddy.as_str(),
                "display_name": "CodeBuddy",
                "title": title,
                "native_project_hash": project_hash,
                "native_session_id": native_session_id,
                "project_index": project_index.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "conversation": conversation.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "session_index": provider_capped_json(session_index, PROVIDER_MAX_PREVIEW_CHARS),
                "files": file_names,
                "limitations": [
                    "The original project path is represented by CodeBuddy's MD5 project directory when not available in the current IDE workspace",
                    "Message file mtimes are used when native message timestamps are absent",
                    "Non-text content blocks and binary attachments are preserved only in capped native JSON metadata"
                ],
            }),
        },
        event: Some(event_envelope),
    }
}

pub(crate) fn codebuddy_event(
    provider_session_id: &str,
    project_hash: &str,
    event: &CodeBuddyEventInput,
) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&event.text, PROVIDER_MAX_TEXT_CHARS);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    let role = provider_role(event.role.as_deref());
    ProviderEventEnvelope {
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type: EventType::Message,
        role: Some(role),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:codebuddy:{CODEBUDDY_SOURCE_FORMAT}:{event_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": event.ref_type.as_deref().unwrap_or("message"),
            "event_id": event_id,
            "native_project_hash": project_hash,
            "native_message_id": event.native_message_id,
            "text": text,
            "truncated": truncated,
            "body": provider_capped_json(&event.raw_message, PROVIDER_MAX_PREVIEW_CHARS),
            "decoded_body": provider_capped_json(&event.decoded_message, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "codebuddy_messages_json",
            "source_format": CODEBUDDY_SOURCE_FORMAT,
            "native_message_id": event.native_message_id,
            "role": event.role,
            "ref_type": event.ref_type,
            "model": event.decoded_message.get("model").cloned(),
        }),
    }
}
