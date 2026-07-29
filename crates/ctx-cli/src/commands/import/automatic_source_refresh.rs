use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ctx_history_capture::ProviderImportWorkResult;
use serde_json::json;

use crate::{
    analytics::{ImportTelemetry, ProviderRefreshTrigger},
    compact_json,
    progress::ProgressReporter,
    semantic::{
        autostart_daemon_and_wait, coordinate_source_backed_refresh, SourceBackedRefreshMode,
    },
    DaemonTriggerCommandArg, ImportArgs,
};

use super::{ImportReport, ImportRunOptions, ImportTotals, ProviderRefreshCollector};

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
    let receipt = refresh
        .receipt
        .clone()
        .context("daemon source refresh published without an authoritative terminal receipt")?;
    let request_id = refresh.request_id.clone();
    let index = refresh.pin.into_index();
    let manifest = index.manifest();
    let current = receipt.current;
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
    context.provider_refreshes.record_source_backed_publication(
        ProviderRefreshTrigger::Import,
        receipt.generation_changed,
    );

    if context.options.print_human {
        progress.finish_line();
        if let Some(previous) = receipt.previous_generation.as_deref() {
            println!("previous_generation: {previous}");
        }
        println!("published_generation: {}", receipt.published_generation);
        println!("generation_changed: {}", receipt.generation_changed);
    }
    progress.done(
        "published",
        format!(
            "Published source-backed generation {}.",
            receipt.published_generation
        ),
        current.certified_source_bytes,
    );

    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources: vec![compact_json(json!({
            "status": "published",
            "source_format": "provider_authoritative_all",
            "change": if receipt.generation_changed { "changed" } else { "no_op" },
            "previous_generation": receipt.previous_generation,
            "published_generation": receipt.published_generation,
            "generation_changed": receipt.generation_changed,
            "current_source_count": current.source_count,
            "current_indexed_documents": current.indexed_documents,
            "current_complete_records": current.complete_records,
            "current_retained_records": current.retained_records,
            "current_rejected_records": current.rejected_records,
            "current_ignored_records": current.ignored_records,
            "current_certified_source_bytes": current.certified_source_bytes,
            "current_sources_with_rejections": current.sources_with_rejections,
            "removed_source_count": current.removed_source_count,
            "policy_schema_hash": manifest.policy_schema_hash.clone(),
            "certified_source_count": current.source_count,
            "certified_source_bytes": current.certified_source_bytes,
            "daemon_request_id": request_id,
            "daemon_request_metadata": {
                "owner": "daemon",
                "trigger": "import",
                "trigger_provenance": "automatic_provider_refresh",
            },
        }))],
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
