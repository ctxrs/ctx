use super::*;

pub(super) struct PreparedClaudeCoreSource {
    pub(super) source: DiscoveredClaudeSession,
    pub(super) stream: String,
    pub(super) page: Box<ClaudeNativePage>,
    pub(super) cursor: ClaudeStoreCursor,
    pub(super) transition: NativePathCursorTransition,
    pub(super) mutation_units: usize,
}

#[derive(Default)]
pub(super) struct PreparedClaudeCoreGroup {
    pub(super) sources: Vec<PreparedClaudeCoreSource>,
    accounting: ClaudeCoreGroupAccounting,
}

#[derive(Clone, Copy)]
struct ClaudeCoreSourceAccounting {
    mutation_units: usize,
    retained_page_bytes: usize,
}

#[derive(Default)]
struct ClaudeCoreGroupAccounting {
    sources: usize,
    mutation_units: usize,
    retained_page_bytes: usize,
}

impl ClaudeCoreGroupAccounting {
    fn try_push(&mut self, source: ClaudeCoreSourceAccounting) -> bool {
        let sources = self.sources.saturating_add(1);
        let mutation_units = self.mutation_units.saturating_add(source.mutation_units);
        let retained_page_bytes = self
            .retained_page_bytes
            .saturating_add(source.retained_page_bytes);
        if sources > CLAUDE_GROUP_MAX_SOURCES
            || sources > NATIVE_PATH_MAX_GROUP_PAGES
            || retained_page_bytes > CLAUDE_GROUP_MAX_RETAINED_PAGE_BYTES
            || mutation_units > NATIVE_PATH_MAX_MUTATION_UNITS
        {
            return false;
        }
        self.sources = sources;
        self.mutation_units = mutation_units;
        self.retained_page_bytes = retained_page_bytes;
        true
    }
}

pub(super) enum PreparedClaudeCore {
    NoOp(ProviderImportSummary),
    Grouped(Box<PreparedClaudeCoreSource>),
    Individual,
}

impl PreparedClaudeCoreSource {
    fn retained_page_bytes(&self) -> usize {
        self.page.serialized_bytes
    }

    fn accounting(&self) -> ClaudeCoreSourceAccounting {
        ClaudeCoreSourceAccounting {
            mutation_units: self.mutation_units,
            retained_page_bytes: self.retained_page_bytes(),
        }
    }
}

impl PreparedClaudeCoreGroup {
    fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub(super) fn retained_page_bytes(&self) -> usize {
        self.accounting.retained_page_bytes
    }

