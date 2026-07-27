use super::*;

pub(super) fn publish_core(
    store: &mut Store,
    reader: &OpenCodeNativePathReader,
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let page_limits = OpenCodeNativePageLimits {
        rows: OPENCODE_NATIVE_STORE_PAGE_ROWS,
        ..OpenCodeNativePageLimits::default()
    };
    let mut scanner = reader.scanner(page_limits)?;
    let resume_frontier = resumable_frontier(stored, context);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut expected_cursor = stored_sync_cursor(stored).cloned();
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut rejection_progress = rejection_progress(stored, context);
        let mut durable_frontier = resumable_frontier(stored, context);
        while let Some(page) = scanner.next_page()? {
            if frontier_at_or_before(page.next_frontier, resume_frontier) {
                expected_cursor = stored_sync_cursor(stored).cloned();
                continue;
            }
            let terminal = page.terminal;
            rejection_progress.observe_page(&page)?;
            if !rejection_progress.blocked {
                durable_frontier = page.next_frontier;
            }
            let generation_phase = if terminal && !rejection_progress.blocked {
                if generation_capture_source_page(&committed_store, context, None, 1)?
                    .0
                    .is_empty()
                {
                    OpenCodeGenerationPhase::Complete
                } else {
                    OpenCodeGenerationPhase::StageSources { after: None }
                }
            } else {
                OpenCodeGenerationPhase::Scan
            };
            let next_wire = next_store_cursor(
                context,
                durable_frontier,
                rejection_progress.rejected_records(),
                rejection_progress.rejections.clone(),
                generation_phase,
            );
            if rejection_progress.blocked
                && expected_cursor
                    .as_ref()
                    .is_some_and(|expected| committed_cursor_matches(expected, &next_wire))
            {
                apply_cursor_rejections(&mut summary, &next_wire);
                summary.set_work_result(ProviderImportWorkResult::NoOp);
                summary.work_remaining = true;
                return Ok(summary);
            }
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                reader,
                expected_cursor.as_ref(),
                context,
                page,
                next_wire,
            )?;
            if page_summary.work_result() == ProviderImportWorkResult::Changed {
                changed_groups = changed_groups.saturating_add(1);
            }
            summary.merge_from(page_summary);
            expected_cursor =
                store.get_sync_cursor(None, &context.adapter.machine_id, &context.cursor_stream)?;
            if rejection_progress.blocked {
                let cursor = expected_cursor
                    .as_ref()
                    .map(|stored| decode_current_cursor(&stored.cursor))
                    .transpose()?
                    .ok_or(CaptureError::SystemInvariant(
                        "OpenCode NativePath rejected page lost its cursor",
                    ))?;
                apply_cursor_rejections(&mut summary, &cursor);
                summary.work_remaining = true;
                return Ok(summary);
            }
            if context.options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                let cursor = expected_cursor
                    .as_ref()
                    .map(|stored| decode_current_cursor(&stored.cursor))
                    .transpose()?
                    .ok_or(CaptureError::SystemInvariant(
                        "OpenCode NativePath committed page lost its cursor",
                    ))?;
                apply_cursor_rejections(&mut summary, &cursor);
                summary.work_remaining =
                    !matches!(cursor.generation_phase, OpenCodeGenerationPhase::Complete);
                return Ok(summary);
            }
        }
        let finished = scanner.finish()?;
        if !same_generation(&finished.persisted_state(), &context.current_state) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut cursor = expected_cursor
            .as_ref()
            .map(|stored| decode_current_cursor(&stored.cursor))
            .transpose()?;
        if let Some(blocked) = cursor
            .as_ref()
            .filter(|cursor| cursor.rejected_records != 0)
        {
            apply_cursor_rejections(&mut summary, blocked);
            summary.work_remaining = true;
            return Ok(summary);
        }
        while let Some(current) = cursor.clone() {
            let (next, changed) = match current.generation_phase.clone() {
                OpenCodeGenerationPhase::Scan | OpenCodeGenerationPhase::Complete => break,
                OpenCodeGenerationPhase::StageSources { after } => publish_source_stage_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    reader,
                    expected_cursor.as_ref(),
                    context,
                    &current,
                    after,
                )?,
                OpenCodeGenerationPhase::Retire { after } => publish_generation_retirement_page(
                    store,
                    &bulk_guard,
                    reader,
                    expected_cursor.as_ref(),
                    context,
                    &current,
                    after.as_ref(),
                )?,
            };
            if changed {
                changed_groups = changed_groups.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
            expected_cursor =
                store.get_sync_cursor(None, &context.adapter.machine_id, &context.cursor_stream)?;
            cursor = Some(next);
            if context.options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed {
                summary.work_remaining = !matches!(
                    cursor.as_ref().map(|cursor| &cursor.generation_phase),
                    Some(OpenCodeGenerationPhase::Complete)
                );
                return Ok(summary);
            }
        }
        if summary.imported == 0 && summary.skipped == 0 && summary.failed == 0 {
            summary.set_work_result(if changed_groups == 0 {
                ProviderImportWorkResult::NoOp
            } else {
                ProviderImportWorkResult::Changed
            });
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn resumable_frontier(
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> OpenCodeNativeFrontier {
    match stored {
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.version == OPENCODE_NATIVE_STORE_CURSOR_VERSION
                && cursor.generation == context.generation
                && same_generation(&cursor.pending_state, &context.current_state) =>
        {
            cursor.frontier
        }
        _ => OpenCodeNativeFrontier {
            phase: OpenCodeNativeScanPhase::Sessions,
            scan_ordinal: 0,
        },
    }
}

pub(super) fn frontier_at_or_before(
    candidate: OpenCodeNativeFrontier,
    committed: OpenCodeNativeFrontier,
) -> bool {
    frontier_key(candidate) <= frontier_key(committed)
}

pub(super) fn frontier_key(frontier: OpenCodeNativeFrontier) -> (u8, u64) {
    (
        match frontier.phase {
            OpenCodeNativeScanPhase::Sessions => 0,
            OpenCodeNativeScanPhase::Events => 1,
            OpenCodeNativeScanPhase::Complete => 2,
        },
        frontier.scan_ordinal,
    )
}

pub(super) fn stored_sync_cursor(stored: &StoredCursor) -> Option<&SyncCursor> {
    match stored {
        StoredCursor::Native { stored, .. } | StoredCursor::Released { stored } => Some(stored),
        StoredCursor::None => None,
    }
}

pub(super) fn committed_cursor_matches(
    stored: &SyncCursor,
    next: &OpenCodeNativeStoreCursor,
) -> bool {
    decode_native_path_committed_cursor(&stored.cursor)
        .ok()
        .and_then(|committed| serde_json::from_str(committed.provider_cursor()).ok())
        .is_some_and(|current: OpenCodeNativeStoreCursor| current == *next)
}

struct OpenCodeRejectionProgress {
    prior_rejected_records: u64,
    scanned_rejected_records: u64,
    blocked: bool,
    reset_on_first_page: bool,
    rejections: Vec<OpenCodeStoredRejection>,
}

impl OpenCodeRejectionProgress {
    fn observe_page(&mut self, page: &OpenCodeNativePage) -> Result<()> {
        if self.reset_on_first_page {
            self.prior_rejected_records = 0;
            self.scanned_rejected_records = 0;
            self.rejections.clear();
            self.reset_on_first_page = false;
        }
        let rejected = u64::try_from(page.rejections.len()).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode NativePath rejection count exceeds u64")
        })?;
        self.scanned_rejected_records = self.scanned_rejected_records.checked_add(rejected).ok_or(
            CaptureError::SystemInvariant("OpenCode NativePath rejection count overflowed"),
        )?;
        if rejected != 0 {
            self.blocked = true;
        }
        for rejection in page_rejections(page) {
            if !self.rejections.contains(&rejection)
                && self.rejections.len() < crate::summaries::MAX_RETAINED_PROVIDER_FAILURES
            {
                self.rejections.push(rejection);
            }
        }
        Ok(())
    }

    fn rejected_records(&self) -> u64 {
        self.prior_rejected_records
            .max(self.scanned_rejected_records)
    }
}

