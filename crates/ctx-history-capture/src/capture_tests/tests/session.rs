#[allow(unused_imports)]
use super::*;

pub(crate) fn provider_collision_capture(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: &str,
    occurred_at: DateTime<Utc>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider,
        source: ProviderSourceEnvelope {
            source_format: source_format.to_owned(),
            machine_id: "test-machine".to_owned(),
            observed_at: occurred_at,
            raw_source_path: Some(raw_source_path.to_owned()),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor: None,
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                provider.as_str(),
                source_format,
                provider_session_id
            )),
            metadata: json!({}),
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
            started_at: occurred_at,
            ended_at: None,
            cwd: Some("/workspace/example".to_owned()),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                provider.as_str(),
                provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: json!({}),
        },
        event: Some(ProviderEventEnvelope {
            provider_event_index: 0,
            provider_event_hash: None,
            cursor: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at,
            fidelity: Fidelity::Imported,
            redaction_state: RedactionState::LocalPreview,
            idempotency_key: Some(format!(
                "provider-event:{}:{}:0",
                provider.as_str(),
                provider_session_id
            )),
            artifacts: Vec::new(),
            payload: json!({"text": "same provider event payload"}),
            metadata: json!({}),
        }),
    }
}

pub(crate) fn provider_collision_file_touch(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: &str,
    occurred_at: DateTime<Utc>,
) -> ProviderFileTouchedEnvelope {
    ProviderFileTouchedEnvelope {
        provider,
        provider_session_id: provider_session_id.to_owned(),
        provider_touch_index: 0,
        provider_event_index: Some(0),
        raw_source_path: Some(raw_source_path.to_owned()),
        path: "src/lib.rs".to_owned(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        occurred_at,
        source_format: source_format.to_owned(),
        metadata: json!({}),
    }
}
