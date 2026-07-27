use super::*;

pub(super) struct ResolvedSource {
    pub(super) source_id: Uuid,
    pub(super) session: Session,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    cursor: &JunieStoreCursor,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedSource> {
    let raw_source_path = session_path.events_path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(&session_path.events_path)?;
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Junie NativePath source has no canonical identity",
    ))?;
    let route_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: route_stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: cursor.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &context.machine_id,
        &resolution.canonical_source_identity,
        provider_session_id,
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Junie,
                provider_session_id,
                JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Junie,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: cursor.frontier.state.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(provider_session_id.to_owned()),
        },
        started_at: cursor.frontier.state.started_at(),
        ended_at: cursor.frontier.state.ended_at_ms.map(timestamp),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": resolution.canonical_source_identity,
                "source_root": source_root,
                "source_revision": cursor.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Junie,
                    provider_session_id,
                    JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "nativepath_publication": PUBLICATION_REVISION,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Junie,
        provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let existed = committed_store.get_session(session_id).is_ok();
    let meta = bounded_junie_index_meta(&session_path.index_meta);
    let session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Junie,
        external_session_id: Some(provider_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: if cursor.terminal {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: cursor.frontier.state.started_at(),
        ended_at: cursor.frontier.state.ended_at_ms.map(timestamp),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:junie:{provider_session_id}"),
                "metadata": {
                    "title": cursor.frontier.state.title,
                    "project_dir": cursor.frontier.state.cwd,
                    "index": provider_capped_json_value(
                        &meta.raw,
                        PROVIDER_MAX_PREVIEW_CHARS,
                    ),
                    "nativepath_publication": PUBLICATION_REVISION,
                },
            }),
        ),
    };
    group.upsert_session(&session)?;
    if !existed {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedSource { source_id, session })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    resolved: &ResolvedSource,
    generation: u64,
    draft: &EventDraft,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let identity_index = generation
        .checked_mul(GENERATION_EVENT_STRIDE)
        .and_then(|base| base.checked_add(draft.event_index))
        .ok_or(CaptureError::SystemInvariant(
            "Junie generation event identity exhausted",
        ))?;
    if draft.event_index >= GENERATION_EVENT_STRIDE {
        return Err(CaptureError::InvalidPayload(
            "Junie session exceeds the provider-local generation event bound".to_owned(),
        ));
    }
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Junie,
        provider_session_id,
        resolved.source_id,
        identity_index,
        identity_index,
        &draft.event_hash,
        None,
        None,
        generation == 0,
    )?;
    let retained = provider_policy_event_text(draft.event_type, &draft.text, &draft.body);
    let policy_body = provider_policy_body(draft.event_type, &draft.body);
    let provider_payload = json!({
        "text": retained.text,
        "text_retention": retained.retention.as_json(),
        "result_evidence": provider_result_identifier_evidence(
            draft.event_type,
            &draft.text,
            &draft.body,
        ),
        "result_outcome": provider_result_outcome_evidence(draft.event_type, &draft.body),
        "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        "body": provider_capped_json_value(&policy_body, PROVIDER_MAX_PREVIEW_CHARS),
    });
    let mut provider_metadata = draft.metadata.clone();
    let locator = draft
        .binding
        .as_ref()
        .and_then(|(binding, role, tag, target, suffix)| {
            verified_locator(binding, *role, *tag, *target, suffix, &draft.text)
        });
    if let Some(locator) = locator {
        attach_verified_content_locator(&mut provider_metadata, locator).ok_or(
            CaptureError::SystemInvariant("Junie verified-content locator collection is malformed"),
        )?;
    }
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": identity_index,
        "native_event_index": draft.event_index,
        "provider_event_hash": draft.event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": format!(
            "{}:line:{}:event:{}",
            resolved
                .session
                .capture_source_id
                .map(|_| "junie-session-events")
                .unwrap_or("junie"),
            draft.source_ordinal.saturating_add(1),
            identity_index,
        ),
        "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": usize::try_from(draft.source_ordinal.saturating_add(1))
            .unwrap_or(usize::MAX),
        "imported_at": context.imported_at,
        "event_idempotency_key":
            format!("provider-event:junie:{provider_session_id}:{identity_index}"),
        "source_record_ordinal": draft.source_ordinal,
        "source_record_subrecord_index": draft.source_subrecord,
        "metadata": provider_metadata,
        "nativepath_generation": generation,
    });
    if let Some(locators) = sync_metadata
        .pointer_mut("/metadata")
        .and_then(Value::as_object_mut)
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
    {
        sync_metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY] = locators;
    }
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &draft.event_hash)
            .unwrap_or(identity.dedupe_key);
    let run = command_run(
        draft,
        options,
        provider_session_id,
        resolved,
        identity.run_source_id,
        generation,
    )?;
    if let Some(run) = &run {
        group.upsert_run(run)?;
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_id: run.as_ref().map(|run| run.id),
        event_type: draft.event_type,
        role: draft.role,
        occurred_at: draft.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::Junie.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": identity_index,
            "native_event_index": draft.event_index,
            "provider_event_hash": draft.event_hash,
            "cursor": format!("line:{}", draft.source_ordinal.saturating_add(1)),
            "artifacts": [],
            "body": compact_provider_result_payload(draft.event_type, &provider_payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    let inserted =
        group.reconcile_provider_event(&event, ProviderEventHashAuthority::ProviderSupplied)?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    if let Some(change) = &draft.file_change {
        let touch_id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Junie,
            provider_session_id,
            resolved.source_id,
            Some(identity_index),
            change.touch_index,
            generation == 0,
        )?;
        group.upsert_file_touched(&FileTouched {
            id: touch_id,
            history_record_id: options.history_record_id,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: change.path.clone(),
            change_kind: Some(change.change_kind),
            old_path: change.old_path.clone(),
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(draft.occurred_at),
            source_id: Some(resolved.source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Junie.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_touch_index": change.touch_index,
                    "provider_event_index": identity_index,
                    "native_event_index": draft.event_index,
                    "source_id": resolved.source_id,
                    "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    "session_id": resolved.session.id,
                    "nativepath_generation": generation,
                }),
            ),
        })?;
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

