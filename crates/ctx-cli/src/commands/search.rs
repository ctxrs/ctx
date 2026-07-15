use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};

use ctx_history_capture::{
    discover_provider_sources_for_provider, ProviderImportSummary, ProviderSourceStatus,
};
use ctx_history_core::database_path;
use ctx_history_store::{ImportWorkClass, Store};

use crate::analytics::AnalyticsProperties;
use crate::commands::import::{
    error_summary, failed_inventory_pending_counts, import_error_scope,
    import_history_source_plugin, import_selected_source, import_totals_json,
    import_work_progress_done, import_work_progress_message, inventory_import_sources,
    one_line_error, publication_recovery_maintenance_warning,
    recover_provider_file_publication_retirement, rejected_source_summary,
    repair_import_maintenance, ExecutableImportSlice, ImportExecutionPolicy, ImportFailureScope,
    ImportPlan, ImportTotals, SourceStats,
};
use crate::commands::setup::{
    indexed_history_item_count, insert_db_size_bucket, insert_store_analytics_counts,
};
use crate::history_source_plugins::{
    discover_history_source_plugins, HistorySourcePluginRefresh, HistorySourcePluginSource,
};
use crate::output::{compact_json, print_json};
use crate::progress::{ProgressArg, ProgressReporter};
use crate::provider_args::ProviderArg;
use crate::provider_sources::{discovered_sources, home_dir, SourceInfo};
use crate::search_filters::{
    missing_search_intent_error, normalize_source_identity_filters, search_filters,
    search_has_intent, search_no_results_target, SearchFilterInput, SearchIntentInput,
    SourceIdentityFilterArgs, SourceIdentityFilters,
};
use crate::search_render::{print_search_result_compact, print_search_result_verbose, SearchDto};
use crate::semantic::search_packet_with_backend;
use crate::store_util::open_existing_store_read_only;
use crate::transcript::shell_quote_arg;
use crate::{analytics, config, semantic, SearchArgs, SearchBackendArg, WAL_TRUNCATE_MIN_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RefreshArg {
    Background,
    Off,
    Wait,
}

impl RefreshArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) struct SearchRefreshReport {
    mode: RefreshArg,
    status: &'static str,
    source_count: usize,
    totals: ImportTotals,
    error: Option<String>,
}

#[derive(Debug)]
struct SearchRefreshFailure {
    error: anyhow::Error,
    totals: ImportTotals,
}

impl std::fmt::Display for SearchRefreshFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for SearchRefreshFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

pub(crate) fn search_refresh_failure_totals(error: &anyhow::Error) -> Option<ImportTotals> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SearchRefreshFailure>())
        .map(|failure| failure.totals.clone())
}

impl SearchRefreshReport {
    pub(crate) fn skipped(mode: RefreshArg, status: &'static str) -> Self {
        Self {
            mode,
            status,
            source_count: 0,
            totals: ImportTotals::default(),
            error: None,
        }
    }

    fn completed(mode: RefreshArg, source_count: usize, totals: ImportTotals) -> Self {
        Self {
            mode,
            status: "completed",
            source_count,
            totals,
            error: None,
        }
    }