fn rejection_progress(
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> OpenCodeRejectionProgress {
    let prior = match stored {
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.version == OPENCODE_NATIVE_STORE_CURSOR_VERSION
                && cursor.generation == context.generation
                && same_generation(&cursor.pending_state, &context.current_state) =>
        {
            Some(cursor)
        }
        _ => None,
    };
    OpenCodeRejectionProgress {
        prior_rejected_records: prior.map_or(0, |cursor| cursor.rejected_records),
        scanned_rejected_records: 0,
        blocked: false,
        reset_on_first_page: prior.is_some_and(|cursor| cursor.rejected_records != 0),
        rejections: prior.map_or_else(Vec::new, |cursor| cursor.rejections.clone()),
    }
}

pub(super) fn page_rejections(page: &OpenCodeNativePage) -> Vec<OpenCodeStoredRejection> {
    page.rejections
        .iter()
        .map(|rejection| OpenCodeStoredRejection {
            native_identity: bounded_text(
                rejection.native_identity.clone(),
                OPENCODE_NATIVE_MAX_REJECTION_IDENTITY_BYTES,
            ),
            line: rejection_line(page, rejection.native_order.is_some()),
            error: bounded_text(
                format!("{}: {}", rejection.kind.label(), rejection.reason),
                OPENCODE_NATIVE_MAX_REJECTION_TEXT_BYTES,
            ),
        })
        .collect()
}

pub(super) fn rejection_line(page: &OpenCodeNativePage, has_native_order: bool) -> usize {
    usize::try_from(if has_native_order {
        page.next_frontier.scan_ordinal
    } else {
        page.position.native_events_seen
    })
    .unwrap_or(usize::MAX)
    .saturating_add(1)
}

pub(super) fn bounded_text(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value
}

pub(super) fn apply_cursor_rejections(
    summary: &mut ProviderImportSummary,
    cursor: &OpenCodeNativeStoreCursor,
) {
    summary.failed = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
    summary.failures = cursor
        .rejections
        .iter()
        .map(|rejection| ProviderImportFailure {
            line: rejection.line,
            error: rejection.error.clone(),
        })
        .collect();
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    reader: &OpenCodeNativePathReader,
    expected_cursor: Option<&SyncCursor>,
    context: &OpenCodePublicationContext<'_>,
    page: OpenCodeNativePage,
    next_wire: OpenCodeNativeStoreCursor,
) -> Result<ProviderImportSummary> {
    if page.source_authority.selected_path() != context.selected_path {
        return Err(CaptureError::SystemInvariant(
            "OpenCode NativePath page escaped its selected source",
        ));
    }
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let next_cursor = provider_sync_cursor(
        context,
        serde_json::to_string(&next_wire)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    );
    let transition = NativePathCursorTransition::new(
        expected_cursor.map(|cursor| cursor.cursor.clone()),
        next_cursor,
    );
    let publication_id = page_publication_id(context, &page, &transition);
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
        for rejection in page_rejections(&page) {
            summary.record_failure(ProviderImportFailure {
                line: rejection.line,
                error: rejection.error,
            });
        }
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    if context.replacement
        && page.expected_frontier.phase == OpenCodeNativeScanPhase::Sessions
        && page.expected_frontier.scan_ordinal == 0
    {
        if let Some(retirement) = replacement_retirement(expected_cursor, context)? {
            group.retire_provider_source_route(&retirement)?;
        }
    }
    let locator = ProviderSourceLocatorObservation {
        provider: context.dialect.provider,
        source_format: context.dialect.source_format.to_owned(),
        machine_id: context.adapter.machine_id.clone(),
        locator_identity: context.locator_identity.clone(),
        cursor_stream: context.cursor_stream.clone(),
        proposed_source_identity: context.canonical_source_identity.clone(),
        raw_source_path: Some(context.raw_source_path.clone()),
        source_revision: context.source_revision.clone(),
        observed_at_ms: context.adapter.imported_at.timestamp_millis(),
    };
    let resolution = group.reconcile_provider_source_locator(&locator)?;
    if resolution.canonical_source_identity != context.canonical_source_identity {
        return Err(CaptureError::InvalidPayload(format!(
            "{} source route resolved to an unexpected logical source",
            context.dialect.display_name
        )));
    }
    let route_binding = resolution.route_binding();
    let mut summary = ProviderImportSummary::default();
    let mut retained = NativePathRetainedSourceEntities::default();
    let page_session_ids = page
        .sessions
        .iter()
        .map(|session| session.native_identity.as_str())
        .collect::<BTreeSet<_>>();
    for session in &page.sessions {
        publish_session(
            committed_store,
            &mut group,
            context,
            &route_binding,
            resolution.relocated,
            &page_session_ids,
            session,
            &mut summary,
            &mut retained,
        )?;
    }
    for event in &page.events {
        publish_event(
            reader,
            committed_store,
            &mut group,
            context,
            &route_binding,
            resolution.relocated,
            event,
            &mut summary,
            &mut retained,
        )?;
    }
    for rejection in page_rejections(&page) {
        summary.record_failure(ProviderImportFailure {
            line: rejection.line,
            error: rejection.error,
        });
    }
    deduplicate_retained_entities(&mut retained);
    if !retained.capture_source_ids.is_empty() {
        group.stage_source_generation_page(&generation_key(context), &retained)?;
    }
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn next_store_cursor(
    context: &OpenCodePublicationContext<'_>,
    frontier: OpenCodeNativeFrontier,
    rejected_records: u64,
    rejections: Vec<OpenCodeStoredRejection>,
    generation_phase: OpenCodeGenerationPhase,
) -> OpenCodeNativeStoreCursor {
    let complete = matches!(generation_phase, OpenCodeGenerationPhase::Complete);
    OpenCodeNativeStoreCursor {
        version: OPENCODE_NATIVE_STORE_CURSOR_VERSION,
        provider: context.dialect.provider.as_str().to_owned(),
        source_format: context.dialect.source_format.to_owned(),
        selected_path: context.selected_path.to_path_buf(),
        cursor_path_identity: context.cursor_path_identity.clone(),
        locator_identity: context.locator_identity.clone(),
        canonical_source_identity: context.canonical_source_identity.clone(),
        source_revision: context.source_revision.clone(),
        generation: context.generation,
        rejected_records,
        rejections,
        frontier,
        generation_phase,
        route_retired: false,
        completed_state: complete.then(|| context.current_state.clone()),
        pending_state: context.current_state.clone(),
    }
}

pub(super) fn provider_sync_cursor(
    context: &OpenCodePublicationContext<'_>,
    cursor: String,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                context.dialect.provider.as_str(),
                context.adapter.machine_id,
                context.cursor_stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.adapter.machine_id.clone(),
        stream: context.cursor_stream.clone(),
        cursor,
        last_synced_at: Some(context.adapter.imported_at),
        timestamps: timestamps(context.adapter.imported_at),
    }
}

pub(super) fn page_publication_id(
    context: &OpenCodePublicationContext<'_>,
    page: &OpenCodeNativePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_PUBLICATION_DOMAIN);
    hash_field(&mut digest, context.dialect.provider.as_str().as_bytes());
    hash_field(&mut digest, context.dialect.source_format.as_bytes());
    hash_field(&mut digest, context.canonical_source_identity.as_bytes());
    digest.update(context.generation.to_le_bytes());
    digest.update(page.identity.0);
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("opencode-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn generation_key(
    context: &OpenCodePublicationContext<'_>,
) -> NativePathSourceGenerationKey {
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_GENERATION_DOMAIN);
    hash_field(&mut digest, context.locator_identity.as_bytes());
    hash_field(&mut digest, context.source_revision.as_bytes());
    digest.update(context.generation.to_be_bytes());
    NativePathSourceGenerationKey {
        provider: context.dialect.provider,
        source_format: context.dialect.source_format.to_owned(),
        machine_id: context.adapter.machine_id.clone(),
        canonical_source_identity: context.canonical_source_identity.clone(),
        locator_identity: context.locator_identity.clone(),
        cursor_stream: context.cursor_stream.clone(),
        source_revision: context.source_revision.clone(),
        generation_id: format!("opencode-nativepath-generation-v1:{:x}", digest.finalize()),
    }
}

