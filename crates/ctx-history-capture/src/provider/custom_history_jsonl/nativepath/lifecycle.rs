use super::*;

pub(super) fn import_parsed(
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    mut parsed: ParsedCustomHistory,
    stamp: Option<&CustomFileStamp>,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let mut summary = std::mem::take(&mut parsed.summary);
    let canonical = build_canonical_history(
        &committed_store,
        context,
        options,
        logical_locator,
        &parsed,
        &mut summary,
    )?;
    parsed.summary = summary;
    let outputs = custom_outputs(&parsed, &canonical.sessions)?;
    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            context,
            options,
            logical_locator,
            stream,
            &parsed,
            &outputs,
        );
        return Ok(parsed.summary);
    }
    if canonical.units.is_empty() && parsed.summary.failed != 0 {
        parsed
            .summary
            .set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(parsed.summary);
    }

    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    let current_anchor = canonical
        .anchor_source
        .as_ref()
        .map(|source| CustomAnchorAuthority {
            capture_source_id: source.id,
            canonical_source_identity: canonical_route_identity(logical_locator),
        });
    let (mut cursor, mut expected_cursor) = initial_cursor(
        stored.as_ref(),
        logical_locator,
        &parsed.source_revision,
        current_anchor,
    )?;
    if cursor.retired {
        return Err(CaptureError::SystemInvariant(
            "custom history reactivation retained a retired cursor",
        ));
    }
    if cursor.phase == CustomCursorPhase::Complete {
        parsed.summary.work_remaining = publish_upstream_cursors(
            store,
            context,
            options,
            &parsed.sources,
            &parsed.events,
            stamp,
            &mut parsed.summary,
        )?;
        if parsed.summary.work_result() != ProviderImportWorkResult::Changed {
            parsed
                .summary
                .set_work_result(ProviderImportWorkResult::NoOp);
        }
        replay_outputs_or_mark_behind(
            store,
            context,
            options,
            logical_locator,
            stream,
            &parsed,
            &outputs,
        );
        return Ok(parsed.summary);
    }
    if matches!(cursor.phase, CustomCursorPhase::Blocked { .. }) {
        parsed
            .summary
            .set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(parsed.summary);
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut changed_groups = 0_usize;
        loop {
            match cursor.phase.clone() {
                CustomCursorPhase::Publish { next_unit } => {
                    let next_unit = usize::try_from(next_unit).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "custom history NativePath unit frontier exceeds platform limits"
                                .to_owned(),
                        )
                    })?;
                    let page_end = next_unit
                        .saturating_add(CUSTOM_CORE_UNITS_PER_PAGE)
                        .min(canonical.units.len());
                    let page = &canonical.units[next_unit..page_end];
                    let needs_anchor_only_page =
                        canonical.units.is_empty() && next_unit == 0 && cursor.anchor.is_some();
                    let next_phase = if page_end < canonical.units.len() {
                        CustomCursorPhase::Publish {
                            next_unit: u64::try_from(page_end).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    "custom history NativePath unit frontier exceeds u64"
                                        .to_owned(),
                                )
                            })?,
                        }
                    } else if parsed.summary.failed != 0 {
                        CustomCursorPhase::Blocked {
                            next_unit: u64::try_from(page_end).unwrap_or(u64::MAX),
                        }
                    } else if cursor.anchor.is_some() {
                        CustomCursorPhase::Retire { after: None }
                    } else {
                        CustomCursorPhase::Complete
                    };
                    let mut next_cursor = cursor.clone();
                    next_cursor.phase = next_phase;
                    let changed = publish_core_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        context,
                        options,
                        logical_locator,
                        stream,
                        &parsed.source_revision,
                        &canonical,
                        page,
                        next_unit,
                        &cursor,
                        &next_cursor,
                        expected_cursor.clone(),
                        stamp,
                        needs_anchor_only_page,
                        &mut parsed.summary,
                    )?;
                    if changed {
                        changed_groups = changed_groups.saturating_add(1);
                    }
                    cursor = next_cursor;
                }
                CustomCursorPhase::Retire { after } => {
                    let (next_cursor, changed) = publish_retirement_page(
                        store,
                        &bulk_guard,
                        context,
                        logical_locator,
                        stream,
                        &cursor,
                        after.as_ref(),
                        expected_cursor.clone(),
                        stamp,
                    )?;
                    if changed {
                        changed_groups = changed_groups.saturating_add(1);
                        parsed
                            .summary
                            .set_work_result(ProviderImportWorkResult::Changed);
                    }
                    cursor = next_cursor;
                }
                CustomCursorPhase::Blocked { .. } | CustomCursorPhase::Complete => break,
            }
            expected_cursor = store
                .get_sync_cursor(None, &context.machine_id, stream)?
                .map(|stored| stored.cursor);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0 {
                parsed.summary.work_remaining =
                    !matches!(cursor.phase, CustomCursorPhase::Complete);
                break;
            }
            if matches!(
                cursor.phase,
                CustomCursorPhase::Blocked { .. } | CustomCursorPhase::Complete
            ) {
                break;
            }
        }
        Ok(())
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(()), Ok(())) => {}
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    }
    drop(bulk_guard);

    if cursor.phase == CustomCursorPhase::Complete {
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
            && parsed.summary.work_result() == ProviderImportWorkResult::Changed
        {
            parsed.summary.work_remaining =
                upstream_cursors_pending(store, context, &parsed.sources, &parsed.events)?;
        } else {
            parsed.summary.work_remaining = publish_upstream_cursors(
                store,
                context,
                options,
                &parsed.sources,
                &parsed.events,
                stamp,
                &mut parsed.summary,
            )?;
        }
        replay_outputs_or_mark_behind(
            store,
            context,
            options,
            logical_locator,
            stream,
            &parsed,
            &outputs,
        );
    }
    Ok(parsed.summary)
}

pub(super) fn retire_missing_source(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: context.source_path.clone().unwrap_or_default(),
            reason: "custom history JSONL source does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = decode_cursor(committed.provider_cursor())?;
    validate_cursor(&prior, logical_locator)?;
    if prior.retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let mut next = prior.clone();
    next.retired = true;
    next.phase = CustomCursorPhase::Complete;
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(&next)?,
            context.imported_at,
        ),
    );
    let retirement = prior
        .anchor
        .as_ref()
        .map(|anchor| ProviderSourceRouteRetirement {
            provider: CaptureProvider::Custom,
            source_format: CUSTOM_ROUTE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: logical_locator.to_owned(),
            cursor_stream: stream.to_owned(),
            expected_canonical_source_identity: anchor.canonical_source_identity.clone(),
            expected_source_revision: prior.source_revision.clone(),
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason,
        });
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(1, 1, PAGE_ACCOUNTING_OVERHEAD_BYTES)?,
        )?;
        let publication_id = missing_publication_id(logical_locator, &transition);
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                group.commit()?;
                return Ok(false);
            }
            NativePathCursorSetClassification::AllExpected => {}
        }
        if let Some(retirement) = &retirement {
            let disposition = group.retire_provider_source_route(retirement)?;
            if disposition != ProviderSourceRouteRetirementDisposition::Retired {
                return Err(CaptureError::InvalidPayload(
                    "custom history source route was already retired before publication".to_owned(),
                ));
            }
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        group.commit()?;
        Ok(true)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let changed = match (operation, finish) {
        (Ok(changed), Ok(())) => changed,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    let mut summary = ProviderImportSummary::default();
    if changed {
        summary.skipped = 1;
        summary.skipped_sessions = 1;
        summary.set_work_result(ProviderImportWorkResult::Changed);
    } else {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    let _ = options;
    Ok(summary)
}
