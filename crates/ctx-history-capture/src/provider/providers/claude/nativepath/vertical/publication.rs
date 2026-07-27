use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    prepared: PreparedClaudeCoreGroup,
) -> Result<ProviderImportSummary> {
    if prepared.sources.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "Claude publication group is empty",
        ));
    }
    for source in &prepared.sources {
        revalidate_discovered_source(&source.source).map_err(map_native_error)?;
    }
    let transitions = prepared
        .sources
        .iter()
        .map(|source| source.transition.clone())
        .collect::<Vec<_>>();
    let publication_id = group_publication_id(&prepared.sources);
    let accounting = NativePathGroupAccounting::new(
        prepared.sources.len(),
        prepared.sources.len(),
        prepared.retained_page_bytes(),
    )?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, &transitions)? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let skipped_events = prepared
                .sources
                .iter()
                .map(|source| source.page.rows.len())
                .sum();
            let mut summary = ProviderImportSummary {
                skipped_events,
                skipped: skipped_events,
                ..ProviderImportSummary::default()
            };
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    for source in &prepared.sources {
        summary.merge_from(write_core_page(
            committed_store,
            &mut group,
            &source.source,
            source_root,
            options,
            &source.stream,
            source.page.as_ref(),
            &source.cursor,
        )?);
    }
    group.prepare_journal_checkpoint()?;
    for source in &prepared.sources {
        revalidate_discovered_source(&source.source).map_err(map_native_error)?;
    }
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    stream: &str,
    page: &ClaudeNativePage,
    checkpoint: &ParseCheckpoint,
    previous: Option<&ClaudeStoreCursor>,
) -> Result<ProviderImportSummary> {
    revalidate_discovered_source(source).map_err(map_native_error)?;
    let revision = source_revision(source, options.inventory_observation_token.as_deref());
    let cursor = next_cursor_state(source, previous, page, checkpoint.clone(), &revision);
    let stored = store.get_sync_cursor(None, &options.machine_id, stream)?;
    let next = provider_sync_cursor(
        &options.machine_id,
        stream.to_owned(),
        encode_store_cursor(&cursor)?,
        options.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = publication_id(source, page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary {
                skipped_events: page.rows.len(),
                skipped: page.rows.len(),
                ..ProviderImportSummary::default()
            };
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = write_core_page(
        committed_store,
        &mut group,
        source,
        source_root,
        options,
        stream,
        page,
        &cursor,
    )?;
    revalidate_discovered_source(source).map_err(map_native_error)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_claude_retirement_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &DiscoveredClaudeSession,
    options: &ClaudeProjectsImportOptions,
    locator_identity: &str,
    stream: &str,
    cursor: &ClaudeStoreCursor,
    after: Option<&ClaudeRetirementFrontier>,
    expected_cursor: Option<String>,
) -> Result<(ClaudeStoreCursor, bool)> {
    revalidate_discovered_source(source).map_err(map_native_error)?;
    let capture_source = store.get_capture_source(cursor.source_id)?;
    let canonical_source_identity = capture_source.descriptor.source_identity.as_deref().ok_or(
        CaptureError::SystemInvariant("Claude generation capture source has no canonical identity"),
    )?;
    let key = claude_generation_key(
        options,
        canonical_source_identity,
        locator_identity,
        stream,
        cursor,
    )?;
    let after_store = after.map(ClaudeRetirementFrontier::to_store).transpose()?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, CLAUDE_RETIREMENT_ACCOUNTING_BYTES)?,
    )?;
    let preview = group.preview_source_generation_retirement_page(
        &key,
        after_store.as_ref(),
        CLAUDE_RETIREMENT_UNITS_PER_PAGE,
    )?;
    let mut next = cursor.clone();
    next.generation_phase = if preview.done {
        ClaudeGenerationPhase::Live
    } else {
        ClaudeGenerationPhase::Retiring {
            after: preview
                .next_after
                .clone()
                .map(ClaudeRetirementFrontier::from_store),
        }
    };
    if matches!(next.generation_phase, ClaudeGenerationPhase::Live) {
        next.generation_source_revision = None;
    }
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &options.machine_id,
            stream.to_owned(),
            encode_store_cursor(&next)?,
            options.imported_at,
        ),
    );
    let publication_id = generation_retirement_publication_id(source, cursor, &transition);
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            return Ok((next, false));
        }
        NativePathCursorSetClassification::AllExpected => {}
    }
    let retired = group.retire_source_generation_page(
        &key,
        after_store.as_ref(),
        CLAUDE_RETIREMENT_UNITS_PER_PAGE,
        options.imported_at.timestamp_millis(),
    )?;
    if retired != preview {
        return Err(CaptureError::SystemInvariant(
            "Claude NativePath retirement preview changed",
        ));
    }
    revalidate_discovered_source(source).map_err(map_native_error)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok((next, true))
}

