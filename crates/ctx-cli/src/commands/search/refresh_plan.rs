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
    let watcher_generation = runtime
        .as_deref_mut()
        .and_then(|runtime| runtime.watcher_generation(&sources));
    if runtime.is_some() && watcher_generation.is_none() {
        if let Some(runtime) = runtime.as_deref_mut() {
            runtime.invalidate();
        }
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
    }
    let work = if let Some(runtime) = runtime.as_deref_mut() {
        let publication_owner = store.effective_provider_file_publication_inventory_owner()?;
        let publication_pending = publication_owner.is_some();
        let rebuild = runtime.cached_work.as_ref().is_none_or(|cached| {
            cached.source_fingerprint != source_fingerprint
                || cached.publication_owner != publication_owner
                || watcher_generation != Some(cached.source_change_generation)
                || (!publication_pending
                    && cached.passes_since_reinventory >= DAEMON_SEARCH_REFRESH_REINVENTORY_PASSES)
        });
        if rebuild {
            let inventory = inventory_import_sources(&store, sources, false)?;
            let inventoried_source_count = inventory.totals.sources;
            let plan = ImportPlan::build(&store, inventory.sources)?;
            let inventory_failures = inventory.failures;
            let failed_inventory_pending =
                failed_inventory_pending_counts(&store, &inventory_failures)?;
            runtime.cached_work = Some(SearchRefreshWork {
                source_fingerprint,
                publication_owner,
                source_change_generation: watcher_generation.unwrap_or(0),
                passes_since_reinventory: 0,
                execution_state: crate::commands::import::ImportExecutionState::for_plan(&plan),
                plan,
                inventory_failures,
                failed_inventory_pending,
                inventoried_source_count,
                planned_total_bytes: inventory.totals.source_bytes,
            });
        }
        let work = runtime
            .cached_work
            .as_mut()
            .expect("daemon refresh cache must be populated");
        work.execution_state.begin_new_pass();
        work.passes_since_reinventory = work.passes_since_reinventory.saturating_add(1);
        work
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
