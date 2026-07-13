use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_capture::{
    catalog_codex_session_tree, import_antigravity_cli_history, import_astrbot_sqlite,
    import_auggie_history, import_claude_projects_jsonl_tree, import_cline_task_json_history,
    import_codebuddy_history, import_codex_history_jsonl, import_codex_session_jsonl,
    import_codex_session_jsonl_tail, import_codex_session_paths, import_continue_cli_sessions,
    import_copilot_cli_session_events, import_crush_sqlite, import_cursor_native_history,
    import_custom_history_jsonl_v1, import_custom_history_jsonl_v1_reader,
    import_deepagents_sqlite, import_factory_ai_droid_sessions, import_firebender_sqlite,
    import_forgecode_sqlite, import_gemini_cli_history, import_goose_sessions_sqlite,
    import_hermes_sqlite, import_junie_history, import_kilo_sqlite, import_kimi_code_cli_history,
    import_kiro_sqlite, import_lingma_sqlite, import_mimocode_sqlite, import_mistral_vibe_history,
    import_mux_history, import_nanoclaw_project, import_openclaw_history, import_opencode_sqlite,
    import_openhands_file_events, import_pi_session_jsonl, import_qoder_history,
    import_qwen_code_history, import_roo_task_json_history, import_rovodev_history,
    import_shelley_sqlite, import_tabnine_cli_history, import_trae_history, import_warp_sqlite,
    import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite, provider_source_spec,
    stable_capture_uuid, AntigravityCliImportOptions, AstrBotSqliteImportOptions,
    AuggieImportOptions, CaptureError, CatalogSummary, ClaudeProjectsImportOptions,
    ClineTaskJsonImportOptions, CodeBuddyImportOptions, CodexHistoryImportOptions,
    CodexSessionCatalogOptions, CodexSessionImportOptions, CodexSessionImportProgressCallback,
    ContinueCliImportOptions, CopilotCliImportOptions, CrushSqliteImportOptions,
    CursorNativeImportOptions, CustomHistoryJsonlV1ImportOptions, DeepAgentsSqliteImportOptions,
    FactoryAiDroidImportOptions, FirebenderSqliteImportOptions, ForgeCodeSqliteImportOptions,
    GeminiCliImportOptions, GooseSessionsSqliteImportOptions, HermesSqliteImportOptions,
    JunieImportOptions, KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MiMoCodeSqliteImportOptions, MistralVibeImportOptions,
    MuxImportOptions, NanoClawImportOptions, OpenClawImportOptions, OpenCodeSqliteImportOptions,
    OpenHandsImportOptions, PiSessionImportOptions, ProviderImportFailure, ProviderImportSummary,
    ProviderImportSupport, ProviderSourceStatus, QoderImportOptions, QwenCodeImportOptions,
    RooTaskJsonImportOptions, RovoDevImportOptions, ShelleySqliteImportOptions,
    TabnineCliImportOptions, TraeImportOptions, WarpSqliteImportOptions,
    WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};
use ctx_history_core::{
    database_path, utc_now, CaptureProvider, CtxHistoryJsonlRecord, HistoryRecord,
};
use ctx_history_store::{
    CatalogIndexedStatus, CatalogSession, CatalogSourceIndexUpdate, SourceImportFile,
    SourceImportFileIndexUpdate, Store, StoreError,
};

use crate::analytics::AnalyticsProperties;
use crate::history_source_plugins::{
    discover_history_source_plugins, run_history_source_plugin, HistorySourcePluginRunOptions,
    HistorySourcePluginSource,
};
use crate::output::print_json;
use crate::progress::{
    format_bytes, format_count, plural, ProgressArg, ProgressReporter, SourceProgressSnapshot,
};
use crate::provider_args::ImportFormatArg;
use crate::provider_sources::{
    discovered_sources, discovered_sources_for_provider, explicit_path_source, import_support_json,
    SourceInfo,
};
use crate::{
    analytics, ImportArgs, LARGE_IMPORT_SOURCE_BYTES_WARNING, LARGE_IMPORT_SOURCE_FILES_WARNING,
    MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES, WAL_TRUNCATE_MIN_BYTES,
};

mod catalog;
mod explicit;
mod inventory;
mod manifest;
mod native;
mod report;
mod requests;

