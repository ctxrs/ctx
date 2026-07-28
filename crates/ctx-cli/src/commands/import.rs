use std::{
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

use ctx_history_capture::{
    provider_source_spec, CaptureError, CaptureWorkLimit, CatalogSummary, ImportProfile,
    ProviderImportSummary,
};
use ctx_history_core::database_path;
use ctx_history_store::{SourceImportFile, Store, StoreError};

use crate::analytics::{self, ImportTelemetry, ProviderRefreshSourceMode, ProviderRefreshTrigger};
use crate::progress::{format_bytes, format_count, plural, ProgressArg, ProgressReporter};
use crate::provider_sources::SourceInfo;
use crate::{ImportArgs, WAL_TRUNCATE_MIN_BYTES};

mod catalog;
mod cold;
mod entry;
mod explicit;
mod inventory;
mod manifest;
mod native;
mod pro_output;
mod provider_refresh;
mod report;
mod requests;
mod totals;

use catalog::source_uses_incremental_event_search;
use cold::{try_codex_cold_cli_import, CodexColdSeed};
pub(crate) use entry::{
    import_report_analytics_outcome, import_report_failure_type, insert_import_error_analytics,
    insert_import_report_analytics, run_import,
};
use explicit::{large_import_notice, run_explicit_format_import, ExplicitFormatImportContext};
pub(crate) use inventory::{
    inventory_available_sources, inventory_import_sources, ImportInventory,
};
use native::import_one_source_with_profile;
pub(crate) use native::{
    import_one_source_for_background_refresh_with_profile,
    import_one_source_for_search_refresh_with_profile,
};
pub(crate) use pro_output::{
    catch_up_pro_outputs, finish_pro_output_inventory,
    import_custom_history_with_canonical_pro_progression, output_inventory_can_finish,
    prepare_core_for_pro_materialization, progress_canonical_pro_after_core_source_attempt,
    CanonicalProSourceProgression,
};
use pro_output::{complete_pro_output_inventory, ProOutputSelection};
pub(crate) use provider_refresh::{
    ImportSourceFailure, ProviderRefreshCollector, ProviderRefreshResourceObservation,
    ProviderRefreshRuntimeFacts,
};
pub(crate) use report::{
    error_summary, import_error_scope, import_failure_type, import_totals_json, one_line_error,
    source_error_reason,
};
use report::{
    history_source_plugin_failure_json, history_source_plugin_import_json, low_disk_space_warning,
    print_history_source_plugin_failed, print_history_source_plugin_imported, print_source_failed,
    print_source_imported, source_failure_json, source_import_json,
};
pub(crate) use report::{ImportFailureScope, ImportFailureType};
pub(crate) use requests::import_history_source_plugin;
use requests::{history_source_plugin_import_requests, import_requests, validate_import_args};
pub(crate) use totals::ImportTotals;

#[derive(Debug)]
pub(crate) struct ImportReport {
    pub(crate) resume: bool,
    pub(crate) totals: ImportTotals,
    pub(crate) inventory: InventoryTotals,
    pub(crate) catalog: CatalogTotals,
    pub(crate) catalog_sources: Vec<Value>,
    pub(crate) sources: Vec<Value>,
}

impl ImportReport {
    pub(crate) fn empty(resume: bool) -> Self {
        Self {
            resume,
            totals: ImportTotals::default(),
            inventory: InventoryTotals::default(),
            catalog: CatalogTotals::default(),
            catalog_sources: Vec::new(),
            sources: Vec::new(),
        }
    }

    pub(crate) fn resume_mode(&self) -> &'static str {
        resume_mode_name(self.resume)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ImportRunOptions {
    pub(crate) progress: ProgressArg,
    pub(crate) json: bool,
    pub(crate) print_human: bool,
    pub(crate) allow_empty_sources: bool,
    pub(crate) include_history_source_plugins: bool,
    pub(crate) operation: &'static str,
}

pub(crate) fn resume_mode_name(resume: bool) -> &'static str {
    if resume {
        "idempotent_rescan"
    } else {
        "normal_scan"
    }
}

pub(crate) fn provider_summary_has_imported_content(summary: &ProviderImportSummary) -> bool {
    summary.has_accepted_content()
}

pub(crate) fn history_record_exists(store: &Store, record_id: Uuid) -> Result<bool> {
    match store.get_record(record_id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn cleanup_rejected_history_record(
    store: &Store,
    record_id: Uuid,
    existed_before_import: bool,
) -> Result<()> {
    let deleted = store.delete_orphan_record(record_id)?;
    if !deleted && !existed_before_import && history_record_exists(store, record_id)? {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "rejected import left content attached to its history record",
        )));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RejectedSourceError {
    message: String,
    summary: ProviderImportSummary,
}

impl std::fmt::Display for RejectedSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RejectedSourceError {}

pub(crate) fn rejected_source_error(
    message: String,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    anyhow::Error::new(RejectedSourceError {
        message,
        summary: summary.clone(),
    })
}

pub(crate) fn rejected_source_summary(error: &anyhow::Error) -> Option<ProviderImportSummary> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RejectedSourceError>())
        .map(|error| error.summary.clone())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogTotals {
    pub(crate) sources: usize,
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) cataloged_sessions: usize,
    pub(crate) cached_sessions: usize,
    pub(crate) parsed_sessions: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) failed_sessions: usize,
}

