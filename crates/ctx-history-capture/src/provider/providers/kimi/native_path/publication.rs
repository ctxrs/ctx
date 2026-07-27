use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn kimi_capture_source(
    context: &ProviderAdapterContext,
    session: &KimiWireSessionState,
    checkpoint: &KimiNativeCheckpoint,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::KimiCodeCli,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(KIMI_CODE_CLI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: checkpoint
            .started_at
            .or(session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::KimiCodeCli,
                    &session.provider_session_id,
                    KIMI_CODE_CLI_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "session_index": session.index_metadata,
            }),
        ),
    }
}

pub(super) fn canonical_kimi_session(
    committed_store: &Store,
    context: &ProviderAdapterContext,
    session: &KimiWireSessionState,
    checkpoint: &KimiNativeCheckpoint,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::KimiCodeCli,
        &session.provider_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::KimiCodeCli,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    let root_session_id = session
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::KimiCodeCli,
                root,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?
        .or(parent_session_id);
    Ok(Session {
        id,
        history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::KimiCodeCli,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: Some(session.agent_id.clone()),
        agent_type: if session.is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if session.is_primary {
            "main".to_owned()
        } else {
            "subagent".to_owned()
        }),
        is_primary: session.is_primary,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: checkpoint
            .started_at
            .or(session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "parent_provider_session_id": session.parent_provider_session_id,
                "root_provider_session_id": session.root_provider_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "agent_id": session.agent_id,
                    "state": session.state_metadata,
                    "agent_state": session.agent_state_metadata,
                    "title": session.title,
                    "last_prompt": session.last_prompt,
                    "archived": session.archived,
                },
            }),
        ),
    })
}

pub(super) fn relationship_placeholders<'a>(
    canonical: &Session,
    native: &'a KimiWireSessionState,
) -> Vec<(Uuid, &'a str)> {
    let mut placeholders = Vec::new();
    if let (Some(id), Some(external)) = (
        canonical.parent_session_id,
        native.parent_provider_session_id.as_deref(),
    ) {
        placeholders.push((id, external));
    }
    if let (Some(id), Some(external)) = (
        canonical.root_session_id,
        native.root_provider_session_id.as_deref(),
    ) {
        if !placeholders.iter().any(|(existing, _)| *existing == id) {
            placeholders.push((id, external));
        }
    }
    placeholders
}

pub(super) fn relationship_placeholder(
    context: &ProviderAdapterContext,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    history_record_id: Option<Uuid>,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::KimiCodeCli,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

pub(super) fn relationship_edge(
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

pub(super) fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_kimi_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    history_record_id: Option<Uuid>,
    raw_ordinal: u64,
    event: &KimiCoreEvent,
    summary: &mut ProviderImportSummary,
) -> Result<Uuid> {
    let event_hash = compute_payload_hash(&event.payload)?;
    let authority = ProviderEventHashAuthority::NormalizedPayloadFallback;
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let allow_legacy_provider_identity =
        session.id == provider_session_uuid(CaptureProvider::KimiCodeCli, provider_session_id);
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::KimiCodeCli,
        provider_session_id,
        source_id,
        event.provider_event_index,
        raw_ordinal,
        &event_hash,
        None,
        Some(raw_ordinal),
        allow_legacy_provider_identity,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let run = kimi_command_run(
        source_id,
        session,
        history_record_id,
        event,
        &event_hash,
        identity.run_source_id,
    )?;
    if let Some(run) = &run {
        group.upsert_run(run)?;
    }
    let mut provider_metadata = event.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": &event_hash,
        "provider_event_hash_authority": authority.as_str(),
        "cursor": event.cursor,
        "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": raw_ordinal.saturating_add(1),
        "source_record_ordinal": raw_ordinal,
        "source_record_subrecord_index": 0,
        "imported_at": context.imported_at,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let canonical = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id,
        session_id: Some(session.id),
        run_id: run.as_ref().map(|run| run.id),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::KimiCodeCli.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": &event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(event.fidelity, sync_metadata),
    };
    let inserted = group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &canonical,
        &event.legacy_provider_event_hash,
    )?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(canonical.id)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_kimi_file_touch(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    history_record_id: Option<Uuid>,
    touch: &KimiFileTouch,
    event_id: Option<Uuid>,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let allow_legacy_provider_identity =
        session.id == provider_session_uuid(CaptureProvider::KimiCodeCli, provider_session_id);
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::KimiCodeCli,
        provider_session_id,
        source_id,
        touch.provider_event_index,
        touch.provider_touch_index,
        allow_legacy_provider_identity,
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: None,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::KimiCodeCli.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "session_id": session.id,
                "imported_at": context.imported_at,
                "metadata": touch.metadata,
            }),
        ),
    })?;
    Ok(())
}

