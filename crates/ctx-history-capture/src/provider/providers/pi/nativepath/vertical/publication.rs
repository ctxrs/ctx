use super::*;

pub(super) struct PiCorePagePublication {
    pub(super) summary: ProviderImportSummary,
    pub(super) relocated_source_id: Option<Uuid>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source_root: &str,
    options: &PiSessionImportOptions,
    path: &Path,
    cursor_stream: &str,
    source_revision: &str,
    _initial_state: &PiCoreState,
    page: crate::provider::native_ingestion::NativeIngestionPage<PiNativeCorePage>,
) -> Result<PiCorePagePublication> {
    let expected_checkpoint =
        PiNativeCheckpoint::decode_frontier(&page.expected_frontier).map_err(map_native_error)?;
    let next_checkpoint =
        PiNativeCheckpoint::decode_frontier(&page.next_safe_frontier).map_err(map_native_error)?;
    let current_state = load_core_state(store, &options.machine_id, cursor_stream)?;
    let resets_generation = expected_checkpoint.complete_offset == 0
        && current_state
            .prior
            .as_ref()
            .is_some_and(|prior| prior.checkpoint != expected_checkpoint);
    if current_state.prior.as_ref().map(|prior| &prior.checkpoint) != Some(&expected_checkpoint)
        && !(current_state.prior.is_none() && expected_checkpoint.complete_offset == 0)
        && !resets_generation
    {
        return Err(CaptureError::InvalidPayload(
            "Pi NativePath Core cursor conflict".to_owned(),
        ));
    }
    let mut next_wire = current_state
        .prior
        .clone()
        .filter(|_| !resets_generation)
        .unwrap_or(PiStoreCursorWire {
            version: PI_STORE_CURSOR_VERSION,
            checkpoint: expected_checkpoint,
            source_revision: source_revision.to_owned(),
            canonical_source_identity: None,
            source_id: None,
            session_id: None,
            provider_session_id: None,
            rejected_records: 0,
        });
    next_wire.checkpoint = next_checkpoint;
    next_wire.source_revision = source_revision.to_owned();
    next_wire.rejected_records = next_wire.rejected_records.saturating_add(
        u64::try_from(
            page.core
                .units
                .iter()
                .filter(|unit| matches!(unit, PiNativeCoreUnit::Rejection(_)))
                .count(),
        )
        .unwrap_or(u64::MAX),
    );
    prime_cursor_identity(
        committed_store,
        source_root,
        options,
        path,
        &page.core,
        &mut next_wire,
    )?;
    let encoded = serde_json::to_string(&next_wire)?;
    let next_cursor = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: cursor_stream.to_owned(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition = NativePathCursorTransition::new(
        current_state
            .expected_store_cursor
            .as_ref()
            .map(|cursor| cursor.cursor.clone()),
        next_cursor,
    );
    let publication_id = publication_id(path, &page, &transition);
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(PiCorePagePublication {
            summary,
            relocated_source_id: None,
        });
    }

    let raw_source_path = path.display().to_string();
    let page_resolution = reconcile_page_source_locator(
        &mut group,
        options,
        path,
        cursor_stream,
        source_revision,
        &next_wire,
    )?;
    let mut resolved = current_state
        .prior
        .as_ref()
        .map(|prior| hydrate_prior_session(committed_store, prior))
        .transpose()?
        .flatten();
    let mut events = BTreeMap::new();
    let mut summary = ProviderImportSummary::default();
    for unit in &page.core.units {
        match unit {
            PiNativeCoreUnit::Session(row) => {
                let page_resolution =
                    page_resolution
                        .as_ref()
                        .ok_or(CaptureError::SystemInvariant(
                            "Pi NativePath session page has no reconciled source locator",
                        ))?;
                let session = resolve_session(
                    committed_store,
                    &mut group,
                    source_root,
                    options,
                    &raw_source_path,
                    source_revision,
                    row,
                    page_resolution,
                )?;
                if next_wire.canonical_source_identity.as_deref()
                    != Some(session.canonical_source_identity.as_str())
                    || next_wire.source_id != Some(session.source_id)
                    || next_wire.session_id != Some(session.session_id)
                    || next_wire.provider_session_id.as_deref()
                        != Some(session.provider_session_id.as_str())
                {
                    return Err(CaptureError::SystemInvariant(
                        "Pi NativePath resolved identity changed after cursor certification",
                    ));
                }
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
                resolved = Some(session);
            }
            PiNativeCoreUnit::Event(row) => {
                let session = resolved.as_ref().ok_or(CaptureError::SystemInvariant(
                    "Pi NativePath event page has no resolved session",
                ))?;
                let event = publish_event(
                    committed_store,
                    &mut group,
                    options,
                    session,
                    row,
                    &mut summary,
                )?;
                events.insert(row.provider_event_index, event);
            }
            PiNativeCoreUnit::FileTouch(row) => {
                let session = resolved.as_ref().ok_or(CaptureError::SystemInvariant(
                    "Pi NativePath file-touch page has no resolved session",
                ))?;
                publish_file_touch(
                    committed_store,
                    &mut group,
                    options,
                    session,
                    row,
                    events.get(&row.provider_event_index.unwrap_or(u64::MAX)),
                    &mut summary,
                )?;
            }
            PiNativeCoreUnit::Rejection(rejection) => {
                summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(rejection.line_number).unwrap_or(usize::MAX),
                    error: rejection.diagnostic.clone(),
                });
            }
        }
    }
    let relocated_source_id = if let Some(resolution) = &page_resolution {
        let resolved = resolved.as_ref().ok_or(CaptureError::SystemInvariant(
            "Pi NativePath reconciled page has no resolved session",
        ))?;
        if resolved.canonical_source_identity != resolution.canonical_source_identity {
            return Err(CaptureError::SystemInvariant(
                "Pi NativePath page source disagreed with locator reconciliation",
            ));
        }
        group
            .bind_capture_source_provider_route(resolved.source_id, &resolution.route_binding())?;
        resolution.relocated.then_some(resolved.source_id)
    } else {
        None
    };
    if !revalidate_pi_source_revision(path, source_revision).map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(PiCorePagePublication {
        summary,
        relocated_source_id,
    })
}