pub(super) fn locator_observation(
    context: &OpenCodePublicationContext<'_>,
) -> ProviderSourceLocatorObservation {
    ProviderSourceLocatorObservation {
        provider: context.dialect.provider,
        source_format: context.dialect.source_format.to_owned(),
        machine_id: context.adapter.machine_id.clone(),
        locator_identity: context.locator_identity.clone(),
        cursor_stream: context.cursor_stream.clone(),
        proposed_source_identity: context.canonical_source_identity.clone(),
        raw_source_path: Some(context.raw_source_path.clone()),
        source_revision: context.source_revision.clone(),
        observed_at_ms: context.adapter.imported_at.timestamp_millis(),
    }
}

pub(super) fn generation_capture_source_page(
    store: &Store,
    context: &OpenCodePublicationContext<'_>,
    after: Option<Uuid>,
    limit: usize,
) -> Result<(Vec<Uuid>, bool)> {
    let mut source_ids = store
        .list_capture_sources()?
        .into_iter()
        .filter(|source| {
            source.descriptor.provider == context.dialect.provider
                && source.descriptor.machine_id == context.adapter.machine_id
                && source.descriptor.source_format.as_deref() == Some(context.dialect.source_format)
                && source.descriptor.source_identity.as_deref()
                    == Some(context.canonical_source_identity.as_str())
                && source.sync.deleted_at.is_none()
                && after.is_none_or(|after| source.id > after)
        })
        .map(|source| source.id)
        .collect::<Vec<_>>();
    source_ids.sort_unstable();
    source_ids.dedup();
    let has_more = source_ids.len() > limit;
    source_ids.truncate(limit);
    Ok((source_ids, has_more))
}

