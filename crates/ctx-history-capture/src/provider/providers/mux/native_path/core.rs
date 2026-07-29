use super::*;

pub(super) fn import_core_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    plan: &MuxSourcePlan,
) -> Result<ProviderImportSummary> {
    if plan
        .prior
        .as_ref()
        .map(|prior| &prior.wire)
        .is_some_and(|wire| {
            wire.terminal
                && !wire.retired
                && wire.source_revision == plan.source_revision
                && wire.metadata_revision == plan.metadata_revision
        })
    {
        if !plan
            .observation
            .revalidate(&plan.path, plan.source.metadata_path.as_deref())?
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let wire =
            plan.prior
                .as_ref()
                .map(|prior| &prior.wire)
                .ok_or(CaptureError::SystemInvariant(
                    "Mux replay lost its committed cursor",
                ))?;
        return Ok(replay_summary(wire, plan.counts_session_projection()));
    }

    let mut session =
        mux_bounded_session_metadata(&plan.source, &plan.metadata_revision, context.imported_at)?;
    let mut summary = ProviderImportSummary::default();
    let (mut reader, mut hasher) = open_reader_at_frontier(&plan.path, &plan.initial_frontier)?;
    let mut frontier = plan.initial_frontier.clone();
    let mut expected_store_cursor = plan.prior.as_ref().map(|prior| prior.stored.clone());
    let mut accepted_events = plan.accepted_events;
    let mut rejected_records = plan.rejected_records;
    let mut first_failure = plan.first_failure.clone();
    let mut emitted_page = false;

    loop {
        let page = read_core_page(
            &mut reader,
            &mut hasher,
            &mut session,
            plan,
            frontier.clone(),
            rejected_records,
            first_failure.clone(),
            context,
        )?;
        let Some(page) = page else {
            break;
        };
        if page.deferred_incomplete
            && plan
                .prior
                .as_ref()
                .map(|prior| &prior.wire)
                .is_some_and(|wire| {
                    !wire.terminal
                        && !wire.retired
                        && wire.source_revision == plan.source_revision
                        && wire.metadata_revision == plan.metadata_revision
                        && wire.generation == plan.generation
                        && wire.frontier == page.next
                })
        {
            summary.skipped = summary.skipped.saturating_add(1);
            summary.work_remaining = true;
            return Ok(summary);
        }
        emitted_page = true;
        rejected_records = page.rejected_records;
        first_failure.clone_from(&page.first_failure);
        let page_events = page.rows.iter().filter(|row| row.event.is_some()).count();
        accepted_events = accepted_events.saturating_add(
            u64::try_from(page_events)
                .map_err(|_| CaptureError::SystemInvariant("Mux event count exceeds u64"))?,
        );
        let page_summary = publish_core_page(
            store,
            bulk_guard,
            configured_root,
            context,
            options,
            plan,
            &session,
            &page,
            accepted_events,
            expected_store_cursor.as_ref(),
        )?;
        summary.merge_from(page_summary);
        frontier = page.next;
        expected_store_cursor =
            store.get_sync_cursor(None, &context.machine_id, &plan.cursor_stream)?;
        if page.terminal {
            break;
        }
        if page.deferred_incomplete {
            summary.work_remaining = true;
            break;
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = true;
            break;
        }
    }

    if !emitted_page {
        return Err(CaptureError::SystemInvariant(
            "Mux changed source emitted no terminal authority page",
        ));
    }
    if summary.failed == 0 && rejected_records > plan.rejected_records {
        summary.failed =
            usize::try_from(rejected_records - plan.rejected_records).unwrap_or(usize::MAX);
    }
    if let Some(failure) = first_failure {
        if summary.failures.is_empty() {
            summary.failures.push(ProviderImportFailure {
                line: failure.line,
                error: failure.error,
            });
        }
    }
    Ok(summary)
}

pub(super) fn replay_summary(wire: &MuxCursorWire, counts_session: bool) -> ProviderImportSummary {
    let skipped_events = usize::try_from(wire.accepted_events).unwrap_or(usize::MAX);
    let failed = usize::try_from(wire.rejected_records).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: skipped_events.saturating_add(usize::from(counts_session)),
        failed,
        skipped_sessions: usize::from(counts_session),
        skipped_events,
        accepted_content_records: skipped_events,
        failures: wire
            .first_failure
            .iter()
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            })
            .collect(),
        ..ProviderImportSummary::default()
    }
}
