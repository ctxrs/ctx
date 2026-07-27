use super::{lifecycle::*, *};

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    page: &NanoClawNativePage,
    cursor: &NanoClawNativeCursor,
    expected_store_cursor: Option<&SyncCursor>,
) -> Result<ProviderImportSummary> {
    let next_sync_cursor = provider_sync_cursor(
        &context.machine_id,
        &live.cursor_stream,
        cursor.encode()?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(
        expected_store_cursor.map(|cursor| cursor.cursor.clone()),
        next_sync_cursor,
    );
    let publication_id = publication_id(live, page, cursor)?;
    let accounting = NativePathGroupAccounting::new(
        1,
        1,
        page.conservative_serialized_bytes
            .saturating_add(transition.next().cursor.len()),
    )?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::NanoClaw,
            source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: live.locator_identity.clone(),
            cursor_stream: live.cursor_stream.clone(),
            proposed_source_identity: live.proposed_source_identity.clone(),
            raw_source_path: Some(live.root.display().to_string()),
            source_revision: live.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let route_binding = resolution.route_binding();
    group.upsert_capture_source(&project_capture_source(
        live,
        context,
        &resolution.canonical_source_identity,
    ))?;
    group.bind_capture_source_provider_route(live.anchor_source_id, &route_binding)?;
    let mut retained = NativePathRetainedSourceEntities::default();
    if cursor.stage_generation {
        retained.capture_source_ids.push(live.anchor_source_id);
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved_sessions = BTreeMap::<String, (Uuid, Uuid)>::new();
    for unit in &page.units {
        let session = match unit {
            NanoClawNativeUnit::Session { session, .. }
            | NanoClawNativeUnit::Message { session, .. } => session,
            NanoClawNativeUnit::Rejection { ordinal, reason } => {
                summary.record_failure(ProviderImportFailure {
                    line: line_number(*ordinal),
                    error: reason.clone(),
                });
                continue;
            }
        };
        let provider_session_id = provider_session_id(session);
        if resolved_sessions.contains_key(&provider_session_id) {
            continue;
        }
        let existing_source = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::NanoClaw,
            NANOCLAW_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &provider_session_id,
        )?;
        let source_id = existing_source
            .as_ref()
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::NanoClaw,
                    &provider_session_id,
                    NANOCLAW_SOURCE_FORMAT,
                    Some(&live.raw_source_path),
                )
            });
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::NanoClaw,
            &provider_session_id,
            source_id,
            Some(&resolution.canonical_source_identity),
        )?;
        let existed = match committed_store.get_session(session_id) {
            Ok(_) => true,
            Err(StoreError::NotFound(_)) => false,
            Err(error) => return Err(error.into()),
        };
        group.upsert_capture_source(&session_capture_source(
            live,
            context,
            session,
            source_id,
            &resolution.canonical_source_identity,
        ))?;
        group.bind_capture_source_provider_route(source_id, &route_binding)?;
        group.upsert_session(&native_session(
            live, context, options, session, source_id, session_id,
        ))?;
        if cursor.stage_generation {
            retained.capture_source_ids.push(source_id);
            retained.session_ids.push(session_id);
        }
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        resolved_sessions.insert(provider_session_id, (source_id, session_id));
    }

    for unit in &page.units {
        let NanoClawNativeUnit::Message {
            ordinal,
            session,
            message,
            locator,
            ..
        } = unit
        else {
            continue;
        };
        let seq = match message
            .seq
            .map(|seq| provider_nonnegative_i64_to_u64(seq, "NanoClaw message seq"))
            .transpose()
        {
            Ok(seq) => seq,
            Err(error) => {
                summary.record_failure(ProviderImportFailure {
                    line: line_number(*ordinal),
                    error: error.to_string(),
                });
                continue;
            }
        };
        let provider_session_id = provider_session_id(session);
        let (source_id, session_id) = resolved_sessions.get(&provider_session_id).copied().ok_or(
            CaptureError::SystemInvariant("NanoClaw message lost its page-local session"),
        )?;
        let (mut event, complete_text) =
            nanoclaw_core_event(session, message, seq, context.imported_at);
        event.metadata["source_record_ordinal"] = json!(ordinal);
        event.metadata["source_record_subrecord_index"] = json!(0);
        attach_nanoclaw_complete_content_locator(
            &mut event,
            locator,
            &nanoclaw_message_digest_values(message),
            &complete_text,
        )?;
        let event_hash = event.provider_event_hash.as_str();
        let legacy_event_hash = format!("{}:{}", message.source, message.id);
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::NanoClaw,
            &provider_session_id,
            source_id,
            event.provider_event_index,
            event.provider_event_index,
            event_hash,
            None,
            None,
            session_id == provider_session_uuid(CaptureProvider::NanoClaw, &provider_session_id),
        )?;
        let normalized = nanoclaw_canonical_event(
            &provider_session_id,
            source_id,
            session_id,
            line_number(*ordinal),
            &event,
            event_hash,
            &identity,
            context,
            options,
        )?;
        if group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
            &normalized,
            &legacy_event_hash,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        if cursor.stage_generation {
            retained.event_ids.push(normalized.id);
        }
    }
    if cursor.stage_generation {
        deduplicate_retained_entities(&mut retained);
        group.stage_source_generation_page(
            &generation_key(
                live,
                context,
                &resolution.canonical_source_identity,
                cursor.generation,
            ),
            &retained,
        )?;
    }
    if !snapshot.revalidate_before_commit()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_source_stage_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    cursor: &NanoClawNativeCursor,
    expected_store_cursor: Option<&SyncCursor>,
) -> Result<NanoClawNativeCursor> {
    let stage = cursor
        .source_stage
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "NanoClaw source-stage publication has no source-stage cursor",
        ))?;
    let canonical_source_identity =
        canonical_source_identity(committed_store, live.anchor_source_id)?;
    let mut source_ids =
        nanoclaw_capture_source_ids(committed_store, context, &canonical_source_identity)?
            .into_iter()
            .filter(|id| stage.after.is_none_or(|after| *id > after))
            .collect::<Vec<_>>();
    source_ids.sort_unstable();
    let has_more = source_ids.len() > NANOCLAW_SOURCE_STAGE_PAGE_IDS;
    source_ids.truncate(NANOCLAW_SOURCE_STAGE_PAGE_IDS);

    let mut next = cursor.clone();
    if has_more {
        next.source_stage = Some(NanoClawSourceStage {
            after: source_ids.last().copied(),
        });
    } else {
        next.source_stage = None;
        next.retirement = Some(NanoClawRetirementRequest { after: None });
    }
    let transition = NativePathCursorTransition::new(
        expected_store_cursor.map(|stored| stored.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            &live.cursor_stream,
            next.encode()?,
            context.imported_at,
        ),
    );
    let publication_id = source_stage_publication_id(live, cursor, &next, &source_ids);
    let accounting = NativePathGroupAccounting::new(
        1,
        1,
        NANOCLAW_LIFECYCLE_PAGE_BYTES
            .saturating_add(source_ids.len().saturating_mul(16))
            .saturating_add(transition.next().cursor.len()),
    )?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution =
        group.reconcile_provider_source_locator(&nanoclaw_locator_observation(live, context))?;
    if resolution.canonical_source_identity != canonical_source_identity {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.upsert_capture_source(&project_capture_source(
        live,
        context,
        &canonical_source_identity,
    ))?;
    group.bind_capture_source_provider_route(live.anchor_source_id, &resolution.route_binding())?;
    source_ids.push(live.anchor_source_id);
    source_ids.sort_unstable();
    source_ids.dedup();
    group.stage_source_generation_page(
        &generation_key(live, context, &canonical_source_identity, cursor.generation),
        &NativePathRetainedSourceEntities {
            capture_source_ids: source_ids,
            ..NativePathRetainedSourceEntities::default()
        },
    )?;
    if !snapshot.revalidate_before_commit()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_omission_retirement_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    cursor: &NanoClawNativeCursor,
    expected_store_cursor: Option<&SyncCursor>,
) -> Result<NanoClawNativeCursor> {
    let request = cursor
        .retirement
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "NanoClaw omission retirement has no retirement cursor",
        ))?;
    let after = request
        .after
        .as_ref()
        .map(NanoClawRetirementFrontier::to_store)
        .transpose()?;
    let canonical_source_identity =
        canonical_source_identity(committed_store, live.anchor_source_id)?;
    let key = generation_key(live, context, &canonical_source_identity, cursor.generation);
    let accounting = NativePathGroupAccounting::new(1, 1, NANOCLAW_LIFECYCLE_PAGE_BYTES)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let preview = group.preview_source_generation_retirement_page(
        &key,
        after.as_ref(),
        NANOCLAW_RETIREMENT_PAGE_ENTITIES,
    )?;
    let mut next = cursor.clone();
    if preview.done {
        next.stage_generation = false;
        next.retirement = None;
        next.terminal = true;
    } else {
        next.retirement = Some(NanoClawRetirementRequest {
            after: preview
                .next_after
                .clone()
                .map(NanoClawRetirementFrontier::from_store),
        });
    }
    let transition = NativePathCursorTransition::new(
        expected_store_cursor.map(|stored| stored.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            &live.cursor_stream,
            next.encode()?,
            context.imported_at,
        ),
    );
    let publication_id = omission_publication_id(live, cursor, &next, &preview);
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution =
        group.reconcile_provider_source_locator(&nanoclaw_locator_observation(live, context))?;
    if resolution.canonical_source_identity != canonical_source_identity {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let actual = group.retire_source_generation_page(
        &key,
        after.as_ref(),
        NANOCLAW_RETIREMENT_PAGE_ENTITIES,
        context.imported_at.timestamp_millis(),
    )?;
    if actual != preview {
        return Err(CaptureError::SystemInvariant(
            "NanoClaw omission retirement diverged from Store preview",
        ));
    }
    if !snapshot.revalidate_before_commit()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn nanoclaw_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &NanoClawCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<Event> {
    let mut provider_metadata = event.metadata.clone();
    let source_record_ordinal = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_ordinal"))
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "NanoClaw source record ordinal annotation is malformed".to_owned(),
            )
        })?;
    let source_record_subrecord_index = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_subrecord_index"))
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "NanoClaw source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value)
                .map(|locators| locators.to_metadata_value())
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "NanoClaw verified content locator annotation is malformed".to_owned(),
                    )
                })
        })
        .transpose()?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::NormalizedPayloadFallback.as_str(),
        "cursor": event.cursor,
        "source_format": NANOCLAW_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::NanoClaw.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": source_record_ordinal,
        "source_record_subrecord_index": source_record_subrecord_index,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::NanoClaw.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

