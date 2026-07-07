use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};

use ctx_history_capture::{discover_provider_sources_for_provider, ProviderSourceStatus};
use ctx_history_core::database_path;
use ctx_history_store::Store;

use crate::analytics::AnalyticsProperties;
use crate::commands::import::{
    error_summary, import_history_source_plugin, import_one_source_without_search_refresh,
    import_totals_json, inventory_import_sources, one_line_error, should_parallelize_import,
    ImportSourceOutcome, ImportTotals, SourceStats,
};
use crate::commands::setup::{
    indexed_history_item_count, insert_db_size_bucket, insert_store_analytics_counts,
};
use crate::history_source_plugins::{
    discover_history_source_plugins, HistorySourcePluginRefresh, HistorySourcePluginSource,
};
use crate::output::{compact_json, print_share_safe_value};
use crate::progress::{ProgressArg, ProgressReporter, SourceProgressSnapshot};
use crate::provider_args::ProviderArg;
use crate::provider_sources::{discovered_sources, home_dir, SourceInfo};
use crate::search_filters::{
    missing_search_intent_error, normalize_source_identity_filters, search_filters,
    search_has_intent, search_no_results_target, SearchFilterInput, SearchIntentInput,
    SourceIdentityFilterArgs, SourceIdentityFilters,
};
use crate::search_render::{print_search_result_compact, print_search_result_verbose, SearchDto};
use crate::store_util::open_existing_store_read_only;
use crate::transcript::shell_quote_arg;
use crate::{analytics, config, SearchArgs, WAL_TRUNCATE_MIN_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RefreshArg {
    Auto,
    Off,
    Strict,
}

impl RefreshArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Strict => "strict",
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
    analytics_properties: &mut AnalyticsProperties,
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
    let refresh = refresh_before_search(&args, &data_root)?;
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
    insert_db_size_bucket(analytics_properties, &db_path);
    if refresh.status == "failed" && args.refresh == RefreshArg::Auto && !had_existing_store {
        return Err(anyhow!(
            "search refresh failed and no existing ctx index is available; run `ctx import` first or retry with `--refresh strict`: {}",
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
    let packet = if uses_composed_terms {
        ctx_history_search::search_packet_terms(&store, &query, &args.term, &options)?
    } else {
        ctx_history_search::search_packet(&store, &query, &options)?
    };
    analytics::insert_duration(
        analytics_properties,
        "query_duration",
        query_started.elapsed(),
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
        print_share_safe_value(SearchDto::packet(
            &store,
            &packet,
            &refresh,
            suggested_next_query,
        ))?;
    } else {
        if refresh.status == "failed" && args.refresh == RefreshArg::Auto {
            if let Some(error) = &refresh.error {
                eprintln!(
                    "warning: search refresh failed; serving existing index; use --refresh strict to fail instead: {error}"
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

fn existing_store_indexed_content(db_path: &Path) -> Option<bool> {
    open_existing_store_read_only(db_path, "ctx search analytics preflight")
        .and_then(|store| indexed_history_item_count(&store))
        .ok()
        .map(|indexed_items| indexed_items > 0)
}

pub(crate) fn refresh_before_search(
    args: &SearchArgs,
    data_root: &Path,
) -> Result<SearchRefreshReport> {
    if args.refresh == RefreshArg::Off {
        return Ok(SearchRefreshReport::skipped(RefreshArg::Off, "skipped"));
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
            Err(err) if args.refresh == RefreshArg::Auto => {
                return Ok(SearchRefreshReport::failed(
                    RefreshArg::Auto,
                    sources.len(),
                    error_summary(&err),
                ));
            }
            Err(err) => return Err(err.context("search refresh failed")),
        };
    if sources.is_empty() && plugin_sources.is_empty() {
        if args.refresh == RefreshArg::Strict {
            return Err(anyhow!(
                "strict search refresh found no supported discovered native provider or enabled auto history-source plugin sources; rerun the search with --refresh off to use the existing index"
            ));
        }
        return Ok(SearchRefreshReport::skipped(args.refresh, "no_sources"));
    }
    let source_count = sources.len().saturating_add(plugin_sources.len());
    match refresh_sources_for_search(data_root, sources, plugin_sources, args.refresh, args.json) {
        Ok(totals) => Ok(SearchRefreshReport::completed(
            args.refresh,
            source_count,
            totals,
        )),
        Err(err) if args.refresh == RefreshArg::Auto => Ok(SearchRefreshReport::failed(
            RefreshArg::Auto,
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

pub(crate) fn refresh_sources_for_search(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
) -> Result<ImportTotals> {
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let store = Store::open(&db_path)?;
    let inventory = inventory_import_sources(&store, sources, false)?;
    let planned_sources = inventory.sources;
    let planned_total_bytes = inventory.totals.source_bytes;
    drop(store);
    if planned_sources.is_empty() && plugin_sources.is_empty() {
        return Ok(ImportTotals::default());
    }

    let progress_arg = match refresh {
        RefreshArg::Strict if json_output => ProgressArg::Json,
        RefreshArg::Strict => ProgressArg::Auto,
        RefreshArg::Auto | RefreshArg::Off => ProgressArg::None,
    };
    let progress = ProgressReporter::new(
        progress_arg,
        json_output,
        "search-refresh",
        planned_total_bytes,
    );
    let mut totals = ImportTotals::default();
    let mut first_refresh_failure = None::<String>;
    if should_parallelize_import(&planned_sources) {
        let source_states = Arc::new(Mutex::new(
            planned_sources
                .iter()
                .map(|plan| SourceProgressSnapshot {
                    completed_bytes: 0,
                    total_bytes: plan.stats.bytes,
                })
                .collect::<Vec<_>>(),
        ));
        let handles = planned_sources
            .into_iter()
            .enumerate()
            .map(|(index, plan)| {
                let db_path = db_path.clone();
                let progress_callback = progress.parallel_codex_import_callback(
                    &plan.source,
                    index,
                    Arc::clone(&source_states),
                );
                thread::spawn(move || -> Result<ImportSourceOutcome> {
                    let mut store = Store::open(&db_path)?;
                    let summary = import_one_source_without_search_refresh(
                        &mut store,
                        &plan.source,
                        progress_callback,
                        false,
                        false,
                        &plan.preinventory,
                    )?;
                    Ok(ImportSourceOutcome {
                        index,
                        source: plan.source,
                        stats: plan.stats,
                        summary,
                    })
                })
            })
            .collect::<Vec<_>>();

        let mut outcomes = Vec::with_capacity(handles.len());
        for handle in handles {
            let result = handle
                .join()
                .map_err(|_| anyhow!("provider import worker panicked"))?;
            match result {
                Ok(outcome) => outcomes.push(outcome),
                Err(err) if refresh == RefreshArg::Auto => {
                    first_refresh_failure.get_or_insert_with(|| error_summary(&err));
                    totals.add_source_failure(&SourceStats::default());
                }
                Err(err) => return Err(err),
            }
        }
        outcomes.sort_by_key(|outcome| outcome.index);
        for outcome in outcomes {
            totals.add(&outcome.summary, &outcome.stats);
        }
    } else {
        let mut store = Store::open(&db_path)?;
        let mut completed_source_bytes = 0u64;
        for plan in planned_sources {
            progress.message(
                "refreshing",
                format!("importing {}", plan.source.provider.as_str()),
            );
            let source_progress =
                progress.codex_import_callback(&plan.source, completed_source_bytes);
            completed_source_bytes = completed_source_bytes.saturating_add(plan.stats.bytes);
            let import_result = import_one_source_without_search_refresh(
                &mut store,
                &plan.source,
                source_progress,
                false,
                false,
                &plan.preinventory,
            );
            match import_result {
                Ok(summary) => {
                    totals.add(&summary, &plan.stats);
                    progress.done(
                        "refreshing",
                        format!("refreshed {}", plan.source.provider.as_str()),
                        completed_source_bytes,
                    );
                }
                Err(err) if refresh == RefreshArg::Auto => {
                    let error = error_summary(&err);
                    first_refresh_failure.get_or_insert_with(|| error.clone());
                    totals.add_source_failure(&plan.stats);
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
                Err(err) => return Err(err),
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
            let import_result =
                import_history_source_plugin(&mut store, &plugin_source, data_root, false, false)
                    .with_context(|| {
                        format!("refresh history source plugin {}", plugin_source.label())
                    });
            match import_result {
                Ok((summary, stats)) => {
                    totals.add(&summary, &stats);
                    progress.done(
                        "refreshing",
                        format!("refreshed history source plugin {}", plugin_source.label()),
                        0,
                    );
                }
                Err(err) if refresh == RefreshArg::Auto => {
                    let error = error_summary(&err);
                    first_refresh_failure.get_or_insert_with(|| error.clone());
                    totals.add_source_failure(&SourceStats::default());
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
                Err(err) => return Err(err),
            }
        }
    }

    if refresh == RefreshArg::Auto && totals.imported_sources == 0 && totals.failed_sources > 0 {
        let detail = first_refresh_failure
            .map(|error| format!("; first failure: {error}"))
            .unwrap_or_default();
        return Err(anyhow!("all search refresh sources failed{detail}"));
    }

    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    Ok(totals)
}
