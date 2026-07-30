use std::{path::PathBuf, time::Instant};

use anyhow::{bail, Context, Result};
use ctx_history_capture::ProviderImportSummary;
use ctx_history_core::{platform_security::establish_private_data_root, CaptureProvider};
use serde_json::json;

use crate::{
    analytics::{
        bytes_bucket, count_bucket, ImportTelemetry, ProviderRefreshSourceMode,
        ProviderRefreshTrigger,
    },
    history_source_plugins::{prepare_source_backed_history_source, select_history_source_plugin},
    progress::{format_bytes, ProgressReporter},
    semantic::{autostart_daemon_and_wait, SourceBackedRefreshMode},
    DaemonTriggerCommandArg, ImportArgs,
};

use super::{
    catalog::source_stats, upsert_explicit_source, ImportReport, ImportRunOptions, ImportTotals,
    ProviderRefreshCollector, ProviderRefreshRuntimeFacts,
};

pub(crate) struct HistorySourcePluginImportContext<'a> {
    pub(crate) args: &'a ImportArgs,
    pub(crate) data_root: PathBuf,
    pub(crate) telemetry: &'a mut ImportTelemetry,
    pub(crate) provider_refreshes: &'a mut ProviderRefreshCollector,
    pub(crate) refresh_trigger: ProviderRefreshTrigger,
    pub(crate) config: &'a crate::config::AppConfig,
    pub(crate) options: ImportRunOptions,
}