pub(super) fn kimi_command_run(
    source_id: Uuid,
    session: &Session,
    history_record_id: Option<Uuid>,
    event: &KimiCoreEvent,
    event_hash: &str,
    run_source_id: Option<Uuid>,
) -> Result<Option<Run>> {
    if event.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let call_id = event.payload.get("call_id").and_then(Value::as_str);
    let run_key = call_id.unwrap_or(event_hash);
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{run_key}",
                    CaptureProvider::KimiCodeCli.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(
                &format!("provider-source:{run_source_id}:run:{run_key}"),
                "run",
            )
        },
    );
    let started_at = kimi_command_started_at(event)?;
    Ok(Some(Run {
        id,
        history_record_id,
        session_id: Some(session.id),
        run_type: RunType::Command,
        status: kimi_command_run_status(&event.payload),
        started_at,
        ended_at: Some(event.occurred_at),
        exit_code: event
            .payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd: event
            .payload
            .get("workdir")
            .or_else(|| event.payload.get("cwd"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        command_preview: event
            .payload
            .get("command")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

pub(super) fn kimi_command_started_at(event: &KimiCoreEvent) -> Result<DateTime<Utc>> {
    let Some(value) = event.payload.get("duration_ms") else {
        return Ok(event.occurred_at);
    };
    if value.is_null() {
        return Ok(event.occurred_at);
    }
    let duration_ms = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration_ms < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration_ms}"
        )));
    }
    let duration = chrono::Duration::try_milliseconds(duration_ms).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms is not representable as milliseconds: {duration_ms}"
        ))
    })?;
    event
        .occurred_at
        .checked_sub_signed(duration)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "duration_ms moves command start before representable time: {duration_ms}"
            ))
        })
}

pub(super) fn kimi_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => match payload
            .get("result_outcome")
            .or_else(|| payload.get("outcome"))
            .or_else(|| payload.get("status"))
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("timeout" | "timed_out" | "timedout" | "cancelled" | "canceled") => {
                RunStatus::Cancelled
            }
            Some("failure" | "failed" | "error" | "errored") => RunStatus::Failed,
            Some("success" | "succeeded" | "complete" | "completed" | "ok" | "passed") => {
                RunStatus::Succeeded
            }
            _ => RunStatus::Partial,
        },
    }
}

pub(super) fn replay_page_summary(page: &KimiCorePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary {
        skipped_sessions: usize::from(page.session_first_observed),
        ..ProviderImportSummary::default()
    };
    let mut skipped_file_touches = 0_usize;
    for unit in &page.units {
        match unit {
            KimiCoreUnit::Event { .. } => {
                summary.skipped_events = summary.skipped_events.saturating_add(1);
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            KimiCoreUnit::FileTouch(_) => {
                skipped_file_touches = skipped_file_touches.saturating_add(1);
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            KimiCoreUnit::Rejection { line, reason } => {
                summary.record_failure(ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                });
            }
        }
    }
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events)
        .saturating_add(skipped_file_touches);
    summary
}

pub(super) fn replay_summary(checkpoint: &KimiNativeCheckpoint) -> ProviderImportSummary {
    let skipped_sessions = usize::from(checkpoint.emitted_session);
    let skipped_events = usize::try_from(checkpoint.accepted_events).unwrap_or(usize::MAX);
    let skipped_file_touches =
        usize::try_from(checkpoint.accepted_file_touches).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: skipped_sessions
            .saturating_add(skipped_events)
            .saturating_add(skipped_file_touches),
        skipped_sessions,
        skipped_events,
        accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
        failed: usize::try_from(checkpoint.rejected_records).unwrap_or(usize::MAX),
        ..ProviderImportSummary::default()
    }
}
