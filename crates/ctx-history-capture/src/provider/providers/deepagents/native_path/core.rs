use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_core(
    store: &mut Store,
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &mut DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    mut cursor: DeepAgentsNativeCursor,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        loop {
            if !snapshot.revalidate(&authority.database_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let next = match cursor.phase.clone() {
                DeepAgentsCorePhase::Threads { after_rowid } => {
                    let page = with_sqlite_read_snapshot(conn, || {
                        build_thread_page(conn, context, after_rowid)
                    })?;
                    publish_thread_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        snapshot,
                        authority,
                        context,
                        options,
                        &cursor,
                        page,
                        &mut summary,
                    )?
                }
                DeepAgentsCorePhase::Writes {
                    after_rowid,
                    active_rowid,
                    next_message_offset,
                    current_thread_id,
                    next_event_index,
                } => {
                    let page = with_sqlite_read_snapshot(conn, || {
                        build_write_page(
                            conn,
                            context,
                            after_rowid,
                            active_rowid,
                            next_message_offset,
                            current_thread_id,
                            next_event_index,
                        )
                    })?;
                    publish_write_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        snapshot,
                        authority,
                        context,
                        options,
                        &cursor,
                        page,
                        &mut summary,
                    )?
                }
                DeepAgentsCorePhase::StageSources { next_source } => publish_source_stage_page(
                    store,
                    &bulk_guard,
                    Some(snapshot),
                    authority,
                    context,
                    &cursor,
                    next_source,
                    false,
                )?,
                DeepAgentsCorePhase::Retire { after } => publish_retirement_page(
                    store,
                    &bulk_guard,
                    Some(snapshot),
                    authority,
                    context,
                    &cursor,
                    after,
                    false,
                )?,
                DeepAgentsCorePhase::Complete => break,
                DeepAgentsCorePhase::MissingStage { .. }
                | DeepAgentsCorePhase::MissingRetire { .. }
                | DeepAgentsCorePhase::MissingComplete => {
                    return Err(CaptureError::InvalidPayload(
                        "Deep Agents live source resumed a disappearance cursor".to_owned(),
                    ));
                }
            };
            authority.canonical_source_identity = next.canonical_source_identity.clone();
            cursor = next;
            summary.set_work_result(ProviderImportWorkResult::Changed);
            if cursor.is_complete() {
                break;
            }
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = true;
                break;
            }
        }
        if cursor.is_complete() {
            apply_terminal_cursor_summary(&mut summary, &cursor);
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

pub(super) fn build_thread_page(
    conn: &Connection,
    context: &ProviderAdapterContext,
    after_rowid: Option<i64>,
) -> Result<DeepAgentsThreadPage> {
    let mut entries = Vec::new();
    let mut after = after_rowid;
    let mut retained_bytes = DEEPAGENTS_PAGE_OVERHEAD_BYTES;
    let mut terminal = false;
    while entries.len() < DEEPAGENTS_PAGE_UNITS {
        let Some(candidate) = deepagents_next_thread_candidate(conn, after)? else {
            terminal = true;
            break;
        };
        after = Some(candidate.rowid);
        let summary = candidate
            .thread_id
            .as_deref()
            .map(|thread_id| deepagents_thread_summary(conn, context, thread_id, None))
            .transpose()?
            .flatten();
        let rejection = candidate.rejection_reason.or_else(|| {
            summary
                .is_none()
                .then(|| "Deep Agents thread has no valid bounded checkpoint metadata".to_owned())
        });
        retained_bytes = retained_bytes.saturating_add(
            summary
                .as_ref()
                .map(|summary| {
                    summary
                        .thread
                        .thread_id
                        .len()
                        .saturating_add(summary.thread.agent_name.as_ref().map_or(0, String::len))
                        .saturating_add(summary.thread.cwd.as_ref().map_or(0, String::len))
                })
                .unwrap_or_default(),
        );
        entries.push(DeepAgentsThreadEntry {
            rowid: candidate.rowid,
            summary,
            rejection,
        });
    }
    ensure_retained_bound(retained_bytes)?;
    Ok(DeepAgentsThreadPage {
        entries,
        next_after_rowid: after,
        terminal,
        retained_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_write_page(
    conn: &Connection,
    context: &ProviderAdapterContext,
    after_rowid: Option<i64>,
    active_rowid: Option<i64>,
    next_message_offset: u32,
    current_thread_id: Option<String>,
    next_event_index: u64,
) -> Result<DeepAgentsWritePage> {
    let candidate = match active_rowid {
        Some(rowid) => deepagents_write_candidate_at(conn, rowid)?
            .ok_or(CaptureError::SourceChangedDuringCapture)?,
        None => match deepagents_next_write_candidate(conn, after_rowid)? {
            Some(candidate) => candidate,
            None => {
                return Ok(DeepAgentsWritePage {
                    key: None,
                    rowid: None,
                    messages: Vec::new(),
                    value_type: None,
                    value: Vec::new(),
                    occurred_at: None,
                    rejection: None,
                    message_rejection_count: 0,
                    message_rejections: Vec::new(),
                    next_phase: DeepAgentsCorePhase::StageSources { next_source: 0 },
                    retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
                });
            }
        },
    };
    let rowid = candidate.rowid;
    let Some(key) = candidate.key.clone() else {
        let rejection = candidate.rejection_reason.clone().unwrap_or_else(|| {
            format!(
                "Deep Agents write exceeds the bounded record limit ({} bytes)",
                candidate.observed_bytes().unwrap_or(u64::MAX)
            )
        });
        return Ok(rejected_write_page(
            candidate,
            current_thread_id,
            next_event_index,
            rejection,
        ));
    };
    let occurred_at =
        deepagents_checkpoint_time(conn, context, &key.thread_id, &key.checkpoint_id)?;
    let Some(occurred_at) = occurred_at else {
        return Ok(rejected_write_page(
            candidate,
            current_thread_id,
            next_event_index,
            format!(
                "Deep Agents writes row references unknown thread_id {}",
                key.thread_id
            ),
        ));
    };
    let (value_type, value) = deepagents_hydrate_write(conn, rowid)?;
    let decoded = match deepagents_messages_from_blob(value_type.as_deref(), &value) {
        Ok(messages) => messages,
        Err(error) => {
            return Ok(rejected_write_page(
                candidate,
                current_thread_id,
                next_event_index,
                error.to_string(),
            ));
        }
    };
    let start = usize::try_from(next_message_offset).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents write message frontier exceeds platform limits".to_owned(),
        )
    })?;
    if start > decoded.messages.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let end = start
        .saturating_add(DEEPAGENTS_PAGE_UNITS)
        .min(decoded.messages.len());
    let mut index =
        if active_rowid.is_some() || current_thread_id.as_deref() == Some(key.thread_id.as_str()) {
            next_event_index
        } else {
            1
        };
    let mut messages = Vec::with_capacity(end.saturating_sub(start));
    for (offset, message) in decoded.messages[start..end].iter().cloned().enumerate() {
        messages.push(DeepAgentsParsedMessage {
            offset: start.saturating_add(offset),
            provider_event_index: index,
            message,
        });
        index = index.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Deep Agents event index overflowed",
        ))?;
    }
    let row_complete = end == decoded.messages.len();
    let (message_rejection_count, message_rejections) = if start == 0 {
        (decoded.rejected_entries, decoded.rejections)
    } else {
        (0, Vec::new())
    };
    let next_phase = if row_complete {
        DeepAgentsCorePhase::Writes {
            after_rowid: Some(rowid),
            active_rowid: None,
            next_message_offset: 0,
            current_thread_id: Some(key.thread_id.clone()),
            next_event_index: index,
        }
    } else {
        DeepAgentsCorePhase::Writes {
            after_rowid,
            active_rowid: Some(rowid),
            next_message_offset: u32::try_from(end).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Deep Agents write contains too many decoded messages".to_owned(),
                )
            })?,
            current_thread_id: Some(key.thread_id.clone()),
            next_event_index: index,
        }
    };
    let retained_bytes = DEEPAGENTS_PAGE_OVERHEAD_BYTES.saturating_add(value.len());
    ensure_retained_bound(retained_bytes)?;
    Ok(DeepAgentsWritePage {
        key: Some(key),
        rowid: Some(rowid),
        messages,
        value_type,
        value,
        occurred_at: Some(occurred_at),
        rejection: None,
        message_rejection_count,
        message_rejections,
        next_phase,
        retained_bytes,
    })
}
