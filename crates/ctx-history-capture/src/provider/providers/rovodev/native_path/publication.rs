use super::*;

pub(super) fn prepare_page(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    document: &PreparedDocument,
    start: usize,
) -> Result<PreparedPage> {
    if start > document.messages.len() {
        return Err(CaptureError::InvalidPayload(
            "RovoDev NativePath frontier exceeds the source".to_owned(),
        ));
    }
    let expected_frontier = frontier(&document.messages, start)?;
    let mut messages = Vec::new();
    let mut retained_bytes = 512_usize;
    let mut units = 5_usize;
    let mut next = start;
    while next < document.messages.len() {
        let prepared = prepare_message(source, context, document, next)?;
        let message_units = prepared
            .event
            .as_ref()
            .map_or(0, |event| {
                usize::from(event.event_type == EventType::CommandOutput) + 1
            })
            .saturating_add(prepared.touches.len())
            .saturating_add(usize::from(prepared.rejection.is_some()));
        if message_units > ROVODEV_PAGE_MAX_UNITS {
            let line = next.saturating_add(1);
            messages.push(PreparedMessage {
                line,
                event: None,
                touches: Vec::new(),
                rejection: Some(failure(
                    line,
                    "RovoDev message exceeds the bounded NativePath mutation page",
                )),
                estimated_bytes: 256,
            });
            next = next.saturating_add(1);
            break;
        }
        let next_units = units.saturating_add(message_units);
        let next_bytes = retained_bytes.saturating_add(prepared.estimated_bytes);
        if !messages.is_empty()
            && (next_units > ROVODEV_PAGE_MAX_UNITS || next_bytes > ROVODEV_PAGE_MAX_BYTES)
        {
            break;
        }
        if next_bytes > ROVODEV_PAGE_MAX_BYTES {
            let line = next.saturating_add(1);
            messages.push(PreparedMessage {
                line,
                event: None,
                touches: Vec::new(),
                rejection: Some(failure(
                    line,
                    "RovoDev message exceeds the bounded NativePath byte page",
                )),
                estimated_bytes: 256,
            });
            next = next.saturating_add(1);
            break;
        }
        units = next_units;
        retained_bytes = next_bytes;
        messages.push(prepared);
        next = next.saturating_add(1);
    }
    let terminal = next == document.messages.len();
    let next_frontier = frontier(&document.messages, next)?;
    Ok(PreparedPage {
        expected_frontier,
        next_frontier,
        terminal,
        messages,
        retained_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &RovoDevSessionSource,
    configured_source_root: &Path,
    root_stream: &str,
    manifest: &mut RovoDevRootManifest,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    observation: &RovoDevSessionObservation,
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    stream: &str,
    expected_cursor: Option<String>,
    prior: Option<&RovoDevNativeCursor>,
    generation: u64,
    replacement: bool,
    document: &PreparedDocument,
    page: PreparedPage,
    aggregate: &mut ProviderImportSummary,
) -> Result<RovoDevNativeCursor> {
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let locator_identity = format!("{source_identity}:generation:{generation}");
    let source_id = source_id_for_generation(
        source,
        source_identity,
        &locator_identity,
        &document.provider_session_id,
        generation,
    );
    let next_cursor = next_cursor(
        source_identity,
        source_revision,
        physical_identity,
        &locator_identity,
        source_id,
        prior,
        generation,
        &page,
        document,
    )?;
    let next_sync_cursor = sync_cursor(
        context,
        stream,
        next_cursor.encode()?,
        CaptureProvider::RovoDev,
    );
    let source_transition = NativePathCursorTransition::new(expected_cursor, next_sync_cursor);
    let next_manifest = manifest_with_entry(
        manifest,
        manifest_entry_with_canonical(source, &next_cursor, None)?,
    );
    let root_transition =
        manifest_transition(store, context, root_stream, manifest, &next_manifest)?;
    let mut transitions = vec![source_transition];
    if let Some(transition) = root_transition {
        transitions.push(transition);
    }
    let publication_id = publication_id(source_identity, source_revision, &page, &transitions);
    let retained_bytes = transitions
        .iter()
        .map(|transition| transition.next().cursor.len())
        .sum::<usize>()
        .saturating_add(page.retained_bytes);
    let accounting = NativePathGroupAccounting::new(1, transitions.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, &transitions)? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            *manifest = next_manifest;
            return Ok(next_cursor);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    if replacement {
        if let Some(prior) = prior {
            if let Some(source_id) = prior.source_id {
                let canonical = committed_store
                    .get_capture_source(source_id)?
                    .descriptor
                    .source_identity
                    .ok_or(CaptureError::SystemInvariant(
                        "RovoDev prior source lost its canonical identity",
                    ))?;
                let retirement = ProviderSourceRouteRetirement {
                    provider: CaptureProvider::RovoDev,
                    source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    locator_identity: prior.locator_identity.clone(),
                    cursor_stream: stream.to_owned(),
                    expected_canonical_source_identity: canonical,
                    expected_source_revision: prior.source_revision.clone(),
                    retired_at_ms: context.imported_at.timestamp_millis(),
                    reason: ProviderSourceRouteRetirementReason::Replaced,
                };
                group.retire_provider_source_route(&retirement)?;
            }
        }
    }

    let mut page_summary = ProviderImportSummary::default();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        source,
        configured_source_root,
        context,
        options,
        source_identity,
        source_revision,
        &locator_identity,
        source_id,
        stream,
        document,
        &mut page_summary,
    )?;
    if let Some(resolved) = resolved.as_ref() {
        for message in &page.messages {
            publish_message(
                committed_store,
                &mut group,
                context,
                options,
                source,
                resolved,
                message,
                &mut page_summary,
            )?;
        }
    }
    for initial in document.initial_failures.iter().take(ROVODEV_MAX_FAILURES) {
        if page.expected_frontier.next_message_index == 0 {
            page_summary.record_failure(ProviderImportFailure {
                line: initial.line,
                error: initial.error.clone(),
            });
        }
    }
    for message in &page.messages {
        if let Some(rejection) = &message.rejection {
            page_summary.record_failure(ProviderImportFailure {
                line: rejection.line,
                error: rejection.error.clone(),
            });
        }
    }

    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    *manifest = next_manifest;
    page_summary.set_work_result(ProviderImportWorkResult::Changed);
    aggregate.merge_from(page_summary);
    Ok(next_cursor)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_message(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    _source: &RovoDevSessionSource,
    resolved: &ResolvedSource,
    message: &PreparedMessage,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let event_id = if let Some(event) = message.event.as_ref() {
        let event_hash = event.provider_event_hash.clone();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::RovoDev,
            resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default(),
            resolved.source_id,
            event.provider_event_index,
            event.provider_event_index,
            &event_hash,
            None,
            Some(event.provider_event_index),
            resolved.session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::RovoDev,
                    resolved
                        .session
                        .external_session_id
                        .as_deref()
                        .unwrap_or_default(),
                ),
        )?;
        let (canonical_event, run) = rovodev_canonical_event(
            resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default(),
            resolved.source_id,
            resolved.session.id,
            message.line,
            event,
            &event_hash,
            &identity,
            context,
            options,
        )?;
        if let Some(run) = run.as_ref() {
            group.upsert_run(run)?;
        }
        if group.reconcile_provider_event(
            &canonical_event,
            ProviderEventHashAuthority::ProviderSupplied,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        Some(canonical_event.id)
    } else {
        None
    };

    for touch in &message.touches {
        let provider_session_id = resolved
            .session
            .external_session_id
            .as_deref()
            .unwrap_or_default();
        let touch_id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::RovoDev,
            provider_session_id,
            resolved.source_id,
            touch.provider_event_index,
            touch.provider_touch_index,
            resolved.session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::RovoDev,
                    provider_session_id,
                ),
        )?;
        let file = rovodev_canonical_file_touch(
            touch,
            provider_session_id,
            options.history_record_id,
            resolved.source_id,
            resolved.session.id,
            event_id,
            touch_id,
        );
        group.upsert_file_touched(&file)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_rejection_cursor(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &RovoDevSessionSource,
    root_stream: &str,
    manifest: &mut RovoDevRootManifest,
    context: &ProviderAdapterContext,
    observation: &RovoDevSessionObservation,
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    stream: &str,
    expected: Option<String>,
    prior: Option<&RovoDevNativeCursor>,
    generation: u64,
    replacement: bool,
    rejection: RovoDevFailure,
) -> Result<RovoDevNativeCursor> {
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let locator_identity = format!("{source_identity}:generation:{generation}");
    let cursor = RovoDevNativeCursor {
        version: ROVODEV_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::RovoDev.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: source_revision.to_owned(),
        physical_identity: physical_identity.to_owned(),
        locator_identity,
        source_id: None,
        frontier: RovoDevFrontier::start(),
        terminal: true,
        missing: false,
        generation,
        accepted_sessions: 0,
        accepted_events: 0,
        accepted_file_touches: 0,
        rejected_records: 1,
        failures: vec![rejection],
    };
    let source_transition = NativePathCursorTransition::new(
        expected,
        sync_cursor(context, stream, cursor.encode()?, CaptureProvider::RovoDev),
    );
    let next_manifest = manifest_with_entry(
        manifest,
        manifest_entry_with_canonical(source, &cursor, None)?,
    );
    let root_transition =
        manifest_transition(store, context, root_stream, manifest, &next_manifest)?;
    let mut transitions = vec![source_transition];
    if let Some(transition) = root_transition {
        transitions.push(transition);
    }
    let publication_id = rejection_publication_id(source_identity, source_revision, &transitions);
    let retained_bytes = transitions
        .iter()
        .map(|transition| transition.next().cursor.len())
        .sum::<usize>()
        .saturating_add(256);
    let accounting = NativePathGroupAccounting::new(1, transitions.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllExpected
    ) {
        if replacement {
            if let Some(prior) = prior {
                if let Some(source_id) = prior.source_id {
                    let canonical = committed_store
                        .get_capture_source(source_id)?
                        .descriptor
                        .source_identity
                        .ok_or(CaptureError::SystemInvariant(
                            "RovoDev rejected prior source lost its canonical identity",
                        ))?;
                    group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                        provider: CaptureProvider::RovoDev,
                        source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
                        machine_id: context.machine_id.clone(),
                        locator_identity: prior.locator_identity.clone(),
                        cursor_stream: stream.to_owned(),
                        expected_canonical_source_identity: canonical,
                        expected_source_revision: prior.source_revision.clone(),
                        retired_at_ms: context.imported_at.timestamp_millis(),
                        reason: ProviderSourceRouteRetirementReason::Replaced,
                    })?;
                }
            }
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    *manifest = next_manifest;
    Ok(cursor)
}
