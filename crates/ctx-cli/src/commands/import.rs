use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_capture::{
    import_antigravity_cli_history, import_append_capable_provider_file, import_astrbot_sqlite,
    import_auggie_history, import_claude_projects_jsonl_tree, import_cline_task_json_history,
    import_codebuddy_history, import_codex_fresh_new_batch, import_codex_history_jsonl,
    import_codex_session_jsonl, import_codex_session_jsonl_tail, import_codex_session_paths,
    import_continue_cli_sessions, import_copilot_cli_session_events, import_crush_sqlite,
    import_cursor_native_history, import_custom_history_jsonl_v1,
    import_custom_history_jsonl_v1_reader, import_deepagents_sqlite,
    import_factory_ai_droid_sessions, import_firebender_sqlite, import_forgecode_sqlite,
    import_gemini_cli_history, import_goose_sessions_sqlite, import_hermes_sqlite,
    import_junie_history, import_kilo_sqlite, import_kimi_code_cli_history, import_kiro_sqlite,
    import_lingma_sqlite, import_mimocode_sqlite, import_mistral_vibe_history, import_mux_history,
    import_nanoclaw_project, import_openclaw_history, import_opencode_sqlite,
    import_openhands_file_events, import_pi_fresh_new_batch, import_pi_session_jsonl,
    import_qoder_history, import_qwen_code_history, import_roo_task_json_history,
    import_rovodev_history, import_shelley_sqlite, import_tabnine_cli_history, import_trae_history,
    import_warp_sqlite, import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite,
    provider_canonical_material_source_format, provider_file_mutation_contract,
    provider_source_spec, stable_capture_uuid, AntigravityCliImportOptions,
    AstrBotSqliteImportOptions, AuggieImportOptions, CaptureError, CatalogSummary,
    ClaudeProjectsImportOptions, ClineTaskJsonImportOptions, CodeBuddyImportOptions,
    CodexHistoryImportOptions, CodexSessionImportOptions, CodexSessionImportProgressCallback,
    ContinueCliImportOptions, CopilotCliImportOptions, CrushSqliteImportOptions,
    CursorNativeImportOptions, CustomHistoryJsonlV1ImportOptions, DeepAgentsSqliteImportOptions,
    FactoryAiDroidImportOptions, FirebenderSqliteImportOptions, ForgeCodeSqliteImportOptions,
    FreshNewImportContext, FreshNewImportOutcome, GeminiCliImportOptions,
    GooseSessionsSqliteImportOptions, HermesSqliteImportOptions, JunieImportOptions,
    KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MiMoCodeSqliteImportOptions, MistralVibeImportOptions,
    MuxImportOptions, NanoClawImportOptions, OpenClawImportOptions, OpenCodeSqliteImportOptions,
    OpenHandsImportOptions, PiSessionImportOptions, ProviderAdmittedJsonlAppendCheckpoint,
    ProviderAppendFileImportDecision, ProviderAppendFileImportMode,
    ProviderAppendFileImportOptions, ProviderFileMutationContract, ProviderFileStableIdentity,
    ProviderImportFailure, ProviderImportSummary, ProviderImportSupport,
    ProviderJsonlAppendCheckpoint, ProviderJsonlResumeState, ProviderSourceStatus,
    QoderImportOptions, QwenCodeImportOptions, RooTaskJsonImportOptions, RovoDevImportOptions,
    ShelleySqliteImportOptions, TabnineCliImportOptions, TraeImportOptions,
    WarpSqliteImportOptions, WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};
