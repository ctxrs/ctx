use super::{lifecycle::*, publication::*, *};

#[allow(clippy::too_many_arguments)]
pub(super) fn import_core(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    central: &rusqlite::Connection,
    snapshot: &NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<CoreOutcome> {
    let stored = committed_store.get_sync_cursor(None, &context.machine_id, &live.cursor_stream)?;
    let prior = decode_prior_cursor(stored, live.anchor_source_id)?;
    if let PriorCursor::Native {
        cursor, retired, ..
    } = &prior
    {
        if !retired
            && cursor.source_revision == live.source_revision
            && cursor.terminal
            && cursor.source_stage.is_none()
            && cursor.retirement.is_none()
        {
            if !snapshot.revalidate()? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(CoreOutcome {
                summary,
                terminal_cursor: Some(cursor.clone()),
            });
        }
    }

    let (mut scanner, mut cursor, mut expected_store_cursor) =
        resume_scanner(central, snapshot, live, prior)?;
    let mut summary = ProviderImportSummary::default();
    let mut committed_groups = 0usize;
    loop {
        let next_cursor = if cursor.source_stage.is_some() {
            summary.set_work_result(ProviderImportWorkResult::Changed);
            publish_source_stage_page(
                store,
                committed_store,
                bulk_guard,
                snapshot,
                live,
                context,
                &cursor,
                expected_store_cursor.as_ref(),
            )?
        } else if cursor.retirement.is_some() {
            summary.set_work_result(ProviderImportWorkResult::Changed);
            publish_omission_retirement_page(
                store,
                committed_store,
                bulk_guard,
                snapshot,
                live,
                context,
                &cursor,
                expected_store_cursor.as_ref(),
            )?
        } else {
            let page = scanner.next_page()?;
            let next_cursor = cursor_after_page(&cursor, &page, &live.source_revision)?;
            let page_summary = publish_page(
                store,
                committed_store,
                bulk_guard,
                snapshot,
                live,
                context,
                options,
                &page,
                &next_cursor,
                expected_store_cursor.as_ref(),
            )?;
            summary.merge_from(page_summary);
            next_cursor
        };
        expected_store_cursor =
            store.get_sync_cursor(None, &context.machine_id, &live.cursor_stream)?;
        if expected_store_cursor.is_none() {
            return Err(CaptureError::SystemInvariant(
                "NanoClaw NativePath commit did not publish its cursor",
            ));
        }
        cursor = next_cursor;
        committed_groups = committed_groups.saturating_add(1);
        if cursor.terminal {
            summary.work_remaining = false;
            return Ok(CoreOutcome {
                summary,
                terminal_cursor: Some(cursor),
            });
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && committed_groups == 1 {
            summary.work_remaining = true;
            return Ok(CoreOutcome {
                summary,
                terminal_cursor: None,
            });
        }
    }
}

pub(super) fn resume_scanner<'connection, 'snapshot>(
    central: &'connection rusqlite::Connection,
    snapshot: &'snapshot NanoClawProjectSnapshot,
    live: &NanoClawLiveProject,
    prior: PriorCursor,
) -> Result<(
    NanoClawNativeScanner<'connection, 'snapshot>,
    NanoClawNativeCursor,
    Option<SyncCursor>,
)> {
    let mut scanner = NanoClawNativeScanner::new(central, snapshot)?;
    match prior {
        PriorCursor::None => Ok((
            scanner,
            NanoClawNativeCursor::initial(
                live.anchor_source_id,
                live.source_revision.clone(),
                0,
                true,
            ),
            None,
        )),
        PriorCursor::Legacy(stored) => Ok((
            scanner,
            NanoClawNativeCursor::initial(
                live.anchor_source_id,
                live.source_revision.clone(),
                1,
                true,
            ),
            Some(stored),
        )),
        PriorCursor::Native {
            stored,
            cursor,
            retired: _,
        } => {
            if cursor.source_stage.is_some() || cursor.retirement.is_some() {
                if cursor.source_revision == live.source_revision {
                    return Ok((scanner, cursor, Some(stored)));
                }
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "NanoClaw NativePath generation exhausted",
                        ))?;
                return Ok((
                    scanner,
                    NanoClawNativeCursor::initial(
                        live.anchor_source_id,
                        live.source_revision.clone(),
                        generation,
                        true,
                    ),
                    Some(stored),
                ));
            }
            let prefix_matches = scanner.seek(cursor.frontier, &cursor.prefix_digest)?;
            if !prefix_matches {
                if cursor.source_revision == live.source_revision {
                    return Err(CaptureError::InvalidPayload(
                        "NanoClaw NativePath cursor does not prove the current source prefix"
                            .to_owned(),
                    ));
                }
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "NanoClaw NativePath generation exhausted",
                        ))?;
                return Ok((
                    NanoClawNativeScanner::new(central, snapshot)?,
                    NanoClawNativeCursor::initial(
                        live.anchor_source_id,
                        live.source_revision.clone(),
                        generation,
                        true,
                    ),
                    Some(stored),
                ));
            }
            let mut next = cursor;
            if next.source_revision != live.source_revision {
                next.generation =
                    next.generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "NanoClaw NativePath generation exhausted",
                        ))?;
                next.source_revision = live.source_revision.clone();
                next.terminal = false;
                // A prefix-proven append resumes after the prior terminal
                // frontier. It must preserve older historical entities rather
                // than treating them as omissions from an incremental page.
                // An in-progress cold/full scan, however, continues staging.
            }
            Ok((scanner, next, Some(stored)))
        }
    }
}

pub(super) fn cursor_after_page(
    prior: &NanoClawNativeCursor,
    page: &NanoClawNativePage,
    source_revision: &str,
) -> Result<NanoClawNativeCursor> {
    if page.expected_frontier != prior.frontier {
        return Err(CaptureError::SystemInvariant(
            "NanoClaw scanner page does not begin at the committed frontier",
        ));
    }
    let mut next = prior.clone();
    next.source_revision = source_revision.to_owned();
    next.frontier = page.next_frontier;
    next.prefix_digest = page.prefix_digest.clone();
    if page.terminal && next.stage_generation {
        next.terminal = false;
        next.source_stage = Some(NanoClawSourceStage { after: None });
    } else {
        next.terminal = page.terminal;
    }
    for unit in &page.units {
        match unit {
            NanoClawNativeUnit::Session { .. } => {
                next.retained_sessions = next.retained_sessions.saturating_add(1)
            }
            NanoClawNativeUnit::Message { .. } => {
                next.retained_events = next.retained_events.saturating_add(1)
            }
            NanoClawNativeUnit::Rejection { .. } => {
                next.rejected_records = next.rejected_records.saturating_add(1)
            }
        }
    }
    Ok(next)
}
