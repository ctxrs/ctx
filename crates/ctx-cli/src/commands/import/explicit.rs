use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use ctx_history_capture::{import_custom_history_jsonl_v1, CustomHistoryJsonlV1ImportOptions};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::analytics::{
    bytes_bucket, count_bucket, ImportTelemetry, ProviderRefreshSourceMode, ProviderRefreshTrigger,
};
use crate::commands::import::catalog::{import_record_for_custom_history, source_stats};
use crate::commands::import::report::{
    custom_format_failure_json, custom_format_import_json, error_summary, import_error_scope,
    import_failure_type, low_disk_space_warning, ImportFailureScope,
};
use crate::commands::import::totals::ImportTotals;
use crate::commands::import::ProviderRefreshCollector;
use crate::commands::import::{
    cleanup_rejected_history_record, history_record_exists, provider_summary_has_imported_content,
    CatalogTotals, ImportReport, ImportRunOptions, InventoryTotals, PlannedImportSource,
    SourceStats,
};
use crate::progress::{format_bytes, format_count, plural, ProgressReporter};
use crate::provider_args::ImportFormatArg;
use crate::{
    ImportArgs, LARGE_IMPORT_SOURCE_BYTES_WARNING, LARGE_IMPORT_SOURCE_FILES_WARNING,
    WAL_TRUNCATE_MIN_BYTES,
};

pub(crate) struct ExplicitFormatImportContext<'a> {
    pub(super) args: &'a ImportArgs,
    pub(super) format: ImportFormatArg,
    pub(super) db_path: PathBuf,
    pub(super) store: Store,
    pub(super) telemetry: &'a mut ImportTelemetry,
    pub(super) provider_refreshes: &'a mut ProviderRefreshCollector,
    pub(super) refresh_trigger: ProviderRefreshTrigger,
    pub(super) options: ImportRunOptions,
}

impl ExplicitFormatImportContext<'_> {
    fn failure_report(
        &mut self,
        path: &Path,
        stats: SourceStats,
        error: &anyhow::Error,
    ) -> ImportReport {
        let mut totals = ImportTotals::default();
        totals.add_source_failure(&stats);
        self.provider_refreshes.record_failure(
            CaptureProvider::Custom,
            self.refresh_trigger,
            ProviderRefreshSourceMode::ExplicitFormat,
            &stats,
            None,
        );
        insert_explicit_format_analytics(self.telemetry, &stats, &totals);
        ImportReport {
            resume: self.args.resume,
            totals,
            inventory: InventoryTotals {
                sources: 1,
                source_files: stats.files,
                source_bytes: stats.bytes,
                ..InventoryTotals::default()
            },
            catalog: CatalogTotals::default(),
            catalog_sources: Vec::new(),
            sources: vec![custom_format_failure_json(
                self.format,
                path,
                &stats,
                &error_summary(error),
                import_failure_type(error),
            )],
        }
    }
}

