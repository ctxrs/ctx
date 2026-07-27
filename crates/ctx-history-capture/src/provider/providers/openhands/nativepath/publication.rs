use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    current_route: Option<&KnownOpenHandsRoute>,
    relocation_route: Option<&KnownOpenHandsRoute>,
    page: PreparedCorePage,
) -> Result<ProviderImportSummary> {
    if !source.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let next = provider_sync_cursor(
        &context.machine_id,
        source.cursor_stream.clone(),
        page.next_cursor.encode()?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(page.expected_cursor.clone(), next);
    let publication_id = publication_id(source, &page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let seed_released_relocation = page.expected_cursor.is_none();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        configured_source_root,
        context,
        options,
        source,
        &page.cursor_revision,
        &page.next_cursor.locator_identity,
        page.next_cursor.legacy_source_layout,
        current_route,
        relocation_route,
        seed_released_relocation,
        &mut summary,
    )?;
    let published_event = page
        .event
        .as_ref()
        .map(|event| {
            publish_event(
                committed_store,
                &mut group,
                context,
                options,
                source,
                &resolved,
                event,
                &mut summary,
            )
        })
        .transpose()?;
    for (touch_ordinal, touch) in &page.touches {
        if published_event.is_some_and(|(_, inserted)| !inserted) {
            continue;
        }
        publish_touch(
            committed_store,
            &mut group,
            options,
            source,
            &resolved,
            published_event.map(|(event_id, _)| event_id),
            *touch_ordinal,
            touch,
        )?;
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    if let Some(rejection) = page.rejection {
        summary.record_failure(rejection);
    }
    if !source.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

struct ResolvedSource {
    source_id: Uuid,
    session: Session,
    legacy_source_layout: bool,
    identity_path: String,
    identity_raw_path: PathBuf,
}

#[allow(clippy::too_many_arguments)]
fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    cursor_revision: &str,
    locator_identity: &str,
    legacy_source_layout: bool,
    current_route: Option<&KnownOpenHandsRoute>,
    relocation_route: Option<&KnownOpenHandsRoute>,
    seed_released_relocation: bool,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedSource> {
    let raw_source_path = source.canonical_path_text.clone();
    let source_root = configured_source_root.display().to_string();
    let physical_fingerprint = source.physical_fingerprint();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenHands NativePath source has no canonical identity",
    ))?;
    if seed_released_relocation {
        if let Some(route) = relocation_route {
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::OpenHands,
                source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.clone(),
                locator_identity: route.locator_identity.clone(),
                cursor_stream: route.current_cursor.stream.clone(),
                proposed_source_identity: route.canonical_source_identity.clone(),
                raw_source_path: Some(route.path.display().to_string()),
                source_revision: physical_fingerprint.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        }
    }
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::OpenHands,
            source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: physical_fingerprint.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let default_source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        &source.session_id,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    if relocation_route.is_some() && !resolution.relocated {
        return Err(CaptureError::InvalidPayload(
            "OpenHands renamed source did not resolve to its exact prior route".to_owned(),
        ));
    }
    let identity_route = if resolution.relocated {
        Some(current_route.or(relocation_route).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "OpenHands relocated source has no exact prior route binding".to_owned(),
            )
        })?)
    } else {
        current_route
    };
    if identity_route.is_some_and(|route| {
        route.canonical_source_identity != resolution.canonical_source_identity
    }) {
        return Err(CaptureError::InvalidPayload(
            "OpenHands current route resolved to a different canonical source".to_owned(),
        ));
    }
    let (source_id, legacy_source_layout, identity_path, identity_raw_path) =
        if let Some(route) = identity_route {
            (
                route.source_id,
                route
                    .checkpoint
                    .as_ref()
                    .map_or(legacy_source_layout, |cursor| cursor.legacy_source_layout),
                route.identity_path.clone(),
                route.identity_raw_path.clone(),
            )
        } else if legacy_source_layout {
            (
                provider_scoped_source_uuid(
                    CaptureProvider::OpenHands,
                    &source.session_id,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    Some(&source.conversation_dir.display().to_string()),
                ),
                true,
                source.path_identity.clone(),
                source.canonical_path.clone(),
            )
        } else {
            (
                default_source_id,
                false,
                source.path_identity.clone(),
                source.canonical_path.clone(),
            )
        };
    let started_at = source_event_timestamp(source).unwrap_or(context.imported_at);
    group.upsert_capture_source(&CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::OpenHands,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(OPENHANDS_FILE_EVENTS_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(source.session_id.clone()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session_id,
                "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": resolution.canonical_source_identity,
                "source_root": source_root,
                "source_revision": physical_fingerprint.clone(),
                "cursor_revision": cursor_revision,
                "physical_source_fingerprint": physical_fingerprint,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::OpenHands,
                    &source.session_id,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "source_metadata": {
                    "adapter": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    "storage": "filesystem_event_service",
                    "conversation_dir": source.conversation_dir,
                    "event_path": source.canonical_path,
                    "event_file_identity": format!(
                        "{:016x}",
                        event_file_identity_index_for_path(&identity_path)
                    ),
                    "native_identity_path": identity_path,
                    "native_identity_raw_path": identity_raw_path,
                    "native_locator_identity": locator_identity,
                    "nativepath_publication": OPENHANDS_NATIVE_CURSOR_VERSION,
                },
            }),
        ),
    })?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::OpenHands,
        &source.session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let proposed_session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::OpenHands,
        external_session_id: Some(source.session_id.clone()),
        external_agent_id: source.user_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: Some(started_at),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session_id,
                "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    "provider": "openhands",
                    "conversation_id": source.session_id,
                    "user_id": source.user_id,
                    "nativepath_publication": OPENHANDS_NATIVE_CURSOR_VERSION,
                },
            }),
        ),
    };
    let (session, existed) = match committed_store.get_session(session_id) {
        Ok(mut existing) => {
            // Each authoritative event file contributes its timestamp to the
            // session's temporal bounds, including provider outputs omitted
            // from Core. Store upsert merges these observations with MIN/MAX,
            // while preserving the released session identity and metadata.
            existing.started_at = started_at;
            existing.ended_at = Some(started_at);
            existing.timestamps.updated_at = context.imported_at;
            (existing, true)
        }
        Err(StoreError::NotFound(_)) => (proposed_session, false),
        Err(error) => return Err(error.into()),
    };
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedSource {
        source_id,
        session,
        legacy_source_layout,
        identity_path,
        identity_raw_path,
    })
}

