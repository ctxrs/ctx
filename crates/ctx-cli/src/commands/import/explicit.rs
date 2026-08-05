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
    explicit_source_for_import, relocate_explicit_source, relocation_authority_for_import,
    upsert_explicit_source, ImportReport, ImportRunOptions, ImportTotals, ProviderRefreshCollector,
    ProviderRefreshRuntimeFacts,
};

pub(crate) struct ExplicitSourceCatalogImportContext<'a> {
    pub(super) args: &'a ImportArgs,
    pub(super) data_root: PathBuf,
    pub(super) telemetry: &'a mut ImportTelemetry,
    pub(super) provider_refreshes: &'a mut ProviderRefreshCollector,
    pub(super) refresh_trigger: ProviderRefreshTrigger,
    pub(super) config: &'a crate::config::AppConfig,
    pub(super) options: ImportRunOptions,
    pub(super) ui: &'a mut crate::ui::Ui,
}

pub(crate) fn run_explicit_source_catalog_import(
    context: ExplicitSourceCatalogImportContext<'_>,
) -> Result<ImportReport> {
    let source = explicit_source_for_import(context.args)?
        .context("explicit source catalog import requires --path")?;
    let stats = source_stats(&source.path)
        .with_context(|| format!("inspect explicit source {}", source.path.display()))?;
    let mut progress = ProgressReporter::new(
        context.ui,
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
    let upsert = if let Some(old_path) = context.args.relocate_from.as_deref() {
        let relocation = relocation_authority_for_import(&context.data_root, old_path)?;
        relocate_explicit_source(&context.data_root, &source, relocation)?
    } else {
        upsert_explicit_source(&context.data_root, &source)?
    };
    let refresh = wait_for_import_core_refresh(
        &context.data_root,
        context.config,
        context.args.no_daemon,
        ImportCoreRefreshRequest::ExplicitCatalog(&upsert.authority),
        &mut progress,
    )?;
    let receipt = refresh
        .receipt
        .as_ref()
        .context("explicit source refresh has no authoritative terminal receipt")?;
    let request_previous_generation = refresh.request_previous_generation.clone();
    let request_generation_changed = refresh.request_generation_changed;
    let published_generation = refresh.pin.generation_id().to_owned();
    let catalog_lineage = upsert.catalog_lineage_hex();
    let requested_outcome = receipt
        .catalog_route_outcome(&catalog_lineage)
        .context("explicit source refresh has no exact catalog-lineage result")?;
    let requested_succeeded = requested_outcome.changed.is_some();
    if requested_outcome.outcome == "not_selected" {
        bail!("explicit source refresh did not select its exact catalog route");
    }
    let requested_failure = receipt
        .source_failures()
        .find(|failure| failure.route_identity == requested_outcome.route_identity);
    let requested_failure_class = requested_outcome.failure_class.as_deref();
    let requested_source_failed = requested_outcome.source_failure_total != 0;
    let requested_rejected = requested_outcome.rejected_record_total != 0;
    let requested_partial = requested_source_failed || requested_rejected;
    let requested_rejection_diagnostics = receipt
        .rejection_diagnostics()
        .filter(|rejection| rejection.route_identity == requested_outcome.route_identity)
        .map(|rejection| {
            json!({
                "source_identity": rejection.source_identity,
                "provider": rejection.provider,
                "path": rejection.source_selector,
                "line": rejection.line,
                "payload_type": rejection.payload_type,
                "class": rejection.class,
                "detail": rejection.detail,
            })
        })
        .collect::<Vec<_>>();
    let requested_changed = if requested_succeeded && request_generation_changed {
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
        // Core certifies the current generation and this route's terminal
        // outcome; it does not expose relational session/event deltas.
        per_run_counts_available: false,
        terminal_route_counts_available: true,
        source_files: stats.files,
        source_bytes: stats.bytes,
        imported_sources: usize::from(requested_succeeded),
        sources_completed_with_rejections: usize::from(requested_rejected),
        failed_sources: requested_outcome.source_failure_total,
        // This is the exact terminal outcome of the selected route, not an
        // inferred delta from whole-generation current counts.
        failed: usize::try_from(requested_outcome.rejected_record_total).unwrap_or(usize::MAX),
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
    context.provider_refreshes.record_core_publication(
        ProviderRefreshTrigger::Import,
        request_generation_changed,
        receipt.source_failure_total(),
        receipt.rejected_record_total(),
    );
    context.telemetry.sources_seen = Some(count_bucket(1));
    context.telemetry.source_files = Some(count_bucket(stats.files as u64));
    context.telemetry.source_bytes = Some(bytes_bucket(stats.bytes));
    context.telemetry.failed_sources =
        Some(count_bucket(requested_outcome.source_failure_total as u64));
    context.telemetry.sessions_imported = None;
    context.telemetry.events_imported = None;
    context.telemetry.edges_imported = None;
    context.telemetry.skipped = None;
    // The refresh receipt exposes current corpus rejections, not a per-run
    // rejection delta. Do not mislabel that retained cardinality as work
    // performed by this import invocation.
    context.telemetry.rejected_records = None;

    let source_status = if !requested_succeeded {
        "failure"
    } else if requested_partial {
        "partial"
    } else {
        "published"
    };
    let failure_scope = match (requested_source_failed, requested_rejected) {
        (false, false) => "none",
        (false, true) => "record",
        (true, false) => "source",
        (true, true) => "record_and_source",
    };
    let failure_type = match (requested_source_failed, requested_rejected) {
        (false, false) => "none",
        (false, true) => "record_rejection",
        (true, true) => "record_rejection_and_source_failure",
        (true, false) => requested_failure_class.map_or("source_failure", |class| {
            if class == "incompatible" {
                "unsupported_schema"
            } else {
                "other"
            }
        }),
    };
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
        "route_identity": requested_outcome.route_identity,
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "catalog_lineage": catalog_lineage,
        "request_overlay": upsert.authority.to_json(),
        "previous_generation": request_previous_generation,
        "published_generation": published_generation,
        "generation_changed": request_generation_changed,
        "scanned_routes": refresh
            .scanned_routes
            .context("published daemon source refresh omitted its scanned route count")?,
        "successful_routes": receipt.successful_route_total(),
        "source_failure_total": receipt.source_failure_total(),
        "route_source_failure_total": requested_outcome.source_failure_total,
        "rejected_record_total": requested_outcome.rejected_record_total,
        "rejection_diagnostics": requested_rejection_diagnostics,
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
    if requested_source_failed {
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
