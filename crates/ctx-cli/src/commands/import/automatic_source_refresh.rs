use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ctx_history_capture::ProviderImportWorkResult;
use serde_json::json;

use crate::{
    analytics::{bytes_bucket, count_bucket, ImportTelemetry},
    progress::ProgressReporter,
    semantic::{
        autostart_daemon_and_wait, coordinate_source_backed_refresh, SourceBackedRefreshMode,
    },
    DaemonTriggerCommandArg, ImportArgs,
};

use super::{
    CatalogTotals, ImportReport, ImportRunOptions, ImportTotals, InventoryTotals,
    ProviderRefreshCollector,
};

pub(super) struct AutomaticSourceRefreshImportContext<'a> {
    pub(super) args: &'a ImportArgs,
    pub(super) data_root: PathBuf,
    pub(super) telemetry: &'a mut ImportTelemetry,
    pub(super) provider_refreshes: &'a mut ProviderRefreshCollector,
    pub(super) config: &'a crate::config::AppConfig,
    pub(super) options: ImportRunOptions,
}

pub(super) fn run_automatic_source_refresh_import(
    context: AutomaticSourceRefreshImportContext<'_>,
) -> Result<ImportReport> {
    if context.args.history_source.is_some() || !context.args.history_source_manifest.is_empty() {
        bail!(
            "history-source plugins without a source-backed adapter are not supported in the v0.26 history epoch; import an approved provider source or explicit JSONL path"
        );
    }

    let progress = ProgressReporter::new(
        context.options.progress,
        context.options.json,
        context.options.operation,
        0,
    );
    progress.message(
        "refreshing",
        "Refreshing the provider-authoritative source index through the ctx daemon.",
    );
    if !context.args.no_daemon {
        autostart_daemon_and_wait(
            &context.data_root,
            context.config,
            DaemonTriggerCommandArg::Import,
        )?;
    }
    let refresh =
        coordinate_source_backed_refresh(&context.data_root, SourceBackedRefreshMode::Wait)
            .context("publish provider sources through daemon-owned source refresh")?;
    let request_id = refresh.request_id.clone();
    let index = refresh.pin.into_index();
    let manifest = index.manifest();
    let generation_id = index.generation_id().to_owned();
    let source_count = manifest.sources.len();
    let source_bytes = manifest.certified_source_bytes;
    let indexed_documents = manifest.indexed_documents;

    let source_files = source_count;
    let imported_events = usize::try_from(indexed_documents).unwrap_or(usize::MAX);
    let mut totals = ImportTotals {
        source_files,
        source_bytes,
        imported_sources: source_count,
        imported_events,
        work_result: ProviderImportWorkResult::Changed,
        ..ImportTotals::default()
    };
    if source_count == 0 {
        totals.work_result = ProviderImportWorkResult::NoOp;
    }

    context.telemetry.sources_seen = Some(count_bucket(source_count as u64));
    context.telemetry.source_files = Some(count_bucket(source_files as u64));
    context.telemetry.source_bytes = Some(bytes_bucket(source_bytes));
    context.telemetry.failed_sources = Some(count_bucket(0));
    context.telemetry.sessions_imported = Some(count_bucket(0));
    context.telemetry.events_imported = Some(count_bucket(indexed_documents));
    context.telemetry.edges_imported = Some(count_bucket(0));
    context.telemetry.skipped = Some(count_bucket(0));
    context.telemetry.rejected_records = Some(count_bucket(0));
    let _ = context.provider_refreshes;

    if context.options.print_human {
        progress.finish_line();
        println!("published_generation: {generation_id}");
    }
    progress.done(
        "published",
        format!("Published source-backed generation {generation_id}."),
        source_bytes,
    );

    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        inventory: InventoryTotals {
            sources: source_count,
            source_files,
            source_bytes,
            ..InventoryTotals::default()
        },
        catalog: CatalogTotals::default(),
        catalog_sources: Vec::new(),
        sources: vec![json!({
            "status": "published",
            "failure_scope": "none",
            "failure_type": "none",
            "provider": context
                .args
                .provider
                .map(|provider| provider.capture_provider().as_str()),
            "source_format": "provider_authoritative_all",
            "source_files": source_files,
            "source_bytes": source_bytes,
            "imported_sessions": 0,
            "imported_events": indexed_documents,
            "imported_edges": 0,
            "skipped_sessions": 0,
            "skipped_events": 0,
            "skipped_edges": 0,
            "skipped": 0,
            "rejected_records": 0,
            "rejections": [],
            "change": if source_count == 0 { "no_op" } else { "changed" },
            "published_generation": generation_id,
            "policy_schema_hash": manifest.policy_schema_hash.clone(),
            "certified_source_count": source_count,
            "certified_source_bytes": source_bytes,
            "daemon_request_id": request_id,
            "daemon_request_metadata": {
                "owner": "daemon",
                "trigger": "import",
                "trigger_provenance": "automatic_provider_refresh",
            },
        })],
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn automatic_import_has_no_legacy_history_store_dependency() {
        let source = include_str!("automatic_source_refresh.rs");
        for forbidden in [
            ["ctx_history_", "store"].concat(),
            ["Store", "::open"].concat(),
            ["work", ".sqlite"].concat(),
            ["projection_", "journal"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "automatic source refresh contains forbidden legacy dependency `{forbidden}`"
            );
        }
    }
}
