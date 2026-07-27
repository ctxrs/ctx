use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

use ctx_history_capture::{
    provider_source_spec, CaptureError, CatalogSummary, ImportProfile, ProviderImportSummary,
    ProviderImportWorkResult,
};
use ctx_history_core::database_path;
use ctx_history_store::{SourceImportFile, Store, StoreError};

use crate::analytics::{
    self, ImportFailureScope as AnalyticsImportFailureScope,
    ImportFailureType as AnalyticsImportFailureType, ImportOutcome as AnalyticsImportOutcome,
    ImportTelemetry, ProviderRefreshSourceMode, ProviderRefreshTrigger,
};
use crate::progress::{
    format_bytes, format_count, plural, ProgressArg, ProgressReporter, SourceProgressSnapshot,
};
use crate::provider_sources::SourceInfo;
use crate::{ImportArgs, WAL_TRUNCATE_MIN_BYTES};

mod catalog;
mod explicit;
mod inventory;
mod manifest;
mod native;
mod provider_refresh;
mod report;
mod requests;
mod totals;

use catalog::source_uses_incremental_event_search;
use explicit::{large_import_notice, run_explicit_format_import, ExplicitFormatImportContext};
pub(crate) use inventory::{
    inventory_available_sources, inventory_import_sources, ImportInventory,
};
use native::import_one_source_with_profile;
pub(crate) use native::{
    import_one_source_for_background_refresh_with_profile,
    import_one_source_for_search_refresh_with_profile,
};
pub(crate) use provider_refresh::{
    ImportSourceFailure, ImportSourceOutcome, ImportSourceRun, ProviderRefreshCollector,
    ProviderRefreshRuntimeFacts,
};
pub(crate) use report::{
    error_summary, import_error_scope, import_failure_type, import_totals_json, one_line_error,
    source_error_reason,
};
use report::{
    history_source_plugin_failure_json, history_source_plugin_import_json, low_disk_space_warning,
    print_history_source_plugin_failed, print_history_source_plugin_imported, print_import_report,
    print_source_failed, print_source_imported, source_failure_json, source_import_json,
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

enum ProOutputSelection {
    Automatic,
    Disabled,
    Connected(crate::pro::ProOutputImport),
}

impl ProOutputSelection {
    fn begin(self, data_root: &Path) -> (Option<crate::pro::ProOutputImport>, bool) {
        match self {
            Self::Automatic => (
                crate::pro::ProOutputImport::begin_if_available(data_root),
                false,
            ),
            Self::Disabled => (None, false),
            Self::Connected(output) => (Some(output), true),
        }
    }
}

pub(crate) trait CanonicalProSourceProgression {
    fn progress_to_committed_core_frontier(&mut self);
}

impl CanonicalProSourceProgression for crate::pro::ProOutputImport {
    fn progress_to_committed_core_frontier(&mut self) {
        crate::pro::ProOutputImport::note_core_source_committed(self);
    }
}

/// Advances canonical Pro after a Core source attempt that either reported a
/// committed change or returned an error after possibly committing one or more
/// NativePath pages. A successful no-op cannot have advanced the journal and is
/// skipped. Output-Pro remains independently replayable.
pub(crate) fn progress_canonical_pro_after_core_source_attempt<P: CanonicalProSourceProgression>(
    pro_output: Option<&mut P>,
    successful_summary: Option<&ProviderImportSummary>,
) {
    if successful_summary
        .is_some_and(|summary| summary.work_result() != ProviderImportWorkResult::Changed)
    {
        return;
    }
    if let Some(pro_output) = pro_output {
        pro_output.progress_to_committed_core_frontier();
    }
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

#[derive(Debug, Clone, Default)]
pub(crate) enum SourcePreinventory {
    #[default]
    None,
    CodexSessionCatalog(CatalogSummary),
    SourceImportManifest,
    SourceRoot(SourceImportFile),
}

impl SourcePreinventory {
    pub(crate) fn codex_session_catalog(&self) -> Option<&CatalogSummary> {
        match self {
            Self::CodexSessionCatalog(summary) => Some(summary),
            Self::None | Self::SourceImportManifest | Self::SourceRoot(_) => None,
        }
    }

    pub(crate) fn source_root_file(&self) -> Option<&SourceImportFile> {
        match self {
            Self::SourceRoot(file) => Some(file),
            Self::None | Self::CodexSessionCatalog(_) | Self::SourceImportManifest => None,
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

pub(crate) fn run_import(
    args: ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    config: &crate::config::AppConfig,
) -> Result<()> {
    if args.partial {
        eprintln!(
            "warning: --partial is deprecated and no longer changes import behavior; tolerant import is now unconditional"
        );
    }
    let json = args.json;
    let progress = args.progress;
    provider_refreshes.start_timing();
    let report = run_import_internal(
        &args,
        data_root,
        telemetry,
        provider_refreshes,
        ProviderRefreshTrigger::Import,
        config,
        ImportRunOptions {
            progress,
            json,
            print_human: !json,
            allow_empty_sources: false,
            include_history_source_plugins: true,
            operation: "import",
        },
    );
    provider_refreshes.stop_timing();
    let report = match report {
        Ok(report) => report,
        Err(err) => {
            insert_import_error_analytics(telemetry, &err);
            return Err(err);
        }
    };
    insert_import_report_analytics(telemetry, &report);
    let (outcome, _) = import_report_analytics_outcome(&report.totals);
    print_import_report(&report, json)?;
    if outcome == "failure" {
        let detail = report
            .sources
            .iter()
            .find_map(|source| source.get("error").and_then(Value::as_str))
            .map(|error| format!("; first failure: {error}"))
            .unwrap_or_default();
        return Err(anyhow!("all import sources failed{detail}"));
    }
    Ok(())
}

pub(crate) fn insert_import_report_analytics(
    telemetry: &mut ImportTelemetry,
    report: &ImportReport,
) {
    let (outcome, failure_scope) = import_report_analytics_outcome(&report.totals);
    telemetry.outcome = Some(match outcome {
        "success" => AnalyticsImportOutcome::Success,
        "failure" => AnalyticsImportOutcome::Failure,
        "completed_with_rejections" => AnalyticsImportOutcome::CompletedWithRejections,
        "completed_with_source_failures" => AnalyticsImportOutcome::CompletedWithSourceFailures,
        _ => AnalyticsImportOutcome::CompletedWithRejectionsAndSourceFailures,
    });
    telemetry.failure_scope = Some(match failure_scope {
        "none" => AnalyticsImportFailureScope::None,
        "record" => AnalyticsImportFailureScope::Record,
        "source" => AnalyticsImportFailureScope::Source,
        _ => AnalyticsImportFailureScope::RecordAndSource,
    });
    telemetry.failure_type = Some(match import_report_failure_type(&report.totals) {
        "none" => AnalyticsImportFailureType::None,
        "record_rejection" => AnalyticsImportFailureType::RecordRejection,
        "source_failure" => AnalyticsImportFailureType::SourceFailure,
        _ => AnalyticsImportFailureType::RecordRejectionAndSourceFailure,
    });
}

pub(crate) fn insert_import_error_analytics(
    telemetry: &mut ImportTelemetry,
    error: &anyhow::Error,
) {
    telemetry.outcome = Some(AnalyticsImportOutcome::Failure);
    telemetry.failure_scope = Some(match import_error_scope(error).as_str() {
        "record" => AnalyticsImportFailureScope::Record,
        "source" => AnalyticsImportFailureScope::Source,
        "record_and_source" => AnalyticsImportFailureScope::RecordAndSource,
        _ => AnalyticsImportFailureScope::Invocation,
    });
    telemetry.failure_type = Some(match import_failure_type(error).as_str() {
        "invalid_request" => AnalyticsImportFailureType::InvalidRequest,
        "store" => AnalyticsImportFailureType::Store,
        "io" => AnalyticsImportFailureType::Io,
        _ => AnalyticsImportFailureType::Other,
    });
}

pub(crate) fn import_report_analytics_outcome(
    totals: &ImportTotals,
) -> (&'static str, &'static str) {
    if totals.imported_sources == 0 && totals.failed_sources > 0 {
        return ("failure", "source");
    }
    match (totals.failed_sources > 0, totals.failed > 0) {
        (false, false) => ("success", "none"),
        (false, true) => ("completed_with_rejections", "record"),
        (true, false) => ("completed_with_source_failures", "source"),
        (true, true) => (
            "completed_with_rejections_and_source_failures",
            "record_and_source",
        ),
    }
}

pub(crate) fn import_report_failure_type(totals: &ImportTotals) -> &'static str {
    match (totals.failed_sources > 0, totals.failed > 0) {
        (false, false) => "none",
        (false, true) => "record_rejection",
        (true, false) => "source_failure",
        (true, true) => "record_rejection_and_source_failure",
    }
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
    let mut store = Store::open(&db_path)?;
    let mut totals = ImportTotals::default();
    let mut imported_sources = Vec::new();
    let native_source_mode = if args.path.is_some() {
        ProviderRefreshSourceMode::ExplicitPath
    } else {
        ProviderRefreshSourceMode::Discovered
    };

    if let Some(format) = args.format {
        return run_explicit_format_import(ExplicitFormatImportContext {
            args,
            format,
            db_path,
            store,
            telemetry,
            provider_refreshes,
            refresh_trigger,
            options,
        });
    }

    let (mut pro_output, require_complete_pro_output) = pro_output_selection.begin(&data_root);
    let requests = import_requests(args)?;
    let plugin_requests = history_source_plugin_import_requests(
        args,
        &data_root,
        options.include_history_source_plugins,
    )?;
    let output_inventory_discovery_complete = args.provider.is_none() && args.path.is_none();
    let inventory_progress =
        ProgressReporter::new(options.progress, options.json, options.operation, 0);
    if requests.is_empty() && plugin_requests.is_empty() {
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
    )
    .context("inventory local history sources")?;
    let planned_sources = inventory.sources;
    let inventory_failures = inventory.failures;
    let output_inventory_discovery_complete =
        output_inventory_discovery_complete && inventory_failures.is_empty();
    let import_profile = pro_output
        .as_ref()
        .map(|output| output.profile().clone())
        .unwrap_or(ImportProfile::CoreOnly);
    let planned_total_bytes = inventory.totals.source_bytes;
    inventory_progress.done(
        "inventorying",
        format!(
            "Found {} history {} ({}).",
            format_count(
                planned_sources
                    .len()
                    .saturating_add(inventory_failures.len())
                    .saturating_add(plugin_requests.len()),
            ),
            plural(
                planned_sources
                    .len()
                    .saturating_add(inventory_failures.len())
                    .saturating_add(plugin_requests.len()),
                "source",
                "sources"
            ),
            format_bytes(planned_total_bytes)
        ),
        planned_total_bytes,
    );
    telemetry.sources_seen = Some(analytics::count_bucket(
        planned_sources
            .len()
            .saturating_add(inventory_failures.len())
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
        let provider_started = Instant::now();
        match import_history_source_plugin(
            &mut store,
            &plugin_source,
            &data_root,
            args.reset_cursor,
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
                    ),
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
                        ),
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
                        ),
                    );
                    return Err(err);
                }
            }
        }
    }

    let native_import_requested = !planned_sources.is_empty();
    if should_parallelize_import(&planned_sources) {
        let final_refresh_required = store.event_search_projection_needs_backfill()?
            || planned_sources
                .iter()
                .any(|plan| !source_uses_incremental_event_search(&plan.source));
        drop(store);

        if options.print_human {
            progress.finish_line();
            println!("sources:");
            for plan in &planned_sources {
                println!(
                    "  {} {} ({} files, {})",
                    plan.source.provider.as_str(),
                    plan.source.path.display(),
                    plan.stats.files,
                    format_bytes(plan.stats.bytes)
                );
            }
        }

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
                let full_rescan = args.resume;
                let join_source = plan.source.clone();
                let join_stats = plan.stats;
                let failure_source = plan.source.clone();
                let import_profile = import_profile.clone();
                let handle = thread::spawn(move || -> ImportSourceRun {
                    let provider_started = Instant::now();
                    let result = (|| -> Result<ProviderImportSummary> {
                        let mut store = Store::open(&db_path)?;
                        native::import_one_source_without_search_refresh_with_profile(
                            &mut store,
                            &plan.source,
                            progress_callback,
                            full_rescan,
                            &plan.preinventory,
                            &import_profile,
                        )
                        .with_context(|| {
                            format!(
                                "import {} source {}",
                                plan.source.provider.as_str(),
                                plan.source.path.display()
                            )
                        })
                    })();
                    match result {
                        Ok(summary) => ImportSourceRun::Imported(ImportSourceOutcome {
                            index,
                            source: plan.source,
                            stats: plan.stats,
                            runtime_facts: Some(ProviderRefreshRuntimeFacts::observed_success(
                                provider_started.elapsed(),
                                &summary,
                            )),
                            summary,
                        }),
                        Err(err) => {
                            let failure_scope = import_error_scope(&err);
                            let failure_type = import_failure_type(&err);
                            let rejected_summary = rejected_source_summary(&err);
                            let error = error_summary(&err);
                            let system_error =
                                (failure_scope == ImportFailureScope::System).then_some(err);
                            ImportSourceRun::Failed(ImportSourceFailure {
                                index,
                                source: failure_source,
                                stats: join_stats,
                                error,
                                failure_scope,
                                failure_type,
                                rejected_summary,
                                system_error,
                                runtime_facts: Some(ProviderRefreshRuntimeFacts::observed_failure(
                                    provider_started.elapsed(),
                                    failure_scope,
                                    failure_type,
                                )),
                            })
                        }
                    }
                });
                (index, join_source, join_stats, handle)
            })
            .collect::<Vec<_>>();

        let mut runs = Vec::with_capacity(handles.len());
        let mut first_error = None;
        for (index, source, stats, handle) in handles {
            let run = match handle.join() {
                Ok(ImportSourceRun::Imported(outcome)) => ImportSourceRun::Imported(outcome),
                Ok(ImportSourceRun::Failed(mut failure)) => {
                    if failure.failure_scope == ImportFailureScope::System {
                        first_error.get_or_insert_with(|| {
                            failure.system_error.take().unwrap_or_else(|| {
                                anyhow!(
                                    "import {} source {}: {}",
                                    failure.source.provider.as_str(),
                                    failure.source.path.display(),
                                    failure.error
                                )
                            })
                        });
                    }
                    ImportSourceRun::Failed(failure)
                }
                Err(_) => {
                    let panic_error =
                        anyhow::Error::new(CaptureError::WorkerPanicked("provider import"));
                    let failure = ImportSourceFailure {
                        index,
                        source,
                        stats,
                        error: error_summary(&panic_error),
                        failure_scope: ImportFailureScope::System,
                        failure_type: ImportFailureType::WorkerPanic,
                        rejected_summary: None,
                        system_error: Some(panic_error),
                        runtime_facts: None,
                    };
                    first_error.get_or_insert_with(|| {
                        anyhow::Error::new(CaptureError::WorkerPanicked("provider import"))
                    });
                    ImportSourceRun::Failed(failure)
                }
            };
            let successful_summary = match &run {
                ImportSourceRun::Imported(outcome) => Some(&outcome.summary),
                ImportSourceRun::Failed(_) => None,
            };
            progress_canonical_pro_after_core_source_attempt(
                pro_output.as_mut(),
                successful_summary,
            );
            runs.push(run);
        }
        if let Some(err) = first_error {
            for run in &runs {
                match run {
                    ImportSourceRun::Imported(outcome) => provider_refreshes.record_import_outcome(
                        refresh_trigger,
                        native_source_mode,
                        outcome,
                    ),
                    ImportSourceRun::Failed(failure) => provider_refreshes.record_import_failure(
                        refresh_trigger,
                        native_source_mode,
                        failure,
                    ),
                }
            }
            return Err(err);
        }

        runs.sort_by_key(ImportSourceRun::index);
        for run in runs {
            match run {
                ImportSourceRun::Imported(outcome) => {
                    totals.add(&outcome.summary, &outcome.stats);
                    provider_refreshes.record_import_outcome(
                        refresh_trigger,
                        native_source_mode,
                        &outcome,
                    );
                    progress.parallel_source_done(
                        &outcome.source,
                        outcome.index,
                        &source_states,
                        outcome.stats,
                        &outcome.summary,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_source_imported(&outcome.source, &outcome.summary);
                    }
                    imported_sources.push(source_import_json(
                        &outcome.source,
                        &outcome.stats,
                        &outcome.summary,
                    ));
                }
                ImportSourceRun::Failed(failure) => {
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
                    progress.parallel_source_failed(
                        &failure.source,
                        failure.index,
                        &source_states,
                        failure.stats,
                        &failure.error,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_source_failed(&failure);
                    }
                    imported_sources.push(source_failure_json(&failure));
                }
            }
        }

        if final_refresh_required {
            progress.message("finalizing", "Refreshing search index...");
            let store = Store::open(&db_path)?;
            store.refresh_search_index()?;
        }
    } else {
        let mut completed_source_bytes = 0u64;
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
            let source_progress =
                progress.codex_import_callback(&plan.source, completed_source_bytes);
            completed_source_bytes = completed_source_bytes.saturating_add(plan.stats.bytes);
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
                        ),
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
                            index: imported_sources.len(),
                            source: plan.source,
                            stats: plan.stats,
                            error,
                            failure_scope,
                            failure_type,
                            rejected_summary,
                            system_error: None,
                            runtime_facts: Some(ProviderRefreshRuntimeFacts::observed_failure(
                                provider_started.elapsed(),
                                failure_scope,
                                failure_type,
                            )),
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
                            ),
                        );
                        return Err(err);
                    }
                }
            }
        }
    }

    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "Optimizing search index...");
        Store::open(&db_path)?.optimize_search_index()?;
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
    telemetry.source_files = Some(analytics::count_bucket(totals.source_files as u64));
    telemetry.failed_sources = Some(analytics::count_bucket(totals.failed_sources as u64));
    telemetry.sessions_imported = Some(analytics::count_bucket(totals.imported_sessions as u64));
    telemetry.events_imported = Some(analytics::count_bucket(totals.imported_events as u64));
    telemetry.edges_imported = Some(analytics::count_bucket(totals.imported_edges as u64));
    telemetry.skipped = Some(analytics::count_bucket(totals.skipped as u64));
    telemetry.rejected_records = Some(analytics::count_bucket(totals.failed as u64));
    if output_inventory_can_finish(output_inventory_discovery_complete, &totals) {
        complete_pro_output_inventory(pro_output, &progress, require_complete_pro_output)?;
    } else if require_complete_pro_output {
        bail!("not_materialized: provider output inventory is incomplete");
    }
    Ok(ImportReport {
        resume: args.resume && native_import_requested,
        totals,
        inventory: inventory.totals,
        catalog: inventory.catalog,
        catalog_sources: inventory.catalog_sources,
        sources: imported_sources,
    })
}