pub(crate) fn run_explicit_format_import(
    mut context: ExplicitFormatImportContext<'_>,
) -> Result<ImportReport> {
    let path = context
        .args
        .path
        .clone()
        .context("--format requires an explicit --path")?;
    let stats = match source_stats(&path)
        .with_context(|| format!("scan import source {}", path.display()))
    {
        Ok(stats) => stats,
        Err(error) if import_error_scope(&error) == ImportFailureScope::System => {
            context.provider_refreshes.record_failure(
                CaptureProvider::Custom,
                context.refresh_trigger,
                ProviderRefreshSourceMode::ExplicitFormat,
                &SourceStats::default(),
                None,
            );
            return Err(error);
        }
        Err(error) => {
            return Ok(context.failure_report(&path, SourceStats::default(), &error));
        }
    };
    context.telemetry.sources_seen = Some(count_bucket(1));
    context.telemetry.source_bytes = Some(bytes_bucket(stats.bytes));

    let progress = ProgressReporter::new(
        context.options.progress,
        context.options.json,
        context.options.operation,
        stats.bytes,
    );
    progress.message(
        "discovering",
        format!(
            "Found 1 {} source ({}).",
            context.format.as_str(),
            format_bytes(stats.bytes)
        ),
    );
    if let Some(warning) = low_disk_space_warning(&context.db_path, stats.bytes) {
        progress.warning(warning);
    }
    if (stats.files >= LARGE_IMPORT_SOURCE_FILES_WARNING
        || stats.bytes >= LARGE_IMPORT_SOURCE_BYTES_WARNING)
        && stats.files > 0
    {
        let notice = format!(
            "Large first import: scanning {} existing history {} ({}). This may take a while.",
            format_count(stats.files),
            plural(stats.files, "file", "files"),
            format_bytes(stats.bytes)
        );
        progress.notice(notice);
    }

    let record = import_record_for_custom_history(&path, context.format);
    let record_id = record.id;
    let record_existed = history_record_exists(&context.store, record_id)?;
    context.store.upsert_record(&record)?;
    progress.message("indexing", format!("importing {}", context.format.as_str()));
    let import_result = match context.format {
        ImportFormatArg::CtxHistoryJsonlV1 => import_custom_history_jsonl_v1(
            &path,
            &mut context.store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(path.clone()),
                history_record_id: Some(record_id),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
    };
    let summary = match import_result {
        Ok(summary) => summary,
        Err(error) if import_error_scope(&error) == ImportFailureScope::System => {
            context.provider_refreshes.record_failure(
                CaptureProvider::Custom,
                context.refresh_trigger,
                ProviderRefreshSourceMode::ExplicitFormat,
                &stats,
                None,
            );
            return Err(error);
        }
        Err(error) => {
            cleanup_rejected_history_record(&context.store, record_id, record_existed)?;
            return Ok(context.failure_report(&path, stats, &error));
        }
    };
    let mut totals = ImportTotals::default();
    if summary.failed > 0 && !provider_summary_has_imported_content(&summary) {
        cleanup_rejected_history_record(&context.store, record_id, record_existed)?;
        totals.add_rejected_source(&summary, &stats);
        context.provider_refreshes.record_failure(
            CaptureProvider::Custom,
            context.refresh_trigger,
            ProviderRefreshSourceMode::ExplicitFormat,
            &stats,
            Some(&summary),
        );
    } else {
        totals.add(&summary, &stats);
        context.provider_refreshes.record_success(
            CaptureProvider::Custom,
            context.refresh_trigger,
            ProviderRefreshSourceMode::ExplicitFormat,
            &summary,
            &stats,
        );
    }
    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "optimizing search index");
        Store::open(&context.db_path)?.optimize_search_index()?;
    }
    progress.message("finalizing", "checkpointing search database");
    Store::open(&context.db_path)?
        .checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    if context.options.print_human {
        progress.finish_line();
    }
    progress.done(
        "finalizing",
        format!("processed 1 {} source file", context.format.as_str()),
        stats.bytes,
    );
    insert_explicit_format_analytics(context.telemetry, &stats, &totals);
    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        inventory: InventoryTotals {
            sources: 1,
            source_files: stats.files,
            source_bytes: stats.bytes,
            ..InventoryTotals::default()
        },
        catalog: CatalogTotals::default(),
        catalog_sources: Vec::new(),
        sources: vec![custom_format_import_json(
            context.format,
            &path,
            &stats,
            &summary,
        )],
    })
}

fn insert_explicit_format_analytics(
    telemetry: &mut ImportTelemetry,
    stats: &SourceStats,
    totals: &ImportTotals,
) {
    telemetry.source_files = Some(count_bucket(stats.files as u64));
    telemetry.failed_sources = Some(count_bucket(totals.failed_sources as u64));
    telemetry.sessions_imported = Some(count_bucket(totals.imported_sessions as u64));
    telemetry.events_imported = Some(count_bucket(totals.imported_events as u64));
    telemetry.edges_imported = Some(count_bucket(totals.imported_edges as u64));
    telemetry.skipped = Some(count_bucket(totals.skipped as u64));
    telemetry.rejected_records = Some(count_bucket(totals.failed as u64));
}

pub(super) fn large_import_notice(
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
