use super::*;

pub(super) fn rejected_write_page(
    candidate: DeepAgentsWriteCandidate,
    current_thread_id: Option<String>,
    next_event_index: u64,
    rejection: String,
) -> DeepAgentsWritePage {
    DeepAgentsWritePage {
        key: candidate.key,
        rowid: Some(candidate.rowid),
        messages: Vec::new(),
        value_type: None,
        value: Vec::new(),
        occurred_at: None,
        rejection: Some(rejection),
        message_rejection_count: 0,
        message_rejections: Vec::new(),
        next_phase: DeepAgentsCorePhase::Writes {
            after_rowid: Some(candidate.rowid),
            active_rowid: None,
            next_message_offset: 0,
            current_thread_id,
            next_event_index,
        },
        retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_thread_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    cursor: &DeepAgentsNativeCursor,
    page: DeepAgentsThreadPage,
    summary: &mut ProviderImportSummary,
) -> Result<DeepAgentsNativeCursor> {
    let mut next = cursor.clone();
    next.phase = if page.terminal {
        DeepAgentsCorePhase::Writes {
            after_rowid: None,
            active_rowid: None,
            next_message_offset: 0,
            current_thread_id: None,
            next_event_index: 1,
        }
    } else {
        DeepAgentsCorePhase::Threads {
            after_rowid: page.next_after_rowid,
        }
    };
    next.accepted_sessions = next.accepted_sessions.saturating_add(
        u64::try_from(
            page.entries
                .iter()
                .filter(|entry| entry.summary.is_some())
                .count(),
        )
        .unwrap_or(u64::MAX),
    );
    let rejection_details = page
        .entries
        .iter()
        .filter_map(|entry| {
            entry.rejection.as_ref().map(|error| ProviderImportFailure {
                line: usize::try_from(entry.rowid).unwrap_or(usize::MAX),
                error: error.clone(),
            })
        })
        .collect::<Vec<_>>();
    record_cursor_rejections(
        &mut next,
        u64::try_from(rejection_details.len()).unwrap_or(u64::MAX),
        &rejection_details,
    );
    next.generation_staged |= page.entries.iter().any(|entry| entry.summary.is_some());
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, page.retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution = reconcile_locator(&mut group, authority, context)?;
    let mut retained = NativePathRetainedSourceEntities::default();
    for entry in &page.entries {
        if entry.rejection.is_some() {
            continue;
        }
        let thread = &entry
            .summary
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "accepted Deep Agents thread has no summary",
            ))?
            .thread;
        let raw_source_path = authority.canonical_database_path.display().to_string();
        let source_id = resolve_source_id(
            committed_store,
            thread,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &raw_source_path,
        )?;
        let source = capture_source(
            source_id,
            thread,
            authority,
            context,
            &raw_source_path,
            &resolution.canonical_source_identity,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = canonical_session(
            committed_store,
            source_id,
            thread,
            context,
            options,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        retained.capture_source_ids.push(source_id);
        retained.session_ids.push(session.id);
    }
    record_summary_rejections(
        summary,
        u64::try_from(rejection_details.len()).unwrap_or(u64::MAX),
        &rejection_details,
    );
    if !retained.capture_source_ids.is_empty() {
        let key = generation_key(
            authority,
            context,
            &resolution.canonical_source_identity,
            cursor.generation,
        );
        group.stage_source_generation_page(&key, &retained)?;
        next.generation_staged = true;
    }
    if !snapshot.revalidate(&authority.database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    next.canonical_source_identity = resolution.canonical_source_identity;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_write_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    cursor: &DeepAgentsNativeCursor,
    page: DeepAgentsWritePage,
    summary: &mut ProviderImportSummary,
) -> Result<DeepAgentsNativeCursor> {
    let publication = page
        .key
        .as_ref()
        .map(|key| committed_source_and_session(committed_store, key, authority, context))
        .transpose()?
        .flatten();
    let row_line = page
        .rowid
        .and_then(|rowid| usize::try_from(rowid).ok())
        .unwrap_or(usize::MAX);
    let mut rejection_count = page.message_rejection_count;
    let mut rejection_details = page
        .message_rejections
        .iter()
        .map(|failure| ProviderImportFailure {
            line: row_line,
            error: format!(
                "Deep Agents message entry {} rejected: {}",
                failure.entry_offset, failure.error
            ),
        })
        .collect::<Vec<_>>();
    if let Some(error) = page.rejection.as_ref() {
        rejection_count = rejection_count.saturating_add(1);
        rejection_details.push(ProviderImportFailure {
            line: row_line,
            error: error.clone(),
        });
    } else if let Some(key) = page.key.as_ref().filter(|_| publication.is_none()) {
        rejection_count = rejection_count.saturating_add(1);
        rejection_details.push(ProviderImportFailure {
            line: row_line,
            error: format!(
                "Deep Agents write references uncommitted thread {}",
                key.thread_id
            ),
        });
    }
    let publication_eligible = page.rejection.is_none() && publication.is_some();
    let core_event_count = if publication_eligible {
        page.messages
            .iter()
            .filter(|message| core_eligible(&message.message))
            .count()
    } else {
        0
    };
    let mut next = cursor.clone();
    next.phase = page.next_phase.clone();
    next.accepted_events = next
        .accepted_events
        .saturating_add(u64::try_from(core_event_count).unwrap_or(u64::MAX));
    record_cursor_rejections(&mut next, rejection_count, &rejection_details);
    next.generation_staged |= publication.is_some();
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, page.retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution = reconcile_locator(&mut group, authority, context)?;
    record_summary_rejections(summary, rejection_count, &rejection_details);
    let mut retained = NativePathRetainedSourceEntities::default();
    if let (Some(key), Some((source, session))) = (page.key.as_ref(), publication.as_ref()) {
        group.bind_capture_source_provider_route(source.id, &resolution.route_binding())?;
        retained.capture_source_ids.push(source.id);
        retained.session_ids.push(session.id);
        if page.rejection.is_none() {
            publish_core_messages(
                committed_store,
                &mut group,
                source,
                session,
                key,
                &page,
                context,
                options,
                summary,
                &mut retained,
            )?;
        }
    }
    if !retained.capture_source_ids.is_empty() {
        let key = generation_key(
            authority,
            context,
            &resolution.canonical_source_identity,
            cursor.generation,
        );
        group.stage_source_generation_page(&key, &retained)?;
        next.generation_staged = true;
    }
    if !snapshot.revalidate(&authority.database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    next.canonical_source_identity = resolution.canonical_source_identity;
    Ok(next)
}

pub(super) fn record_cursor_rejections(
    cursor: &mut DeepAgentsNativeCursor,
    rejected_records: u64,
    details: &[ProviderImportFailure],
) {
    cursor.rejected_records = cursor.rejected_records.saturating_add(rejected_records);
    let remaining =
        crate::summaries::MAX_RETAINED_PROVIDER_FAILURES.saturating_sub(cursor.rejections.len());
    cursor
        .rejections
        .extend(details.iter().take(remaining).cloned());
}

pub(super) fn record_summary_rejections(
    summary: &mut ProviderImportSummary,
    rejected_records: u64,
    details: &[ProviderImportFailure],
) {
    summary.failed = summary
        .failed
        .saturating_add(usize::try_from(rejected_records).unwrap_or(usize::MAX));
    let remaining =
        crate::summaries::MAX_RETAINED_PROVIDER_FAILURES.saturating_sub(summary.failures.len());
    summary
        .failures
        .extend(details.iter().take(remaining).cloned());
}

pub(super) fn apply_terminal_cursor_summary(
    summary: &mut ProviderImportSummary,
    cursor: &DeepAgentsNativeCursor,
) {
    summary.accepted_content_records = summary
        .accepted_content_records
        .max(usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX));
    summary.failed = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
    summary.failures = cursor.rejections.clone();
    summary.set_terminal_outcome(ProviderImportTerminalOutcome::CoreCursorCommitted);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_source_stage_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: Option<&ProviderSqliteSourceSnapshot>,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    cursor: &DeepAgentsNativeCursor,
    next_source: usize,
    missing: bool,
) -> Result<DeepAgentsNativeCursor> {
    let sources = known_capture_sources(store, authority, context)?;
    if next_source > sources.len() {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents source-stage cursor exceeds the current source set".to_owned(),
        ));
    }
    let end = next_source
        .saturating_add(DEEPAGENTS_PAGE_UNITS)
        .min(sources.len());
    let page = &sources[next_source..end];
    let terminal = end == sources.len();
    let mut next = cursor.clone();
    next.generation_staged |= !page.is_empty();
    next.phase = if terminal {
        if next.generation_staged {
            if missing {
                DeepAgentsCorePhase::MissingRetire { after: None }
            } else {
                DeepAgentsCorePhase::Retire { after: None }
            }
        } else if missing {
            DeepAgentsCorePhase::MissingComplete
        } else {
            DeepAgentsCorePhase::Complete
        }
    } else if missing {
        DeepAgentsCorePhase::MissingStage { next_source: end }
    } else {
        DeepAgentsCorePhase::StageSources { next_source: end }
    };
    let retained_bytes =
        page.iter()
            .try_fold(DEEPAGENTS_PAGE_OVERHEAD_BYTES, |total, source| {
                total.checked_add(serde_json::to_vec(source)?.len()).ok_or(
                    CaptureError::SystemInvariant(
                        "Deep Agents source-stage retained bytes overflowed",
                    ),
                )
            })?;
    ensure_retained_bound(retained_bytes)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let resolution = reconcile_locator(&mut group, authority, context)?;
    if !page.is_empty() {
        let mut retained = NativePathRetainedSourceEntities::default();
        for source in page {
            let mut source = source.clone();
            source.descriptor.source_identity = Some(resolution.canonical_source_identity.clone());
            source.descriptor.raw_source_path =
                Some(authority.canonical_database_path.display().to_string());
            source.descriptor.source_root =
                Some(authority.configured_source_root.display().to_string());
            source.sync.deleted_at = None;
            if let Some(metadata) = source.sync.metadata.as_object_mut() {
                metadata.insert(
                    "source_identity".to_owned(),
                    json!(resolution.canonical_source_identity),
                );
                metadata.insert(
                    "source_revision".to_owned(),
                    json!(authority.source_revision),
                );
                metadata.insert(
                    "nativepath_publication".to_owned(),
                    json!(DEEPAGENTS_NATIVE_PARSER_REVISION),
                );
            }
            group.upsert_capture_source(&source)?;
            group.bind_capture_source_provider_route(source.id, &resolution.route_binding())?;
            retained.capture_source_ids.push(source.id);
        }
        let key = generation_key(
            authority,
            context,
            &resolution.canonical_source_identity,
            cursor.generation,
        );
        group.stage_source_generation_page(&key, &retained)?;
    }
    revalidate_optional(snapshot, &authority.database_path)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    next.canonical_source_identity = resolution.canonical_source_identity;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_retirement_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: Option<&ProviderSqliteSourceSnapshot>,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    cursor: &DeepAgentsNativeCursor,
    after: Option<SerializableRetirementFrontier>,
    missing: bool,
) -> Result<DeepAgentsNativeCursor> {
    let store_after = after
        .as_ref()
        .map(SerializableRetirementFrontier::to_store)
        .transpose()?;
    let predicted = predict_retirement_page(
        store,
        authority,
        context,
        store_after.as_ref(),
        DEEPAGENTS_RETIREMENT_UNITS,
    )?;
    let mut next = cursor.clone();
    next.phase = if predicted.done {
        if missing {
            DeepAgentsCorePhase::MissingComplete
        } else {
            DeepAgentsCorePhase::Complete
        }
    } else {
        let next_after = predicted
            .next_after
            .clone()
            .map(SerializableRetirementFrontier::from_store);
        if missing {
            DeepAgentsCorePhase::MissingRetire { after: next_after }
        } else {
            DeepAgentsCorePhase::Retire { after: next_after }
        }
    };
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let transition = cursor_transition(context, authority, stored.as_ref(), &next)?;
    let publication_id =
        publication_id(authority, cursor, &next, transition.next().cursor.as_str());
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, DEEPAGENTS_PAGE_OVERHEAD_BYTES)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(next);
    }
    let key = generation_key(
        authority,
        context,
        &cursor.canonical_source_identity,
        cursor.generation,
    );
    let actual = group.retire_source_generation_page(
        &key,
        store_after.as_ref(),
        DEEPAGENTS_RETIREMENT_UNITS,
        context.imported_at.timestamp_millis(),
    )?;
    if actual.next_after != predicted.next_after || actual.done != predicted.done {
        return Err(CaptureError::SystemInvariant(
            "Deep Agents retirement frontier diverged from typed Store authority",
        ));
    }
    if missing && actual.done {
        let retirement = ProviderSourceRouteRetirement {
            provider: CaptureProvider::DeepAgents,
            source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.route_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
            expected_source_revision: authority.source_revision.clone(),
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason: missing_retirement_reason(
                &authority.configured_source_root,
                &authority.database_path,
            ),
        };
        let _ = group.retire_provider_source_route(&retirement)?;
    }
    revalidate_optional(snapshot, &authority.database_path)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok(next)
}
