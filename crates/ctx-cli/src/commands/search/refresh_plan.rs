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
    let _disk_io_pacing = ctx_history_capture::install_disk_io_pacer(disk_io_pacer);
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
        let publication_owner = store.effective_provider_file_publication_inventory_owner()?;
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
        if runtime.cached_work.is_none()
            && watcher_changes
                .as_ref()
                .is_some_and(|changes| !changes.dirty_paths.is_empty())
        {
            runtime.request_inventory(SearchInventoryReason::SourcesChanged);
        }
        let force_rebuild = runtime.durable_sources_changed(&source_fingerprint)
            || runtime.cached_work.as_ref().is_some_and(|cached| {
                cached.source_fingerprint != source_fingerprint
                    || cached.publication_owner != publication_owner
            });
        if force_rebuild {
            runtime.request_inventory(SearchInventoryReason::SourcesChanged);
        }
        let inventory_sources = sources
            .iter()
            .filter(|source| {
                publication_owner.as_ref().is_none_or(|owner| {
                    !source_matches_publication_owner(source, owner)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let publication_page_count = usize::from(publication_pending);
        let inventory_source_count = inventory_sources
            .len()
            .saturating_add(publication_page_count);
        if runtime.prepare_inventory(&source_fingerprint, inventory_source_count, force_rebuild) {
            if let Some(source_index) = runtime.next_inventory_source_index() {
                let include_publication_owner = publication_pending && source_index == 0;
                let source_page_index = source_index.saturating_sub(publication_page_count);
                let inventory_page = if include_publication_owner {
                    Vec::new()
                } else {
                    inventory_sources
                        .get(source_page_index)
                        .cloned()
                        .into_iter()
                        .collect::<Vec<_>>()
                };
                let inventoried_paths = inventory_page
                    .iter()
                    .map(|source| source.path.clone())
                    .collect::<BTreeSet<_>>();
                let inventory = match inventory_import_sources_page(
                    &store,
                    inventory_page,
                    true,
                    include_publication_owner,
                ) {
                    Ok(inventory) => inventory,
                    Err(error) => {
                        runtime.note_inventory_error(error_summary(&error));
                        return Err(error);
                    }
                };
                let source_bytes = inventory.totals.source_bytes;
                let operation_count = daemon_search_inventory_operation_count(
                    inventory.totals.sources,
                    inventory.totals.source_files,
                );
                pace_daemon_search_inventory(source_bytes, operation_count);
                merge_search_inventory_page(
                    &store,
                    &source_fingerprint,
                    publication_owner.clone(),
                    inventoried_paths,
                    inventory,
                    &mut runtime.cached_work,
                )?;
                runtime.note_inventory_source_completed(source_bytes, operation_count);
            }
            if runtime.inventory_is_complete() {
                runtime.complete_inventory();
                if let Some(work) = runtime.cached_work.as_mut() {
                    work.last_reinventory_at = Instant::now();
                    work.passes_since_reinventory = 0;
                }
            }
        } else if let Some(changes) = watcher_changes {
            if !changes.dirty_paths.is_empty() {
                let refresh_result = refresh_dirty_search_sources(
                    &store,
                    &sources,
                    &changes.dirty_paths,
                    runtime
                        .cached_work
                        .as_mut()
                        .expect("daemon refresh cache must be populated"),
                );
                if let Err(error) = refresh_result {
                    runtime.invalidate();
                    return Err(error);
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

const DAEMON_SEARCH_INVENTORY_OPERATION_BYTES: u64 = 4 * 1024;

fn daemon_search_inventory_operation_count(source_count: usize, source_files: usize) -> u64 {
    let sources = u64::try_from(source_count).unwrap_or(u64::MAX);
    let files = u64::try_from(source_files).unwrap_or(u64::MAX);
    sources.saturating_add(files.saturating_mul(2))
}

fn pace_daemon_search_inventory(source_bytes: u64, operation_count: u64) {
    ctx_history_capture::pace_current_disk_io(
        source_bytes.saturating_add(
            operation_count.saturating_mul(DAEMON_SEARCH_INVENTORY_OPERATION_BYTES),
        ),
    );
}

fn merge_search_inventory_page(
    store: &Store,
    source_fingerprint: &str,
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

fn refresh_dirty_search_sources(
    store: &Store,
    sources: &[SourceInfo],
    dirty_paths: &BTreeSet<PathBuf>,
    work: &mut SearchRefreshWork,
) -> Result<()> {
    let dirty_sources = sources
        .iter()
        .filter(|source| dirty_paths.contains(&source.path))
        .cloned()
        .collect::<Vec<_>>();
    if dirty_sources.is_empty() {
        return Ok(());
    }

    let inventory = inventory_import_sources(store, dirty_sources, true)?;
    let operation_count = daemon_search_inventory_operation_count(
        inventory.totals.sources,
        inventory.totals.source_files,
    );
    pace_daemon_search_inventory(inventory.totals.source_bytes, operation_count);
    let mut planned_sources = work.plan.sources.clone();
    planned_sources.retain(|planned| !dirty_paths.contains(&planned.source.path));
    planned_sources.extend(
        inventory
            .sources
            .into_iter()
            .filter(|planned| dirty_paths.contains(&planned.source.path)),
    );
    let plan = ImportPlan::build(store, planned_sources)?;

    work.inventory_failures
        .retain(|failure| !dirty_paths.contains(&failure.source.path));
    work.inventory_failures.extend(inventory.failures);
    work.failed_inventory_pending =
        failed_inventory_pending_counts(store, &work.inventory_failures)?;
    work.planned_total_bytes = plan.sources.iter().fold(0u64, |total, source| {
        total.saturating_add(source.stats.bytes)
    });
    work.execution_state = work
        .execution_state
        .rebase_for_plan(&work.plan, &plan, dirty_paths);
    work.plan = plan;
    Ok(())
}

fn daemon_search_refresh_reinventory_due(
    elapsed_since_reinventory: StdDuration,
    publication_pending: bool,
) -> bool {
    !publication_pending && elapsed_since_reinventory >= DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL
}