#[cfg(test)]
pub(crate) use catalog::{catalog_import_checkpoint_matches, sha256_file_prefix_hex};
use catalog::{
    import_incremental_codex_session_tree, import_record_for_custom_history,
    import_record_for_history_source_plugin, import_record_for_source, source_stats,
    source_uses_incremental_event_search,
};
use explicit::run_explicit_format_import;
pub(crate) use inventory::{
    inventory_available_sources, inventory_import_sources, ImportInventory,
};
use native::{import_one_source, validate_source_import_supported};
pub(crate) use native::{
    import_one_source_for_search_refresh, import_one_source_without_search_refresh,
};
use report::{
    custom_format_failure_json, custom_format_import_json, history_source_plugin_failure_json,
    history_source_plugin_import_json, import_failure_type, low_disk_space_warning,
    print_history_source_plugin_failed, print_history_source_plugin_imported, print_import_report,
    print_source_failed, print_source_imported, source_failure_json, source_import_json,
};
pub(crate) use report::{
    error_summary, import_error_retryability, import_error_scope, import_totals_json,
    one_line_error, source_error_reason,
};
pub(crate) use report::{ImportFailureScope, ImportFailureType, ImportRetryability};
pub(crate) use requests::import_history_source_plugin;
use requests::{history_source_plugin_import_requests, import_requests, validate_import_args};

#[derive(Debug, Clone, Default)]
pub(crate) struct ImportTotals {
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) imported_sources: usize,
    pub(crate) sources_completed_with_rejections: usize,
    pub(crate) failed_sources: usize,
    pub(crate) imported_sessions: usize,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) skipped_events: usize,
    pub(crate) skipped_edges: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
}

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

#[derive(Debug, Clone, Copy)]
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

impl ImportTotals {
    pub(crate) fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.sources_completed_with_rejections += usize::from(summary.failed > 0);
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped_sessions += summary.skipped_sessions;
        self.skipped_events += summary.skipped_events;
        self.skipped_edges += summary.skipped_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
    }

    pub(crate) fn add_source_failure(&mut self, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.failed_sources += 1;
    }

    pub(crate) fn add_rejected_source(
        &mut self,
        summary: &ProviderImportSummary,
        stats: &SourceStats,
    ) {
        self.add_source_failure(stats);
        self.skipped_sessions = self
            .skipped_sessions
            .saturating_add(summary.skipped_sessions);
        self.skipped_events = self.skipped_events.saturating_add(summary.skipped_events);
        self.skipped_edges = self.skipped_edges.saturating_add(summary.skipped_edges);
        self.skipped = self.skipped.saturating_add(summary.skipped);
        self.failed = self.failed.saturating_add(summary.failed);
    }
}

pub(crate) fn provider_summary_has_imported_content(summary: &ProviderImportSummary) -> bool {
    summary.has_accepted_content()
}

pub(crate) fn provider_summary_import_status(
    summary: &ProviderImportSummary,
) -> CatalogIndexedStatus {
    if summary.failed == 0 {
        CatalogIndexedStatus::Indexed
    } else if provider_summary_has_imported_content(summary) {
        CatalogIndexedStatus::CompletedWithRejections
    } else {
        CatalogIndexedStatus::Rejected
    }
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
    CodexSessionCatalog {
        summary: CatalogSummary,
        inventory_generation: u64,
    },
    SourceImportFiles {
        files: Vec<SourceImportFile>,
        inventory_generation: u64,
    },
    SourceRoot {
        file: SourceImportFile,
        inventory_generation: u64,
    },
}

impl SourcePreinventory {
    pub(crate) fn codex_session_catalog(&self) -> Option<&CatalogSummary> {
        match self {
            Self::CodexSessionCatalog { summary, .. } => Some(summary),
            Self::None | Self::SourceImportFiles { .. } | Self::SourceRoot { .. } => None,
        }
    }

    pub(crate) fn source_import_files(&self) -> Option<&[SourceImportFile]> {
        match self {
            Self::SourceImportFiles { files, .. } => Some(files),
            Self::None | Self::CodexSessionCatalog { .. } | Self::SourceRoot { .. } => None,
        }
    }

