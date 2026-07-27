use super::*;

pub(super) fn prompt_event(
    ordinal: u64,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    text: String,
) -> CodexNativeEvent {
    CodexNativeEvent {
        provider_event_index: ordinal,
        provider_event_hash: None,
        cursor: Some(format!("line:{line_number}")),
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at,
        fidelity: Fidelity::SummaryOnly,
        idempotency_key: Some(format!("provider-event:codex:prompt-history:{ordinal}")),
        artifacts: Vec::new(),
        payload: json!({
            "text": text,
            "source_format": SOURCE_FORMAT,
            "nativepath_schema": 1,
        }),
        metadata: json!({
            "source": "codex_history",
            "source_format": SOURCE_FORMAT,
            "source_fidelity": "prompt_log_only",
        }),
    }
}

pub(super) fn capture_source(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
    options: &CodexHistoryImportOptions,
    started_at: Option<DateTime<Utc>>,
) -> CaptureSource {
    CaptureSource {
        id: cursor.capture_source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: options.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_format: Some(SOURCE_FORMAT.to_owned()),
            source_root: Some(authority.raw_source_path.clone()),
            source_identity: Some(cursor.canonical_source_identity.clone()),
            external_session_id: None,
        },
        started_at: started_at.unwrap_or(options.imported_at),
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::SummaryOnly,
            json!({
                "source_format": SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": options.imported_at,
                "source_identity": cursor.canonical_source_identity,
                "source_root": authority.raw_source_path,
                "source_revision": cursor.source_revision,
                "nativepath_publication": "codex-prompt-history-v1",
            }),
        ),
    }
}

pub(super) fn session(
    source_id: Uuid,
    id: Uuid,
    native_id: &str,
    started_at: DateTime<Utc>,
    options: &CodexHistoryImportOptions,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some(native_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::SummaryOnly,
            json!({
                "provider_session_id": native_id,
                "source_format": SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": options.imported_at,
                "session_idempotency_key": format!("provider-session:codex:{native_id}"),
                "metadata": {
                    "source_format": SOURCE_FORMAT,
                    "source_fidelity": "prompt_log_only",
                    "limitations": [
                        "user prompts only",
                        "no assistant responses",
                        "no tool calls",
                        "no command output",
                        "no child session relationships"
                    ],
                },
            }),
        ),
    }
}

pub(super) fn locator_observation(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
    observed_at: DateTime<Utc>,
) -> ProviderSourceLocatorObservation {
    locator_observation_for_revision(
        authority,
        &cursor.canonical_source_identity,
        &cursor.source_revision,
        observed_at,
    )
}

pub(super) fn locator_observation_for_revision(
    authority: &SourceAuthority,
    proposed_source_identity: &str,
    source_revision: &str,
    observed_at: DateTime<Utc>,
) -> ProviderSourceLocatorObservation {
    ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: authority.machine_id.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        proposed_source_identity: proposed_source_identity.to_owned(),
        raw_source_path: Some(authority.physical_path.display().to_string()),
        source_revision: source_revision.to_owned(),
        observed_at_ms: observed_at.timestamp_millis(),
    }
}

pub(super) fn generation_key(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: authority.machine_id.clone(),
        canonical_source_identity: cursor.canonical_source_identity.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        source_revision: cursor.source_revision.clone(),
        generation_id: cursor.generation_id.clone(),
    }
}

pub(super) fn route_retirement(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
    retired_at: DateTime<Utc>,
) -> ProviderSourceRouteRetirement {
    ProviderSourceRouteRetirement {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: authority.machine_id.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
        expected_source_revision: cursor.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    }
}

pub(super) fn sync_cursor(
    options: &CodexHistoryImportOptions,
    stream: String,
    cursor: String,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!("provider-cursor:codex:{}:{stream}", options.machine_id),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream,
        cursor,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    }
}

pub(super) fn replay_summary(cursor: &PromptHistoryCursor) -> ProviderImportSummary {
    let skipped_events = usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
    let skipped_sessions = usize::try_from(cursor.session_runs).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: skipped_events.saturating_add(skipped_sessions),
        failed: usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX),
        skipped_sessions,
        skipped_events,
        accepted_content_records: skipped_events,
        ..ProviderImportSummary::default()
    }
}
