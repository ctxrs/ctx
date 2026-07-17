pub(crate) fn refresh_before_search(
    args: &SearchArgs,
    data_root: &Path,
    daemon_enabled: bool,
) -> Result<SearchRefreshReport> {
    if args.refresh == RefreshArg::Off {
        return Ok(SearchRefreshReport::skipped(RefreshArg::Off, "skipped"));
    }
    if args.refresh == RefreshArg::Background && daemon_enabled {
        return Ok(SearchRefreshReport::skipped(
            RefreshArg::Background,
            "daemon_background",
        ));
    }
    let execution_policy = match args.refresh {
        RefreshArg::Wait => ImportExecutionPolicy::Drain,
        RefreshArg::Background | RefreshArg::Off => ImportExecutionPolicy::Interactive,
    };
    let _disk_io_pacing =
        ctx_history_capture::install_disk_io_pacer(execution_policy.disk_io_pacer());
    let source_identity = normalize_source_identity_filters(SourceIdentityFilterArgs::from(args))?;
    if !source_identity.is_empty()
        && args
            .provider
            .is_some_and(|provider| !matches!(provider, ProviderArg::Custom))
    {
        return Err(anyhow!(
            "custom history source filters can only be combined with --provider custom"
        ));
    }
    let sources = if source_identity.is_empty() {
        search_refresh_sources(args.provider)
    } else {
        Vec::new()
    };
    let plugin_sources =
        match search_refresh_plugin_sources(data_root, args.provider, &source_identity) {
            Ok(sources) => sources,
            Err(err) if args.refresh == RefreshArg::Background => {
                return Ok(SearchRefreshReport::failed(
                    RefreshArg::Background,
                    sources.len(),
                    ImportTotals::default(),
                    error_summary(&err),
                ));
            }
            Err(err) => return Err(err.context("search refresh failed")),
        };
    if sources.is_empty()
        && plugin_sources.is_empty()
        && !search_refresh_has_publication_work(data_root)?
    {
        if args.refresh == RefreshArg::Wait {
            return Err(anyhow!(
                "wait search refresh found no supported discovered native provider or enabled auto history-source plugin sources; rerun the search with --refresh off to use the existing index"
            ));
        }
        return Ok(SearchRefreshReport::skipped(args.refresh, "no_sources"));
    }
    let source_count = sources.len().saturating_add(plugin_sources.len());
    match refresh_sources_for_search(
        data_root,
        sources,
        plugin_sources,
        args.refresh,
        args.json,
        execution_policy,
    ) {
        Ok(totals) => Ok(SearchRefreshReport::completed(
            args.refresh,
            source_count,
            totals,
        )),
        Err(err) if args.refresh == RefreshArg::Background => Ok(SearchRefreshReport::failed(
            RefreshArg::Background,
            source_count,
            search_refresh_failure_totals(&err).unwrap_or_default(),
            error_summary(&err),
        )),
        Err(err) => Err(err.context("search refresh failed")),
    }
}

pub(crate) fn search_refresh_sources(provider: Option<ProviderArg>) -> Vec<SourceInfo> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut sources = if let Some(provider) = provider {
        discover_provider_sources_for_provider(&home, provider.capture_provider())
    } else {
        discovered_sources()
    };
    sources
        .drain(..)
        .filter(|source| {
            source.exists
                && source.import_support.is_auto_importable()
                && source.status == ProviderSourceStatus::Available
                && source.source_format != "codex_history_jsonl"
        })
        .collect()
}

pub(crate) fn search_refresh_plugin_sources(
    data_root: &Path,
    provider: Option<ProviderArg>,
    source_identity: &SourceIdentityFilters,
) -> Result<Vec<HistorySourcePluginSource>> {
    if !matches!(provider, None | Some(ProviderArg::Custom)) {
        return Ok(Vec::new());
    }
    Ok(discover_history_source_plugins(data_root, &[])?
        .into_iter()
        .filter(|source| {
            source.enabled
                && source.refresh == HistorySourcePluginRefresh::Auto
                && source_identity.matches_plugin_source(source)
        })
        .collect())
}

pub(crate) fn search_refresh_has_publication_work(data_root: &Path) -> Result<bool> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Ok(false);
    }
    let store = Store::open(&db_path)?;
    Ok(store.has_pending_provider_file_publications()?
        || store.provider_file_publication_retirement_work_count()? > 0)
}

pub(crate) fn refresh_sources_for_search(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
    execution_policy: ImportExecutionPolicy,
) -> Result<ImportTotals> {
    refresh_sources_for_search_inner(
        data_root,
        sources,
        plugin_sources,
        refresh,
        json_output,
        execution_policy,
        None,
    )
}

