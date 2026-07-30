use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result};
use ctx_history_capture::ProviderImportSummary;
use serde_json::json;

use crate::{
    analytics::{
        bytes_bucket, count_bucket, ImportTelemetry, ProviderRefreshSourceMode,
        ProviderRefreshTrigger,
    },
    progress::{format_bytes, ProgressReporter},
    semantic::{autostart_daemon_and_wait, SourceBackedRefreshMode},
    DaemonTriggerCommandArg, ImportArgs,
};

use super::{
    catalog::source_stats, explicit_source_for_import, upsert_explicit_source, ImportReport,
    ImportRunOptions, ImportTotals, ProviderRefreshCollector, ProviderRefreshRuntimeFacts,
};

pub(crate) struct ExplicitSourceCatalogImportContext<'a> {
    pub(super) args: &'a ImportArgs,
    pub(super) data_root: PathBuf,
    pub(super) telemetry: &'a mut ImportTelemetry,
    pub(super) provider_refreshes: &'a mut ProviderRefreshCollector,
    pub(super) refresh_trigger: ProviderRefreshTrigger,
    pub(super) config: &'a crate::config::AppConfig,
    pub(super) options: ImportRunOptions,
}

pub(crate) fn run_explicit_source_catalog_import(
    context: ExplicitSourceCatalogImportContext<'_>,
) -> Result<ImportReport> {
    let source = explicit_source_for_import(context.args)?
        .context("explicit source catalog import requires --path")?;
    let stats = source_stats(&source.path)
        .with_context(|| format!("inspect explicit source {}", source.path.display()))?;
    let progress = ProgressReporter::new(
        context.options.progress,
        context.options.json,
        context.options.operation,
        stats.bytes,
    );
    progress.message(
        "cataloging",
        format!(
            "Cataloging {} source {} ({}).",
            source.provider.as_str(),
            source.path.display(),
            format_bytes(stats.bytes)
        ),
    );

    let started = Instant::now();
    let upsert = upsert_explicit_source(&context.data_root, &source)?;
    if !context.args.no_daemon {
        autostart_daemon_and_wait(
            &context.data_root,
            context.config,
            DaemonTriggerCommandArg::Import,
        )?;
    }
    let refresh = SourceBackedRefreshMode::Wait
        .coordinate_explicit_source_catalog(&context.data_root, &upsert.authority)
        .context("publish explicit source through daemon-owned source refresh")?;
    let published_generation = refresh.pin.generation_id().to_owned();

    let summary = ProviderImportSummary {
        imported: 1,
        ..ProviderImportSummary::default()
    };
    context.provider_refreshes.record_success_with_facts(
        source.provider,
        context.refresh_trigger,
        if context.args.input_format.is_some() {
            ProviderRefreshSourceMode::ExplicitFormat
        } else {
            ProviderRefreshSourceMode::ExplicitPath
        },
        &summary,
        &stats,
        ProviderRefreshRuntimeFacts::observed_success(started.elapsed(), &summary),
    );

    let mut totals = ImportTotals::default();
    totals.add(&summary, &stats);
    context.telemetry.sources_seen = Some(count_bucket(1));
    context.telemetry.source_files = Some(count_bucket(stats.files as u64));
    context.telemetry.source_bytes = Some(bytes_bucket(stats.bytes));
    context.telemetry.failed_sources = Some(count_bucket(0));
    context.telemetry.sessions_imported = Some(count_bucket(0));
    context.telemetry.events_imported = Some(count_bucket(0));
    context.telemetry.edges_imported = Some(count_bucket(0));
    context.telemetry.skipped = Some(count_bucket(0));
    context.telemetry.rejected_records = Some(count_bucket(0));

    if context.options.print_human {
        progress.finish_line();
        println!("published_generation: {published_generation}");
    }
    progress.done(
        "published",
        format!("Published source-backed generation {published_generation}."),
        stats.bytes,
    );
    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources: vec![json!({
            "status": "published",
            "failure_scope": "none",
            "failure_type": "none",
            "provider": upsert.provider.as_str(),
            "path": upsert.path,
            "source_format": upsert.source_format,
            "source_files": stats.files,
            "source_bytes": stats.bytes,
            "catalog_changed": upsert.changed,
            "catalog_lineage": upsert.catalog_lineage_hex(),
            "catalog_authority": upsert.authority.to_json(),
            "published_generation": published_generation,
            "daemon_request_id": refresh.request_id,
            "daemon_request_metadata": {
                "owner": "daemon",
                "trigger": "import",
                "trigger_provenance": "explicit_source_catalog",
            },
            "change": summary.work_result().as_str(),
            "imported_sessions": 0,
            "imported_events": 0,
            "imported_edges": 0,
            "skipped_sessions": 0,
            "skipped_events": 0,
            "skipped_edges": 0,
            "skipped": 0,
            "rejected_records": 0,
            "rejections": [],
        })],
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn explicit_catalog_import_cannot_reach_the_legacy_store_epoch() {
        let implementation = include_str!("explicit.rs");
        let catalog = include_str!("explicit_source_catalog.rs");
        let forbidden_dependencies = [
            ["ctx_history_", "store"].concat(),
            ["Store", "::open"].concat(),
            ["work", ".sqlite"].concat(),
            ["import_custom_history_", "jsonl_v1"].concat(),
        ];
        for source in [implementation, catalog] {
            for forbidden in &forbidden_dependencies {
                assert!(
                    !source.contains(forbidden),
                    "explicit catalog import contains forbidden legacy dependency `{forbidden}`"
                );
            }
        }

        let dispatch = include_str!("../import.rs");
        assert!(!dispatch.contains("database_path"));
        assert!(!dispatch.contains(&["Store", "::open"].concat()));
    }
}
