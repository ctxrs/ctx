use super::*;

pub(super) fn publish_goose_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &GoosePublicationContext<'_>,
    publication: GoosePagePublication<'_>,
) -> Result<ProviderImportSummary> {
    let GoosePagePublication {
        reader,
        source_revision,
        page,
        retained_events,
        rejected_records,
        terminal_state,
    } = publication;
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let raw_source_path = reader
        .source_observation()
        .source_path()
        .display()
        .to_string();
    let locator_identity = provider_path_identity(reader.source_observation().source_path())?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
    if goose_page_already_committed(stored.as_ref(), source_revision, page)? {
        let mut summary = skipped_page_summary(page);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let wire = GooseNativeCursorWire {
        version: GOOSE_NATIVE_CURSOR_VERSION,
        kind: GOOSE_NATIVE_CURSOR_KIND.to_owned(),
        selected_path: reader.source_observation().source_path().to_path_buf(),
        source_revision: source_revision.to_owned(),
        frontier: page.next_frontier,
        retained_events,
        rejected_records,
        terminal_state,
    };
    validate_goose_cursor_predecessor(stored.as_ref(), source_revision, page.expected_frontier)?;
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            context.machine_id,
            stream.clone(),
            encode_goose_cursor(&wire)?,
            context.imported_at,
        ),
    );
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let publication_id = goose_publication_id(page.identity.0, &transition);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(skipped_page_summary(page));
    }

    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        Some(&context.source_root.display().to_string()),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Goose NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Goose,
            source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.to_owned(),
            locator_identity,
            cursor_stream: stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::<String, ResolvedSession>::new();
    for native in &page.sessions {
        let value = resolve_goose_session(
            committed_store,
            context,
            native,
            &raw_source_path,
            &resolution.canonical_source_identity,
        )?;
        group.upsert_capture_source(&goose_capture_source(
            context,
            Some(native),
            value.source_id,
            &raw_source_path,
            &resolution.canonical_source_identity,
            source_revision,
        ))?;
        group.bind_capture_source_provider_route(value.source_id, &resolution.route_binding())?;
        let existed = committed_store.get_session(value.session.id).is_ok();
        group.upsert_session(&value.session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        resolved.insert(native.native_identity.clone(), value);
    }
    for event in &page.events {
        let value = if let Some(value) = resolved.get(&event.session_identity) {
            value
        } else {
            let source = committed_store
                .capture_source_by_canonical_identity_session(
                    CaptureProvider::Goose,
                    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                    context.machine_id,
                    &resolution.canonical_source_identity,
                    &event.session_identity,
                )?
                .ok_or(CaptureError::SystemInvariant(
                    "Goose retained event has no committed native session source",
                ))?;
            let session_id = provider_import_session_uuid(
                committed_store,
                CaptureProvider::Goose,
                &event.session_identity,
                source.id,
                Some(&resolution.canonical_source_identity),
            )?;
            resolved
                .entry(event.session_identity.clone())
                .or_insert(ResolvedSession {
                    source_id: source.id,
                    session: committed_store.get_session(session_id)?,
                })
        };
        publish_goose_event(
            &mut group,
            committed_store,
            context,
            value.source_id,
            &value.session,
            event,
            &resolution.canonical_source_identity,
            reader.snapshot_connection(),
            &mut summary,
        )?;
    }
    if page.sessions.is_empty() && page.events.is_empty() {
        let source_id = goose_empty_source_id(&resolution.canonical_source_identity);
        group.upsert_capture_source(&goose_capture_source(
            context,
            None,
            source_id,
            &raw_source_path,
            &resolution.canonical_source_identity,
            source_revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    }
    record_rejections(page, &mut summary);
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn publish_goose_observation(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &GoosePublicationContext<'_>,
    reader: &GooseNativePathReader,
    source_revision: &str,
    terminal_state: GooseNativePersistedState,
) -> Result<ProviderImportSummary> {
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let path = reader.source_observation().source_path();
    let raw_source_path = path.display().to_string();
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
    if stored.as_ref().is_some_and(|cursor| {
        decode_goose_cursor(&cursor.cursor)
            .ok()
            .flatten()
            .is_some_and(|wire| {
                wire.source_revision == source_revision
                    && wire.frontier.phase == GooseNativeScanPhase::Complete
            })
    }) {
        return Ok(ProviderImportSummary::default());
    }
    let wire = GooseNativeCursorWire {
        version: GOOSE_NATIVE_CURSOR_VERSION,
        kind: GOOSE_NATIVE_CURSOR_KIND.to_owned(),
        selected_path: path.to_path_buf(),
        source_revision: source_revision.to_owned(),
        frontier: terminal_state.core_frontier,
        retained_events: 0,
        rejected_records: 0,
        terminal_state: Some(terminal_state),
    };
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            context.machine_id,
            stream.clone(),
            encode_goose_cursor(&wire)?,
            context.imported_at,
        ),
    );
    let publication_id = goose_publication_id([0; 32], &transition);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllExpected
    ) {
        let proposed_source_identity = provider_source_identity(
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            Some(&context.source_root.display().to_string()),
            Some(&raw_source_path),
            None,
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Goose empty NativePath source has no canonical identity",
        ))?;
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Goose,
                source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.to_owned(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = goose_empty_source_id(&resolution.canonical_source_identity);
        group.upsert_capture_source(&goose_capture_source(
            context,
            None,
            source_id,
            &raw_source_path,
            &resolution.canonical_source_identity,
            source_revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn resolve_goose_session(
    committed_store: &Store,
    context: &GoosePublicationContext<'_>,
    native: &GooseNativeSession,
    raw_source_path: &str,
    source_identity: &str,
) -> Result<ResolvedSession> {
    let source_id = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            context.machine_id,
            source_identity,
            &native.native_identity,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Goose,
                &native.native_identity,
                GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
            )
        });
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Goose,
        &native.native_identity,
        source_id,
        Some(source_identity),
    )?;
    let started_at = goose_timestamp(native.row.created_at.as_deref(), context.imported_at);
    let ended_at = native
        .row
        .updated_at
        .as_deref()
        .map(|value| goose_timestamp(Some(value), started_at));
    Ok(ResolvedSession {
        source_id,
        session: Session {
            id,
            history_record_id: context.history_record_id,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Goose,
            external_session_id: Some(native.native_identity.clone()),
            external_agent_id: native.row.provider_name.clone(),
            agent_type: AgentType::Primary,
            role_hint: native
                .row
                .session_type
                .clone()
                .or_else(|| Some("primary".to_owned())),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at,
            ended_at,
            timestamps: timestamps(context.imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": native.native_identity,
                    "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "native_rowid": native.sqlite_rowid,
                    "name": native.row.name,
                    "description": native.row.description,
                    "user_set_name": native.row.user_set_name,
                    "session_type": native.row.session_type,
                    "working_dir": native.row.working_dir,
                    "extension_data": native.row.extension_data,
                    "provider_name": native.row.provider_name,
                    "model_config": native.row.model_config_json,
                    "goose_mode": native.row.goose_mode,
                    "archived_at": native.row.archived_at,
                    "project_id": native.row.project_id,
                    "tokens": {
                        "total": native.row.total_tokens,
                        "input": native.row.input_tokens,
                        "output": native.row.output_tokens,
                        "accumulated_total": native.row.accumulated_total_tokens,
                        "accumulated_input": native.row.accumulated_input_tokens,
                        "accumulated_output": native.row.accumulated_output_tokens,
                    },
                    "accumulated_cost": native.row.accumulated_cost,
                }),
            ),
        },
    })
}