pub(crate) fn prepare_core_for_pro_materialization(data_root: &Path) -> Result<()> {
    run_pro_materialization_import(data_root, ProOutputSelection::Disabled)
}

pub(crate) fn catch_up_pro_outputs(
    data_root: &Path,
    output: crate::pro::ProOutputImport,
) -> Result<()> {
    run_pro_materialization_import(data_root, ProOutputSelection::Connected(output))
}

fn run_pro_materialization_import(
    data_root: &Path,
    pro_output_selection: ProOutputSelection,
) -> Result<()> {
    let args = ImportArgs {
        provider: None,
        path: None,
        history_source: None,
        history_source_manifest: Vec::new(),
        reset_cursor: false,
        format: None,
        all: true,
        resume: false,
        partial: false,
        no_daemon: true,
        json: false,
        progress: ProgressArg::None,
    };
    let config = crate::config::AppConfig::load(data_root)?;
    let mut telemetry = ImportTelemetry::from_args(&args);
    let mut provider_refreshes = ProviderRefreshCollector::default();
    run_import_internal_with_pro_output(
        &args,
        data_root.to_path_buf(),
        &mut telemetry,
        &mut provider_refreshes,
        ProviderRefreshTrigger::Setup,
        &config,
        ImportRunOptions {
            progress: ProgressArg::None,
            json: false,
            print_human: false,
            allow_empty_sources: true,
            include_history_source_plugins: false,
            operation: "pro-materialization",
        },
        pro_output_selection,
    )
    .map(|_| ())
}