    fn failed(mode: RefreshArg, source_count: usize, totals: ImportTotals, error: String) -> Self {
        Self {
            mode,
            status: "failed",
            source_count,
            totals,
            error: Some(error),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "mode": self.mode.as_str(),
            "status": self.status,
            "source_count": self.source_count,
            "totals": import_totals_json(&self.totals),
            "error": self.error,
        }))
    }
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
    config: &config::AppConfig,
) -> Result<()> {
    if !search_has_intent(SearchIntentInput {
        query: args.query.as_deref(),
        terms: &args.term,
        file: args.file.as_deref(),
    }) {
        return Err(missing_search_intent_error());
    }

    let db_path = database_path(data_root.clone());
    let had_existing_store = db_path.exists();
    let indexed_content_before_search = if had_existing_store {
        existing_store_indexed_content(&db_path)
    } else {
        Some(false)
    };
    analytics::insert_bool(
        analytics_properties,
        "had_existing_store_before_search",
        had_existing_store,
    );
    analytics::insert_bool(
        analytics_properties,
        "indexed_content_before_search_known",
        indexed_content_before_search.is_some(),
    );
    analytics::insert_bool(
        analytics_properties,
        "had_indexed_content_before_search",
        indexed_content_before_search.unwrap_or(false),
    );
    let refresh_started = Instant::now();
    let refresh = refresh_before_search(&args, &data_root, config.daemon.enabled)?;
    analytics::insert_duration(
        analytics_properties,
        "refresh_duration",
        refresh_started.elapsed(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_refresh_mode",
        refresh.mode.as_str(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_refresh_status",
        refresh.status,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "search_refresh_source_count_bucket",
        refresh.source_count as u64,
    );
    let backend_override = args.backend;
    let requested_backend = resolve_search_backend(backend_override, config)?;
    let semantic_enabled = config.semantic_search_enabled();
    if args.refresh == RefreshArg::Background
        && semantic_enabled
        && semantic::semantic_query_service_supported()
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        semantic::maybe_autostart_daemon_for_search(&data_root, config);
        semantic::wait_for_daemon_query_service(&data_root, StdDuration::from_secs(3));
    }
    insert_db_size_bucket(analytics_properties, &db_path);
    if refresh.status == "failed" && args.refresh == RefreshArg::Background && !had_existing_store {
        return Err(anyhow!(
            "search refresh failed and no existing ctx index is available; run `ctx import` first or retry with `--refresh wait`: {}",
            refresh.error.as_deref().unwrap_or("unknown refresh error")
        ));
    }
    let store = if args.refresh == RefreshArg::Off
        || refresh.status == "failed"
        || refresh.status == "completed"
        || had_existing_store
    {
        open_existing_store_read_only(&db_path, "ctx search")?
    } else {
        Store::open(&db_path)?
    };
    analytics::insert_bool(
        analytics_properties,
        "store_created_by_search",
        !had_existing_store && db_path.exists(),
    );
    insert_store_analytics_counts(analytics_properties, &store)?;
    analytics::insert_bool(
        analytics_properties,
        "has_indexed_content_after_search",
        indexed_history_item_count(&store)? > 0,
    );
    let source_identity = SourceIdentityFilterArgs::from(&args);
    let query = args.query.unwrap_or_default();
    let query_term_count = query
        .split_whitespace()
        .filter(|term| !term.trim().is_empty())
        .count()
        .saturating_add(
            args.term
                .iter()
                .filter(|term| !term.trim().is_empty())
                .count(),
        );
    analytics::insert_text_length_bucket(
        analytics_properties,
        "query_length_bucket",
        query.chars().count(),
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "query_term_count_bucket",
        query_term_count as u64,
    );
    let event_results = args.events || args.session.is_some();
    let options = ctx_history_search::PacketOptions {
        limit: args.limit,
        filters: search_filters(
            SearchFilterInput {
                session: args.session,
                provider: args.provider,
                source_identity,
                workspace: args.workspace.clone(),
                since: args.since.clone(),
                primary_only: args.primary_only,
                include_subagents: args.include_subagents,
                event_type: args.event_type.clone(),
                file: args.file.clone(),
                include_current_session: args.include_current_session,
            },
            Some(&store),
        )?,
        result_mode: if event_results {
            ctx_history_search::SearchResultMode::Events
        } else {
            ctx_history_search::SearchResultMode::Sessions
        },
        ..ctx_history_search::PacketOptions::default()
    };
    let uses_composed_terms = args.term.iter().any(|term| !term.trim().is_empty());
    let query_started = Instant::now();
    let (packet, retrieval) = search_packet_with_backend(
        &store,
        &data_root,
        &query,
        &args.term,
        &options,
        requested_backend,
        semantic_enabled,
        args.semantic_weight,
        args.refresh,
        !args.json,
    )?;
    analytics::insert_duration(
        analytics_properties,
        "query_duration",
        query_started.elapsed(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_backend_requested",
        requested_backend.as_str(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_backend_effective",
        retrieval.effective_mode().as_str(),
    );
    let result_count = packet.results.len();
    let citation_count = packet
        .results
        .iter()
        .map(|result| result.citations.len())
        .sum::<usize>();
    analytics::insert_count_bucket(
        analytics_properties,
        "result_count_bucket",
        result_count as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "citation_count_bucket",
        citation_count as u64,
    );
    analytics::insert_bool(analytics_properties, "zero_result", result_count == 0);
    let render_started = Instant::now();
    if args.json {
        let suggested_next_query = (!uses_composed_terms).then_some(query.as_str());
        print_json(SearchDto::packet(
            &store,
            &packet,
            &refresh,
            &retrieval,
            suggested_next_query,
        ))?;
    } else {
        if refresh.status == "failed" && args.refresh == RefreshArg::Background {
            if let Some(error) = &refresh.error {
                eprintln!(
                    "warning: search refresh failed; serving existing index; use --refresh wait to fail instead: {error}"
                );
            }
        }
        if packet.results.is_empty() {
            if let Some(file) = args
                .file
                .as_deref()
                .filter(|_| query.trim().is_empty() && !uses_composed_terms)
            {
                println!("no indexed events touched {}", file.display());
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!(
                        "next: ctx search {}",
                        shell_quote_arg(&file.display().to_string())
                    );
                }
            } else {
                println!(
                    "no results for {}",
                    search_no_results_target(&query, &args.term)
                );
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!("next: try broader terms with ctx search --term \"<term>\"");
                }
            }
        }
        let suggested_next_query = (!uses_composed_terms).then_some(query.as_str());
        for (index, result) in packet.results.iter().enumerate() {
            if args.verbose {
                print_search_result_verbose(result, suggested_next_query);
            } else {
                print_search_result_compact(index + 1, result);
            }
        }
    }
    analytics::insert_duration(
        analytics_properties,
        "render_duration",
        render_started.elapsed(),
    );
    Ok(())
}

pub(crate) fn resolve_search_backend(
    backend: Option<SearchBackendArg>,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    let semantic_enabled = config.semantic_search_enabled();
    match backend {
        Some(SearchBackendArg::Semantic) if !semantic_enabled => Err(anyhow!(
            "semantic search is disabled. Set [search] semantic = true in ctx config to enable the local semantic preview"
        )),
        Some(SearchBackendArg::Semantic) if !semantic::semantic_query_service_supported() => Err(
            anyhow!(
                "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical"
            ),
        ),
        Some(SearchBackendArg::Semantic) if !config.daemon.enabled => Err(anyhow!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical"
        )),
        value
            if semantic_enabled
                && semantic::semantic_query_service_supported()
                && !config.daemon.enabled
                && !matches!(value, Some(SearchBackendArg::Lexical)) =>
        {
            Err(anyhow!(
                "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical"
            ))
        }
        Some(value) => Ok(value),
        None if semantic_enabled => Ok(SearchBackendArg::Hybrid),
        None => Ok(SearchBackendArg::Lexical),
    }
}

