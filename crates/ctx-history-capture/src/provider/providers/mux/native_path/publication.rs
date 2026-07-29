use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    plan: &MuxSourcePlan,
    session_metadata: &MuxBoundedSessionMetadata,
    page: &MuxPreparedPage,
    accepted_events: u64,
    expected_store_cursor: Option<&SyncCursor>,
) -> Result<ProviderImportSummary> {
    let wire = MuxCursorWire {
        version: MUX_CURSOR_VERSION,
        capture_revision: MUX_CAPTURE_REVISION,
        policy_revision: MUX_POLICY_REVISION,
        kind: plan.kind,
        canonical_path: plan.observation.canonical_path.clone(),
        source_revision: plan.source_revision.clone(),
        metadata_revision: plan.metadata_revision.clone(),
        generation: plan.generation,
        frontier: page.next.clone(),
        terminal: page.terminal,
        retired: false,
        accepted_events,
        rejected_records: page.rejected_records,
        first_failure: page.first_failure.clone(),
    };
    let next = mux_sync_cursor(context, &plan.cursor_stream, &wire)?;
    let transition = NativePathCursorTransition::new(
        expected_store_cursor.map(|cursor| cursor.cursor.clone()),
        next,
    );
    // A drained oversized record contributes to source progress but is rejected
    // before publication. Account for the bounded retained page, not raw bytes
    // that will never enter the transaction.
    let accounting = NativePathGroupAccounting::new(1, 1, MUX_PAGE_MAX_BYTES.saturating_add(1024))?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut publication = store.begin_native_path_publication_group(admission, accounting)?;
    let publication_id = core_publication_id(plan, page);
    let classification =
        publication.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let mut summary = ProviderImportSummary::default();
    match classification {
        NativePathCursorSetClassification::AllExpected => {
            let locator = ProviderSourceLocatorObservation {
                provider: CaptureProvider::Mux,
                source_format: MUX_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.clone(),
                locator_identity: plan.path_identity.clone(),
                cursor_stream: plan.cursor_stream.clone(),
                proposed_source_identity: plan.canonical_source_identity.clone(),
                raw_source_path: Some(plan.path.display().to_string()),
                source_revision: plan.source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            };
            let resolution = publication.reconcile_provider_source_locator(&locator)?;
            let source_id = mux_source_uuid(&resolution.canonical_source_identity);
            publication.upsert_capture_source(&mux_capture_source(
                source_id,
                configured_root,
                context,
                plan,
                session_metadata,
                &resolution.canonical_source_identity,
            )?)?;
            publication
                .bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
            let session = mux_session(
                source_id,
                configured_root,
                context,
                options.history_record_id,
                session_metadata,
            )?;
            let session_was_present = mux_session_exists(store, session.id)?;
            publication.upsert_session(&session)?;
            if let Some(parent_session_id) = session.parent_session_id {
                publication.upsert_projection_neutral_session_edge(
                    &canonical_actor(&session),
                    &mux_parent_edge(
                        source_id,
                        configured_root,
                        context,
                        session_metadata,
                        &session,
                        parent_session_id,
                    ),
                )?;
                summary.imported_edges = summary.imported_edges.saturating_add(1);
            }
            for row in &page.rows {
                let Some(event) = row.event.as_ref() else {
                    continue;
                };
                let event_hash = row
                    .event_hash
                    .as_deref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Mux retained event has no provider hash",
                    ))?;
                let event_identity_source_id = source_id;
                let identity = avoid_provider_source_event_seq_collision(
                    store,
                    provider_source_event_import_identity(
                        event_identity_source_id,
                        row.native_ordinal,
                        event_hash,
                    ),
                    event_identity_source_id,
                    row.native_ordinal,
                    row.native_ordinal,
                )?;
                let event = mux_canonical_event(
                    &session_metadata.provider_session_id,
                    source_id,
                    session.id,
                    row,
                    event,
                    event_hash,
                    &identity,
                    context,
                    options,
                )?;
                if publication.reconcile_provider_event(
                    &event,
                    ProviderEventHashAuthority::ProviderSupplied,
                )? {
                    summary.imported = summary.imported.saturating_add(1);
                    summary.imported_events = summary.imported_events.saturating_add(1);
                } else {
                    summary.skipped = summary.skipped.saturating_add(1);
                    summary.skipped_events = summary.skipped_events.saturating_add(1);
                }
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
                for touch in &row.file_touches {
                    let touch_id = provider_file_touch_import_id(
                        store,
                        CaptureProvider::Mux,
                        &session_metadata.provider_session_id,
                        event_identity_source_id,
                        touch.provider_event_index,
                        touch.provider_touch_index,
                        false,
                    )?;
                    publication.upsert_file_touched(&mux_canonical_file_touch(
                        touch,
                        &session_metadata.provider_session_id,
                        options.history_record_id,
                        source_id,
                        session.id,
                        Some(event.id),
                        touch_id,
                    ))?;
                    summary.accepted_content_records =
                        summary.accepted_content_records.saturating_add(1);
                }
            }
            publication.prepare_journal_checkpoint()?;
            revalidate_source(plan)?;
            publication.publish_cursor_set()?;
            if page.expected.next_offset == 0 && plan.counts_session_projection() {
                summary.imported_sessions = usize::from(!session_was_present);
                summary.skipped_sessions = usize::from(session_was_present);
                summary.imported = summary.imported.saturating_add(summary.imported_sessions);
                summary.skipped = summary.skipped.saturating_add(summary.skipped_sessions);
            }
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            revalidate_source(plan)?;
            summary.skipped = summary.skipped.saturating_add(page.rows.len());
            summary.skipped_events = summary
                .skipped_events
                .saturating_add(page.rows.iter().filter(|row| row.event.is_some()).count());
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    publication.commit()?;
    let page_rejections = page
        .rejected_records
        .saturating_sub(page.previous_rejected_records);
    summary.failed = summary
        .failed
        .saturating_add(usize::try_from(page_rejections).unwrap_or(usize::MAX));
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn mux_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    row: &MuxPreparedRow,
    event: &MuxCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<Event> {
    let verified_message_locator = if event.event_type == ctx_history_core::EventType::Message {
        let content_ref = row
            .message_content_ref
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "Mux message lost its complete ContentRef",
            ))?;
        let profile = verified_content_profile(
            CaptureProvider::Mux,
            MUX_SOURCE_FORMAT,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Mux message route has no verified-content profile",
        ))?;
        let locator = VerifiedContentLocatorV1::new(
            VerifiedContentRole::MessageBody,
            profile,
            content_ref,
            CompleteContentSourceFamily::Jsonl,
            MUX_LOCATOR_KIND,
            row.source_locator.value(),
            row.native_record_id.clone(),
            row.source_record_digest.clone(),
        )
        .ok_or(CaptureError::SystemInvariant(
            "Mux message locator exceeds the bounded canonical schema",
        ))?;
        Some(
            VerifiedContentLocatorsV1::singleton(locator)
                .ok_or(CaptureError::SystemInvariant(
                    "Mux message locator collection exceeds its bound",
                ))?
                .to_metadata_value(),
        )
    } else {
        None
    };
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event.cursor,
        "source_format": MUX_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": row.line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Mux.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": row.source_record_ordinal,
        "source_record_subrecord_index": 0,
        "metadata": event.metadata,
    });
    if let (Some(metadata), Some(locator)) =
        (sync_metadata.as_object_mut(), verified_message_locator)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locator);
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
            "provider": CaptureProvider::Mux.as_str(),
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

