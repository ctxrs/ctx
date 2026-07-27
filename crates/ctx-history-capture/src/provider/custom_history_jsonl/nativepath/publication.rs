use super::*;

pub(super) fn upstream_cursor_targets(
    context: &ProviderAdapterContext,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
) -> Vec<CustomUpstreamCursorTarget> {
    sources
        .values()
        .filter_map(|(_, source)| {
            let source_checkpoint = source
                .cursor
                .as_ref()
                .and_then(|cursor| cursor.after.as_ref())
                .map(|checkpoint| (checkpoint.cursor.clone(), checkpoint.observed_at));
            let event_checkpoint = events
                .iter()
                .filter(|(_, event)| event.source_id == source.source_id)
                .filter_map(|(line, event)| {
                    event
                        .native_cursor
                        .as_ref()
                        .map(|cursor| (*line, cursor.clone(), event.occurred_at))
                })
                .max_by_key(|(line, _, _)| *line)
                .map(|(_, cursor, observed_at)| (cursor, observed_at));
            let (raw_cursor, observed_at) = source_checkpoint.or(event_checkpoint)?;
            Some(CustomUpstreamCursorTarget {
                machine_id: source
                    .machine_id
                    .clone()
                    .unwrap_or_else(|| context.machine_id.clone()),
                stream: custom_history_jsonl_v1_cursor_stream(
                    &source.provider_key,
                    &source.source_id,
                    &source.source_format,
                ),
                raw_cursor,
                observed_at,
            })
        })
        .collect()
}

pub(super) fn pending_upstream_cursor_transitions(
    store: &Store,
    context: &ProviderAdapterContext,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
) -> Result<Vec<NativePathCursorTransition>> {
    let mut transitions = Vec::new();
    for target in upstream_cursor_targets(context, sources, events) {
        let stored = store.get_sync_cursor(None, &target.machine_id, &target.stream)?;
        let stored_is_native = stored
            .as_ref()
            .is_some_and(|cursor| decode_native_path_committed_cursor(&cursor.cursor).is_ok());
        let stored_raw = stored
            .as_ref()
            .map(|cursor| decode_released_or_native_upstream_cursor(&cursor.cursor))
            .transpose()?;
        if stored_is_native && stored_raw.as_deref() == Some(target.raw_cursor.as_str()) {
            continue;
        }
        let next = CustomUpstreamCursor {
            version: CUSTOM_UPSTREAM_CURSOR_VERSION,
            parser_revision: CUSTOM_PARSER_REVISION.to_owned(),
            policy_revision: CUSTOM_POLICY_REVISION.to_owned(),
            raw_cursor: target.raw_cursor,
        };
        transitions.push(NativePathCursorTransition::new(
            stored.map(|cursor| cursor.cursor),
            provider_sync_cursor(
                &target.machine_id,
                target.stream,
                serde_json::to_string(&next)?,
                target.observed_at,
            ),
        ));
        if transitions.len() == CUSTOM_UPSTREAM_CURSORS_PER_PAGE {
            break;
        }
    }
    Ok(transitions)
}

pub(super) fn upstream_cursors_pending(
    store: &Store,
    context: &ProviderAdapterContext,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
) -> Result<bool> {
    Ok(!pending_upstream_cursor_transitions(store, context, sources, events)?.is_empty())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_upstream_cursors(
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
    stamp: Option<&CustomFileStamp>,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    let mut transitions = pending_upstream_cursor_transitions(store, context, sources, events)?;
    if transitions.is_empty() {
        return Ok(false);
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut changed_groups = 0_usize;
        loop {
            if !revalidate(stamp)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let publication_id = upstream_publication_id(&transitions);
            let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
            let mut group = store.begin_native_path_publication_group(
                admission,
                NativePathGroupAccounting::new(
                    1,
                    transitions.len(),
                    PAGE_ACCOUNTING_OVERHEAD_BYTES,
                )?,
            )?;
            match group.classify_cursor_set(&publication_id, &transitions)? {
                NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                    group.commit()?;
                }
                NativePathCursorSetClassification::AllExpected => {
                    if !revalidate(stamp)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    group.commit()?;
                    summary.set_work_result(ProviderImportWorkResult::Changed);
                }
            }
            changed_groups = changed_groups.saturating_add(1);
            transitions = pending_upstream_cursor_transitions(store, context, sources, events)?;
            if transitions.is_empty()
                || (options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0)
            {
                break;
            }
        }
        Ok(!transitions.is_empty())
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(work_remaining), Ok(())) => Ok(work_remaining),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(crate) fn decode_released_or_native_upstream_cursor(encoded: &str) -> Result<String> {
    match decode_native_path_committed_cursor(encoded) {
        Ok(committed) => {
            let cursor: CustomUpstreamCursor = serde_json::from_str(committed.provider_cursor())
                .map_err(|_| {
                    CaptureError::InvalidPayload(
                        "custom history NativePath upstream cursor is corrupt".to_owned(),
                    )
                })?;
            if cursor.version != CUSTOM_UPSTREAM_CURSOR_VERSION
                || cursor.parser_revision != CUSTOM_PARSER_REVISION
                || cursor.policy_revision != CUSTOM_POLICY_REVISION
            {
                return Err(CaptureError::InvalidPayload(
                    "custom history NativePath upstream cursor has an unreleased revision"
                        .to_owned(),
                ));
            }
            Ok(cursor.raw_cursor)
        }
        Err(_) => {
            let looks_native = serde_json::from_str::<Value>(encoded)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|object| {
                    object.contains_key("publication_id")
                        || object.contains_key("provider_cursor")
                        || object.contains_key("journal_checkpoint")
                });
            if looks_native {
                Err(CaptureError::InvalidPayload(
                    "custom history NativePath cursor envelope is corrupt".to_owned(),
                ))
            } else {
                Ok(encoded.to_owned())
            }
        }
    }
}