fn existing_store_indexed_content(db_path: &Path) -> Option<bool> {
    open_existing_store_read_only(db_path, "ctx search analytics preflight")
        .and_then(|store| indexed_history_item_count(&store))
        .ok()
        .map(|indexed_items| indexed_items > 0)
}

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
        && !search_refresh_has_retirement_work(data_root)?
    {
        if args.refresh == RefreshArg::Wait {
            return Err(anyhow!(
                "wait search refresh found no supported discovered native provider or enabled auto history-source plugin sources; rerun the search with --refresh off to use the existing index"
            ));
        }
        return Ok(SearchRefreshReport::skipped(args.refresh, "no_sources"));
    }
    let source_count = sources.len().saturating_add(plugin_sources.len());
    let execution_policy = match args.refresh {
        RefreshArg::Wait => ImportExecutionPolicy::Drain,
        RefreshArg::Background | RefreshArg::Off => ImportExecutionPolicy::Interactive,
    };
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

pub(crate) fn search_refresh_has_retirement_work(data_root: &Path) -> Result<bool> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Ok(false);
    }
    Ok(Store::open(&db_path)?.provider_file_publication_retirement_work_count()? > 0)
}

pub(crate) fn refresh_sources_for_search(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
    execution_policy: ImportExecutionPolicy,
) -> Result<ImportTotals> {
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let mut store = Store::open(&db_path)?;
    let refresh_source_count = sources.len().saturating_add(plugin_sources.len());
    let had_indexed_content = store.indexed_history_item_count()? > 0;
    let search_projection_needs_backfill = store.event_search_projection_needs_backfill()?;
    let inventory = inventory_import_sources(&store, sources, false)?;
    let plan = ImportPlan::build(&store, inventory.sources)?;
    let mut execution_state = crate::commands::import::ImportExecutionState::for_plan(&plan);
    let inventory_failures = inventory.failures;
    let failed_inventory_pending = failed_inventory_pending_counts(&store, &inventory_failures)?;
    let planned_total_bytes = inventory.totals.source_bytes;
    let mut totals = ImportTotals::default();

    let progress_arg = match refresh {
        RefreshArg::Wait if json_output => ProgressArg::Json,
        RefreshArg::Wait => ProgressArg::Auto,
        RefreshArg::Background | RefreshArg::Off => ProgressArg::None,
    };
    let progress = ProgressReporter::new(
        progress_arg,
        json_output,
        "search-refresh",
        planned_total_bytes,
    );
    totals.fresh_units_pending = failed_inventory_pending.0;
    totals.recovery_units_pending = failed_inventory_pending.1;
    let mut first_refresh_failure = None::<String>;
    for failure in inventory_failures {
        first_refresh_failure.get_or_insert_with(|| failure.error.clone());
        totals.add_source_failure(&failure.stats);
        progress.warning(format!(
            "skipped {} during inventory: {}",
            failure.source.provider.as_str(),
            one_line_error(&failure.error)
        ));
    }
    let tolerate_source_errors = refresh == RefreshArg::Background;
    let mut imported_native_sources = BTreeSet::new();
    let mut failed_native_sources = BTreeSet::new();
    let fresh_slice_limit = execution_policy.fresh_slice_limit();
    if let Err(error) = execute_search_refresh_plan_class(
        &mut store,
        &plan,
        &mut execution_state,
        ImportWorkClass::Fresh,
        plan.fresh_units,
        fresh_slice_limit,
        &progress,
        json_output,
        tolerate_source_errors,
        &mut totals,
        &mut first_refresh_failure,
        &mut imported_native_sources,
        &mut failed_native_sources,
        execution_policy == ImportExecutionPolicy::Drain,
    ) {
        return Err(refresh_failure_with_totals(error, &store, &plan, totals));
    }

    if !plugin_sources.is_empty() {
        for plugin_source in plugin_sources {
            progress.message(
                "refreshing",
                format!("running history source plugin {}", plugin_source.label()),
            );
            let import_result =
                import_history_source_plugin(&mut store, &plugin_source, data_root, false)
                    .with_context(|| {
                        format!("refresh history source plugin {}", plugin_source.label())
                    });
            match import_result {
                Ok((summary, stats)) => {
                    warn_on_rejected_records(
                        &progress,
                        json_output,
                        &plugin_source.label(),
                        &summary,
                    );
                    totals.add(&summary, &stats);
                    progress.done(
                        "refreshing",
                        format!("refreshed history source plugin {}", plugin_source.label()),
                        0,
                    );
                }
                Err(err)
                    if refresh == RefreshArg::Background
                        && import_error_scope(&err) == ImportFailureScope::Source =>
                {
                    let error = error_summary(&err);
                    first_refresh_failure.get_or_insert_with(|| error.clone());
                    add_refresh_source_failure(&mut totals, &SourceStats::default(), &err);
                    progress.done(
                        "refreshing",
                        format!(
                            "skipped history source plugin {}: {}",
                            plugin_source.label(),
                            one_line_error(&error)
                        ),
                        0,
                    );
                }
                Err(err) => {
                    return Err(refresh_failure_with_totals(err, &store, &plan, totals));
                }
            }
        }
    }

    let maintenance = match repair_import_maintenance(&store, execution_policy) {
        Ok(maintenance) => maintenance,
        Err(error) => return Err(refresh_failure_with_totals(error, &store, &plan, totals)),
    };
    totals.durable_progress |= maintenance.processed_rows > 0;
    let recovery_slice_limit = execution_policy.recovery_slice_limit();
    let recovery_units = plan.pending_count(&store, ImportWorkClass::Recovery)?;
    if let Err(error) = execute_search_refresh_plan_class(
        &mut store,
        &plan,
        &mut execution_state,
        ImportWorkClass::Recovery,
        recovery_units,
        recovery_slice_limit,
        &progress,
        json_output,
        tolerate_source_errors,
        &mut totals,
        &mut first_refresh_failure,
        &mut imported_native_sources,
        &mut failed_native_sources,
        execution_policy == ImportExecutionPolicy::Drain,
    ) {
        return Err(refresh_failure_with_totals(error, &store, &plan, totals));
    }

    if search_projection_needs_backfill {
        if let Err(error) = store.refresh_search_index() {
            return Err(refresh_failure_with_totals(
                error.into(),
                &store,
                &plan,
                totals,
            ));
        }
    }

    let all_sources_failed = all_refresh_sources_failed(refresh_source_count, &totals);
    let all_rejected_without_prior_index = !had_indexed_content
        && totals.imported_sessions == 0
        && totals.imported_events == 0
        && totals.failed > 0;
    if refresh == RefreshArg::Background && (all_sources_failed || all_rejected_without_prior_index)
    {
        let detail = first_refresh_failure
            .map(|error| format!("; first failure: {error}"))
            .or_else(|| {
                (totals.failed > 0).then(|| {
                    format!(
                        "; background refresh imported no content and reported {} failure(s)",
                        totals.failed
                    )
                })
            })
            .unwrap_or_default();
        return Err(refresh_failure_with_totals(
            anyhow!("all search refresh sources failed{detail}"),
            &store,
            &plan,
            totals,
        ));
    }

    let (fresh_units_pending, recovery_units_pending) = match plan.pending_counts(&store) {
        Ok(counts) => counts,
        Err(error) => {
            return Err(refresh_failure_with_totals(error, &store, &plan, totals));
        }
    };
    totals.fresh_units_pending = fresh_units_pending.saturating_add(failed_inventory_pending.0);
    totals.recovery_units_pending = recovery_units_pending
        .saturating_add(failed_inventory_pending.1)
        .saturating_add(usize::from(!maintenance.complete));

    if let Err(error) = store.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES) {
        return Err(anyhow::Error::new(SearchRefreshFailure {
            error: error.into(),
            totals,
        }));
    }
    Ok(totals)
}