    pub(crate) fn source_root_file(&self) -> Option<&SourceImportFile> {
        match self {
            Self::SourceRoot { file, .. } => Some(file),
            Self::None | Self::CodexSessionCatalog { .. } | Self::SourceImportFiles { .. } => None,
        }
    }

    pub(crate) fn source_root_observation(&self) -> Option<(&SourceImportFile, u64)> {
        match self {
            Self::SourceRoot {
                file,
                inventory_generation,
            } => Some((file, *inventory_generation)),
            Self::None | Self::CodexSessionCatalog { .. } | Self::SourceImportFiles { .. } => None,
        }
    }

    pub(crate) fn inventory_generation(&self) -> Option<u64> {
        match self {
            Self::None => None,
            Self::CodexSessionCatalog {
                inventory_generation,
                ..
            }
            | Self::SourceImportFiles {
                inventory_generation,
                ..
            }
            | Self::SourceRoot {
                inventory_generation,
                ..
            } => Some(*inventory_generation),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceStats {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) change_token: Option<[u8; 32]>,
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
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let json = args.json;
    let progress = args.progress;
    let report = match run_import_internal(
        &args,
        data_root,
        analytics_properties,
        ImportRunOptions {
            progress,
            json,
            print_human: !json,
            allow_empty_sources: false,
            include_history_source_plugins: true,
            operation: "import",
        },
    ) {
        Ok(report) => report,
        Err(err) => {
            insert_import_error_analytics(analytics_properties, &err);
            return Err(err);
        }
    };
    insert_import_report_analytics(analytics_properties, &report);
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
    analytics_properties: &mut AnalyticsProperties,
    report: &ImportReport,
) {
    let (outcome, failure_scope) = import_report_analytics_outcome(&report.totals);
    analytics_properties.insert(
        "import_outcome".to_owned(),
        Value::String(outcome.to_owned()),
    );
    analytics_properties.insert(
        "import_failure_scope".to_owned(),
        Value::String(failure_scope.to_owned()),
    );
    analytics_properties.insert(
        "import_failure_type".to_owned(),
        Value::String(import_report_failure_type(&report.totals).to_owned()),
    );
}

pub(crate) fn insert_import_error_analytics(
    analytics_properties: &mut AnalyticsProperties,
    error: &anyhow::Error,
) {
    analytics_properties.insert(
        "import_outcome".to_owned(),
        Value::String("failure".to_owned()),
    );
    analytics_properties.insert(
        "import_failure_scope".to_owned(),
        Value::String(import_error_scope(error).as_str().to_owned()),
    );
    analytics_properties.insert(
        "import_failure_type".to_owned(),
        Value::String(import_failure_type(error).as_str().to_owned()),
    );
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
    analytics_properties: &mut AnalyticsProperties,
    options: ImportRunOptions,
) -> Result<ImportReport> {
    validate_import_args(args)?;
    fs::create_dir_all(&data_root).map_err(|source| CaptureError::SystemIo {
        operation: "initialize ctx data root",
        source,
    })?;
    let db_path = database_path(data_root.clone());
    let mut store = Store::open(&db_path)?;
    let mut totals = ImportTotals::default();
    let mut imported_sources = Vec::new();

    if let Some(format) = args.format {
        return run_explicit_format_import(
            args,
            format,
            db_path,
            store,
            analytics_properties,
            options,
        );
    }

    let requests = import_requests(args)?;
    let plugin_requests = history_source_plugin_import_requests(
        args,
        &data_root,
        options.include_history_source_plugins,
    )?;
    if requests.is_empty() && plugin_requests.is_empty() {
        if options.allow_empty_sources {
            return Ok(ImportReport::empty(args.resume));
        }
        return Err(anyhow!(
            "no importable provider history sources found; use --path, --history-source, or run `ctx sources`"
        ));
    }

    let inventory_progress =
        ProgressReporter::new(options.progress, options.json, options.operation, 0);
    inventory_progress.message("inventorying", "Preparing local history...");
    let inventory =
        inventory_import_sources(&store, requests).context("inventory local history sources")?;
    let planned_sources = inventory.sources;
    let inventory_failures = inventory.failures;
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
    analytics::insert_count_bucket(
        analytics_properties,
        "sources_seen_bucket",
        planned_sources
            .len()
            .saturating_add(inventory_failures.len())
            .saturating_add(plugin_requests.len()) as u64,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "source_bytes_bucket",
        planned_total_bytes,
    );

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
        match import_history_source_plugin(
            &mut store,
            &plugin_source,
            &data_root,
            args.reset_cursor,
        ) {
            Ok((summary, stats)) => {
                totals.add(&summary, &stats);
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
                let handle = thread::spawn(move || -> ImportSourceRun {
                    let result = (|| -> Result<ProviderImportSummary> {
                        let mut store = Store::open(&db_path)?;
                        import_one_source_without_search_refresh(
                            &mut store,
                            &plan.source,
                            progress_callback,
                            full_rescan,
                            &plan.preinventory,
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
            match handle.join() {
                Ok(ImportSourceRun::Imported(outcome)) => {
                    runs.push(ImportSourceRun::Imported(outcome))
                }
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
                    runs.push(ImportSourceRun::Failed(failure));
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
                    };
                    first_error.get_or_insert_with(|| {
                        anyhow::Error::new(CaptureError::WorkerPanicked("provider import"))
                    });
                    runs.push(ImportSourceRun::Failed(failure));
                }
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }

        runs.sort_by_key(ImportSourceRun::index);
        for run in runs {
            match run {
                ImportSourceRun::Imported(outcome) => {
                    totals.add(&outcome.summary, &outcome.stats);
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
            match import_one_source(
                &mut store,
                &plan.source,
                source_progress,
                args.resume,
                &plan.preinventory,
            ) {
                Ok(summary) => {
                    totals.add(&summary, &plan.stats);
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
                        };
                        if let Some(summary) = failure.rejected_summary.as_ref() {
                            totals.add_rejected_source(summary, &failure.stats);
                        } else {
                            totals.add_source_failure(&failure.stats);
                        }
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
    analytics::insert_count_bucket(
        analytics_properties,
        "source_files_bucket",
        totals.source_files as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "failed_sources_bucket",
        totals.failed_sources as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "sessions_imported_bucket",
        totals.imported_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "events_imported_bucket",
        totals.imported_events as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "edges_imported_bucket",
        totals.imported_edges as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "skipped_bucket",
        totals.skipped as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "rejected_records_bucket",
        totals.failed as u64,
    );
    Ok(ImportReport {
        resume: args.resume && native_import_requested,
        totals,
        inventory: inventory.totals,
        catalog: inventory.catalog,
        catalog_sources: inventory.catalog_sources,
        sources: imported_sources,
    })
}

fn source_provider_label(source: &SourceInfo) -> &'static str {
    provider_source_spec(source.provider)
        .map(|spec| spec.display_name)
        .unwrap_or_else(|| source.provider.as_str())
}

#[derive(Debug)]
pub(crate) struct ImportSourceOutcome {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) summary: ProviderImportSummary,
}

#[derive(Debug)]
pub(crate) struct ImportSourceFailure {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) error: String,
    pub(crate) failure_scope: ImportFailureScope,
    pub(crate) failure_type: ImportFailureType,
    pub(crate) rejected_summary: Option<ProviderImportSummary>,
    pub(crate) system_error: Option<anyhow::Error>,
}

#[derive(Debug)]
enum ImportSourceRun {
    Imported(ImportSourceOutcome),
    Failed(ImportSourceFailure),
}

impl ImportSourceRun {
    pub(crate) fn index(&self) -> usize {
        match self {
            Self::Imported(outcome) => outcome.index,
            Self::Failed(failure) => failure.index,
        }
    }
}

pub(crate) fn should_parallelize_import(planned_sources: &[PlannedImportSource]) -> bool {
    let _ = planned_sources;
    false
}

pub(crate) fn large_import_notice(
    planned_sources: &[PlannedImportSource],
    planned_total_bytes: u64,
) -> Option<String> {
    let planned_total_files = planned_sources
        .iter()
        .map(|plan| plan.stats.files)
        .sum::<usize>();
    if planned_total_files < LARGE_IMPORT_SOURCE_FILES_WARNING
        && planned_total_bytes < LARGE_IMPORT_SOURCE_BYTES_WARNING
    {
        return None;
    }
    Some(format!(
        "Large first import: scanning {} existing history {} ({}). This may take a while.",
        format_count(planned_total_files),
        plural(planned_total_files, "file", "files"),
        format_bytes(planned_total_bytes)
    ))
}