pub(crate) fn run_history_source_plugin_import(
    context: HistorySourcePluginImportContext<'_>,
) -> Result<ImportReport> {
    if context.args.all {
        bail!(
            "the source-backed history plugin route imports one explicitly selected source; use --history-source or a manifest containing exactly one source"
        );
    }
    let source = select_history_source_plugin(
        &context.data_root,
        &context.args.history_source_manifest,
        context.args.history_source.as_deref(),
    )?;
    let progress = ProgressReporter::new(
        context.options.progress,
        context.options.json,
        context.options.operation,
        0,
    );
    progress.message(
        "exporting",
        format!(
            "Exporting history source plugin {} for daemon-owned source refresh.",
            source.label()
        ),
    );

    let started = Instant::now();
    establish_private_data_root(&context.data_root)
        .context("protect ctx data root before history-source export")?;
    let prepared = prepare_source_backed_history_source(
        source,
        &context.data_root,
        context.args.reset_cursor,
    )?;
    let stats = source_stats(prepared.snapshot_path()).with_context(|| {
        format!(
            "inspect managed history source plugin snapshot {}",
            prepared.snapshot_path().display()
        )
    })?;
    let upsert = upsert_explicit_source(&context.data_root, prepared.provider_source())?;
    if !context.args.no_daemon {
        autostart_daemon_and_wait(
            &context.data_root,
            context.config,
            DaemonTriggerCommandArg::Import,
        )?;
    }
    let refresh = SourceBackedRefreshMode::Wait
        .coordinate_explicit_source_catalog(&context.data_root, &upsert.authority)
        .context("publish history source plugin through daemon-owned source refresh")?;
    let receipt = refresh
        .receipt
        .as_ref()
        .context("history source plugin refresh has no authoritative terminal receipt")?;
    let published_generation = refresh.pin.generation_id().to_owned();
    prepared.commit_cursor()?;

    let mut summary = ProviderImportSummary {
        imported: usize::from(prepared.work_kind.changed()),
        skipped: prepared
            .skipped_records
            .saturating_add(usize::from(!prepared.work_kind.changed())),
        imported_sessions: prepared.imported_sessions,
        imported_events: prepared.imported_events,
        imported_edges: prepared.imported_edges,
        ..ProviderImportSummary::default()
    };
    if prepared.work_kind.changed()
        && summary.imported_sessions == 0
        && summary.imported_events == 0
        && summary.imported_edges == 0
    {
        summary.imported = 1;
    }
    context.provider_refreshes.record_success_with_facts(
        CaptureProvider::Custom,
        context.refresh_trigger,
        ProviderRefreshSourceMode::HistorySourcePlugin,
        &summary,
        &stats,
        ProviderRefreshRuntimeFacts::observed_success(started.elapsed(), &summary),
    );
    context.provider_refreshes.record_source_backed_publication(
        ProviderRefreshTrigger::Import,
        receipt.generation_changed,
    );

    let mut totals = ImportTotals::default();
    totals.add(&summary, &stats);
    totals.current_source_count = Some(receipt.current.source_count);
    totals.current_indexed_documents = Some(receipt.current.indexed_documents);
    totals.current_complete_records = Some(receipt.current.complete_records);
    totals.current_retained_records = Some(receipt.current.retained_records);
    totals.current_rejected_records = Some(receipt.current.rejected_records);
    totals.current_ignored_records = Some(receipt.current.ignored_records);
    totals.current_certified_source_bytes = Some(receipt.current.certified_source_bytes);
    totals.current_sources_with_rejections = Some(receipt.current.sources_with_rejections);
    totals.removed_source_count = Some(receipt.current.removed_source_count);

    context.telemetry.sources_seen = Some(count_bucket(1));
    context.telemetry.source_files = Some(count_bucket(stats.files as u64));
    context.telemetry.source_bytes = Some(bytes_bucket(stats.bytes));
    context.telemetry.failed_sources = Some(count_bucket(0));
    context.telemetry.sessions_imported = Some(count_bucket(summary.imported_sessions as u64));
    context.telemetry.events_imported = Some(count_bucket(summary.imported_events as u64));
    context.telemetry.edges_imported = Some(count_bucket(summary.imported_edges as u64));
    context.telemetry.skipped = Some(count_bucket(summary.skipped as u64));
    context.telemetry.rejected_records = Some(count_bucket(0));

    let completion = if context.options.progress == crate::progress::ProgressArg::Json {
        format!(
            "Published history source plugin {} in source-backed generation {published_generation}.",
            prepared.source().label()
        )
    } else {
        format!(
            "Published history source plugin {}.",
            prepared.source().label()
        )
    };
    progress.finish_line();
    progress.done("published", completion, stats.bytes);

    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources: vec![json!({
            "status": "published",
            "failure_scope": "none",
            "failure_type": "none",
            "provider": CaptureProvider::Custom.as_str(),
            "kind": "history_source_plugin",
            "plugin": prepared.source().plugin_name,
            "history_source": prepared.source().history_source(),
            "plugin_source": prepared.source().label(),
            "provider_key": prepared.source().provider_key,
            "source_id": prepared.source().source_id,
            "source_format": prepared.source().source_format,
            "route_source_format": prepared.provider_source().source_format,
            "path": prepared.snapshot_path(),
            "source_files": stats.files,
            "source_bytes": stats.bytes,
            "catalog_changed": upsert.changed,
            "catalog_lineage": upsert.catalog_lineage_hex(),
            "catalog_authority": upsert.authority.to_json(),
            "published_generation": published_generation,
            "generation_changed": receipt.generation_changed,
            "daemon_request_id": refresh.request_id,
            "daemon_request_metadata": {
                "owner": "daemon",
                "trigger": "import",
                "trigger_provenance": "history_source_plugin",
            },
            "change": if receipt.generation_changed { "changed" } else { "no_op" },
            "work_kind": prepared.work_kind.as_str(),
            "imported_sessions": summary.imported_sessions,
            "imported_events": summary.imported_events,
            "imported_edges": summary.imported_edges,
            "skipped_sessions": summary.skipped_sessions,
            "skipped_events": summary.skipped_events,
            "skipped_edges": summary.skipped_edges,
            "skipped": summary.skipped,
            "rejected_records": 0,
            "rejections": [],
            "provider_source_authority": true,
            "plugin_stderr_bytes": prepared.plugin_stderr.len(),
            "display_source_bytes": format_bytes(stats.bytes),
        })],
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_import_route_has_no_legacy_store_dependency() {
        let source = include_str!("history_source_plugin.rs");
        for forbidden in [
            ["ctx_history_", "store"].concat(),
            ["Store", "::open"].concat(),
            ["work", ".sqlite"].concat(),
            ["import_custom_history_", "jsonl_v1"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "history source plugin route contains forbidden legacy dependency `{forbidden}`"
            );
        }
    }
}