impl CatalogTotals {
    pub(crate) fn add(&mut self, summary: &CatalogSummary) {
        self.sources += 1;
        self.source_files += summary.source_files;
        self.source_bytes = self.source_bytes.saturating_add(summary.source_bytes);
        self.cataloged_sessions += summary.cataloged_sessions;
        self.cached_sessions += summary.cached_sessions;
        self.parsed_sessions += summary.parsed_sessions;
        self.skipped_sessions += summary.skipped_sessions;
        self.failed_sessions += summary.failed_sessions;
    }

    fn merge_from(&mut self, other: Self) {
        self.sources = self.sources.saturating_add(other.sources);
        self.source_files = self.source_files.saturating_add(other.source_files);
        self.source_bytes = self.source_bytes.saturating_add(other.source_bytes);
        self.cataloged_sessions = self
            .cataloged_sessions
            .saturating_add(other.cataloged_sessions);
        self.cached_sessions = self.cached_sessions.saturating_add(other.cached_sessions);
        self.parsed_sessions = self.parsed_sessions.saturating_add(other.parsed_sessions);
        self.skipped_sessions = self.skipped_sessions.saturating_add(other.skipped_sessions);
        self.failed_sessions = self.failed_sessions.saturating_add(other.failed_sessions);
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InventoryTotals {
    pub(crate) sources: usize,
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) codex_catalog_sources: usize,
    pub(crate) codex_catalog_sessions: usize,
    pub(crate) source_import_files: usize,
}

impl InventoryTotals {
    fn merge_from(&mut self, other: Self) {
        self.sources = self.sources.saturating_add(other.sources);
        self.source_files = self.source_files.saturating_add(other.source_files);
        self.source_bytes = self.source_bytes.saturating_add(other.source_bytes);
        self.codex_catalog_sources = self
            .codex_catalog_sources
            .saturating_add(other.codex_catalog_sources);
        self.codex_catalog_sessions = self
            .codex_catalog_sessions
            .saturating_add(other.codex_catalog_sessions);
        self.source_import_files = self
            .source_import_files
            .saturating_add(other.source_import_files);
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) enum SourcePreinventory {
    #[default]
    None,
    CodexSessionCatalog(CatalogSummary),
    SourceImportManifest,
    SourceRoot(SourceImportFile),
}

impl SourcePreinventory {
    pub(crate) fn source_root_file(&self) -> Option<&SourceImportFile> {
        match self {
            Self::SourceRoot(file) => Some(file),
            Self::CodexSessionCatalog(_catalog) => None,
            Self::None | Self::SourceImportManifest => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceStats {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) change_token: Option<[u8; 32]>,
}

fn provider_path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        anyhow!(
            "provider transcript paths must be valid UTF-8: {}",
            path.display()
        )
    })
}

fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedImportSource {
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) preinventory: SourcePreinventory,
}