fn source_event_timestamp(source: &OpenHandsObservedFile) -> Option<DateTime<Utc>> {
    source
        .raw_bytes
        .as_deref()
        .and_then(|bytes| decode_openhands_event(&source.canonical_path, bytes).ok())
        .map(|event| event.timestamp())
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    resolved: &ResolvedSource,
    event: &OpenHandsEventFact,
    summary: &mut ProviderImportSummary,
) -> Result<(Uuid, bool)> {
    let event_hash = event.provider_event_hash.as_str();
    let provider_event_index = event_identity_index_for_path(&resolved.identity_path, event_hash);
    let relocated_identity = (source.canonical_path != resolved.identity_raw_path)
        .then(|| exact_relocated_openhands_event_identity(committed_store, resolved, event_hash))
        .transpose()?
        .flatten();
    let exact_legacy_source = openhands_legacy_filename_index_candidate(&source.canonical_path)
        .map(|provider_event_index| ExactLegacySourceEventCandidate {
            source_id: provider_scoped_source_uuid(
                CaptureProvider::OpenHands,
                &source.session_id,
                OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                Some(&source.conversation_dir.display().to_string()),
            ),
            provider_event_index,
        });
    let identity = relocated_identity.map_or_else(
        || {
            exact_legacy_source
                .map(|candidate| {
                    exact_legacy_openhands_event_identity(
                        committed_store,
                        source,
                        resolved.source_id,
                        event_hash,
                        candidate,
                    )
                })
                .transpose()?
                .flatten()
                .map_or_else(
                    || {
                        provider_event_import_identity_with_exact_legacy_source(
                            committed_store,
                            CaptureProvider::OpenHands,
                            &source.session_id,
                            resolved.source_id,
                            provider_event_index,
                            provider_event_index,
                            event_hash,
                            None,
                            openhands_legacy_filename_index_candidate(&source.canonical_path),
                            resolved.session.id
                                == provider_session_uuid(
                                    CaptureProvider::OpenHands,
                                    &source.session_id,
                                ),
                        )
                    },
                    Ok,
                )
        },
        Ok,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or(identity.dedupe_key);
    let mut provider_metadata = event.metadata.clone();
    if let Some(metadata) = provider_metadata.as_object_mut() {
        metadata.insert(
            "provider_event_identity_index".to_owned(),
            Value::from(provider_event_index),
        );
        metadata.insert(
            "event_file_identity".to_owned(),
            Value::from(format!("{provider_event_index:016x}")),
        );
    }
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": source.session_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": event.cursor,
        "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": openhands_line_number(&source.canonical_path),
        "imported_at": context.imported_at,
        "source_record_ordinal": 0,
        "source_record_subrecord_index": 0,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::OpenHands.as_str(),
            "provider_session_id": source.session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    let inserted = group
        .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok((normalized.id, inserted))
}

fn exact_legacy_openhands_event_identity(
    committed_store: &Store,
    source: &OpenHandsObservedFile,
    incoming_source_id: Uuid,
    event_hash: &str,
    candidate: ExactLegacySourceEventCandidate,
) -> Result<Option<ProviderEventImportIdentity>> {
    let legacy_identity = provider_source_event_import_identity(
        candidate.source_id,
        candidate.provider_event_index,
        event_hash,
    );
    let event_id = match committed_store.event_id_by_dedupe_key(&legacy_identity.dedupe_key) {
        Ok(event_id) => event_id,
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let event = committed_store.get_event(event_id)?;
    if event
        .sync
        .metadata
        .pointer("/metadata/event_path")
        .and_then(Value::as_str)
        != Some(source.canonical_path_text.as_str())
    {
        return Ok(None);
    }
    Ok(event
        .dedupe_key
        .map(|dedupe_key| ProviderEventImportIdentity {
            id: event.id,
            seq: event.seq,
            dedupe_key,
            run_source_id: event.capture_source_id.or(Some(incoming_source_id)),
        }))
}

fn exact_relocated_openhands_event_identity(
    committed_store: &Store,
    resolved: &ResolvedSource,
    event_hash: &str,
) -> Result<Option<ProviderEventImportIdentity>> {
    let provider_event_index = if resolved.legacy_source_layout {
        openhands_legacy_filename_index_candidate(&resolved.identity_raw_path).unwrap_or(0)
    } else {
        event_identity_index_for_path(&resolved.identity_path, event_hash)
    };
    let identity =
        provider_source_event_import_identity(resolved.source_id, provider_event_index, event_hash);
    let event_id = match committed_store.event_id_by_dedupe_key(&identity.dedupe_key) {
        Ok(event_id) => event_id,
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let event = committed_store.get_event(event_id)?;
    let identity_raw_path = resolved.identity_raw_path.display().to_string();
    if event.capture_source_id != Some(resolved.source_id)
        || event
            .sync
            .metadata
            .pointer("/metadata/event_path")
            .and_then(Value::as_str)
            != Some(identity_raw_path.as_str())
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

// This publication boundary keeps every identity input explicit and auditable.
#[allow(clippy::too_many_arguments)]
fn publish_touch(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &ProviderImportOptions,
    source: &OpenHandsObservedFile,
    resolved: &ResolvedSource,
    event_id: Option<Uuid>,
    touch_ordinal: usize,
    touch: &OpenHandsTouchFact,
) -> Result<()> {
    let touch_ordinal = u64::try_from(touch_ordinal)
        .map_err(|_| CaptureError::SystemInvariant("OpenHands touch ordinal exceeds u64"))?;
    let (provider_event_index, provider_touch_index) = if resolved.legacy_source_layout {
        let legacy_event_index =
            openhands_legacy_filename_index_candidate(&resolved.identity_raw_path).unwrap_or(0);
        let provider_touch_index = legacy_event_index
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(touch_ordinal))
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands legacy touch identity overflowed",
            ))?;
        (Some(legacy_event_index), provider_touch_index)
    } else {
        let provider_event_index =
            event_identity_index_for_path(&resolved.identity_path, &touch.provider_event_hash);
        let provider_touch_index = if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
            touch_ordinal
        } else {
            (provider_event_index << 16) | touch_ordinal
        };
        (Some(provider_event_index), provider_touch_index)
    };
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::OpenHands,
        &source.session_id,
        resolved.source_id,
        provider_event_index,
        provider_touch_index,
        resolved.session.id
            == provider_session_uuid(CaptureProvider::OpenHands, &source.session_id),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::OpenHands.as_str(),
                "provider_session_id": source.session_id,
                "provider_touch_index": provider_touch_index,
                "provider_event_index": provider_event_index,
                "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                "session_id": resolved.session.id,
                "metadata": touch.metadata,
            }),
        ),
    })?;
    Ok(())
}

pub(super) fn record_unchanged_source(
    store: &Store,
    source: &OpenHandsObservedFile,
    context: &ProviderAdapterContext,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let stored = load_stored_core_cursor(store, source, &context.machine_id)?;
    let StoredCoreCursor::Native { cursor, .. } = stored else {
        return Err(CaptureError::SystemInvariant(
            "OpenHands unchanged source lost its NativePath cursor",
        ));
    };
    let sessions = usize::from(cursor.accepted_event || cursor.accepted_file_touches != 0);
    let events = usize::from(cursor.accepted_event);
    let touches = usize::try_from(cursor.accepted_file_touches).unwrap_or(usize::MAX);
    summary.skipped_sessions = summary.skipped_sessions.saturating_add(sessions);
    summary.skipped_events = summary.skipped_events.saturating_add(events);
    summary.skipped = summary
        .skipped
        .saturating_add(sessions)
        .saturating_add(events)
        .saturating_add(touches);
    summary.accepted_content_records = summary
        .accepted_content_records
        .saturating_add(events)
        .saturating_add(touches);
    if cursor.rejected_records != 0 {
        summary.failed = summary
            .failed
            .saturating_add(usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX));
    }
    Ok(())
}