pub(super) fn verified_locator(
    binding: &RecordSetBinding,
    role: VerifiedContentRole,
    tag: u8,
    target: u32,
    suffix: &str,
    content: &str,
) -> Option<VerifiedContentLocatorV1> {
    if role == VerifiedContentRole::MessageBody
        && content.chars().count() <= crate::PROVIDER_MAX_TEXT_CHARS
    {
        return None;
    }
    let encoded = binding.encoded(tag, target)?;
    let record_digest = binding.record_digest()?;
    let content_ref = ContentRef::from_bytes(content.as_bytes())?;
    let profile = verified_content_profile(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        role,
    )?;
    VerifiedContentLocatorV1::new(
        role,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        RECORD_SET_KIND,
        &encoded,
        binding.native_record_id(suffix)?,
        record_digest,
    )
}

pub(super) fn command_run(
    draft: &EventDraft,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    resolved: &ResolvedSource,
    run_source_id: Option<Uuid>,
    generation: u64,
) -> Result<Option<Run>> {
    if draft.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let timed_out = draft
        .body
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_code = draft
        .body
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let call_id = draft
        .body
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or(&draft.event_hash);
    let run_key = if generation == 0 {
        call_id.to_owned()
    } else {
        format!("generation:{generation}:{call_id}")
    };
    let id = run_source_id.map_or_else(
        || {
            crate::stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{run_key}",
                    CaptureProvider::Junie.as_str()
                ),
                "run",
            )
        },
        |source_id| {
            crate::stable_capture_uuid(&format!("provider-source:{source_id}:run:{run_key}"), "run")
        },
    );
    let duration = draft
        .body
        .get("duration_ms")
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .and_then(chrono::Duration::try_milliseconds);
    Ok(Some(Run {
        id,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_type: RunType::Command,
        status: if timed_out {
            RunStatus::Cancelled
        } else if exit_code.is_some_and(|code| code != 0) {
            RunStatus::Failed
        } else {
            RunStatus::Partial
        },
        started_at: duration
            .and_then(|duration| draft.occurred_at.checked_sub_signed(duration))
            .unwrap_or(draft.occurred_at),
        ended_at: Some(draft.occurred_at),
        exit_code,
        cwd: resolved
            .session
            .sync
            .metadata
            .pointer("/metadata/project_dir")
            .and_then(Value::as_str)
            .map(str::to_owned),
        command_preview: draft
            .body
            .get("command")
            .and_then(Value::as_str)
            .map(|value| provider_local_preview(value, PROVIDER_MAX_PREVIEW_CHARS).0),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(draft.occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": draft.event_index,
                "provider_event_hash": draft.event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

pub(super) fn publication_id(
    source_identity: &str,
    cursor: &JunieStoreCursor,
    rows: &[EventDraft],
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-junie-nativepath-publication-v1\0");
    digest.update(source_identity.as_bytes());
    digest.update(cursor.generation.to_le_bytes());
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    for row in rows {
        digest.update(row.event_index.to_le_bytes());
        digest.update(row.event_hash.as_bytes());
        digest.update(row.event_type.as_str().as_bytes());
        digest.update(row.text.as_bytes());
        digest.update(serde_json::to_vec(&row.body).unwrap_or_default());
    }
    format!("junie-nativepath-v1:{:x}", digest.finalize())
}