    fn try_push(
        &mut self,
        source: Box<PreparedClaudeCoreSource>,
    ) -> std::result::Result<(), Box<PreparedClaudeCoreSource>> {
        if self
            .sources
            .iter()
            .any(|current| current.stream == source.stream)
            || !self.accounting.try_push(source.accounting())
        {
            return Err(source);
        }
        self.sources.push(*source);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn import_core_sources_grouped(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    sources: &[DiscoveredClaudeSession],
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    committed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let mut pending = PreparedClaudeCoreGroup::default();
    let store_path = store.path().to_path_buf();
    {
        let mut consume = |source: &DiscoveredClaudeSession,
                           prepared_source: PreparedClaudeCore|
         -> Result<bool> {
            match prepared_source {
                PreparedClaudeCore::NoOp(source_summary) => summary.merge_from(source_summary),
                PreparedClaudeCore::Individual => {
                    if flush_group(
                        store,
                        committed_store,
                        bulk_guard,
                        source_root,
                        options,
                        &mut pending,
                        &mut summary,
                        committed_groups,
                    )? && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    {
                        return Ok(true);
                    }
                    let committed_before = *committed_groups;
                    summary.merge_from(import_source(
                        store,
                        committed_store,
                        bulk_guard,
                        source,
                        source_root,
                        options,
                        committed_groups,
                    )?);
                    if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        && *committed_groups > committed_before
                    {
                        return Ok(true);
                    }
                }
                PreparedClaudeCore::Grouped(prepared) => {
                    let prepared = match pending.try_push(prepared) {
                        Ok(()) => return Ok(false),
                        Err(prepared) => prepared,
                    };
                    flush_group(
                        store,
                        committed_store,
                        bulk_guard,
                        source_root,
                        options,
                        &mut pending,
                        &mut summary,
                        committed_groups,
                    )?;
                    if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                        return Ok(true);
                    }
                    pending.try_push(prepared).map_err(|_| {
                        CaptureError::SystemInvariant(
                            "Claude source does not fit an empty bounded publication group",
                        )
                    })?;
                }
            }
            Ok(false)
        };
        if options.capture_work_limit == CaptureWorkLimit::Drain {
            prepare_grouped_core_sources_parallel(
                &store_path,
                sources,
                options,
                |source, prepared_source| {
                    if consume(source, prepared_source)? {
                        return Err(CaptureError::SystemInvariant(
                            "draining Claude preparation stopped after one safe group",
                        ));
                    }
                    Ok(())
                },
            )?;
        } else {
            let preparation_store = Store::open_read_only(&store_path)?;
            for source in sources {
                let prepared_source =
                    prepare_grouped_core_source(&preparation_store, source, options)?;
                if consume(source, prepared_source)? {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }
    }
    let committed = flush_group(
        store,
        committed_store,
        bulk_guard,
        source_root,
        options,
        &mut pending,
        &mut summary,
        committed_groups,
    )?;
    if committed && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
        summary.work_remaining = true;
    }
    if options.capture_work_limit == CaptureWorkLimit::Drain {
        for source in sources {
            let locator_identity = provider_path_identity(&source.canonical_path)?;
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::Claude,
                CLAUDE_PROJECTS_SOURCE_FORMAT,
                &locator_identity,
            );
            let pending_retirement = store
                .get_sync_cursor(None, &options.machine_id, &stream)?
                .map(|cursor| decode_store_cursor(&cursor.cursor))
                .transpose()?
                .is_some_and(|cursor| {
                    matches!(
                        cursor,
                        ClaudeStoredCursor::Native(ClaudeStoreCursor {
                            generation_phase: ClaudeGenerationPhase::Retiring { .. },
                            ..
                        })
                    )
                });
            if pending_retirement {
                summary.merge_from(import_source(
                    store,
                    committed_store,
                    bulk_guard,
                    source,
                    source_root,
                    options,
                    committed_groups,
                )?);
            }
        }
    }
    Ok(summary)
}

pub(super) fn preparation_worker_count(source_count: usize, available_parallelism: usize) -> usize {
    source_count
        .min(available_parallelism.max(1))
        .min(CLAUDE_CORE_PREPARATION_MAX_WORKERS)
}

pub(super) fn preparation_lane_capacity(workers: usize) -> usize {
    CLAUDE_CORE_PREPARATION_QUEUE_MAX_SOURCES / workers.max(1)
}

pub(super) fn prepare_grouped_core_sources_parallel<Consume>(
    store_path: &Path,
    sources: &[DiscoveredClaudeSession],
    options: &ClaudeProjectsImportOptions,
    mut consume: Consume,
) -> Result<()>
where
    Consume: FnMut(&DiscoveredClaudeSession, PreparedClaudeCore) -> Result<()>,
{
    if sources.len() <= 1 {
        let store = Store::open_read_only(store_path)?;
        for source in sources {
            consume(
                source,
                prepare_grouped_core_source(&store, source, options)?,
            )?;
        }
        return Ok(());
    }
    let available_parallelism = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let workers = preparation_worker_count(sources.len(), available_parallelism);
    let lane_capacity = preparation_lane_capacity(workers);
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| -> Result<()> {
        let (senders, receivers): (Vec<_>, Vec<_>) = (0..workers)
            .map(|_| mpsc::sync_channel(lane_capacity))
            .unzip();
        let mut handles = Vec::with_capacity(workers);
        let mut spawn_error = None;
        for (worker, sender) in senders.into_iter().enumerate() {
            let cancelled = &cancelled;
            let spawned = thread::Builder::new()
                .name(format!("ctx-claude-prepare-{worker}"))
                .spawn_scoped(scope, move || {
                    let store = match Store::open_read_only(store_path) {
                        Ok(store) => store,
                        Err(error) => {
                            let _ = sender.send(Err(error.into()));
                            return;
                        }
                    };
                    for source in sources.iter().skip(worker).step_by(workers) {
                        if cancelled.load(Ordering::Acquire) {
                            return;
                        }
                        let prepared = prepare_grouped_core_source(&store, source, options);
                        let failed = prepared.is_err();
                        if sender.send(prepared).is_err() || failed {
                            return;
                        }
                    }
                });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    spawn_error = Some(error);
                    break;
                }
            }
        }
        let operation = if spawn_error.is_none() {
            (|| {
                for (index, source) in sources.iter().enumerate() {
                    let prepared = receivers[index % workers].recv().map_err(|_| {
                        CaptureError::SystemInvariant(
                            "parallel Claude source preparation ended before its ordered source",
                        )
                    })??;
                    consume(source, prepared)?;
                }
                Ok(())
            })()
        } else {
            Ok(())
        };
        cancelled.store(true, Ordering::Release);
        drop(receivers);
        let mut worker_panicked = false;
        for handle in handles {
            worker_panicked |= handle.join().is_err();
        }
        if let Some(source) = spawn_error {
            return Err(CaptureError::SystemIo {
                operation: "Claude source preparation worker spawn",
                source,
            });
        }
        if worker_panicked {
            return Err(CaptureError::WorkerPanicked("Claude source preparation"));
        }
        operation
    })
}