use ctx_history_core::{
    database_path, utc_now, CaptureProvider, CtxHistoryJsonlRecord, HistoryRecord,
};
use ctx_history_store::{
    CatalogImportWork, CatalogIndexedStatus, CatalogSession, CatalogSourceIndexUpdate,
    EventSearchBulkMaintenanceOutcome, ImportPendingReason, ImportWorkClass,
    ProviderFileCheckpoint, ProviderFileCheckpointKey, ProviderFileImportOutcome,
    ProviderFileInventoryObservation, ProviderFilePublicationCommit,
    ProviderFilePublicationCompletion, ProviderFilePublicationKind, ProviderFilePublicationPhase,
    ProviderFilePublicationRetirementWork, SourceImportFile, SourceImportFileIndexUpdate,
    SourceImportFileWork, Store, StoreError,
};

use crate::analytics::AnalyticsProperties;
use crate::history_source_plugins::{
    discover_history_source_plugins, run_history_source_plugin, HistorySourcePluginRunOptions,
    HistorySourcePluginSource,
};
use crate::output::print_json;
use crate::progress::{format_bytes, format_count, plural, ProgressArg, ProgressReporter};
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
mod fixed_point;
mod inventory;
mod manifest;
mod native;
mod report;
mod requests;
mod scheduler;

