use std::{path::PathBuf, time::Instant};

use anyhow::{bail, Context, Result};
use ctx_history_capture::{ProviderImportSummary, ProviderImportWorkResult};
use serde_json::json;

use crate::{
    analytics::{
        bytes_bucket, count_bucket, ImportTelemetry, ProviderRefreshSourceMode,
        ProviderRefreshTrigger,
    },
    progress::{format_bytes, ProgressReporter},
    ImportArgs,
};

use super::{
    catalog::source_stats,
    core_refresh::{wait_for_import_core_refresh, ImportCoreRefreshRequest},
    explicit_source_for_import, upsert_explicit_source, ImportReport, ImportRunOptions,
    ImportTotals, ProviderRefreshCollector, ProviderRefreshRuntimeFacts,
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
    )?;

    let started = Instant::now();
    let upsert = upsert_explicit_source(&context.data_root, &source)?;
    let refresh = wait_for_import_core_refresh(
        &context.data_root,
        context.config,
        context.args.no_daemon,
        ImportCoreRefreshRequest::ExplicitCatalog(&upsert.authority),
        &progress,
    )?;
    let receipt = refresh
        .receipt
        .as_ref()
        .context("explicit source refresh has no authoritative terminal receipt")?;
    let published_generation = refresh.pin.generation_id().to_owned();
    let catalog_lineage = upsert.catalog_lineage_hex();
    let requested_outcome = receipt
        .catalog_route_outcomes
        .iter()
        .find(|outcome| outcome.catalog_lineage == catalog_lineage)
        .context("explicit source refresh has no exact catalog-lineage result")?;
    let requested_succeeded = requested_outcome.outcome == "succeeded";
    if requested_outcome.outcome == "not_selected" {
        bail!("explicit source refresh did not select its exact catalog route");
    }
    let requested_failure = receipt
        .source_failures
        .iter()
        .find(|failure| failure.route_identity == requested_outcome.route_identity);
    let requested_failure_class = requested_outcome.failure_class.as_deref();
    let requested_failed = requested_failure_class.is_some();
    let requested_changed = if requested_succeeded {
        requested_outcome
            .changed
            .context("successful explicit source route has no change result")?
    } else {
        false
    };

    let summary = ProviderImportSummary {
        imported: usize::from(requested_succeeded && requested_changed),
        skipped: usize::from(requested_succeeded && !requested_changed),
        ..ProviderImportSummary::default()
    };
    if requested_succeeded {
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
    }

    let current = &receipt.current;
    let totals = ImportTotals {
        per_run_counts_available: true,
        source_files: stats.files,
        source_bytes: stats.bytes,
        imported_sources: usize::from(requested_succeeded),
        failed_sources: usize::from(requested_failed),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if requested_succeeded && requested_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    };
    context
        .provider_refreshes
        .record_core_publication(ProviderRefreshTrigger::Import, receipt.generation_changed);
    context.telemetry.sources_seen = Some(count_bucket(1));
    context.telemetry.source_files = Some(count_bucket(stats.files as u64));
    context.telemetry.source_bytes = Some(bytes_bucket(stats.bytes));
    context.telemetry.failed_sources = Some(count_bucket(u64::from(requested_failed)));
    context.telemetry.sessions_imported = None;
    context.telemetry.events_imported = None;
    context.telemetry.edges_imported = None;
    context.telemetry.skipped = None;
    // The refresh receipt exposes current corpus rejections, not a per-run
    // rejection delta. Do not mislabel that retained cardinality as work
    // performed by this import invocation.
    context.telemetry.rejected_records = None;

    let completion = if let Some(failure_class) = requested_failure_class {
        if context.options.progress == crate::progress::ProgressArg::Json {
            format!(
                "Explicit {} route failed ({}); Core generation {published_generation} remains authoritative.",
                source.provider.as_str(),
                failure_class
            )
        } else {
            format!(
                "The {} source failed ({}); retained history remains available.",
                source.provider.as_str(),
                failure_class
            )
        }
    } else if context.options.progress == crate::progress::ProgressArg::Json {
        format!("Published Core generation {published_generation}.")
    } else {
        "Published the source for indexing.".to_owned()
    };
    progress.finish_line()?;
    progress.done(
        if requested_failed {
            "failed"
        } else {
            "published"
        },
        completion,
        stats.bytes,
    )?;
    let source_status = if requested_failed {
        "failure"
    } else {
        "published"
    };
    let failure_scope = if requested_failed { "source" } else { "none" };
    let failure_type = requested_failure_class.map_or("none", |class| {
        if class == "incompatible" {
            "unsupported_schema"
        } else {
            "other"
        }
    });
    let route_change = if requested_succeeded && requested_changed {
        "changed"
    } else {
        "no_op"
    };
    let mut source_report = crate::compact_json(json!({
        "status": source_status,
        "failure_scope": failure_scope,
        "failure_type": failure_type,
        "provider": upsert.provider.as_str(),
        "path": upsert.path,
        "source_format": upsert.source_format,
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "catalog_changed": upsert.changed,
        "catalog_lineage": catalog_lineage,
        "catalog_authority": upsert.authority.to_json(),
        "previous_generation": receipt.previous_generation,
        "published_generation": published_generation,
        "generation_changed": receipt.generation_changed,
        "scanned_routes": receipt.selected_route_total,
        "successful_routes": receipt.successful_route_total,
        "source_failure_total": receipt.source_failure_total(),
        "daemon_request_id": refresh.request_id,
        "daemon_request_metadata": {
            "owner": "daemon",
            "operation": context.options.operation,
            "trigger": "import",
            "trigger_provenance": "explicit_source_catalog",
        },
        "change": route_change,
        "current_source_count": current.source_count,
        "current_indexed_documents": current.indexed_documents,
        "current_complete_records": current.complete_records,
        "current_retained_records": current.retained_records,
        "current_rejected_records": current.rejected_records,
        "current_ignored_records": current.ignored_records,
        "current_certified_source_bytes": current.certified_source_bytes,
        "current_sources_with_rejections": current.sources_with_rejections,
        "removed_source_count": current.removed_source_count,
    }));
    if requested_failed {
        let source_identity = requested_failure
            .map(|failure| failure.source_identity.as_str())
            .unwrap_or("unavailable_in_bounded_diagnostics");
        let source_selector = requested_failure
            .map(|failure| failure.source_selector.as_str())
            .unwrap_or("");
        let detail = requested_failure
            .map(|failure| failure.detail.as_str())
            .unwrap_or("source failure detail omitted from bounded diagnostics");
        let failure_fields = json!({
            "source_identity": source_identity,
            "source_selector": source_selector,
            "source_failure_class": requested_failure_class,
            "carried_forward": requested_failure.is_some_and(|failure| failure.carried_forward),
            "detail": detail,
            "error": detail,
            "imported_sessions": 0,
            "imported_events": 0,
            "imported_edges": 0,
            "skipped_sessions": 0,
            "skipped_events": 0,
            "skipped_edges": 0,
            "skipped": 0,
            "rejected_records": 0,
            "rejections": [],
        });
        let (serde_json::Value::Object(report), serde_json::Value::Object(failure_fields)) =
            (&mut source_report, failure_fields)
        else {
            unreachable!("explicit import report fields are JSON objects")
        };
        report.extend(failure_fields);
    }
    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources: vec![source_report],
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