pub(super) fn attach_nanoclaw_complete_content_locator(
    event: &mut NanoClawCoreEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != ctx_history_core::EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("NanoClaw content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "NanoClaw message route must have a verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        nanoclaw_logical_record_digest(values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "NanoClaw complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("NanoClaw verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(super) fn nanoclaw_logical_record_digest(
    values: &[NativeSqliteValue],
) -> Result<CompleteContentBodyDigest> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-complete-content-sqlite-logical-row-v1\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("NanoClaw SHA-256 formatting produced an invalid digest"),
    )
}

pub(super) fn project_capture_source(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    canonical_source_identity: &str,
) -> CaptureSource {
    CaptureSource {
        id: live.anchor_source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::NanoClaw,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: Some(live.root.display().to_string()),
            raw_source_path: Some(live.raw_source_path.clone()),
            source_format: Some(NANOCLAW_SOURCE_FORMAT.to_owned()),
            source_root: Some(live.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(NANOCLAW_PROJECT_EXTERNAL_SESSION.to_owned()),
        },
        started_at: context.imported_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "source_format": NANOCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": live.source_root,
                "source_revision": live.source_revision,
                "nativepath_publication": NANOCLAW_NATIVE_PUBLICATION_REVISION,
                "project_anchor": true,
            }),
        ),
    }
}