pub(super) fn upstream_publication_id(transitions: &[NativePathCursorTransition]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-upstream-publication-v1\0");
    for transition in transitions {
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        } else {
            digest.update(0_u64.to_be_bytes());
        }
        digest.update((transition.next().stream.len() as u64).to_be_bytes());
        digest.update(transition.next().stream.as_bytes());
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("custom-history-upstream-sha256-v1:{:x}", digest.finalize())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    source_revision: &str,
    canonical: &CanonicalCustomHistory,
    page: &[CoreUnit],
    page_start: usize,
    current_cursor: &CustomNativeCursor,
    next_cursor: &CustomNativeCursor,
    expected_cursor: Option<String>,
    stamp: Option<&CustomFileStamp>,
    anchor_only: bool,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if page.is_empty() && !anchor_only && current_cursor.anchor.is_none() {
        return publish_cursor_only(
            store,
            bulk_guard,
            context,
            stream,
            current_cursor,
            next_cursor,
            expected_cursor,
            stamp,
        );
    }
    let anchor = current_cursor
        .anchor
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "custom history NativePath Core page has no source anchor",
        ))?;
    let mut retained = NativePathRetainedSourceEntities::default();
    retained.capture_source_ids.push(anchor.capture_source_id);
    let mut retained_bytes = PAGE_ACCOUNTING_OVERHEAD_BYTES;
    for unit in page {
        unit.retained(&mut retained);
        retained_bytes = retained_bytes.saturating_add(unit.retained_bytes()?);
    }
    dedupe_retained(&mut retained);
    if retained_bytes > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "custom history Core page exceeds the NativePath retained-byte bound".to_owned(),
        ));
    }
    let generation_key = generation_key(
        context,
        logical_locator,
        stream,
        source_revision,
        current_cursor.generation,
        anchor,
    );
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(next_cursor)?,
            context.imported_at,
        ),
    );
    let publication_id = publication_id(
        logical_locator,
        current_cursor.generation,
        page_start,
        &transition,
    );
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            return Ok(false);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    if page_start == 0 {
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Custom,
                source_format: CUSTOM_ROUTE_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.clone(),
                locator_identity: logical_locator.to_owned(),
                cursor_stream: stream.to_owned(),
                proposed_source_identity: anchor.canonical_source_identity.clone(),
                raw_source_path: context
                    .source_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                source_revision: source_revision.to_owned(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        if resolution.canonical_source_identity != anchor.canonical_source_identity {
            return Err(CaptureError::InvalidPayload(
                "custom history NativePath route canonical identity changed unexpectedly"
                    .to_owned(),
            ));
        }
        if let Some(anchor_source) = &canonical.anchor_source {
            group.upsert_capture_source(anchor_source)?;
        }
        apply_core_units(committed_store, &mut group, page, summary, options)?;
        group.bind_capture_source_provider_route(
            anchor.capture_source_id,
            &resolution.route_binding(),
        )?;
    } else {
        apply_core_units(committed_store, &mut group, page, summary, options)?;
    }
    group.stage_source_generation_page(&generation_key, &retained)?;
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    let _ = canonical;
    Ok(true)
}