fn source_provider_label(source: &SourceInfo) -> &'static str {
    provider_source_spec(source.provider)
        .map(|spec| spec.display_name)
        .unwrap_or_else(|| source.provider.as_str())
}

pub(crate) fn should_parallelize_import(planned_sources: &[PlannedImportSource]) -> bool {
    let _ = planned_sources;
    false
}

pub(crate) fn output_inventory_can_finish(discovery_complete: bool, totals: &ImportTotals) -> bool {
    discovery_complete && totals.failed_sources == 0 && !totals.capture_work_remaining
}

pub(crate) fn finish_pro_output_inventory(
    output: Option<crate::pro::ProOutputImport>,
    progress: &ProgressReporter,
) {
    let Some(output) = output else {
        return;
    };
    if let Err(error) = output.finish() {
        let warning = crate::pro::ProOutputImport::finish_warning(&error);
        if progress.is_enabled() {
            progress.warning(&warning);
        } else {
            eprintln!("warning: {warning}");
        }
    }
}

fn complete_pro_output_inventory(
    output: Option<crate::pro::ProOutputImport>,
    progress: &ProgressReporter,
    required: bool,
) -> Result<()> {
    if !required {
        finish_pro_output_inventory(output, progress);
        return Ok(());
    }
    let output = output.ok_or_else(|| {
        anyhow!("not_materialized: connected Pro output materialization session is missing")
    })?;
    output.finish().map(|_| ())
}