pub(super) fn session_capture_source(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    session: &NanoClawSessionRow,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    let provider_session_id = provider_session_id(session);
    let started_at = provider_timestamp_millis(session.created_at, context.imported_at);
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::NanoClaw,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.agent_group_folder.clone(),
            raw_source_path: Some(live.raw_source_path.clone()),
            source_format: Some(NANOCLAW_SOURCE_FORMAT.to_owned()),
            source_root: Some(live.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(provider_session_id.clone()),
        },
        started_at,
        ended_at: session
            .last_active
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at)),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": NANOCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": live.source_root,
                "source_revision": live.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::NanoClaw,
                    &provider_session_id,
                    NANOCLAW_SOURCE_FORMAT,
                    Some(&live.raw_source_path),
                ),
                "adapter": NANOCLAW_SOURCE_FORMAT,
                "central_db": live.central_path,
                "sqlite_user_version": live.user_version,
                "schema_fingerprint": live.schema_fingerprint,
                "support_level": "explicit",
                "nativepath_publication": NANOCLAW_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

pub(super) fn native_session(
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    row: &NanoClawSessionRow,
    source_id: Uuid,
    session_id: Uuid,
) -> Session {
    let provider_session_id = provider_session_id(row);
    let started_at = provider_timestamp_millis(row.created_at, context.imported_at);
    let ended_at = row
        .last_active
        .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at));
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::NanoClaw,
        external_session_id: Some(provider_session_id.clone()),
        external_agent_id: row.agent_provider.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("container-session".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": NANOCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::NanoClaw.as_str(),
                    provider_session_id,
                ),
                "metadata": {
                    "source_format": NANOCLAW_SOURCE_FORMAT,
                    "session_id": row.id,
                    "agent_group_id": row.agent_group_id,
                    "agent_group_name": row.agent_group_name,
                    "agent_provider": row.agent_provider,
                    "status": row.status,
                    "container_status": row.container_status,
                    "messaging_group_id": row.messaging_group_id,
                    "messaging": {
                        "channel_type": row.messaging_channel_type,
                        "platform_id": row.messaging_platform_id,
                        "instance": row.messaging_instance,
                        "name": row.messaging_name,
                        "thread_id": row.thread_id,
                    },
                    "central_db": live.central_path,
                    "sqlite_user_version": live.user_version,
                    "schema_fingerprint": live.schema_fingerprint,
                    "nativepath_publication": NANOCLAW_NATIVE_PUBLICATION_REVISION,
                },
            }),
        ),
    }
}