pub(crate) fn run_import_internal(
    args: &ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
    config: &crate::config::AppConfig,
    options: ImportRunOptions,
) -> Result<ImportReport> {
    run_import_internal_with_pro_output(
        args,
        data_root,
        telemetry,
        provider_refreshes,
        refresh_trigger,
        config,
        options,
        ProOutputSelection::Automatic,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_import_internal_with_pro_output(
    args: &ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
    config: &crate::config::AppConfig,
    options: ImportRunOptions,
    pro_output_selection: ProOutputSelection,
) -> Result<ImportReport> {
    let _ = config;
    validate_import_args(args)?;
    fs::create_dir_all(&data_root).map_err(|source| CaptureError::SystemIo {
        operation: "initialize ctx data root",
        source,
    })?;
    let db_path = database_path(data_root.clone());
    // Refuse before a single provider file is read: a superseded projection
    // cannot absorb an import, and the user needs the reason, not a per-source
    // failure buried in a report.
    crate::provider_projection::ensure_native_provider_projection_at(&db_path)?;
    let automatic_pro_output = pro_output_selection.is_automatic();
    let (mut pro_output, require_complete_pro_output) = pro_output_selection.begin(&data_root);
    let prior_route_store =
        if args.input_format.is_none() && args.path.as_deref().is_some_and(|path| !path.exists()) {
            Some(Store::open(&db_path)?)
        } else {
            None
        };
    let mut requests = if args.input_format.is_none() {
        import_requests(args, prior_route_store.as_ref())?
    } else {
        Vec::new()
    };
    drop(prior_route_store);
    let plugin_requests = if args.input_format.is_none() {
        history_source_plugin_import_requests(
            args,
            &data_root,
            options.include_history_source_plugins,
        )?
    } else {
        Vec::new()
    };

    if let Some(format) = args.input_format {
        let store = Store::open(&db_path)?;
        return run_explicit_format_import(ExplicitFormatImportContext {
            args,
            format,
            db_path,
            store,
            telemetry,
            provider_refreshes,
            refresh_trigger,
            options,
            pro_output,
        });
    }

    let cold_seed = try_codex_cold_cli_import(
        args,
        &requests,
        &db_path,
        provider_refreshes,
        refresh_trigger,
        &options,
    )?;
    if let Some(seed) = cold_seed.as_ref() {
        seed.remove_consumed_from(&mut requests);
        if pro_output.is_none() && automatic_pro_output {
            pro_output = crate::pro::ProOutputImport::begin_if_available(&data_root);
        }
        if let Some(output) = pro_output.as_mut() {
            replay_codex_cold_seed_to_pro(&db_path, seed, output);
        }
    }

    let mut store = Store::open(&db_path)?;
    if requests.iter().any(source_uses_incremental_event_search) {
        native::ensure_search_projection_ready_for_provider_import(&store)?;
    }
    let cold_source_count = cold_seed
        .as_ref()
        .map_or(0, |seed| seed.report.inventory.sources);
    let cold_source_bytes = cold_seed
        .as_ref()
        .map_or(0, |seed| seed.report.inventory.source_bytes);
    let mut seed_report = cold_seed
        .map(|seed| seed.report)
        .unwrap_or_else(|| ImportReport::empty(false));
    let mut totals = std::mem::take(&mut seed_report.totals);
    let mut combined_inventory = std::mem::take(&mut seed_report.inventory);
    let mut combined_catalog = std::mem::take(&mut seed_report.catalog);
    let mut combined_catalog_sources = std::mem::take(&mut seed_report.catalog_sources);
    let mut imported_sources = std::mem::take(&mut seed_report.sources);
    let native_source_mode = if args.path.is_some() {
        ProviderRefreshSourceMode::ExplicitPath
    } else {
        ProviderRefreshSourceMode::Discovered
    };
    let output_inventory_discovery_complete = args.provider.is_none() && args.path.is_none();
    let inventory_progress =
        ProgressReporter::new(options.progress, options.json, options.operation, 0);
    if requests.is_empty() && plugin_requests.is_empty() {
        if cold_source_count > 0 {
            telemetry.sources_seen = Some(analytics::count_bucket(cold_source_count as u64));
            telemetry.source_bytes = Some(analytics::bytes_bucket(cold_source_bytes));
            update_terminal_import_telemetry(telemetry, &totals);
            if output_inventory_can_finish(output_inventory_discovery_complete, &totals) {
                complete_pro_output_inventory(
                    pro_output,
                    &inventory_progress,
                    require_complete_pro_output,
                )?;
            } else if require_complete_pro_output {
                bail!("not_materialized: provider output inventory is incomplete");
            }
            return Ok(ImportReport {
                resume: false,
                totals,
                inventory: combined_inventory,
                catalog: combined_catalog,
                catalog_sources: combined_catalog_sources,
                sources: imported_sources,
            });
        }
        if options.allow_empty_sources {
            if output_inventory_discovery_complete {
                complete_pro_output_inventory(
                    pro_output,
                    &inventory_progress,
                    require_complete_pro_output,
                )?;
            } else if require_complete_pro_output {
                bail!("not_materialized: provider output inventory is incomplete");
            }
            return Ok(ImportReport::empty(args.resume));
        }
        return Err(anyhow!(
            "no importable provider history sources found; use --path, --history-source, or run `ctx sources`"
        ));
    }
    inventory_progress.message("inventorying", "Preparing local history...");
    // Explicit single-file and generic paths must reach the native adapter's
    // certified-cursor gate. Pro-enabled runs also requeue unchanged sources so
    // the public parser can observe them and replay from private progress.
    let pro_output_enabled = pro_output.is_some();
    let force_inventory_reindex = args.resume || args.path.is_some() || pro_output_enabled;
    let allow_incremental_codex_catalog =
        pro_output_enabled || (args.path.is_some() && !args.resume);
    let inventory = inventory_import_sources(
        &store,
        requests,
        force_inventory_reindex,
        allow_incremental_codex_catalog,
        args.path.is_some(),
    )
    .context("inventory local history sources")?;
    let planned_sources = inventory.sources;
    let inventory_failures = inventory.failures;
    let output_inventory_discovery_complete =
        output_inventory_discovery_complete && inventory_failures.is_empty();
    combined_inventory.merge_from(inventory.totals);
    combined_catalog.merge_from(inventory.catalog);
    combined_catalog_sources.extend(inventory.catalog_sources);
    let import_profile = pro_output
        .as_ref()
        .map(|output| output.profile().clone())
        .unwrap_or(ImportProfile::CoreOnly);
    let planned_total_bytes = combined_inventory.source_bytes;
    inventory_progress.done(
        "inventorying",
        format!(
            "Found {} history {} ({}).",
            format_count(
                combined_inventory
                    .sources
                    .saturating_add(plugin_requests.len()),
            ),
            plural(
                combined_inventory
                    .sources
                    .saturating_add(plugin_requests.len()),
                "source",
                "sources"
            ),
            format_bytes(planned_total_bytes)
        ),
        planned_total_bytes,
    );
    telemetry.sources_seen = Some(analytics::count_bucket(
        combined_inventory
            .sources
            .saturating_add(plugin_requests.len()) as u64,
    ));
    telemetry.source_bytes = Some(analytics::bytes_bucket(planned_total_bytes));

    let progress = ProgressReporter::new(
        options.progress,
        options.json,
        options.operation,
        planned_total_bytes,
    );
    if let Some(warning) = low_disk_space_warning(&db_path, planned_total_bytes) {
        progress.warning(warning);
    }
    if let Some(notice) = large_import_notice(&planned_sources, planned_total_bytes) {
        progress.notice(notice);
    }

    for failure in inventory_failures {
        totals.add_source_failure(&failure.stats);
        provider_refreshes.record_failure(
            failure.source.provider,
            refresh_trigger,
            native_source_mode,
            &failure.stats,
            None,
        );
        progress.done(
            "inventorying",
            format!(
                "skipped {}: {}",
                failure.source.provider.as_str(),
                source_error_reason(&failure.source, &failure.error)
            ),
            0,
        );
        if options.print_human {
            progress.finish_line();
            print_source_failed(&failure);
        }
        imported_sources.push(source_failure_json(&failure));
    }

    for plugin_source in plugin_requests {
        if options.print_human {
            progress.finish_line();
            println!("importing history source plugin {}", plugin_source.label());
        }
        progress.message(
            "indexing",
            format!("running history source plugin {}", plugin_source.label()),
        );
        let provider_resources = ProviderRefreshResourceObservation::begin();
        let provider_started = Instant::now();
        match import_history_source_plugin(
            &mut store,
            &plugin_source,
            &data_root,
            args.reset_cursor,
            CaptureWorkLimit::Drain,
            &import_profile,
            pro_output.as_mut(),
        ) {
            Ok((summary, stats)) => {
                totals.add(&summary, &stats);
                provider_refreshes.record_success_with_facts(
                    ctx_history_core::CaptureProvider::Custom,
                    refresh_trigger,
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
                    "indexing",
                    format!("imported history source plugin {}", plugin_source.label()),
                    planned_total_bytes,
                );
                if options.print_human {
                    progress.finish_line();
                    print_history_source_plugin_imported(&plugin_source, &summary);
                }
                imported_sources.push(history_source_plugin_import_json(
                    &plugin_source,
                    &stats,
                    &summary,
                ));
            }
            Err(err) => {
                let failure_scope = import_error_scope(&err);
                let failure_type = import_failure_type(&err);
                let rejected_summary = rejected_source_summary(&err);
                let error = error_summary(&err);
                if failure_scope == ImportFailureScope::Source {
                    if let Some(summary) = rejected_summary.as_ref() {
                        totals.add_rejected_source(summary, &SourceStats::default());
                    } else {
                        totals.add_source_failure(&SourceStats::default());
                    }
                    provider_refreshes.record_failure_with_facts(
                        ctx_history_core::CaptureProvider::Custom,
                        refresh_trigger,
                        ProviderRefreshSourceMode::HistorySourcePlugin,
                        &SourceStats::default(),
                        rejected_summary.as_ref(),
                        ProviderRefreshRuntimeFacts::observed_failure(
                            provider_started.elapsed(),
                            failure_scope,
                            failure_type,
                        )
                        .with_resource_observation(provider_resources),
                    );
                    progress.done(
                        "indexing",
                        format!(
                            "skipped history source plugin {}: {}",
                            plugin_source.label(),
                            one_line_error(&error)
                        ),
                        planned_total_bytes,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_history_source_plugin_failed(
                            &plugin_source,
                            &error,
                            rejected_summary.as_ref(),
                        );
                    }
                    imported_sources.push(history_source_plugin_failure_json(
                        &plugin_source,
                        &error,
                        rejected_summary.as_ref(),
                        failure_type,
                    ));
                } else {
                    provider_refreshes.record_failure_with_facts(
                        ctx_history_core::CaptureProvider::Custom,
                        refresh_trigger,
                        ProviderRefreshSourceMode::HistorySourcePlugin,
                        &SourceStats::default(),
                        rejected_summary.as_ref(),
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

    let native_import_requested = cold_source_count > 0 || !planned_sources.is_empty();
    let mut completed_source_bytes = cold_source_bytes;
    for plan in planned_sources {
        if options.print_human {
            progress.finish_line();
            println!(
                "importing {} {} ({} files, {})",
                plan.source.provider.as_str(),
                plan.source.path.display(),
                plan.stats.files,
                format_bytes(plan.stats.bytes)
            );
        }
        let source_progress = progress.codex_import_callback(&plan.source, completed_source_bytes);
        completed_source_bytes = completed_source_bytes.saturating_add(plan.stats.bytes);
        let provider_resources = ProviderRefreshResourceObservation::begin();
        let provider_started = Instant::now();
        let import_result = import_one_source_with_profile(
            &mut store,
            &plan.source,
            source_progress,
            args.resume,
            &plan.preinventory,
            &import_profile,
        );
        progress_canonical_pro_after_core_source_attempt(
            pro_output.as_mut(),
            import_result.as_ref().ok(),
        );
        match import_result {
            Ok(summary) => {
                totals.add(&summary, &plan.stats);
                provider_refreshes.record_success_with_facts(
                    plan.source.provider,
                    refresh_trigger,
                    native_source_mode,
                    &summary,
                    &plan.stats,
                    ProviderRefreshRuntimeFacts::observed_success(
                        provider_started.elapsed(),
                        &summary,
                    )
                    .with_resource_observation(provider_resources),
                );
                progress.done(
                    "indexing",
                    format!("Indexed {}.", source_provider_label(&plan.source)),
                    completed_source_bytes,
                );
                if options.print_human {
                    progress.finish_line();
                    print_source_imported(&plan.source, &summary);
                }
                imported_sources.push(source_import_json(&plan.source, &plan.stats, &summary));
            }
            Err(err) => {
                let failure_scope = import_error_scope(&err);
                let failure_type = import_failure_type(&err);
                let rejected_summary = rejected_source_summary(&err);
                let error = error_summary(&err);
                if failure_scope == ImportFailureScope::Source {
                    let failure = ImportSourceFailure {
                        source: plan.source,
                        stats: plan.stats,
                        error,
                        failure_type,
                        rejected_summary,
                        runtime_facts: Some(
                            ProviderRefreshRuntimeFacts::observed_failure(
                                provider_started.elapsed(),
                                failure_scope,
                                failure_type,
                            )
                            .with_resource_observation(provider_resources),
                        ),
                    };
                    if let Some(summary) = failure.rejected_summary.as_ref() {
                        totals.add_rejected_source(summary, &failure.stats);
                    } else {
                        totals.add_source_failure(&failure.stats);
                    }
                    provider_refreshes.record_import_failure(
                        refresh_trigger,
                        native_source_mode,
                        &failure,
                    );
                    progress.done(
                        "indexing",
                        format!(
                            "skipped {}: {}",
                            failure.source.provider.as_str(),
                            source_error_reason(&failure.source, &failure.error)
                        ),
                        completed_source_bytes,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_source_failed(&failure);
                    }
                    imported_sources.push(source_failure_json(&failure));
                } else {
                    provider_refreshes.record_failure_with_facts(
                        plan.source.provider,
                        refresh_trigger,
                        native_source_mode,
                        &plan.stats,
                        rejected_summary.as_ref(),
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

    progress.message("finalizing", "Checkpointing search database...");
    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;

    if options.print_human {
        progress.finish_line();
    }
    progress.done(
        "finalizing",
        format!(
            "Processed {} source {}.",
            format_count(totals.source_files),
            plural(totals.source_files, "file", "files")
        ),
        totals.source_bytes,
    );
    update_terminal_import_telemetry(telemetry, &totals);
    if output_inventory_can_finish(output_inventory_discovery_complete, &totals) {
        complete_pro_output_inventory(pro_output, &progress, require_complete_pro_output)?;
    } else if require_complete_pro_output {
        bail!("not_materialized: provider output inventory is incomplete");
    }
    Ok(ImportReport {
        resume: args.resume && native_import_requested,
        totals,
        inventory: combined_inventory,
        catalog: combined_catalog,
        catalog_sources: combined_catalog_sources,
        sources: imported_sources,
    })
}

fn replay_codex_cold_seed_to_pro(
    db_path: &Path,
    seed: &CodexColdSeed,
    output: &mut crate::pro::ProOutputImport,
) {
    output.note_core_source_committed();
    let profile = output.replay_only_profile();
    let result = (|| -> Result<()> {
        let mut store = Store::open(db_path)?;
        for source in &seed.consumed_sources {
            import_one_source_with_profile(
                &mut store,
                source,
                None,
                false,
                &SourcePreinventory::None,
                &profile,
            )?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        output.mark_output_replay_behind(&error);
    }
}

fn update_terminal_import_telemetry(telemetry: &mut ImportTelemetry, totals: &ImportTotals) {
    telemetry.source_files = Some(analytics::count_bucket(totals.source_files as u64));
    telemetry.failed_sources = Some(analytics::count_bucket(totals.failed_sources as u64));
    telemetry.sessions_imported = Some(analytics::count_bucket(totals.imported_sessions as u64));
    telemetry.events_imported = Some(analytics::count_bucket(totals.imported_events as u64));
    telemetry.edges_imported = Some(analytics::count_bucket(totals.imported_edges as u64));
    telemetry.skipped = Some(analytics::count_bucket(totals.skipped as u64));
    telemetry.rejected_records = Some(analytics::count_bucket(totals.failed as u64));
}

fn source_provider_label(source: &SourceInfo) -> &'static str {
    provider_source_spec(source.provider)
        .map(|spec| spec.display_name)
        .unwrap_or_else(|| source.provider.as_str())
}