#[cfg(test)]
pub(crate) use catalog::{catalog_import_checkpoint_matches, sha256_file_prefix_hex};
use catalog::{
    import_incremental_codex_session_tree, import_record_for_custom_history,
    import_record_for_history_source_plugin, import_record_for_source, source_stats,
};
use explicit::run_explicit_format_import;
pub(crate) use fixed_point::{
    drain_fixed_point_action, drain_import_maintenance, DrainFixedPointAction,
};
#[cfg(test)]
pub(crate) use inventory::{inject_inventory_failure_once, InventoryFailurePoint};
pub(crate) use inventory::{
    inventory_available_sources, inventory_dirty_source_path, inventory_import_sources,
    source_matches_publication_owner, DirtySourcePathInventoryOutcome, ImportInventory,
    ImportInventoryCursor, ImportInventoryCursorStep, ImportInventorySliceProgress,
};
use native::validate_source_import_supported;
pub(crate) use native::{
    import_selected_source, publication_recovery_maintenance_warning,
    recover_provider_file_publication_retirement,
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
use scheduler::{bounded_unplanned_root_work_counts, SelectedImportWork};
pub(crate) use scheduler::{
    ExecutableImportSlice, ImportExecutionPolicy, ImportExecutionResult, ImportExecutionState,
    ImportPlan, IMPORT_PENDING_REPORT_LIMIT,
};

include!("import/state.rs");
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
    let _disk_io_pacing =
        ctx_history_capture::install_disk_io_pacer(ImportExecutionPolicy::Drain.disk_io_pacer());
    validate_import_args(args)?;
    fs::create_dir_all(&data_root).map_err(|source| CaptureError::SystemIo {
        operation: "initialize ctx data root",
        source,
    })?;
    let db_path = database_path(data_root.clone());
    let mut store = Store::open(&db_path)?;
    let mut totals = ImportTotals::default();
    let mut imported_sources = Vec::new();
    let opening_progress =
        ProgressReporter::new(options.progress, options.json, options.operation, 0);
    let opening_maintenance = drain_import_maintenance(&store)?;
    totals.durable_progress |= opening_maintenance.made_durable_progress;
    if let Some(reason) = opening_maintenance.pending_reason {
        opening_progress.warning(reason.diagnostic());
        totals.recovery_units_pending = 1;
        let mut report = ImportReport::empty(args.resume);
        report.totals = totals;
        return Ok(report);
    }

    if let Some(format) = args.format {
        let mut report = run_explicit_format_import(
            args,
            format,
            db_path,
            store,
            analytics_properties,
            options,
        )?;
        report.totals.durable_progress |= totals.durable_progress;
        report.totals.recovery_units_processed = report
            .totals
            .recovery_units_processed
            .saturating_add(totals.recovery_units_processed);
        report.totals.recovery_units_pending = report
            .totals
            .recovery_units_pending
            .saturating_add(totals.recovery_units_pending);
        return Ok(report);
    }

    let requests = import_requests(args)?;
    let plugin_requests = history_source_plugin_import_requests(
        args,
        &data_root,
        options.include_history_source_plugins,
    )?;
    let has_publication_work = has_provider_file_publication_work(&store)?;
    if requests.is_empty() && plugin_requests.is_empty() && !has_publication_work {
        if options.allow_empty_sources {
            let mut report = ImportReport::empty(args.resume);
            report.totals = totals;
            return Ok(report);
        }
        return Err(anyhow!(
            "no importable provider history sources found; use --path, --history-source, or run `ctx sources`"
        ));
    }

    let inventory_progress =
        ProgressReporter::new(options.progress, options.json, options.operation, 0);
    inventory_progress.message("inventorying", "Preparing local history...");
    let inventory = inventory_import_sources(&store, requests, args.resume)
        .context("inventory local history sources")?;
    let mut plan = ImportPlan::build(&store, inventory.sources)?;
    let mut execution_state = ImportExecutionState::for_plan(&plan);
    let mut inventory_failures = ImportInventoryFailures::new(inventory.failures);
    let mut failed_inventory_pending = inventory_failures.pending_counts(&store, &plan)?;
    let inventory_failure_count = inventory_failures.values().count();
    let planned_total_bytes = inventory.totals.source_bytes;
    inventory_progress.done(
        "inventorying",
        format!(
            "Found {} history {} ({}).",
            format_count(
                plan.sources
                    .len()
                    .saturating_add(inventory_failure_count)
                    .saturating_add(plugin_requests.len()),
            ),
            plural(
                plan.sources
                    .len()
                    .saturating_add(inventory_failure_count)
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
        plan.sources
            .len()
            .saturating_add(inventory_failure_count)
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
    if let Some(notice) = large_import_notice(&plan.sources, planned_total_bytes) {
        progress.notice(notice);
    }

    for failure in inventory_failures.values() {
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
    }

    let native_import_requested = !plan.sources.is_empty() || plan.recovery_units > 0;
    let reinventory_sources = stable_reinventory_sources(&plan, &inventory_failures);
    let mut native_reports = NativeSourceReports::default();
    let mut pass_source_plan_progress = false;
    let mut pass_retryable_blocker = false;
    loop {
        execution_state.begin_new_pass();
        let fresh_units = plan.pending_count(&store, ImportWorkClass::Fresh)?;
        if fresh_units == 0 {
            break;
        }
        let result = execute_import_plan_class_for_report(
            &mut store,
            &plan,
            &mut execution_state,
            ImportWorkClass::Fresh,
            fresh_units,
            &progress,
            options,
            &mut totals,
            &mut native_reports,
        )?;
        pass_source_plan_progress |= result.made_durable_progress;
        pass_retryable_blocker |= result.retryable_blocker;
        if !result.result.made_durable_progress() {
            break;
        }
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

    let (maintenance_complete, final_checkpoint_allowed) = 'fixed_point: loop {
        execution_state.begin_new_pass();
        let maintenance = drain_import_maintenance(&store)?;
        let mut maintenance_complete = maintenance.is_complete();
        let mut maintenance_pending = maintenance.pending_reason;
        totals.durable_progress |= maintenance.made_durable_progress;

        if maintenance_complete {
            let recovery_units = plan.pending_count(&store, ImportWorkClass::Recovery)?;
            let recovery = execute_import_plan_class_for_report(
                &mut store,
                &plan,
                &mut execution_state,
                ImportWorkClass::Recovery,
                recovery_units,
                &progress,
                options,
                &mut totals,
                &mut native_reports,
            )?;
            pass_source_plan_progress |= recovery.made_durable_progress;
            pass_retryable_blocker |= recovery.retryable_blocker;

            execution_state.begin_new_pass();
            let fresh_units = plan.pending_count(&store, ImportWorkClass::Fresh)?;
            let fresh = execute_import_plan_class_for_report(
                &mut store,
                &plan,
                &mut execution_state,
                ImportWorkClass::Fresh,
                fresh_units,
                &progress,
                options,
                &mut totals,
                &mut native_reports,
            )?;
            pass_source_plan_progress |= fresh.made_durable_progress;
            pass_retryable_blocker |= fresh.retryable_blocker;

            let trailing = drain_import_maintenance(&store)?;
            totals.durable_progress |= trailing.made_durable_progress;
            maintenance_complete = trailing.is_complete();
            maintenance_pending = trailing.pending_reason;
        }

        let (fresh_units_pending, recovery_units_pending) = plan.pending_counts(&store)?;
        let has_pending_work = fresh_units_pending > 0
            || recovery_units_pending > 0
            || !maintenance_complete
            || !inventory_failures.is_empty()
            || has_provider_file_publication_work(&store)?;
        let blocker = maintenance_pending
            .map(DrainFixedPointBlocker::Maintenance)
            .or_else(|| {
                (pass_retryable_blocker || !inventory_failures.is_empty())
                    .then_some(DrainFixedPointBlocker::RetryableExternal)
            });
        match drain_fixed_point_action(has_pending_work, pass_source_plan_progress, blocker)? {
            DrainFixedPointAction::Complete => {
                break 'fixed_point (maintenance_complete, true);
            }
            DrainFixedPointAction::RetryableBlocked(blocker) => {
                progress.warning(blocker.diagnostic());
                break 'fixed_point (maintenance_complete, false);
            }
            DrainFixedPointAction::Reinventory => {}
        }

        let inventory = inventory_import_sources(&store, reinventory_sources.clone(), false)
            .context("re-inventory local history sources after import progress")?;
        let changes = inventory_failures.reconcile(&inventory.sources, inventory.failures);
        for failure in changes.newly_failed {
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
        }
        plan = ImportPlan::build(&store, inventory.sources)?;
        failed_inventory_pending = inventory_failures.pending_counts(&store, &plan)?;
        execution_state = ImportExecutionState::for_plan(&plan);
        pass_source_plan_progress = false;
        pass_retryable_blocker = false;
    };

    let (fresh_units_pending, recovery_units_pending) = plan.pending_counts(&store)?;
    totals.fresh_units_pending =
        capped_pending_add(fresh_units_pending, failed_inventory_pending.0);
    totals.recovery_units_pending = capped_pending_add(
        capped_pending_add(recovery_units_pending, failed_inventory_pending.1),
        usize::from(!maintenance_complete),
    );
    for failure in inventory_failures.values() {
        native_reports.record_inventory_failure(failure);
    }
    native_reports.apply_totals(&mut totals);
    native_reports.append_json(&plan, &mut imported_sources);

    if store.event_search_projection_needs_backfill()? {
        progress.message("finalizing", "Refreshing search index...");
        store.refresh_search_index()?;
    }

    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "Optimizing search index...");
        Store::open(&db_path)?.optimize_search_index()?;
    }

    if maintenance_complete && final_checkpoint_allowed {
        progress.message("finalizing", "Checkpointing search database...");
        Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    }

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

fn has_provider_file_publication_work(store: &Store) -> Result<bool> {
    Ok(store.has_pending_provider_file_publications()?
        || !store
            .list_provider_file_publication_retirement_work(1)?
            .is_empty())
}

#[allow(clippy::too_many_arguments)]
fn execute_import_plan_class_for_report(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut ImportExecutionState,
    class: ImportWorkClass,
    remaining_units: usize,
    progress: &ProgressReporter,
    options: ImportRunOptions,
    totals: &mut ImportTotals,
    native_reports: &mut NativeSourceReports,
) -> Result<ImportClassExecution> {
    execute_import_plan_class_for_report_tracked(
        store,
        plan,
        execution_state,
        class,
        remaining_units,
        progress,
        options,
        totals,
        native_reports,
        || {},
    )
}

#[derive(Debug)]
struct ImportClassExecution {
    result: ImportExecutionResult,
    made_durable_progress: bool,
    retryable_blocker: bool,
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn execute_import_plan_class_for_report_with_pre_lock_hook(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut ImportExecutionState,
    class: ImportWorkClass,
    remaining_units: usize,
    progress: &ProgressReporter,
    options: ImportRunOptions,
    totals: &mut ImportTotals,
    native_reports: &mut NativeSourceReports,
    before_bulk_lock: impl FnMut(),
) -> Result<ImportExecutionResult> {
    Ok(execute_import_plan_class_for_report_tracked(
        store,
        plan,
        execution_state,
        class,
        remaining_units,
        progress,
        options,
        totals,
        native_reports,
        before_bulk_lock,
    )?
    .result)
}

#[allow(clippy::too_many_arguments)]
fn execute_import_plan_class_for_report_tracked(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut ImportExecutionState,
    class: ImportWorkClass,
    mut remaining_units: usize,
    progress: &ProgressReporter,
    options: ImportRunOptions,
    totals: &mut ImportTotals,
    native_reports: &mut NativeSourceReports,
    mut before_bulk_lock: impl FnMut(),
) -> Result<ImportClassExecution> {
    let mut completed_bytes = 0u64;
    let mut execution_result = ImportExecutionResult::default();
    let mut made_durable_progress = false;
    let mut retryable_blocker = false;
    while remaining_units > 0 {
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
            break;
        }
        let validation_units = validation_failures
            .iter()
            .map(|failure| failure.stats.files)
            .sum::<usize>();
        let selected_units = slice.units.saturating_add(validation_units);
        remaining_units = remaining_units.saturating_sub(selected_units);
        let mut system_error = None;
        let mut completed_units = 0usize;
        let mut deferred_units = 0usize;
        let mut maintenance_progress = false;
        let mut source_durable_progress = false;
        let mut stop_admission = false;
        for validation_failure in validation_failures {
            let source_plan = &plan.sources[validation_failure.source_index];
            native_reports.record_failure(
                &source_plan.source,
                validation_failure.stats,
                &validation_failure.error,
            );
            let failure = ImportSourceFailure {
                source: source_plan.source.clone(),
                stats: validation_failure.stats,
                error: error_summary(&validation_failure.error),
                failure_type: import_failure_type(&validation_failure.error),
                rejected_summary: rejected_source_summary(&validation_failure.error),
            };
            progress.done(
                "indexing",
                format!(
                    "skipped {}: {}",
                    failure.source.provider.as_str(),
                    source_error_reason(&failure.source, &failure.error)
                ),
                completed_bytes,
            );
            if options.print_human {
                progress.finish_line();
                print_source_failed(&failure);
            }
        }
        for retirement in &slice.retirements {
            execution_state.record_retirement_attempt(retirement);
            progress.message("repairing", "repairing prior hidden provider history");
            match recover_provider_file_publication_retirement(store, retirement, true) {
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
            let outcome_has_error = import_error.is_some();
            let mut outcome_completed_units = 0usize;
            let mut outcome_completed_bytes = 0u64;
            let mut outcome_deferred_units = 0usize;
            let had_outcome = outcome.is_some();
            if let Some(outcome) = outcome {
                stop_admission |= outcome.stop_admission;
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
                let completed_stats = SourceStats {
                    files: outcome.completed_units,
                    bytes: outcome.completed_bytes,
                    change_token: selected.stats.change_token,
                };
                let no_op_stats = (outcome.completed_units == 0
                    && deferred == 0
                    && !outcome_has_error
                    && outcome.summary != ProviderImportSummary::default())
                .then_some(selected.stats);
                native_reports.record_outcome(
                    &source_plan.source,
                    &outcome.summary,
                    completed_stats,
                    no_op_stats,
                );
                if outcome.completed_units > 0 {
                    completed_bytes = completed_bytes.saturating_add(completed_stats.bytes);
                    let (phase, message) = import_work_progress_done(class, &source_plan.source);
                    progress.done(phase, message, completed_bytes);
                    if options.print_human {
                        progress.finish_line();
                        print_source_imported(&source_plan.source, &outcome.summary);
                    }
                } else if deferred > 0 && !made_durable_progress {
                    progress.done(
                        phase,
                        format!(
                            "Deferred incomplete {} history.",
                            source_provider_label(&source_plan.source)
                        ),
                        completed_bytes,
                    );
                }
                if deferred > 0 && !made_durable_progress {
                    progress.warning(format!(
                        "{} {} history unit(s) remain pending until their current write completes.",
                        deferred,
                        source_provider_label(&source_plan.source)
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
                if let Some(warning) = native::publication_recovery_maintenance_warning(&err) {
                    progress.warning(warning.to_string());
                }
                let failure_scope = import_error_scope(&err);
                let failure_type = import_failure_type(&err);
                let rejected_summary = rejected_source_summary(&err);
                let error = error_summary(&err);
                if failure_scope == ImportFailureScope::System {
                    system_error = Some(err);
                    break;
                }
                retryable_blocker |=
                    import_error_retryability(&err) == ImportRetryability::Retryable;
                let failure_stats = SourceStats {
                    files: selected.stats.files.saturating_sub(
                        outcome_completed_units.saturating_add(outcome_deferred_units),
                    ),
                    bytes: selected.stats.bytes.saturating_sub(outcome_completed_bytes),
                    change_token: selected.stats.change_token,
                };
                native_reports.record_failure(&source_plan.source, failure_stats, &err);
                let failure = ImportSourceFailure {
                    source: source_plan.source.clone(),
                    stats: failure_stats,
                    error,
                    failure_type,
                    rejected_summary,
                };
                progress.done(
                    "indexing",
                    format!(
                        "skipped {}: {}",
                        failure.source.provider.as_str(),
                        source_error_reason(&failure.source, &failure.error)
                    ),
                    completed_bytes,
                );
                if options.print_human {
                    progress.finish_line();
                    print_source_failed(&failure);
                }
            }
            if outcome_deferred_units > 0 && store.has_pending_provider_file_publications()? {
                break;
            }
            if stop_admission {
                break;
            }
        }
        match store.finish_event_search_bulk_mode(&bulk_guard) {
            Ok(EventSearchBulkMaintenanceOutcome::Complete) => {}
            Ok(EventSearchBulkMaintenanceOutcome::Pending) => stop_admission = true,
            Err(StoreError::WalCheckpointBusy { .. }) => stop_admission = true,
            Err(error) => return Err(error.into()),
        }
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
        made_durable_progress |=
            completed_units > 0 || maintenance_progress || source_durable_progress;
        retryable_blocker |= stop_admission || deferred_units > 0;
        if stop_admission {
            execution_result.stop_admission();
        }
        totals.durable_progress |=
            completed_units > 0 || maintenance_progress || source_durable_progress;
        if let Some(error) = system_error {
            return Err(error);
        }
        if stop_admission {
            break;
        }
        if deferred_units > 0 && store.has_pending_provider_file_publications()? {
            break;
        }
    }
    Ok(ImportClassExecution {
        result: execution_result,
        made_durable_progress,
        retryable_blocker,
    })
}

fn source_provider_label(source: &SourceInfo) -> &'static str {
    provider_source_spec(source.provider)
        .map(|spec| spec.display_name)
        .unwrap_or_else(|| source.provider.as_str())
}

#[derive(Debug, Clone)]
pub(crate) struct ImportSourceFailure {
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) error: String,
    pub(crate) failure_type: ImportFailureType,
    pub(crate) rejected_summary: Option<ProviderImportSummary>,
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