pub(super) fn mux_session_exists(store: &Store, session_id: Uuid) -> Result<bool> {
    match store.get_session(session_id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(error) => Err(CaptureError::Store(error)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn mux_canonical_file_touch(
    touch: &MuxFileTouch,
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    FileTouched {
        id: touch_id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Mux.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "raw_source_path": touch.raw_source_path,
                "source_id": source_id,
                "source_format": MUX_SOURCE_FORMAT,
                "source_root": provider_source_root(
                    touch.source_root.as_deref(),
                    touch.raw_source_path.as_deref(),
                ),
                "metadata": touch.metadata,
                "session_id": session_id,
            }),
        ),
    }
}

pub(super) fn mux_sync_cursor(
    context: &ProviderAdapterContext,
    stream: &str,
    wire: &MuxCursorWire,
) -> Result<SyncCursor> {
    let cursor = serde_json::to_string(wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Mux.as_str(),
                context.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    })
}

pub(super) fn core_publication_id(plan: &MuxSourcePlan, page: &MuxPreparedPage) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/mux-nativepath/core-page/v1\0");
    digest.update(plan.canonical_source_identity.as_bytes());
    digest.update(plan.source_revision.as_bytes());
    digest.update(plan.generation.to_le_bytes());
    digest.update(serde_json::to_vec(&page.expected).unwrap_or_default());
    digest.update(serde_json::to_vec(&page.next).unwrap_or_default());
    digest.update([u8::from(page.terminal)]);
    format!("{MUX_PUBLICATION_PREFIX}core:{}", hex(&digest.finalize()))
}

pub(super) fn revalidate_source(plan: &MuxSourcePlan) -> Result<()> {
    if plan
        .observation
        .revalidate(&plan.path, plan.source.metadata_path.as_deref())?
    {
        Ok(())
    } else {
        Err(CaptureError::SourceChangedDuringCapture)
    }
}

pub(super) fn mux_source_uuid(canonical_source_identity: &str) -> Uuid {
    stable_capture_uuid(canonical_source_identity, "mux-nativepath-capture-source")
}