pub(super) fn goose_capture_source(
    context: &GoosePublicationContext<'_>,
    native: Option<&GooseNativeSession>,
    source_id: Uuid,
    raw_source_path: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    let started_at = native.map_or(context.imported_at, |session| {
        goose_timestamp(session.row.created_at.as_deref(), context.imported_at)
    });
    let ended_at = native.and_then(|session| {
        session
            .row
            .updated_at
            .as_deref()
            .map(|value| goose_timestamp(Some(value), started_at))
    });
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Goose,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: native.and_then(|session| session.row.working_dir.clone()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source_root.display().to_string()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: native.map(|session| session.native_identity.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": native.map(|session| &session.native_identity),
                "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_revision": source_revision,
                "source_identity_key": native.map(|session| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::Goose,
                        &session.native_identity,
                        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_goose_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &GoosePublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    native: &GooseNativeEvent,
    canonical_source_identity: &str,
    snapshot: &rusqlite::Connection,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    if native.file_touches.len() > GOOSE_MAX_TOUCHES_PER_EVENT {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose native event {} has {} file relationships; bounded Store publication permits at most {GOOSE_MAX_TOUCHES_PER_EVENT}",
            native.native_identity,
            native.file_touches.len()
        )));
    }
    let provider_event_index = u64::try_from(native.native_order).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "Goose native event {} has a negative message order",
            native.native_identity
        ))
    })?;
    let legacy_provider_event_index = goose_legacy_event_index(native);
    let exact_v025_identity = exact_v025_goose_event_identity(
        committed_store,
        context,
        source_id,
        session,
        canonical_source_identity,
        legacy_provider_event_index,
        &native.provider_message_identity,
    )?;
    let identity = exact_v025_identity.clone().map_or_else(
        || {
            provider_event_import_identity_with_exact_legacy_source(
                committed_store,
                CaptureProvider::Goose,
                &native.session_identity,
                source_id,
                provider_event_index,
                provider_event_index,
                &native.provider_message_identity,
                None,
                Some(legacy_provider_event_index),
                session.id
                    == crate::provider::importer::provider_session_uuid(
                        CaptureProvider::Goose,
                        &native.session_identity,
                    ),
            )
        },
        Ok,
    )?;
    let payload_hash = goose_event_payload_hash(native);
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &payload_hash)
            .unwrap_or(identity.dedupe_key);
    let event_type = match native.kind {
        GooseNativeEventKind::Message => EventType::Message,
        GooseNativeEventKind::ToolCall => EventType::ToolCall,
        GooseNativeEventKind::ToolOutput => EventType::ToolOutput,
    };
    let occurred_at = native.created_timestamp.map_or_else(
        || goose_timestamp(native.timestamp.as_deref(), session.started_at),
        |timestamp| provider_timestamp_seconds(Some(timestamp as f64), session.started_at),
    );
    let retained_text =
        provider_policy_event_text(event_type, &native.searchable_text, &native.content);
    let body = provider_capped_json(
        &provider_policy_body(event_type, &native.content),
        PROVIDER_MAX_PREVIEW_CHARS,
    );
    let mut sync_metadata = json!({
        "provider_session_id": native.session_identity,
        "provider_event_index": provider_event_index,
        "legacy_provider_event_index": legacy_provider_event_index,
        "provider_event_hash": &payload_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "cursor": format!(
            "session:{}:message:{}:rowid:{}",
            native.session_identity, native.provider_message_identity, native.sqlite_rowid
        ),
        "source_record_ordinal": provider_event_index,
        "native_order": native.native_order,
        "native_rowid": native.sqlite_rowid,
        "native_identity": native.native_identity,
        "identity_degraded": native.identity_degraded,
        "tokens": native.tokens_json,
        "metadata": native.metadata_json,
    });
    let payload = json!({
        "provider": CaptureProvider::Goose.as_str(),
        "provider_session_id": native.session_identity,
        "provider_event_index": provider_event_index,
        "provider_event_hash": &payload_hash,
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "output_preview": (event_type == EventType::ToolOutput)
            .then_some(native.searchable_text.as_str()),
        "result_outcome": native.content.get("result_outcome"),
        "exit_code": native.content.get("exit_code"),
        "duration_ms": native.content.get("duration_ms"),
        "timed_out": native.content.get("timed_out"),
        "call_id": native.content.get("call_id"),
        "body": body,
        "artifacts": [],
    });
    if event_type == EventType::Message
        && native.searchable_text.chars().count() > PROVIDER_MAX_TEXT_CHARS
    {
        let complete_text =
            super::super::normalization::goose_complete_content_text(&native.content)
                .unwrap_or_else(|| native.searchable_text.clone());
        super::super::content::attach_message_locator(
            snapshot,
            native.sqlite_rowid,
            &native.provider_message_identity,
            &payload,
            &mut sync_metadata,
            complete_text,
        )?;
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type,
        role: Some(provider_role(Some(&native.role))),
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    let changed = if exact_v025_identity.is_some() {
        group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &event,
            &native.provider_message_identity,
        )?
    } else {
        group.reconcile_provider_event(
            &event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )?
    };
    if changed {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for touch in &native.file_touches {
        let packed_touch = provider_event_index
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(u64::from(touch.ordinal)))
            .ok_or(CaptureError::SystemInvariant(
                "Goose file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Goose,
            &native.session_identity,
            source_id,
            Some(provider_event_index),
            packed_touch,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Goose,
                    &native.session_identity,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.history_record_id,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: Some(touch.change_kind),
            old_path: touch.old_path.clone(),
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Goose.as_str(),
                    "provider_session_id": native.session_identity,
                    "provider_event_index": provider_event_index,
                    "provider_touch_index": touch.ordinal,
                    "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                    "evidence": touch.evidence,
                }),
            ),
        })?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn exact_v025_goose_event_identity(
    store: &Store,
    context: &GoosePublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    canonical_source_identity: &str,
    legacy_provider_event_index: u64,
    provider_message_identity: &str,
) -> Result<Option<ProviderEventImportIdentity>> {
    let source = match store.get_capture_source(source_id) {
        Ok(source) => source,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if source.descriptor.kind != CaptureSourceKind::ProviderImport
        || source.descriptor.provider != CaptureProvider::Goose
        || source.descriptor.machine_id != context.machine_id
        || source.descriptor.source_format.as_deref() != Some(GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT)
        || source.descriptor.source_identity.as_deref() != Some(canonical_source_identity)
        || source.descriptor.external_session_id.as_deref()
            != session.external_session_id.as_deref()
    {
        return Ok(None);
    }
    let legacy = provider_source_event_import_identity(
        source.id,
        legacy_provider_event_index,
        provider_message_identity,
    );
    let event = match store.get_event(legacy.id) {
        Ok(event) => event,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let expected_dedupe_prefix = format!(
        "provider-source:{}:{legacy_provider_event_index}:",
        source.id
    );
    if event.id != legacy.id
        || event.capture_source_id != Some(source.id)
        || event.session_id != Some(session.id)
        || !event
            .dedupe_key
            .as_deref()
            .is_some_and(|key| key.starts_with(&expected_dedupe_prefix))
    {
        return Ok(None);
    }
    Ok(event
        .dedupe_key
        .map(|dedupe_key| ProviderEventImportIdentity {
            id: event.id,
            seq: event.seq,
            dedupe_key,
            run_source_id: event.capture_source_id,
        }))
}