pub(super) fn reconcile_page_source_locator(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &PiSessionImportOptions,
    path: &Path,
    cursor_stream: &str,
    source_revision: &str,
    cursor: &PiStoreCursorWire,
) -> Result<Option<ProviderSourceLocatorResolution>> {
    let Some(proposed_source_identity) = cursor.canonical_source_identity.as_ref() else {
        if cursor.source_id.is_some()
            || cursor.session_id.is_some()
            || cursor.provider_session_id.is_some()
        {
            return Err(CaptureError::SystemInvariant(
                "Pi NativePath cursor has a partial source identity",
            ));
        }
        return Ok(None);
    };
    if cursor.source_id.is_none()
        || cursor.session_id.is_none()
        || cursor.provider_session_id.is_none()
    {
        return Err(CaptureError::SystemInvariant(
            "Pi NativePath cursor has a partial source identity",
        ));
    }
    let raw_source_path = path.display().to_string();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Pi,
            source_format: PI_SOURCE_FORMAT.to_owned(),
            machine_id: options.machine_id.clone(),
            locator_identity: provider_path_identity(path)?,
            cursor_stream: cursor_stream.to_owned(),
            proposed_source_identity: proposed_source_identity.clone(),
            raw_source_path: Some(raw_source_path),
            source_revision: source_revision.to_owned(),
            observed_at_ms: options.imported_at.timestamp_millis(),
        })?;
    if resolution.canonical_source_identity != *proposed_source_identity {
        return Err(CaptureError::SystemInvariant(
            "Pi NativePath locator reconciliation changed certified source identity",
        ));
    }
    Ok(Some(resolution))
}

