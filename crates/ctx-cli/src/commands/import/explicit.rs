use std::{path::PathBuf, time::Instant};

use anyhow::{bail, Context, Result};
use ctx_history_capture::{
    ProviderImportSummary, ProviderImportWorkResult, ProviderSource, SourceBackedRoute,
    SourceBackedRouteDriver, SourceBackedSelectorAuthority,
};
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
    let requested_route_identity = explicit_route_identity(&source)?;
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
    let refresh = wait_for_import_core_refresh(
        &context.data_root,
        context.config,
        context.args.no_daemon,
        ImportCoreRefreshRequest::ExplicitCatalog(&upsert.authority),
    )?;
    let receipt = refresh
        .receipt
        .as_ref()
        .context("explicit source refresh has no authoritative terminal receipt")?;
    let published_generation = refresh.pin.generation_id().to_owned();
    let requested_succeeded = receipt
        .successful_route_ids
        .iter()
        .any(|route| route == &requested_route_identity);
    let requested_failure = receipt
        .source_failures
        .iter()
        .find(|failure| failure.route_identity == requested_route_identity);
    if !receipt
        .selected_route_ids
        .iter()
        .any(|route| route == &requested_route_identity)
    {
        bail!(
            "explicit source refresh receipt did not select requested route {requested_route_identity}"
        );
    }
    if requested_succeeded == requested_failure.is_some() {
        bail!(
            "explicit source refresh receipt has no unique terminal result for requested route {requested_route_identity}"
        );
    }

    let summary = ProviderImportSummary {
        imported: usize::from(requested_succeeded && receipt.generation_changed),
        skipped: usize::from(requested_succeeded && !receipt.generation_changed),
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
        failed_sources: usize::from(requested_failure.is_some()),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if requested_succeeded && receipt.generation_changed {
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
    context.telemetry.failed_sources = Some(count_bucket(u64::from(requested_failure.is_some())));
    context.telemetry.sessions_imported = None;
    context.telemetry.events_imported = None;
    context.telemetry.edges_imported = None;
    context.telemetry.skipped = None;
    // The refresh receipt exposes current corpus rejections, not a per-run
    // rejection delta. Do not mislabel that retained cardinality as work
    // performed by this import invocation.
    context.telemetry.rejected_records = None;

    let completion = if let Some(failure) = requested_failure {
        if context.options.progress == crate::progress::ProgressArg::Json {
            format!(
                "Explicit {} route failed ({}); Core generation {published_generation} remains authoritative.",
                source.provider.as_str(),
                failure.class
            )
        } else {
            format!(
                "The {} source failed ({}); retained history remains available.",
                source.provider.as_str(),
                failure.class
            )
        }
    } else if context.options.progress == crate::progress::ProgressArg::Json {
        format!("Published Core generation {published_generation}.")
    } else {
        "Published the source for indexing.".to_owned()
    };
    progress.finish_line();
    progress.done(
        if requested_failure.is_some() {
            "failed"
        } else {
            "published"
        },
        completion,
        stats.bytes,
    );
    let source_status = if requested_failure.is_some() {
        "failure"
    } else {
        "published"
    };
    let failure_scope = if requested_failure.is_some() {
        "source"
    } else {
        "none"
    };
    let failure_type = requested_failure.map_or("none", |failure| {
        if failure.class == "incompatible" {
            "unsupported_schema"
        } else {
            "other"
        }
    });
    let route_change = if requested_succeeded && receipt.generation_changed {
        "changed"
    } else {
        "no_op"
    };
    let source_selector = upsert.path.display().to_string();
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
        "catalog_lineage": upsert.catalog_lineage_hex(),
        "catalog_authority": upsert.authority.to_json(),
        "previous_generation": receipt.previous_generation,
        "published_generation": published_generation,
        "generation_changed": receipt.generation_changed,
        "scanned_routes": receipt.selected_route_ids.len(),
        "successful_routes": receipt.successful_route_ids.len(),
        "source_failure_total": receipt.source_failures.len(),
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
    if let Some(failure) = requested_failure {
        let detail = format!(
            "{} source refresh failed ({})",
            source.provider.as_str(),
            failure.class
        );
        let failure_fields = json!({
            "source_identity": failure.source_identity,
            "source_selector": source_selector,
            "source_failure_class": failure.class,
            "carried_forward": failure.carried_forward,
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

fn explicit_route_identity(source: &ProviderSource) -> Result<String> {
    let route = SourceBackedRoute::explicit_manual(
        source.clone(),
        // Explicit catalog entries are registered with this exact authority in
        // `register_catalog_entry`; deriving the receipt key through the same
        // route constructor keeps reporting bound to the executed route.
        SourceBackedSelectorAuthority::ExplicitPath,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| false),
    )
    .map_err(anyhow::Error::from)
    .context("derive requested explicit source-backed route")?;
    Ok(route
        .metadata()
        .route_identity
        .as_ref()
        .context("requested explicit source-backed route has no identity")?
        .as_str()
        .to_owned())
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