#[allow(clippy::too_many_arguments)]
fn write_core_page(
    committed_store: &Store,
    group: &mut NativePathPublicationGroup<'_>,
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    stream: &str,
    page: &ClaudeNativePage,
    cursor: &ClaudeStoreCursor,
) -> Result<ProviderImportSummary> {
    let raw_path = source.canonical_path.display().to_string();
    let source_root = source_root.display().to_string();
    let locator_identity = provider_path_identity(&source.canonical_path)?;
    let proposed_source_identity = stable_capture_uuid(
        &format!(
            "claude-nativepath-session:{}",
            source.key.provider_session_id()
        ),
        "provider-source-root",
    )
    .to_string();
    let revision = source_revision(source, options.inventory_observation_token.as_deref());
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Claude,
            source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
            machine_id: options.machine_id.clone(),
            locator_identity: locator_identity.clone(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_path.clone()),
            source_revision: revision.clone(),
            observed_at_ms: options.imported_at.timestamp_millis(),
        })?;
    let provider_session_id = source.key.provider_session_id();
    let source_id = cursor.source_id;
    let session_id = provider_session_uuid(CaptureProvider::Claude, &provider_session_id);
    let parent_id = source
        .key
        .parent_provider_session_id()
        .map(|parent| provider_session_uuid(CaptureProvider::Claude, parent));
    let started_at = page
        .session
        .started_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .unwrap_or(options.imported_at);
    let capture_source = capture_source(
        source,
        source_id,
        &options.machine_id,
        source_root.as_str(),
        &resolution.canonical_source_identity,
        &revision,
        &page.session,
        started_at,
        options.imported_at,
    );
    let mut retained = NativePathRetainedSourceEntities::default();
    retained.capture_source_ids.push(capture_source.id);
    retained.session_ids.push(session_id);
    group.upsert_capture_source(&capture_source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    if let Some(parent_id) = parent_id {
        if committed_store.get_session(parent_id).is_err() {
            group.upsert_session(&relationship_placeholder(
                parent_id,
                source_id,
                source.key.root_session_id.as_str(),
                options,
            ))?;
        }
    }
    let session = canonical_session(
        source,
        source_id,
        session_id,
        parent_id,
        &page.session,
        started_at,
        options,
    );
    let session_existed = committed_store.get_session(session_id).is_ok();
    group.upsert_session(&session)?;
    let mut summary = ProviderImportSummary::default();
    if session_existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = parent_id {
        let edge = relationship_edge(source_id, session_id, parent_id, options);
        retained.session_edge_ids.push(edge.id);
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    let existing_rows = existing_claude_event_identities(committed_store, session_id, &page.rows)?;
    for row in &page.rows {
        publish_row(
            committed_store,
            group,
            source_id,
            &session,
            row,
            options,
            &mut summary,
            &mut retained,
            &existing_rows,
            !matches!(cursor.generation_phase, ClaudeGenerationPhase::Live),
        )?;
    }
    if !matches!(cursor.generation_phase, ClaudeGenerationPhase::Live) {
        dedupe_retained(&mut retained);
        group.stage_source_generation_page(
            &claude_generation_key(
                options,
                &resolution.canonical_source_identity,
                &locator_identity,
                stream,
                cursor,
            )?,
            &retained,
        )?;
    }
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(rejection.source_record_ordinal)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.diagnostic.clone(),
        });
    }
    Ok(summary)
}