#[allow(clippy::too_many_arguments)]
fn flush_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    pending: &mut PreparedClaudeCoreGroup,
    summary: &mut ProviderImportSummary,
    committed_groups: &mut usize,
) -> Result<bool> {
    if pending.is_empty() {
        return Ok(false);
    }
    let group = std::mem::take(pending);
    summary.merge_from(publish_core_group(
        store,
        committed_store,
        bulk_guard,
        source_root,
        options,
        group,
    )?);
    *committed_groups = committed_groups.saturating_add(1);
    Ok(true)
}

fn prepare_grouped_core_source(
    store: &Store,
    source: &DiscoveredClaudeSession,
    options: &ClaudeProjectsImportOptions,
) -> Result<PreparedClaudeCore> {
    if source.key.parent_provider_session_id().is_some() {
        return Ok(PreparedClaudeCore::Individual);
    }
    let locator_identity = provider_path_identity(&source.canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Claude,
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    let prior = stored
        .as_ref()
        .map(|cursor| decode_store_cursor(&cursor.cursor))
        .transpose()?
        .and_then(|cursor| match cursor {
            ClaudeStoredCursor::Native(cursor) => Some(cursor),
            ClaudeStoredCursor::Released(_) => None,
        });
    if prior.as_ref().is_some_and(|cursor| {
        matches!(
            cursor.generation_phase,
            ClaudeGenerationPhase::Retiring { .. }
        )
    }) {
        return Ok(PreparedClaudeCore::Individual);
    }
    let scanner_previous = prior.as_ref().map(|cursor| cursor.checkpoint.clone());
    let mut scanner = ClaudeNativeScanner::new(
        source.clone(),
        scanner_previous.as_ref(),
        ClaudeNativeProfile::CoreOnly,
    )
    .map_err(map_native_error)?;
    let first = scanner.next_page().map_err(map_native_error)?;
    let Some(ClaudeNativeOwnedPage::Core(page)) = first else {
        let finished = scanner.finish().map_err(map_native_error)?;
        if !finished.source_certified {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut summary = ProviderImportSummary {
            skipped_sessions: 1,
            skipped: 1,
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(PreparedClaudeCore::NoOp(summary));
    };
    let checkpoint = scanner.checkpoint_at(&page.next_safe_frontier, page.terminal);
    if scanner.next_page().map_err(map_native_error)?.is_some() {
        return Ok(PreparedClaudeCore::Individual);
    }
    let finished = scanner.finish().map_err(map_native_error)?;
    if !finished.source_certified {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let revision = source_revision(source, options.inventory_observation_token.as_deref());
    let cursor = next_cursor_state(source, prior.as_ref(), page.as_ref(), checkpoint, &revision);
    let next = provider_sync_cursor(
        &options.machine_id,
        stream.clone(),
        encode_store_cursor(&cursor)?,
        options.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let file_touches = page
        .rows
        .iter()
        .filter_map(|row| row.tool_call.as_ref())
        .map(|call| call.file_touches.len())
        .sum::<usize>();
    let mut mutation_units = 5_usize
        .saturating_add(page.rows.len())
        .saturating_add(file_touches);
    if !matches!(cursor.generation_phase, ClaudeGenerationPhase::Live) {
        mutation_units = mutation_units
            .saturating_add(3)
            .saturating_add(page.rows.len())
            .saturating_add(file_touches);
    }
    if mutation_units > NATIVE_PATH_MAX_MUTATION_UNITS
        || page.serialized_bytes > CLAUDE_GROUP_MAX_RETAINED_PAGE_BYTES
    {
        return Ok(PreparedClaudeCore::Individual);
    }
    Ok(PreparedClaudeCore::Grouped(Box::new(
        PreparedClaudeCoreSource {
            source: source.clone(),
            stream,
            page,
            cursor,
            transition,
            mutation_units,
        },
    )))
}