pub(super) fn apply_core_units(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    page: &[CoreUnit],
    summary: &mut ProviderImportSummary,
    _options: &CustomHistoryJsonlV1ImportOptions,
) -> Result<()> {
    for unit in page {
        match unit {
            CoreUnit::Session(unit) => {
                let existed = committed_store.get_session(unit.session.id).is_ok();
                group.upsert_session(&unit.session)?;
                if existed {
                    summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                } else {
                    summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
            }
            CoreUnit::Event(unit) => {
                if let Some(run) = &unit.run {
                    group.upsert_run(run)?;
                }
                if group.reconcile_provider_event(&unit.event, unit.authority)? {
                    summary.imported_events = summary.imported_events.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                } else {
                    summary.skipped_events = summary.skipped_events.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                }
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            CoreUnit::FileTouch(unit) => {
                group.upsert_file_touched(&unit.file)?;
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            CoreUnit::Edge(unit) => {
                let existed = committed_store.session_edge_exists(unit.edge.id)?;
                group.upsert_projection_neutral_session_edge(&unit.actor, &unit.edge)?;
                if existed {
                    summary.skipped_edges = summary.skipped_edges.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                } else {
                    summary.imported_edges = summary.imported_edges.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_retirement_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    logical_locator: &str,
    stream: &str,
    cursor: &CustomNativeCursor,
    after: Option<&CustomRetirementFrontier>,
    expected_cursor: Option<String>,
    stamp: Option<&CustomFileStamp>,
) -> Result<(CustomNativeCursor, bool)> {
    let anchor = cursor.anchor.as_ref().ok_or(CaptureError::SystemInvariant(
        "custom history NativePath retirement has no source anchor",
    ))?;
    let key = generation_key(
        context,
        logical_locator,
        stream,
        &cursor.source_revision,
        cursor.generation,
        anchor,
    );
    let after_store = after.map(CustomRetirementFrontier::to_store).transpose()?;
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, PAGE_ACCOUNTING_OVERHEAD_BYTES)?,
    )?;
    let preview = group.preview_source_generation_retirement_page(
        &key,
        after_store.as_ref(),
        CUSTOM_RETIREMENT_UNITS_PER_PAGE,
    )?;
    let mut next = cursor.clone();
    next.phase = if preview.done {
        CustomCursorPhase::Complete
    } else {
        CustomCursorPhase::Retire {
            after: preview
                .next_after
                .clone()
                .map(CustomRetirementFrontier::from_store),
        }
    };
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(&next)?,
            context.imported_at,
        ),
    );
    let publication_id = retirement_publication_id(logical_locator, cursor.generation, &transition);
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
        CUSTOM_RETIREMENT_UNITS_PER_PAGE,
        context.imported_at.timestamp_millis(),
    )?;
    if retired != preview {
        return Err(CaptureError::SystemInvariant(
            "custom history NativePath retirement preview changed",
        ));
    }
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok((next, true))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_cursor_only(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    stream: &str,
    current: &CustomNativeCursor,
    next: &CustomNativeCursor,
    expected_cursor: Option<String>,
    stamp: Option<&CustomFileStamp>,
) -> Result<bool> {
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(next)?,
            context.imported_at,
        ),
    );
    let publication_id =
        publication_id(&current.logical_locator, current.generation, 0, &transition);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, PAGE_ACCOUNTING_OVERHEAD_BYTES)?,
    )?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            Ok(false)
        }
        NativePathCursorSetClassification::AllExpected => {
            group.prepare_journal_checkpoint()?;
            group.publish_cursor_set()?;
            group.commit()?;
            Ok(true)
        }
    }
}

pub(super) fn initial_cursor(
    stored: Option<&SyncCursor>,
    logical_locator: &str,
    source_revision: &str,
    current_anchor: Option<CustomAnchorAuthority>,
) -> Result<(CustomNativeCursor, Option<String>)> {
    let Some(stored) = stored else {
        return Ok((
            new_cursor(logical_locator, source_revision, 0, current_anchor),
            None,
        ));
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let prior = decode_cursor(committed.provider_cursor())?;
        validate_cursor(&prior, logical_locator)?;
        if prior.source_revision == source_revision && !prior.retired {
            return Ok((prior, Some(stored.cursor.clone())));
        }
        let generation = prior
            .generation
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "custom history NativePath generation exhausted",
            ))?;
        return Ok((
            new_cursor(
                logical_locator,
                source_revision,
                generation,
                current_anchor.or(prior.anchor),
            ),
            Some(stored.cursor.clone()),
        ));
    }
    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_none() {
        return Err(CaptureError::InvalidPayload(
            "custom history cursor is neither NativePath nor a released migration cursor"
                .to_owned(),
        ));
    }
    Ok((
        new_cursor(logical_locator, source_revision, 0, current_anchor),
        Some(stored.cursor.clone()),
    ))
}

pub(super) fn new_cursor(
    logical_locator: &str,
    source_revision: &str,
    generation: u64,
    anchor: Option<CustomAnchorAuthority>,
) -> CustomNativeCursor {
    CustomNativeCursor {
        version: CUSTOM_NATIVE_CURSOR_VERSION,
        parser_revision: CUSTOM_PARSER_REVISION.to_owned(),
        policy_revision: CUSTOM_POLICY_REVISION.to_owned(),
        logical_locator: logical_locator.to_owned(),
        source_revision: source_revision.to_owned(),
        generation,
        phase: CustomCursorPhase::Publish { next_unit: 0 },
        anchor,
        retired: false,
    }
}