#[cfg(test)]
mod pro_output_inventory_tests {
    use super::*;

    #[derive(Default)]
    struct TestCanonicalProProgression {
        frontier_checks: usize,
    }

    impl CanonicalProSourceProgression for TestCanonicalProProgression {
        fn progress_to_committed_core_frontier(&mut self) {
            self.frontier_checks += 1;
        }
    }

    #[test]
    fn empty_full_inventory_can_finish() {
        assert!(output_inventory_can_finish(true, &ImportTotals::default()));
    }

    #[test]
    fn failed_or_incomplete_inventory_cannot_finish() {
        let mut failed = ImportTotals::default();
        failed.failed_sources = 1;
        assert!(!output_inventory_can_finish(true, &failed));

        let mut incomplete = ImportTotals::default();
        incomplete.capture_work_remaining = true;
        assert!(!output_inventory_can_finish(true, &incomplete));
        assert!(!output_inventory_can_finish(
            false,
            &ImportTotals::default()
        ));
    }

    #[test]
    fn explicit_import_progresses_after_changed_or_failed_core_attempts() {
        let mut changed = ProviderImportSummary::default();
        changed.imported = 1;
        let no_op = ProviderImportSummary::default();
        let mut progression = TestCanonicalProProgression::default();

        progress_canonical_pro_after_core_source_attempt(Some(&mut progression), Some(&changed));
        progress_canonical_pro_after_core_source_attempt(Some(&mut progression), Some(&no_op));
        progress_canonical_pro_after_core_source_attempt(Some(&mut progression), None);

        assert_eq!(progression.frontier_checks, 2);
    }
}