pub(super) fn prime_cursor_identity(
    committed_store: &Store,
    source_root: &str,
    options: &PiSessionImportOptions,
    path: &Path,
    core: &PiNativeCorePage,
    cursor: &mut PiStoreCursorWire,
) -> Result<()> {
    let provider_session_id = core.units.iter().find_map(|unit| match unit {
        PiNativeCoreUnit::Session(row) => Some(row.provider_session_id.as_str()),
        PiNativeCoreUnit::Event(row) => Some(row.provider_session_id.as_str()),
        PiNativeCoreUnit::FileTouch(row) => Some(row.provider_session_id.as_str()),
        PiNativeCoreUnit::Rejection(_) => None,
    });
    let Some(provider_session_id) = provider_session_id else {
        return Ok(());
    };
    if cursor
        .provider_session_id
        .as_deref()
        .is_some_and(|prior| prior != provider_session_id)
    {
        cursor.source_id = None;
        cursor.session_id = None;
    }
    let raw_source_path = path.display().to_string();
    let canonical_source_identity = provider_source_identity(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        Some(source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Pi NativePath source has no canonical identity",
    ))?;
    let source_id = cursor
        .source_id
        .or(committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Pi,
                PI_SOURCE_FORMAT,
                &options.machine_id,
                &canonical_source_identity,
                provider_session_id,
            )?
            .map(|source| source.id))
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Pi,
                provider_session_id,
                PI_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Pi,
        provider_session_id,
        source_id,
        Some(&canonical_source_identity),
    )?;
    cursor.canonical_source_identity = Some(canonical_source_identity);
    cursor.source_id = Some(source_id);
    cursor.session_id = Some(session_id);
    cursor.provider_session_id = Some(provider_session_id.to_owned());
    Ok(())
}

pub(super) fn hydrate_prior_session(
    committed_store: &Store,
    cursor: &PiStoreCursorWire,
) -> Result<Option<ResolvedPiSession>> {
    let (Some(source_id), Some(session_id), Some(provider_session_id), Some(canonical)) = (
        cursor.source_id,
        cursor.session_id,
        cursor.provider_session_id.as_ref(),
        cursor.canonical_source_identity.as_ref(),
    ) else {
        return Ok(None);
    };
    let session = committed_store.get_session(session_id)?;
    Ok(Some(ResolvedPiSession {
        source_id,
        session_id,
        provider_session_id: provider_session_id.clone(),
        canonical_source_identity: canonical.clone(),
        session,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source_root: &str,
    options: &PiSessionImportOptions,
    raw_source_path: &str,
    source_revision: &str,
    row: &PiNativeSessionRow,
    resolution: &ProviderSourceLocatorResolution,
) -> Result<ResolvedPiSession> {
    let source_id = if resolution.relocated {
        committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Pi,
                PI_SOURCE_FORMAT,
                &options.machine_id,
                &resolution.canonical_source_identity,
                &row.provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::Pi,
                    &row.provider_session_id,
                    PI_SOURCE_FORMAT,
                    Some(raw_source_path),
                )
            })
    } else {
        provider_scoped_source_uuid(
            CaptureProvider::Pi,
            &row.provider_session_id,
            PI_SOURCE_FORMAT,
            Some(raw_source_path),
        )
    };
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Pi,
            machine_id: options.machine_id.clone(),
            process_id: None,
            cwd: row.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(PI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(row.provider_session_id.clone()),
        },
        started_at: row.started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "source_format": PI_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "imported_at": options.imported_at,
                "source_idempotency_key": row.source_idempotency_key,
                "source_identity": resolution.canonical_source_identity,
                "source_root": source_root,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Pi,
                    &row.provider_session_id,
                    PI_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "source_metadata": row.source_metadata,
                "session_metadata": row.session_metadata,
                "source_revision": source_revision,
                "nativepath_publication": 1,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Pi,
        &row.provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Pi,
        external_session_id: Some(row.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: row.started_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "source_format": PI_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "imported_at": options.imported_at,
                "session_idempotency_key": row.session_idempotency_key,
                "metadata": row.session_metadata,
            }),
        ),
    };
    group.upsert_session(&session)?;
    Ok(ResolvedPiSession {
        source_id,
        session_id,
        provider_session_id: row.provider_session_id.clone(),
        canonical_source_identity: resolution.canonical_source_identity.clone(),
        session,
    })
}