pub(super) fn deduplicate_retained_entities(retained: &mut NativePathRetainedSourceEntities) {
    retained.capture_source_ids.sort_unstable();
    retained.capture_source_ids.dedup();
    retained.session_ids.sort_unstable();
    retained.session_ids.dedup();
    retained.session_edge_ids.sort_unstable();
    retained.session_edge_ids.dedup();
    retained.run_ids.sort_unstable();
    retained.run_ids.dedup();
    retained.event_ids.sort_unstable();
    retained.event_ids.dedup();
    retained.file_touch_ids.sort_unstable();
    retained.file_touch_ids.dedup();
}

pub(super) fn lifecycle_publication_id<T: Serialize>(
    domain: &[u8],
    context: &OpenCodePublicationContext<'_>,
    current: &OpenCodeNativeStoreCursor,
    next: &OpenCodeNativeStoreCursor,
    page: &T,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(domain);
    hash_field(&mut digest, context.dialect.provider.as_str().as_bytes());
    hash_field(&mut digest, context.dialect.source_format.as_bytes());
    hash_field(&mut digest, context.canonical_source_identity.as_bytes());
    digest.update(context.generation.to_be_bytes());
    hash_field(&mut digest, serde_json::to_string(current)?.as_bytes());
    hash_field(&mut digest, serde_json::to_string(next)?.as_bytes());
    hash_field(&mut digest, serde_json::to_string(page)?.as_bytes());
    Ok(format!(
        "opencode-nativepath-lifecycle-v1:{:x}",
        digest.finalize()
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_source_stage_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    reader: &OpenCodeNativePathReader,
    expected_cursor: Option<&SyncCursor>,
    context: &OpenCodePublicationContext<'_>,
    current: &OpenCodeNativeStoreCursor,
    after: Option<Uuid>,
) -> Result<(OpenCodeNativeStoreCursor, bool)> {
    let (source_ids, has_more) = generation_capture_source_page(
        committed_store,
        context,
        after,
        OPENCODE_NATIVE_SOURCE_STAGE_IDS,
    )?;
    let generation_phase = if source_ids.is_empty() {
        OpenCodeGenerationPhase::Complete
    } else if has_more {
        OpenCodeGenerationPhase::StageSources {
            after: source_ids.last().copied(),
        }
    } else {
        OpenCodeGenerationPhase::Retire { after: None }
    };
    let next = next_store_cursor(context, current.frontier, 0, Vec::new(), generation_phase);
    let transition = NativePathCursorTransition::new(
        expected_cursor.map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(context, serde_json::to_string(&next)?),
    );
    let publication_id = lifecycle_publication_id(
        OPENCODE_NATIVE_SOURCE_STAGE_DOMAIN,
        context,
        current,
        &next,
        &source_ids,
    )?;
    let accounting = NativePathGroupAccounting::new(
        1,
        1,
        OPENCODE_NATIVE_LIFECYCLE_PAGE_BYTES
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
        return Ok((next, false));
    }
    let resolution = group.reconcile_provider_source_locator(&locator_observation(context))?;
    if resolution.canonical_source_identity != context.canonical_source_identity {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if !source_ids.is_empty() {
        for source_id in &source_ids {
            group.bind_capture_source_provider_route(*source_id, &resolution.route_binding())?;
        }
        group.stage_source_generation_page(
            &generation_key(context),
            &NativePathRetainedSourceEntities {
                capture_source_ids: source_ids,
                ..NativePathRetainedSourceEntities::default()
            },
        )?;
    }
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok((next, true))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_generation_retirement_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    reader: &OpenCodeNativePathReader,
    expected_cursor: Option<&SyncCursor>,
    context: &OpenCodePublicationContext<'_>,
    current: &OpenCodeNativeStoreCursor,
    after: Option<&OpenCodeRetirementFrontier>,
) -> Result<(OpenCodeNativeStoreCursor, bool)> {
    let store_after = after
        .map(OpenCodeRetirementFrontier::to_store)
        .transpose()?;
    let accounting = NativePathGroupAccounting::new(1, 1, OPENCODE_NATIVE_LIFECYCLE_PAGE_BYTES)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let preview = group.preview_source_generation_retirement_page(
        &generation_key(context),
        store_after.as_ref(),
        OPENCODE_NATIVE_RETIREMENT_ENTITIES,
    )?;
    let generation_phase = if preview.done {
        OpenCodeGenerationPhase::Complete
    } else {
        OpenCodeGenerationPhase::Retire {
            after: preview
                .next_after
                .clone()
                .map(OpenCodeRetirementFrontier::from_store),
        }
    };
    let next = next_store_cursor(context, current.frontier, 0, Vec::new(), generation_phase);
    let transition = NativePathCursorTransition::new(
        expected_cursor.map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(context, serde_json::to_string(&next)?),
    );
    let preview_identity = (
        preview
            .next_after
            .as_ref()
            .map(|frontier| (frontier.kind.as_str(), frontier.id)),
        preview.done,
        preview.inspected,
        preview.retired,
    );
    let publication_id = lifecycle_publication_id(
        OPENCODE_NATIVE_RETIREMENT_DOMAIN,
        context,
        current,
        &next,
        &preview_identity,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok((next, false));
    }
    let resolution = group.reconcile_provider_source_locator(&locator_observation(context))?;
    if resolution.canonical_source_identity != context.canonical_source_identity {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let actual = group.retire_source_generation_page(
        &generation_key(context),
        store_after.as_ref(),
        OPENCODE_NATIVE_RETIREMENT_ENTITIES,
        context.adapter.imported_at.timestamp_millis(),
    )?;
    if actual != preview {
        return Err(CaptureError::SystemInvariant(
            "OpenCode NativePath retirement diverged from Store preview",
        ));
    }
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok((next, true))
}

pub(super) fn replacement_retirement(
    expected_cursor: Option<&SyncCursor>,
    context: &OpenCodePublicationContext<'_>,
) -> Result<Option<ProviderSourceRouteRetirement>> {
    if !context.replacement {
        return Ok(None);
    }
    let Some(expected_cursor) = expected_cursor else {
        return Ok(None);
    };
    let Ok(prior) = decode_current_cursor(&expected_cursor.cursor) else {
        return Ok(None);
    };
    if prior.route_retired || prior.locator_identity == context.locator_identity {
        return Ok(None);
    }
    Ok(Some(ProviderSourceRouteRetirement {
        provider: context.dialect.provider,
        source_format: context.dialect.source_format.to_owned(),
        machine_id: context.adapter.machine_id.clone(),
        locator_identity: prior.locator_identity,
        cursor_stream: context.cursor_stream.clone(),
        expected_canonical_source_identity: prior.canonical_source_identity,
        expected_source_revision: prior.source_revision,
        retired_at_ms: context.adapter.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::Replaced,
    }))
}

pub(super) fn decode_current_cursor(encoded: &str) -> Result<OpenCodeNativeStoreCursor> {
    let committed = decode_native_path_committed_cursor(encoded)?;
    serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}