pub(crate) fn refresh_sources_for_search_with_runtime(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
    execution_policy: ImportExecutionPolicy,
    runtime: &mut SearchRefreshRuntime,
) -> Result<ImportTotals> {
    refresh_sources_for_search_inner(
        data_root,
        sources,
        plugin_sources,
        refresh,
        json_output,
        execution_policy,
        Some(runtime),
    )
}

fn refresh_sources_for_search_inner(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
    execution_policy: ImportExecutionPolicy,
    mut runtime: Option<&mut SearchRefreshRuntime>,
) -> Result<ImportTotals> {
    let disk_io_pacer = runtime
        .as_deref_mut()
        .map(|runtime| runtime.disk_io_pacer(execution_policy))
        .unwrap_or_else(|| execution_policy.disk_io_pacer());
    let _disk_io_pacing = ctx_history_capture::install_disk_io_pacer(disk_io_pacer.clone());
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let mut store = Store::open(&db_path)?;
    let had_indexed_content = store.indexed_history_item_count()? > 0;
    let search_projection_needs_backfill = store.event_search_projection_needs_backfill()?;
    let source_fingerprint = search_refresh_source_fingerprint(&sources);
    let watcher_changes = runtime
        .as_deref_mut()
        .and_then(|runtime| runtime.watcher_changes(&sources));
    let work = if let Some(runtime) = runtime.as_deref_mut() {
        if let Some(changes) = watcher_changes.as_ref() {
            runtime.retain_dirty_paths(&changes.dirty_paths);
        }
        let (publication_state_marker, publication_owner) =
            store.effective_provider_file_publication_inventory_snapshot()?;
        let publication_pending = publication_owner.is_some();
        let periodic_reinventory = runtime.inventory_progress.is_none()
            && runtime.cached_work.as_ref().is_some_and(|cached| {
                daemon_search_refresh_reinventory_due(
                    cached.last_reinventory_at.elapsed(),
                    publication_pending,
                )
            });
        if periodic_reinventory {
            runtime.request_inventory(SearchInventoryReason::HealthySweep);
        }
        if watcher_changes
            .as_ref()
            .is_some_and(|changes| changes.full_rebuild)
        {
            runtime.request_inventory(SearchInventoryReason::SourcesChanged);
        }
        let publication_rebuild = runtime
            .cached_work
            .as_ref()
            .is_some_and(|cached| cached.publication_state_marker != publication_state_marker);
        let force_rebuild = runtime.durable_sources_changed(&source_fingerprint)
            || runtime
                .cached_work
                .as_ref()
                .is_some_and(|cached| cached.source_fingerprint != source_fingerprint)
            || publication_rebuild;
        if force_rebuild {
            runtime.request_inventory(if publication_rebuild {
                SearchInventoryReason::PublicationChanged
            } else {
                SearchInventoryReason::SourcesChanged
            });
        }
        if runtime.cached_work.is_none()
            && runtime.inventory_progress.is_none()
            && !runtime.pending_dirty_paths.is_empty()
        {
            runtime.request_inventory(SearchInventoryReason::SourcesChanged);
        } else if runtime.inventory_progress.is_none()
            && !runtime.pending_full_inventory
            && !runtime.pending_dirty_paths.is_empty()
        {
            process_dirty_source_paths_page(&store, &sources, runtime)?;
        }
        let inventory_sources = sources
            .iter()
            .filter(|source| {
                publication_owner
                    .as_ref()
                    .is_none_or(|owner| !source_matches_publication_owner(source, owner))
            })
            .cloned()
            .collect::<Vec<_>>();
        if runtime.prepare_inventory(
            &inventory_sources,
            &source_fingerprint,
            &publication_state_marker,
            publication_owner.clone(),
            force_rebuild,
        )? {
            let operations_before = disk_io_pacer.filesystem_operation_count();
            let inventory_step = match runtime.advance_inventory(&store) {
                Ok(step) => step,
                Err(error) => {
                    runtime.note_inventory_error(error_summary(&error));
                    return Err(error);
                }
            };
            let operation_count = disk_io_pacer
                .filesystem_operation_count()
                .saturating_sub(operations_before);
            let publication_state_after_page = store
                .effective_provider_file_publication_inventory_snapshot()?
                .0;
            if runtime.inventory_publication_state_changed(&publication_state_after_page) {
                runtime.restart_inventory_after_publication_transition();
            } else {
                match inventory_step {
                    ImportInventoryCursorStep::Pending(slice) => {
                        runtime.note_inventory_slice(slice, operation_count);
                    }
                    ImportInventoryCursorStep::SourceComplete(inventory) => {
                        let source_bytes = inventory.totals.source_bytes;
                        let inventoried_paths = inventory
                            .sources
                            .iter()
                            .map(|planned| planned.source.path.clone())
                            .chain(
                                inventory
                                    .failures
                                    .iter()
                                    .map(|failure| failure.source.path.clone()),
                            )
                            .collect::<BTreeSet<_>>();
                        let (bound_publication_state, bound_publication_owner) =
                            runtime.inventory_publication_snapshot().ok_or_else(|| {
                                anyhow!("inventory page lost its publication snapshot")
                            })?;
                        merge_search_inventory_page(
                            &store,
                            &source_fingerprint,
                            bound_publication_state,
                            bound_publication_owner,
                            inventoried_paths,
                            inventory,
                            &mut runtime.cached_work,
                        )?;
                        runtime.note_inventory_source_completed(source_bytes, operation_count);
                    }
                    ImportInventoryCursorStep::Complete => runtime.note_inventory_cursor_complete(),
                }
            }
            if runtime.inventory_is_complete() {
                let completed_scoped = runtime
                    .inventory_progress
                    .as_ref()
                    .is_some_and(|progress| progress.scoped);
                runtime.complete_inventory();
                if !completed_scoped {
                    if let Some(work) = runtime.cached_work.as_mut() {
                        work.last_reinventory_at = Instant::now();
                        work.passes_since_reinventory = 0;
                    }
                }
            }
        }
        if let Some(work) = runtime.cached_work.as_mut() {
            work.execution_state.begin_new_pass();
            work.passes_since_reinventory = work.passes_since_reinventory.saturating_add(1);
            work
        } else {
            let plan = ImportPlan {
                sources: Vec::new(),
                fresh_units: 0,
                recovery_units: 0,
            };
            let mut execution_state =
                crate::commands::import::ImportExecutionState::for_plan(&plan);
            return execute_search_refresh_work(
                data_root,
                &mut store,
                plugin_sources.len(),
                had_indexed_content,
                search_projection_needs_backfill,
                plugin_sources,
                refresh,
                json_output,
                execution_policy,
                &plan,
                &mut execution_state,
                Vec::new(),
                (0, 0),
                0,
            )
            .map(|execution| execution.totals);
        }
    } else {
        let inventory = inventory_import_sources(&store, sources, false)?;
        let refresh_source_count = inventory
            .totals
            .sources
            .saturating_add(plugin_sources.len());
        let plan = ImportPlan::build(&store, inventory.sources)?;
        let inventory_failures = inventory.failures;
        let failed_inventory_pending =
            failed_inventory_pending_counts(&store, &inventory_failures)?;
        let planned_total_bytes = inventory.totals.source_bytes;
        let mut execution_state = crate::commands::import::ImportExecutionState::for_plan(&plan);
        return execute_search_refresh_work(
            data_root,
            &mut store,
            refresh_source_count,
            had_indexed_content,
            search_projection_needs_backfill,
            plugin_sources,
            refresh,
            json_output,
            execution_policy,
            &plan,
            &mut execution_state,
            inventory_failures,
            failed_inventory_pending,
            planned_total_bytes,
        )
        .map(|execution| execution.totals);
    };

    let result = execute_search_refresh_work(
        data_root,
        &mut store,
        work.inventoried_source_count
            .saturating_add(plugin_sources.len()),
        had_indexed_content,
        search_projection_needs_backfill,
        plugin_sources,
        refresh,
        json_output,
        execution_policy,
        &work.plan,
        &mut work.execution_state,
        work.inventory_failures.clone(),
        work.failed_inventory_pending,
        work.planned_total_bytes,
    );
    match result {
        Ok(execution) => Ok(execution.totals),
        Err(error) => {
            if let Some(runtime) = runtime {
                runtime.invalidate();
            }
            Err(error)
        }
    }
}