fn all_refresh_sources_failed(source_count: usize, totals: &ImportTotals) -> bool {
    source_count > 0 && totals.failed_sources >= source_count
}

fn refresh_failure_with_totals(
    error: anyhow::Error,
    store: &Store,
    plan: &ImportPlan,
    mut totals: ImportTotals,
) -> anyhow::Error {
    if let Ok((fresh, recovery)) = plan.pending_counts(store) {
        totals.fresh_units_pending = totals.fresh_units_pending.saturating_add(fresh);
        totals.recovery_units_pending = totals.recovery_units_pending.saturating_add(recovery);
    }
    anyhow::Error::new(SearchRefreshFailure { error, totals })
}

#[allow(clippy::too_many_arguments)]
fn execute_search_refresh_plan_class(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut crate::commands::import::ImportExecutionState,
    class: ImportWorkClass,
    remaining_units: usize,
    max_slices: Option<usize>,
    progress: &ProgressReporter,
    json_output: bool,
    tolerate_source_errors: bool,
    totals: &mut ImportTotals,
    first_refresh_failure: &mut Option<String>,
    imported_sources: &mut BTreeSet<usize>,
    failed_sources: &mut BTreeSet<usize>,
    drain_retirements: bool,
) -> Result<crate::commands::import::ImportExecutionResult> {
    execute_search_refresh_plan_class_with_pre_lock_hook(
        store,
        plan,
        execution_state,
        class,
        remaining_units,
        max_slices,
        progress,
        json_output,
        tolerate_source_errors,
        totals,
        first_refresh_failure,
        imported_sources,
        failed_sources,
        drain_retirements,
        || {},
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_search_refresh_plan_class_with_pre_lock_hook(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut crate::commands::import::ImportExecutionState,
    class: ImportWorkClass,
    mut remaining_units: usize,
    max_slices: Option<usize>,
    progress: &ProgressReporter,
    json_output: bool,
    tolerate_source_errors: bool,
    totals: &mut ImportTotals,
    first_refresh_failure: &mut Option<String>,
    imported_sources: &mut BTreeSet<usize>,
    failed_sources: &mut BTreeSet<usize>,
    drain_retirements: bool,
    mut before_bulk_lock: impl FnMut(),
) -> Result<crate::commands::import::ImportExecutionResult> {
    let mut completed_bytes = 0u64;
    let mut completed_slices = 0usize;
    let mut execution_result = crate::commands::import::ImportExecutionResult::default();
    while remaining_units > 0 && max_slices.is_none_or(|limit| completed_slices < limit) {
        let Some(executable) = plan.select_slice_for_execution_with_pre_lock_hook(
            store,
            class,
            remaining_units,
            execution_state,
            &mut before_bulk_lock,
        )?
        else {
            break;
        };
        let ExecutableImportSlice {
            slice,
            bulk_guard,
            validation_failures,
        } = executable;
        if slice.is_empty() && validation_failures.is_empty() {
            store.finish_event_search_bulk_mode(&bulk_guard)?;
            continue;
        }
        let validation_units = validation_failures
            .iter()
            .map(|failure| failure.stats.files)
            .sum::<usize>();
        let selected_units = slice.units.saturating_add(validation_units);
        remaining_units = remaining_units.saturating_sub(selected_units);
        completed_slices += 1;
        let mut system_error = None;
        let mut completed_units = 0usize;
        let mut deferred_units = 0usize;
        let mut maintenance_progress = false;
        let mut source_durable_progress = false;
        for validation_failure in validation_failures {
            if !tolerate_source_errors {
                system_error = Some(validation_failure.error);
                break;
            }
            let source_plan = &plan.sources[validation_failure.source_index];
            let error = error_summary(&validation_failure.error);
            first_refresh_failure.get_or_insert_with(|| error.clone());
            let first_source_result = failed_sources.insert(validation_failure.source_index);
            add_refresh_source_failure(
                totals,
                &validation_failure.stats,
                &validation_failure.error,
            );
            if !first_source_result {
                totals.failed_sources = totals.failed_sources.saturating_sub(1);
            }
            progress.done(
                "refreshing",
                format!(
                    "skipped {}: {}",
                    source_plan.source.provider.as_str(),
                    one_line_error(&error)
                ),
                completed_bytes,
            );
        }
        for retirement in &slice.retirements {
            if system_error.is_some() {
                break;
            }
            execution_state.record_retirement_attempt(retirement);
            progress.message("repairing", "repairing prior hidden provider history");
            match recover_provider_file_publication_retirement(store, retirement, drain_retirements)
            {
                Ok(outcome) => {
                    maintenance_progress |= outcome.made_durable_progress;
                    if outcome.completed {
                        completed_units = completed_units.saturating_add(1);
                    }
                    for warning in outcome.maintenance_warnings {
                        progress.warning(warning.to_string());
                    }
                }
                Err(error) => {
                    system_error = Some(error);
                    break;
                }
            }
        }
        for selected in slice.sources {
            if system_error.is_some() {
                break;
            }
            let source_plan = &plan.sources[selected.source_index];
            let (phase, message) = import_work_progress_message(class, source_plan.source.provider);
            progress.message(phase, message);
            let source_progress =
                progress.codex_import_callback(&source_plan.source, completed_bytes);
            execution_state.record_source_attempt(&selected.work);
            if let Err(error) = selected.persist_attempt_started(store) {
                system_error = Some(error);
                break;
            }
            let import_result = import_selected_source(
                store,
                &source_plan.source,
                source_progress,
                &selected.preinventory,
                &selected.work,
            );
            let (outcome, import_error) = match import_result {
                Ok(result) => (Some(result.outcome), result.remaining_error),
                Err(error) => (None, Some(error)),
            };
            let mut outcome_completed_units = 0usize;
            let mut outcome_completed_bytes = 0u64;
            let mut outcome_deferred_units = 0usize;
            let had_outcome = outcome.is_some();
            if let Some(outcome) = outcome {
                let made_durable_progress = outcome.made_durable_progress();
                execution_state.record_source_outcome(
                    selected.source_index,
                    &selected.work,
                    outcome.post_import_preinventory.clone(),
                );
                source_durable_progress |= made_durable_progress;
                outcome_completed_units = outcome.completed_units;
                outcome_completed_bytes = outcome.completed_bytes;
                outcome_deferred_units = outcome.deferred_units;
                completed_units = completed_units.saturating_add(outcome.completed_units);
                let deferred = outcome.deferred_units;
                deferred_units = deferred_units.saturating_add(deferred);
                warn_on_rejected_records(
                    progress,
                    json_output,
                    source_plan.source.provider.as_str(),
                    &outcome.summary,
                );
                if made_durable_progress {
                    let completed_stats = SourceStats {
                        files: outcome.completed_units,
                        bytes: outcome.completed_bytes,
                        change_token: selected.stats.change_token,
                    };
                    completed_bytes = completed_bytes.saturating_add(completed_stats.bytes);
                    let first_source_result = imported_sources.insert(selected.source_index);
                    totals.add(&outcome.summary, &completed_stats);
                    if !first_source_result {
                        totals.imported_sources = totals.imported_sources.saturating_sub(1);
                    }
                    let (phase, message) = import_work_progress_done(class, &source_plan.source);
                    progress.done(phase, message, completed_bytes);
                } else {
                    progress.done(
                        phase,
                        format!(
                            "Deferred incomplete {} history.",
                            source_plan.source.provider.as_str()
                        ),
                        completed_bytes,
                    );
                }
                if deferred > 0 {
                    progress.warning(format!(
                            "{deferred} {} history unit(s) remain pending until their current write completes.",
                            source_plan.source.provider.as_str()
                        ));
                }
            }
            if let Some(err) = import_error {
                if !had_outcome {
                    execution_state.record_source_outcome(
                        selected.source_index,
                        &selected.work,
                        None,
                    );
                }
                if tolerate_source_errors && import_error_scope(&err) == ImportFailureScope::Source
                {
                    if let Some(warning) = publication_recovery_maintenance_warning(&err) {
                        progress.warning(warning.to_string());
                    }
                    let error = error_summary(&err);
                    first_refresh_failure.get_or_insert_with(|| error.clone());
                    let first_source_result = failed_sources.insert(selected.source_index);
                    let failure_stats = SourceStats {
                        files: selected.stats.files.saturating_sub(
                            outcome_completed_units.saturating_add(outcome_deferred_units),
                        ),
                        bytes: selected.stats.bytes.saturating_sub(outcome_completed_bytes),
                        change_token: selected.stats.change_token,
                    };
                    add_refresh_source_failure(totals, &failure_stats, &err);
                    if !first_source_result {
                        totals.failed_sources = totals.failed_sources.saturating_sub(1);
                    }
                    progress.done(
                        "refreshing",
                        format!(
                            "skipped {}: {}",
                            source_plan.source.provider.as_str(),
                            one_line_error(&error)
                        ),
                        completed_bytes,
                    );
                } else {
                    if let Some(warning) = publication_recovery_maintenance_warning(&err) {
                        progress.warning(warning.to_string());
                    }
                    system_error = Some(err);
                    break;
                }
            }
        }
        store.finish_event_search_bulk_mode(&bulk_guard)?;
        match class {
            ImportWorkClass::Fresh => {
                totals.fresh_units_processed =
                    totals.fresh_units_processed.saturating_add(completed_units);
            }
            ImportWorkClass::Recovery => {
                totals.recovery_units_processed = totals
                    .recovery_units_processed
                    .saturating_add(completed_units);
            }
        }
        execution_result.add_slice(
            selected_units,
            completed_units,
            deferred_units,
            maintenance_progress || source_durable_progress,
        );
        totals.durable_progress |=
            completed_units > 0 || maintenance_progress || source_durable_progress;
        if let Some(error) = system_error {
            return Err(error);
        }
    }
    Ok(execution_result)
}

fn add_refresh_source_failure(
    totals: &mut ImportTotals,
    stats: &SourceStats,
    error: &anyhow::Error,
) {
    if let Some(summary) = rejected_source_summary(error) {
        totals.add_rejected_source(&summary, stats);
    } else {
        totals.add_source_failure(stats);
    }
}

fn warn_on_rejected_records(
    progress: &ProgressReporter,
    json_output: bool,
    source: &str,
    summary: &ProviderImportSummary,
) {
    if summary.failed == 0 {
        return;
    }
    let first_failure = summary
        .failures
        .first()
        .map(|failure| {
            format!(
                "; first failure at line {}: {}",
                failure.line, failure.error
            )
        })
        .unwrap_or_default();
    let warning = format!(
        "refreshed {source} with {} rejected history record(s){first_failure}",
        summary.failed
    );
    if progress.is_enabled() {
        progress.warning(warning);
    } else if !json_output {
        eprintln!("warning: {warning}");
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;
    use crate::commands::import::{run_import_internal, ImportRunOptions};
    use crate::provider_args::NativeProviderArg;
    use crate::provider_sources::explicit_path_source;
    use crate::ImportArgs;
    use ctx_history_core::{utc_now, CaptureProvider};
    use ctx_history_store::{CatalogIndexedStatus, SourceImportFileIndexUpdate};

    fn write_pi_source(root: &Path, count: usize, label: &str) -> SourceInfo {
        fs::create_dir_all(root).unwrap();
        for index in 0..count {
            fs::write(
                root.join(format!("{index:03}.jsonl")),
                format!(
                    "{}\n{}\n",
                    json!({
                        "type": "session",
                        "id": format!("{label}-{index}"),
                        "timestamp": "2026-07-14T12:00:00Z"
                    }),
                    json!({
                        "type": "message",
                        "id": format!("{label}-message-{index}"),
                        "timestamp": "2026-07-14T12:00:01Z",
                        "message": {"role": "user", "content": format!("{label} {index}")}
                    })
                ),
            )
            .unwrap();
        }
        explicit_path_source(CaptureProvider::Pi, root.to_path_buf())
    }

    fn seed_failed_pi_backlog(data_root: &Path, count: usize) -> SourceInfo {
        fs::create_dir_all(data_root).unwrap();
        let source = write_pi_source(&data_root.join("pi-backlog"), count, "recovery");
        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
        let (files, inventory_generation) = match &inventory.sources[0].preinventory {
            crate::commands::import::SourcePreinventory::SourceImportFiles {
                files,
                inventory_generation,
            } => (files, *inventory_generation),
            other => panic!("unexpected Pi inventory: {other:?}"),
        };
        for file in files {
            assert_eq!(
                store
                    .record_source_import_file_result(
                        file.provider,
                        SourceImportFileIndexUpdate {
                            source_root: &file.source_root,
                            source_path: &file.source_path,
                            file_size_bytes: file.file_size_bytes,
                            file_modified_at_ms: file.file_modified_at_ms,
                            import_revision: file.import_revision,
                            inventory_generation,
                            metadata: &file.metadata,
                            indexed_at_ms: utc_now().timestamp_millis(),
                        },
                        CatalogIndexedStatus::Failed,
                        Some("deterministic recovery fixture"),
                    )
                    .unwrap(),
                1
            );
        }
        source
    }

    fn refresh(
        data_root: &Path,
        sources: Vec<SourceInfo>,
        policy: ImportExecutionPolicy,
    ) -> ImportTotals {
        refresh_sources_for_search(
            data_root,
            sources,
            Vec::new(),
            RefreshArg::Background,
            false,
            policy,
        )
        .unwrap()
    }

    #[test]
    fn healthy_no_op_source_prevents_all_sources_failed() {
        let totals = ImportTotals {
            failed_sources: 1,
            ..ImportTotals::default()
        };

        assert!(!all_refresh_sources_failed(2, &totals));
        assert!(all_refresh_sources_failed(1, &totals));
    }

    #[test]
    fn search_refresh_drops_a_completion_that_wins_the_bulk_lock() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        fs::create_dir_all(&data_root).unwrap();
        let db_path = database_path(data_root.clone());
        let source = write_pi_source(&data_root.join("pi-race"), 1, "bulk-winner");
        let mut lock_store = Store::open(&db_path).unwrap();
        let inventory = inventory_import_sources(&lock_store, vec![source.clone()], false).unwrap();
        let waiting_plan = ImportPlan::build(&lock_store, inventory.sources).unwrap();
        assert_eq!(waiting_plan.fresh_units, 1);

        let guard = lock_store.begin_event_search_bulk_mode().unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let waiting_db_path = db_path.clone();
        let waiter = std::thread::spawn(move || {
            let mut waiting_store = Store::open(waiting_db_path).unwrap();
            let progress = ProgressReporter::new(ProgressArg::None, false, "search-refresh", 0);
            let mut totals = ImportTotals::default();
            let mut first_refresh_failure = None;
            let mut imported_sources = BTreeSet::new();
            let mut failed_sources = BTreeSet::new();
            let mut execution_state =
                crate::commands::import::ImportExecutionState::for_plan(&waiting_plan);
            let result = execute_search_refresh_plan_class_with_pre_lock_hook(
                &mut waiting_store,
                &waiting_plan,
                &mut execution_state,
                ImportWorkClass::Fresh,
                waiting_plan.fresh_units,
                None,
                &progress,
                false,
                false,
                &mut totals,
                &mut first_refresh_failure,
                &mut imported_sources,
                &mut failed_sources,
                false,
                || waiting_tx.send(()).unwrap(),
            );
            (
                result,
                totals,
                first_refresh_failure,
                imported_sources,
                failed_sources,
            )
        });
        waiting_rx.recv().unwrap();

        let completion_inventory =
            inventory_import_sources(&lock_store, vec![source], false).unwrap();
        let completion_plan = ImportPlan::build(&lock_store, completion_inventory.sources).unwrap();
        let completion_slice = completion_plan
            .select_slice(
                &lock_store,
                ImportWorkClass::Fresh,
                completion_plan.fresh_units,
            )
            .unwrap();
        let selected = &completion_slice.sources[0];
        let source_plan = &completion_plan.sources[selected.source_index];
        import_selected_source(
            &mut lock_store,
            &source_plan.source,
            None,
            &selected.preinventory,
            &selected.work,
        )
        .unwrap();
        lock_store.finish_event_search_bulk_mode(&guard).unwrap();
        drop(guard);

        let (result, totals, first_failure, imported_sources, failed_sources) =
            waiter.join().unwrap();
        result.unwrap();
        assert_eq!(totals.fresh_units_processed, 0);
        assert_eq!(totals.failed_sources, 0);
        assert!(first_failure.is_none());
        assert!(imported_sources.is_empty());
        assert!(failed_sources.is_empty());
    }

    #[test]
    fn background_refresh_isolates_source_removed_before_locked_revalidation() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        fs::create_dir_all(&data_root).unwrap();
        let source = write_pi_source(&data_root.join("pi-removed"), 1, "removed");
        let source_root = source.path.clone();
        let mut store = Store::open(database_path(data_root)).unwrap();
        let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
        let plan = ImportPlan::build(&store, inventory.sources).unwrap();
        assert_eq!(plan.fresh_units, 1);

        let progress = ProgressReporter::new(ProgressArg::None, false, "search-refresh", 0);
        let mut totals = ImportTotals::default();
        let mut first_refresh_failure = None;
        let mut imported_sources = BTreeSet::new();
        let mut failed_sources = BTreeSet::new();
        let mut execution_state = crate::commands::import::ImportExecutionState::for_plan(&plan);
        let result = execute_search_refresh_plan_class_with_pre_lock_hook(
            &mut store,
            &plan,
            &mut execution_state,
            ImportWorkClass::Fresh,
            plan.fresh_units,
            None,
            &progress,
            false,
            true,
            &mut totals,
            &mut first_refresh_failure,
            &mut imported_sources,
            &mut failed_sources,
            false,
            || fs::remove_dir_all(&source_root).unwrap(),
        )
        .unwrap();

        assert_eq!(result.selected_units, 1);
        assert_eq!(result.completed_units, 0);
        assert_eq!(totals.failed_sources, 1);
        assert!(first_refresh_failure.is_some());
        assert!(imported_sources.is_empty());
        assert_eq!(failed_sources, BTreeSet::from([0]));
        let guard = store.begin_event_search_bulk_mode().unwrap();
        store.finish_event_search_bulk_mode(&guard).unwrap();
    }

    #[test]
    fn repeated_foreground_refreshes_advance_one_recovery_slice() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let backlog = seed_failed_pi_backlog(&data_root, 130);
        let fresh = write_pi_source(&data_root.join("pi-fresh"), 1, "fresh-first");

        let first = refresh(
            &data_root,
            vec![backlog.clone(), fresh.clone()],
            ImportExecutionPolicy::Interactive,
        );
        assert_eq!(first.fresh_units_processed, 1);
        assert_eq!(first.recovery_units_processed, 64);
        assert_eq!(first.recovery_units_pending, 66);

        let second = refresh(
            &data_root,
            vec![backlog.clone(), fresh.clone()],
            ImportExecutionPolicy::Interactive,
        );
        assert_eq!(second.fresh_units_processed, 0);
        assert_eq!(second.recovery_units_processed, 64);
        assert_eq!(second.recovery_units_pending, 2);

        let third = refresh(
            &data_root,
            vec![backlog, fresh],
            ImportExecutionPolicy::Interactive,
        );
        assert_eq!(third.recovery_units_processed, 2);
        assert_eq!(third.recovery_units_pending, 0);
    }

    #[test]
    fn daemon_advances_recovery_even_when_fresh_work_exists() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let backlog = seed_failed_pi_backlog(&data_root, 130);

        let first = refresh(
            &data_root,
            vec![backlog.clone()],
            ImportExecutionPolicy::Daemon,
        );
        assert_eq!(first.recovery_units_processed, 64);
        assert_eq!(first.recovery_units_pending, 66);

        let fresh = write_pi_source(&data_root.join("pi-daemon-fresh"), 1, "daemon-fresh");
        let fresh_cycle = refresh(
            &data_root,
            vec![backlog.clone(), fresh.clone()],
            ImportExecutionPolicy::Daemon,
        );
        assert_eq!(fresh_cycle.fresh_units_processed, 1);
        assert_eq!(fresh_cycle.recovery_units_processed, 64);
        assert_eq!(fresh_cycle.recovery_units_pending, 2);

        let second_recovery = refresh(
            &data_root,
            vec![backlog.clone(), fresh.clone()],
            ImportExecutionPolicy::Daemon,
        );
        assert_eq!(second_recovery.recovery_units_processed, 2);
        assert_eq!(second_recovery.recovery_units_pending, 0);
        let drained = refresh(
            &data_root,
            vec![backlog, fresh],
            ImportExecutionPolicy::Daemon,
        );
        assert_eq!(drained.recovery_units_processed, 0);
        assert_eq!(drained.recovery_units_pending, 0);
    }

    #[test]
    fn setup_operation_drains_all_recovery_work() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let backlog = seed_failed_pi_backlog(&data_root, 130);
        let args = ImportArgs {
            provider: Some(NativeProviderArg::Pi),
            path: Some(backlog.path),
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            format: None,
            all: false,
            resume: false,
            no_daemon: true,
            json: false,
            progress: ProgressArg::None,
        };
        let report = run_import_internal(
            &args,
            data_root,
            &mut serde_json::Map::new(),
            ImportRunOptions {
                progress: ProgressArg::None,
                json: false,
                print_human: false,
                allow_empty_sources: false,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )
        .unwrap();
        assert_eq!(report.totals.recovery_units_processed, 130);
        assert_eq!(report.totals.recovery_units_pending, 0);
    }
}
