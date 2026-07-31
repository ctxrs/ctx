use std::{path::PathBuf, time::Instant};

use anyhow::{bail, Context, Result};
use ctx_history_capture::{ProviderImportSummary, ProviderImportWorkResult};
use ctx_history_core::{platform_security::establish_private_data_root, CaptureProvider};
use serde_json::json;

use crate::{
    analytics::{
        bytes_bucket, count_bucket, ImportTelemetry, ProviderRefreshSourceMode,
        ProviderRefreshTrigger,
    },
    history_source_plugins::{prepare_source_backed_history_source, select_history_source_plugin},
    progress::{format_bytes, ProgressReporter},
    semantic::{
        autostart_daemon_and_wait, wait_for_source_backed_relational_generation,
        SourceBackedRefreshMode,
    },
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
        "cataloging",
        format!(
            "Cataloging provider-owned history source plugin path for {}.",
            source.label()
        ),
    );

    let started = Instant::now();
    establish_private_data_root(&context.data_root)
        .context("protect ctx data root before history-source registration")?;
    let prepared = prepare_source_backed_history_source(source, context.args.reset_cursor)?;
    let stats = source_stats(prepared.source_path()).with_context(|| {
        format!(
            "inspect provider-owned history source plugin path {}",
            prepared.source_path().display()
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
    wait_for_source_backed_relational_generation(
        &context.data_root,
        &published_generation,
        context.args.no_daemon,
    )
    .context("converge required relational projection after history source publication")?;

    let summary = ProviderImportSummary {
        imported: usize::from(receipt.generation_changed),
        skipped: usize::from(!receipt.generation_changed),
        ..ProviderImportSummary::default()
    };
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

    let current = &receipt.current;
    let totals = ImportTotals {
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if receipt.generation_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    };

    context.telemetry.sources_seen = Some(count_bucket(1));
    context.telemetry.source_files = Some(count_bucket(stats.files as u64));
    context.telemetry.source_bytes = Some(bytes_bucket(stats.bytes));
    context.telemetry.failed_sources = Some(count_bucket(0));
    context.telemetry.sessions_imported = None;
    context.telemetry.events_imported = None;
    context.telemetry.edges_imported = None;
    context.telemetry.skipped = None;
    context.telemetry.rejected_records = None;

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
        sources: vec![crate::compact_json(json!({
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
            "path": prepared.source_path(),
            "source_files": stats.files,
            "source_bytes": stats.bytes,
            "catalog_changed": upsert.changed,
            "catalog_lineage": upsert.catalog_lineage_hex(),
            "catalog_authority": upsert.authority.to_json(),
            "previous_generation": receipt.previous_generation,
            "published_generation": published_generation,
            "generation_changed": receipt.generation_changed,
            "daemon_request_id": refresh.request_id,
            "daemon_request_metadata": {
                "owner": "daemon",
                "trigger": "import",
                "trigger_provenance": "history_source_plugin",
            },
            "change": if receipt.generation_changed { "changed" } else { "no_op" },
            "current_source_count": current.source_count,
            "current_indexed_documents": current.indexed_documents,
            "current_complete_records": current.complete_records,
            "current_retained_records": current.retained_records,
            "current_rejected_records": current.rejected_records,
            "current_ignored_records": current.ignored_records,
            "current_certified_source_bytes": current.certified_source_bytes,
            "current_sources_with_rejections": current.sources_with_rejections,
            "removed_source_count": current.removed_source_count,
            "provider_source_authority": true,
            "display_source_bytes": format_bytes(stats.bytes),
        }))],
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