fn process_dirty_source_paths_page(
    store: &Store,
    sources: &[SourceInfo],
    runtime: &mut SearchRefreshRuntime,
) -> Result<()> {
    const DIRTY_PATH_PAGE_LIMIT: usize = 64;

    let page = runtime
        .pending_dirty_paths
        .iter()
        .take(DIRTY_PATH_PAGE_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    let mut affected_sources = BTreeSet::new();
    let mut updated_plans = BTreeMap::new();
    let mut directly_observed_roots = BTreeSet::new();
    for dirty in &page {
        let Some(source) = sources
            .iter()
            .find(|source| source.path == dirty.source_path)
        else {
            runtime.request_inventory(SearchInventoryReason::SourcesChanged);
            continue;
        };
        let exact_codex_path = source.provider == ctx_history_core::CaptureProvider::Codex
            && source.source_format == "codex_session_jsonl_tree";
        if !exact_codex_path && !directly_observed_roots.insert(source.path.clone()) {
            continue;
        }
        affected_sources.insert(source.path.clone());
        match crate::commands::import::inventory_dirty_source_path(
            store,
            source,
            &dirty.changed_path,
        )? {
            crate::commands::import::DirtySourcePathInventoryOutcome::Applied { updated_plan } => {
                if let Some(plan) = updated_plan {
                    updated_plans.insert(source.path.clone(), plan);
                }
            }
            crate::commands::import::DirtySourcePathInventoryOutcome::RequiresSourceInventory => {
                runtime.request_source_inventory(source.path.clone());
            }
        }
    }
    for dirty in page {
        runtime.pending_dirty_paths.remove(&dirty);
    }

    let Some(work) = runtime.cached_work.as_mut() else {
        return Ok(());
    };
    for planned in &mut work.plan.sources {
        if let Some(updated) = updated_plans.remove(&planned.source.path) {
            *planned = updated;
        }
    }
    if affected_sources.is_empty() {
        return Ok(());
    }
    let next_plan = ImportPlan::build(store, work.plan.sources.clone())?;
    work.execution_state =
        work.execution_state
            .rebase_for_plan(&work.plan, &next_plan, &affected_sources);
    work.plan = next_plan;
    work.planned_total_bytes = work.plan.sources.iter().fold(0u64, |total, source| {
        total.saturating_add(source.stats.bytes)
    });
    Ok(())
}

fn merge_search_inventory_page(
    store: &Store,
    source_fingerprint: &str,
    publication_state_marker: String,
    publication_owner: Option<ProviderFilePublicationInventoryOwner>,
    mut inventoried_paths: BTreeSet<PathBuf>,
    inventory: crate::commands::import::ImportInventory,
    cached_work: &mut Option<SearchRefreshWork>,
) -> Result<()> {
    let inventoried_source_count = inventory.totals.sources;
    inventoried_paths.extend(
        inventory
            .sources
            .iter()
            .map(|planned| planned.source.path.clone()),
    );
    if let Some(work) = cached_work.as_mut() {
        let mut planned_sources = work.plan.sources.clone();
        planned_sources.retain(|planned| !inventoried_paths.contains(&planned.source.path));
        planned_sources.extend(inventory.sources);
        let plan = ImportPlan::build(store, planned_sources)?;
        work.inventory_failures
            .retain(|failure| !inventoried_paths.contains(&failure.source.path));
        work.inventory_failures.extend(inventory.failures);
        work.failed_inventory_pending =
            failed_inventory_pending_counts(store, &work.inventory_failures)?;
        work.planned_total_bytes = plan.sources.iter().fold(0u64, |total, source| {
            total.saturating_add(source.stats.bytes)
        });
        work.execution_state =
            work.execution_state
                .rebase_for_plan(&work.plan, &plan, &inventoried_paths);
        work.plan = plan;
        work.source_fingerprint = source_fingerprint.to_owned();
        work.publication_state_marker = publication_state_marker;
        work.publication_owner = publication_owner;
        work.inventoried_source_count = work
            .plan
            .sources
            .len()
            .saturating_add(work.inventory_failures.len());
        return Ok(());
    }

    let plan = ImportPlan::build(store, inventory.sources)?;
    let inventory_failures = inventory.failures;
    let failed_inventory_pending = failed_inventory_pending_counts(store, &inventory_failures)?;
    let planned_total_bytes = plan.sources.iter().fold(0u64, |total, source| {
        total.saturating_add(source.stats.bytes)
    });
    *cached_work = Some(SearchRefreshWork {
        source_fingerprint: source_fingerprint.to_owned(),
        publication_state_marker,
        publication_owner,
        passes_since_reinventory: 0,
        last_reinventory_at: Instant::now(),
        execution_state: crate::commands::import::ImportExecutionState::for_plan(&plan),
        plan,
        inventory_failures,
        failed_inventory_pending,
        inventoried_source_count,
        planned_total_bytes,
    });
    Ok(())
}

fn daemon_search_refresh_reinventory_due(
    elapsed_since_reinventory: StdDuration,
    publication_pending: bool,
) -> bool {
    !publication_pending && elapsed_since_reinventory >= DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL
}
