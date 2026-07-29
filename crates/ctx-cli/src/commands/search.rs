use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};

use ctx_history_capture::{
    discover_provider_sources_for_provider, CaptureWorkLimit, ImportProfile, ProviderImportSummary,
    ProviderSourceStatus,
};
use ctx_history_core::database_path;
use ctx_history_store::Store;

use crate::analytics::{
    count_bucket, duration_bucket, text_length_bucket, ProviderRefreshSourceMode,
    ProviderRefreshTrigger, RefreshStatus, SearchTelemetry,
};
use crate::commands::import::{
    error_summary, finish_pro_output_inventory, import_error_scope, import_history_source_plugin,
    import_one_source_for_background_refresh_with_profile,
    import_one_source_for_search_refresh_with_profile, import_totals_json,
    inventory_import_sources, one_line_error, output_inventory_can_finish,
    progress_canonical_pro_after_core_source_attempt, rejected_source_summary,
    CanonicalProSourceProgression, ImportFailureScope, ImportTotals, ProviderRefreshCollector,
    ProviderRefreshResourceObservation, ProviderRefreshRuntimeFacts, SourceStats,
};
use crate::commands::setup::{
    analytics_preflight, indexed_history_item_count, insert_db_size_bucket,
    insert_store_analytics_counts,
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
use crate::{config, semantic, SearchArgs, SearchBackendArg, WAL_TRUNCATE_MIN_BYTES};

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

    fn failed(mode: RefreshArg, source_count: usize, error: String) -> Self {
        Self {
            mode,
            status: "failed",
            source_count,
            totals: ImportTotals::default(),
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
    telemetry: &mut SearchTelemetry,
    _provider_refreshes: &mut ProviderRefreshCollector,
    _config: &config::AppConfig,
) -> Result<()> {
    crate::commands::source_index::run_search(args, data_root, telemetry)
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

fn existing_store_indexed_content(db_path: &Path) -> Result<bool> {
    open_existing_store_read_only(db_path, "ctx search analytics preflight")
        .and_then(|store| indexed_history_item_count(&store))
        .map(|indexed_items| indexed_items > 0)
}

pub(crate) fn refresh_before_search(
    args: &SearchArgs,
    data_root: &Path,
    provider_refreshes: &mut ProviderRefreshCollector,
    config: &config::AppConfig,
) -> Result<SearchRefreshReport> {
    if args.refresh == RefreshArg::Off {
        return Ok(SearchRefreshReport::skipped(RefreshArg::Off, "skipped"));
    }
    if args.refresh == RefreshArg::Background
        && config.daemon.enabled
        && database_path(data_root.to_path_buf()).exists()
    {
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
                    error_summary(&err),
                ));
            }
            Err(err) => return Err(err.context("search refresh failed")),
        };
    let output_inventory_complete = args.provider.is_none() && source_identity.is_empty();
    if sources.is_empty() && plugin_sources.is_empty() && !output_inventory_complete {
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
        args.format.is_json(),
        provider_refreshes,
        config,
        output_inventory_complete,
    ) {
        Ok(_) if source_count == 0 => Ok(SearchRefreshReport::skipped(args.refresh, "no_sources")),
        Ok(totals) => Ok(SearchRefreshReport::completed(
            args.refresh,
            source_count,
            totals,
        )),
        Err(err) if args.refresh == RefreshArg::Background => Ok(SearchRefreshReport::failed(
            RefreshArg::Background,
            source_count,
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

pub(crate) fn progress_search_refresh_canonical_pro<P: CanonicalProSourceProgression>(
    pro_output: Option<&mut P>,
    successful_summary: Option<&ProviderImportSummary>,
) {
    progress_canonical_pro_after_core_source_attempt(pro_output, successful_summary);
}

pub(crate) fn history_source_plugin_work_limit(refresh: RefreshArg) -> CaptureWorkLimit {
    match refresh {
        RefreshArg::Background => CaptureWorkLimit::OneSafeGroup,
        RefreshArg::Off | RefreshArg::Wait => CaptureWorkLimit::Drain,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_sources_for_search(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
    provider_refreshes: &mut ProviderRefreshCollector,
    config: &config::AppConfig,
    output_inventory_complete: bool,
) -> Result<ImportTotals> {
    let _ = config;
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let store = Store::open(&db_path)?;
    let had_indexed_content = store.indexed_history_item_count()? > 0;
    let mut pro_output = crate::pro::ProOutputImport::begin_if_available(data_root);
    let pro_output_enabled = pro_output.is_some();
    let inventory = inventory_import_sources(
        &store,
        sources,
        pro_output_enabled,
        pro_output_enabled,
        false,
    )?;
    let planned_sources = inventory.sources;
    let inventory_failures = inventory.failures;
    let planned_total_bytes = inventory.totals.source_bytes;
    drop(store);
    let output_inventory_complete = output_inventory_complete && inventory_failures.is_empty();
    let import_profile = pro_output
        .as_ref()
        .map(|output| output.profile().clone())
        .unwrap_or(ImportProfile::CoreOnly);

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
    if planned_sources.is_empty() && inventory_failures.is_empty() && plugin_sources.is_empty() {
        if output_inventory_complete {
            finish_pro_output_inventory(pro_output, &progress);
        }
        return Ok(ImportTotals::default());
    }
    let mut totals = ImportTotals::default();
    let mut refresh_failures = Vec::<String>::new();
    for failure in inventory_failures {
        refresh_failures.push(failure.error.clone());
        totals.add_source_failure(&failure.stats);
        provider_refreshes.record_failure(
            failure.source.provider,
            ProviderRefreshTrigger::Search,
            ProviderRefreshSourceMode::Discovered,
            &failure.stats,
            None,
        );
        progress.warning(format!(
            "skipped {} during inventory: {}",
            failure.source.provider.as_str(),
            one_line_error(&failure.error)
        ));
    }
    let mut store = Store::open(&db_path)?;
    let mut completed_source_bytes = 0u64;
    for plan in planned_sources {
        progress.message(
            "refreshing",
            format!("importing {}", plan.source.provider.as_str()),
        );
        let source_progress = progress.codex_import_callback(&plan.source, completed_source_bytes);
        completed_source_bytes = completed_source_bytes.saturating_add(plan.stats.bytes);
        let provider_resources = ProviderRefreshResourceObservation::begin();
        let provider_started = Instant::now();
        let import_result = if refresh == RefreshArg::Background {
            import_one_source_for_background_refresh_with_profile(
                &mut store,
                &plan.source,
                source_progress,
                &plan.preinventory,
                &import_profile,
            )
        } else {
            import_one_source_for_search_refresh_with_profile(
                &mut store,
                &plan.source,
                source_progress,
                &plan.preinventory,
                &import_profile,
            )
        };
        progress_search_refresh_canonical_pro(pro_output.as_mut(), import_result.as_ref().ok());
        match import_result {
            Ok(summary) => {
                warn_on_rejected_records(
                    &progress,
                    json_output,
                    plan.source.provider.as_str(),
                    &summary,
                );
                totals.add(&summary, &plan.stats);
                provider_refreshes.record_success_with_facts(
                    plan.source.provider,
                    ProviderRefreshTrigger::Search,
                    ProviderRefreshSourceMode::Discovered,
                    &summary,
                    &plan.stats,
                    ProviderRefreshRuntimeFacts::observed_success(
                        provider_started.elapsed(),
                        &summary,
                    )
                    .with_resource_observation(provider_resources),
                );
                progress.done(
                    "refreshing",
                    format!("refreshed {}", plan.source.provider.as_str()),
                    completed_source_bytes,
                );
            }
            Err(err) if import_error_scope(&err) == ImportFailureScope::Source => {
                let error = error_summary(&err);
                refresh_failures.push(error.clone());
                add_refresh_source_failure(&mut totals, &plan.stats, &err);
                let failure_scope = import_error_scope(&err);
                let failure_type = crate::commands::import::import_failure_type(&err);
                provider_refreshes.record_failure_with_facts(
                    plan.source.provider,
                    ProviderRefreshTrigger::Search,
                    ProviderRefreshSourceMode::Discovered,
                    &plan.stats,
                    rejected_source_summary(&err).as_ref(),
                    ProviderRefreshRuntimeFacts::observed_failure(
                        provider_started.elapsed(),
                        failure_scope,
                        failure_type,
                    )
                    .with_resource_observation(provider_resources),
                );
                progress.done(
                    "refreshing",
                    format!(
                        "skipped {}: {}",
                        plan.source.provider.as_str(),
                        one_line_error(&error)
                    ),
                    completed_source_bytes,
                );
            }
            Err(err) => {
                let failure_scope = import_error_scope(&err);
                let failure_type = crate::commands::import::import_failure_type(&err);
                provider_refreshes.record_failure_with_facts(
                    plan.source.provider,
                    ProviderRefreshTrigger::Search,
                    ProviderRefreshSourceMode::Discovered,
                    &plan.stats,
                    rejected_source_summary(&err).as_ref(),
                    ProviderRefreshRuntimeFacts::observed_failure(
                        provider_started.elapsed(),
                        failure_scope,
                        failure_type,
                    )
                    .with_resource_observation(provider_resources),
                );
                return Err(err);
            }
        }
    }

    if !plugin_sources.is_empty() {
        let mut store = Store::open(&db_path)?;
        for plugin_source in plugin_sources {
            progress.message(
                "refreshing",
                format!("running history source plugin {}", plugin_source.label()),
            );
            let provider_resources = ProviderRefreshResourceObservation::begin();
            let provider_started = Instant::now();
            let import_result = import_history_source_plugin(
                &mut store,
                &plugin_source,
                data_root,
                false,
                history_source_plugin_work_limit(refresh),
                &import_profile,
                pro_output.as_mut(),
            )
            .with_context(|| format!("refresh history source plugin {}", plugin_source.label()));
            match import_result {
                Ok((summary, stats)) => {
                    warn_on_rejected_records(
                        &progress,
                        json_output,
                        &plugin_source.label(),
                        &summary,
                    );
                    totals.add(&summary, &stats);
                    provider_refreshes.record_success_with_facts(
                        ctx_history_core::CaptureProvider::Custom,
                        ProviderRefreshTrigger::Search,
                        ProviderRefreshSourceMode::HistorySourcePlugin,
                        &summary,
                        &stats,
                        ProviderRefreshRuntimeFacts::observed_success(
                            provider_started.elapsed(),
                            &summary,
                        )
                        .with_resource_observation(provider_resources),
                    );
                    progress.done(
                        "refreshing",
                        format!("refreshed history source plugin {}", plugin_source.label()),
                        0,
                    );
                }
                Err(err) if import_error_scope(&err) == ImportFailureScope::Source => {
                    let error = error_summary(&err);
                    refresh_failures.push(error.clone());
                    add_refresh_source_failure(&mut totals, &SourceStats::default(), &err);
                    let failure_scope = import_error_scope(&err);
                    let failure_type = crate::commands::import::import_failure_type(&err);
                    provider_refreshes.record_failure_with_facts(
                        ctx_history_core::CaptureProvider::Custom,
                        ProviderRefreshTrigger::Search,
                        ProviderRefreshSourceMode::HistorySourcePlugin,
                        &SourceStats::default(),
                        rejected_source_summary(&err).as_ref(),
                        ProviderRefreshRuntimeFacts::observed_failure(
                            provider_started.elapsed(),
                            failure_scope,
                            failure_type,
                        )
                        .with_resource_observation(provider_resources),
                    );
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
                    let failure_scope = import_error_scope(&err);
                    let failure_type = crate::commands::import::import_failure_type(&err);
                    provider_refreshes.record_failure_with_facts(
                        ctx_history_core::CaptureProvider::Custom,
                        ProviderRefreshTrigger::Search,
                        ProviderRefreshSourceMode::HistorySourcePlugin,
                        &SourceStats::default(),
                        rejected_source_summary(&err).as_ref(),
                        ProviderRefreshRuntimeFacts::observed_failure(
                            provider_started.elapsed(),
                            failure_scope,
                            failure_type,
                        )
                        .with_resource_observation(provider_resources),
                    );
                    return Err(err);
                }
            }
        }
    }

    let all_sources_failed = totals.imported_sources == 0 && totals.failed_sources > 0;
    let all_rejected_without_prior_index = !had_indexed_content
        && totals.imported_sessions == 0
        && totals.imported_events == 0
        && totals.failed > 0;
    if refresh == RefreshArg::Background
        && !totals.capture_work_remaining
        && (all_sources_failed || all_rejected_without_prior_index)
    {
        let detail = refresh_failures
            .first()
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
        return Err(anyhow!("all search refresh sources failed{detail}"));
    }

    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    if refresh == RefreshArg::Wait && !refresh_failures.is_empty() {
        let failure_count = refresh_failures.len();
        let failures = refresh_failures
            .iter()
            .enumerate()
            .map(|(index, error)| format!("{}. {}", index + 1, one_line_error(error)))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(anyhow!(
            "{failure_count} search refresh source failure(s) after attempting all planned sources: {failures}"
        ));
    }
    if output_inventory_can_finish(output_inventory_complete, &totals) {
        finish_pro_output_inventory(pro_output, &progress);
    }
    Ok(totals)
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
mod canonical_pro_progression_tests;
