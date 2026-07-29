use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_core_source(
    store: &mut Store,
    committed_store: &Store,
    bulk: &EventSearchBulkGuard,
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    changed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let observation = JunieSessionObservation::read(session_path)?;
    let provider_session_id = junie_provider_session_id(session_path)?;
    let locator_identity = provider_path_identity(&session_path.events_path)?;
    let canonical_identity = provider_path_identity(&observation.canonical_path)?;
    let source_identity = format!("junie-session-events:{canonical_identity}");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let origin = load_cursor(store, &context.machine_id, &stream, &source_identity)?;
    let mut plan = plan_cursor(
        session_path,
        &observation,
        &source_identity,
        context.imported_at,
        origin,
    )?;
    if plan.cursor.terminal
        && plan.cursor.frontier.pending.is_none()
        && plan.cursor.source_revision == observation.source_revision()
        && plan.cursor.observed_length == observation.events_file.length
        && plan.cursor.frontier.offset == observation.events_file.length
    {
        let mut summary = ProviderImportSummary {
            skipped_sessions: 1,
            skipped: 1,
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    let mut published_any = false;
    loop {
        if !observation.revalidate(session_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let parsed = parse_session_turn(session_path, &plan.cursor.frontier)?;
        validate_pending_replay(&plan.cursor.frontier, &parsed)?;
        if parsed.next_event_index > GENERATION_EVENT_STRIDE {
            return Err(CaptureError::InvalidPayload(
                "Junie session exceeds the provider-local generation event bound".to_owned(),
            ));
        }
        if parsed.incomplete {
            let retained = parsed.rejections.len() as u64;
            for rejection in parsed.rejections {
                summary.record_failure(rejection);
            }
            summary.failed = summary.failed.saturating_add(
                usize::try_from(parsed.rejection_count.saturating_sub(retained))
                    .unwrap_or(usize::MAX),
            );
            break;
        }
        if parsed.terminal
            && !parsed.after_state.saw_supported_event
            && session_path.require_supported_events
            && parsed.rejection_count == 0
        {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: session_path.events_path.clone(),
                reason: "Junie events.jsonl contained no supported session events",
            });
        }
        if parsed.start_offset == parsed.end_offset
            && parsed.rows.is_empty()
            && plan.cursor.terminal
            && plan.cursor.source_revision == observation.source_revision()
        {
            break;
        }
        let pending_start = plan
            .cursor
            .frontier
            .pending
            .as_ref()
            .map_or(0_usize, |pending| pending.next_row as usize);
        if pending_start > parsed.rows.len() {
            return Err(CaptureError::InvalidPayload(
                "Junie pending page frontier exceeds the reparsed turn".to_owned(),
            ));
        }
        let mut row_start = pending_start;
        let first_publication_for_turn = plan.cursor.frontier.pending.is_none();
        loop {
            let row_end = core_page_end(&parsed.rows, row_start)?;
            let mut next_cursor = plan.cursor.clone();
            next_cursor.source_revision = observation.source_revision();
            next_cursor.observed_length = observation.events_file.length;
            next_cursor.device = observation.events_file.device;
            next_cursor.inode = observation.events_file.inode;
            next_cursor.retired = false;
            if first_publication_for_turn && row_start == 0 {
                next_cursor.rejected_records = next_cursor
                    .rejected_records
                    .saturating_add(parsed.rejection_count);
            }
            if row_end < parsed.rows.len() {
                next_cursor.terminal = false;
                next_cursor.frontier.pending = Some(PendingTurn {
                    start_offset: parsed.start_offset,
                    end_offset: parsed.end_offset,
                    start_ordinal: parsed.start_ordinal,
                    end_ordinal: parsed.end_ordinal,
                    base_event_index: parsed.base_event_index,
                    next_event_index: parsed.next_event_index,
                    next_row: u32::try_from(row_end).map_err(|_| {
                        CaptureError::InvalidPayload("Junie turn row count exceeds u32".to_owned())
                    })?,
                    row_count: u32::try_from(parsed.rows.len()).map_err(|_| {
                        CaptureError::InvalidPayload("Junie turn row count exceeds u32".to_owned())
                    })?,
                    turn_sha256: parsed.turn_sha256,
                    terminal: parsed.terminal,
                    after_state: parsed.after_state.clone(),
                    after_prefix_sha256: parsed.after_prefix_sha256,
                });
            } else {
                next_cursor.frontier = Frontier {
                    offset: parsed.end_offset,
                    next_ordinal: parsed.end_ordinal,
                    next_event_index: parsed.next_event_index,
                    prefix_sha256: parsed.after_prefix_sha256,
                    state: parsed.after_state.clone(),
                    pending: None,
                };
                next_cursor.terminal =
                    parsed.terminal && parsed.end_offset == observation.events_file.length;
            }
            if !observation.revalidate(session_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let page = publish_core_page(
                store,
                committed_store,
                bulk,
                session_path,
                context,
                options,
                &provider_session_id,
                &source_identity,
                &stream,
                plan.expected.clone(),
                &plan.cursor,
                &next_cursor,
                &parsed.rows[row_start..row_end],
            )?;
            if page.work_result() == ProviderImportWorkResult::Changed {
                *changed_groups = changed_groups.saturating_add(1);
                published_any = true;
            }
            summary.merge_from(page);
            if first_publication_for_turn && row_start == 0 {
                let retained = parsed.rejections.len() as u64;
                for rejection in &parsed.rejections {
                    summary.record_failure(rejection.clone());
                }
                summary.failed = summary.failed.saturating_add(
                    usize::try_from(parsed.rejection_count.saturating_sub(retained))
                        .unwrap_or(usize::MAX),
                );
            }
            plan.cursor = next_cursor;
            plan.expected = store
                .get_sync_cursor(None, &context.machine_id, &stream)?
                .map(|cursor| cursor.cursor);
            row_start = row_end;
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && published_any {
                return Ok(summary);
            }
            if row_start >= parsed.rows.len() {
                break;
            }
        }
        if plan.cursor.frontier.pending.is_some() {
            continue;
        }
        if plan.cursor.terminal {
            break;
        }
    }
    if !published_any && summary.failed == 0 {
        summary.skipped_sessions = 1;
        summary.skipped = summary.skipped.saturating_add(1);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

pub(super) fn validate_pending_replay(frontier: &Frontier, parsed: &ParsedTurn) -> Result<()> {
    let Some(pending) = &frontier.pending else {
        return Ok(());
    };
    if pending.start_offset != parsed.start_offset
        || pending.end_offset != parsed.end_offset
        || pending.start_ordinal != parsed.start_ordinal
        || pending.end_ordinal != parsed.end_ordinal
        || pending.base_event_index != parsed.base_event_index
        || pending.next_event_index != parsed.next_event_index
        || pending.row_count as usize != parsed.rows.len()
        || pending.turn_sha256 != parsed.turn_sha256
        || pending.terminal != parsed.terminal
        || pending.after_state != parsed.after_state
        || pending.after_prefix_sha256 != parsed.after_prefix_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn core_page_end(rows: &[EventDraft], start: usize) -> Result<usize> {
    if start >= rows.len() {
        return Ok(start);
    }
    let mut bytes = 0_usize;
    let mut end = start;
    while end < rows.len() && end - start < CORE_PAGE_MAX_ROWS {
        let next = serde_json::to_vec(&rows[end].body)?
            .len()
            .saturating_add(serde_json::to_vec(&rows[end].metadata)?.len());
        if end != start && bytes.saturating_add(next) > CORE_PAGE_MAX_BYTES {
            break;
        }
        if next > CORE_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie normalized Core row exceeds the bounded NativePath page".to_owned(),
            ));
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    Ok(end)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk: &EventSearchBulkGuard,
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_identity: &str,
    stream: &str,
    expected_cursor: Option<String>,
    prior: &JunieStoreCursor,
    next: &JunieStoreCursor,
    rows: &[EventDraft],
) -> Result<ProviderImportSummary> {
    let encoded = next.encode()?;
    let next_sync = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.to_owned(),
        cursor: encoded,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(expected_cursor, next_sync);
    let publication_id = publication_id(source_identity, next, rows, &transition);
    let retained_bytes = rows.iter().try_fold(next.encode()?.len(), |bytes, row| {
        let row_bytes = serde_json::to_vec(&row.body)?
            .len()
            .saturating_add(serde_json::to_vec(&row.metadata)?.len())
            .saturating_add(row.text.len());
        Ok::<_, CaptureError>(bytes.saturating_add(row_bytes))
    })?;
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk)?;
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
    let resolved = resolve_source(
        committed_store,
        &mut group,
        session_path,
        context,
        options,
        provider_session_id,
        next,
        &mut summary,
    )?;
    for row in rows {
        publish_event(
            committed_store,
            &mut group,
            context,
            options,
            provider_session_id,
            &resolved,
            next.generation,
            row,
            &mut summary,
        )?;
    }
    let observation = JunieSessionObservation::read(session_path)?;
    if observation.source_revision() != next.source_revision
        || observation.events_file.length != next.observed_length
        || observation.events_file.device != next.device
        || observation.events_file.inode != next.inode
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    if prior.generation != next.generation && rows.is_empty() {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
    }
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}