pub(super) fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &PiSessionImportOptions,
    resolved: &ResolvedPiSession,
    row: &PiNativeEventRow,
    summary: &mut ProviderImportSummary,
) -> Result<Event> {
    let event_hash = compute_payload_hash(&row.payload)?;
    let event_hash_authority = ProviderEventHashAuthority::NormalizedPayloadFallback;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Pi,
        &row.provider_session_id,
        resolved.source_id,
        row.provider_event_identity_index,
        row.provider_event_index,
        &event_hash,
        None,
        Some(row.provider_event_index),
        resolved.session_id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Pi,
                &row.provider_session_id,
            ),
    )?;
    let line_number = usize::try_from(row.locator.line_number).unwrap_or(usize::MAX);
    let run = pi_command_run(
        row,
        &event_hash,
        identity.run_source_id,
        options.history_record_id,
        resolved.session_id,
        resolved.source_id,
    )?;
    if let Some(run) = &run {
        group.upsert_run(run)?;
    }
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let mut provider_metadata = row.metadata.clone();
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": row.provider_session_id,
        "provider_event_index": row.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": event_hash_authority.as_str(),
        "cursor": row.cursor,
        "source_format": PI_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderExport,
        "fixture_line": line_number,
        "imported_at": options.imported_at,
        "event_idempotency_key": row.idempotency_key,
        "source_record_ordinal": row.locator.source_record_ordinal,
        "source_record_subrecord_index": 0_u32,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session_id),
        run_id: run.as_ref().map(|run| run.id),
        event_type: row.event_type,
        role: row.role,
        occurred_at: row.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::Pi.as_str(),
            "provider_session_id": row.provider_session_id,
            "provider_event_index": row.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": row.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(row.event_type, &row.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event(&event, event_hash_authority)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(event)
}

pub(super) fn pi_command_run(
    row: &PiNativeEventRow,
    event_hash: &str,
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    session_id: Uuid,
    source_id: Uuid,
) -> Result<Option<Run>> {
    if row.event_type != ctx_history_core::EventType::CommandOutput {
        return Ok(None);
    }
    let duration_ms = match row.payload.get("duration_ms") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(|| {
            CaptureError::InvalidPayload("duration_ms must be an integer".to_owned())
        })?),
    };
    let started_at = match duration_ms {
        Some(duration_ms) if duration_ms < 0 => {
            return Err(CaptureError::InvalidPayload(format!(
                "duration_ms must be nonnegative, got {duration_ms}"
            )));
        }
        Some(duration_ms) => {
            let duration = chrono::Duration::try_milliseconds(duration_ms).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_ms}"
                ))
            })?;
            row.occurred_at
                .checked_sub_signed(duration)
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "duration_ms moves command start before representable time: {duration_ms}"
                    ))
                })?
        }
        None => row.occurred_at,
    };
    let call_id = row.payload.get("call_id").and_then(Value::as_str);
    let run_key = call_id.unwrap_or(event_hash);
    let run_id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{}:run:{run_key}",
                    CaptureProvider::Pi.as_str(),
                    row.provider_session_id
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
    let command_preview = row
        .payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let cwd = row
        .payload
        .get("workdir")
        .or_else(|| row.payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    Ok(Some(Run {
        id: run_id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: pi_command_run_status(&row.payload),
        started_at,
        ended_at: Some(row.occurred_at),
        exit_code: row
            .payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(row.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "provider_event_index": row.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

pub(super) fn pi_command_run_status(payload: &Value) -> RunStatus {
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

pub(super) fn publish_file_touch(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &PiSessionImportOptions,
    resolved: &ResolvedPiSession,
    row: &PiNativeFileTouchRow,
    event: Option<&Event>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::Pi,
        &row.provider_session_id,
        resolved.source_id,
        row.provider_event_index,
        row.provider_touch_index,
        resolved.session_id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Pi,
                &row.provider_session_id,
            ),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id: event.map(|event| event.id),
        vcs_workspace_id: None,
        path: row.path.clone(),
        change_kind: row.change_kind,
        old_path: row.old_path.clone(),
        line_count_delta: row.line_count_delta,
        confidence: row.confidence,
        timestamps: timestamps(row.occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Pi.as_str(),
                "provider_session_id": row.provider_session_id,
                "provider_touch_index": row.provider_touch_index,
                "provider_event_index": row.provider_event_index,
                "source_format": row.source_format,
                "raw_source_path": row.raw_source_path,
                "source_root": row.source_root,
                "metadata": row.metadata,
                "session_id": resolved.session.id,
            }),
        ),
    })?;
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}
