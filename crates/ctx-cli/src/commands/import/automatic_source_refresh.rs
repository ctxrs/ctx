use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ctx_history_capture::{
    discover_provider_sources, validate_provider_source_roots_outside_data_root,
    DiscoveryIssueKind, ProviderImportWorkResult, ProviderSourceStatus,
};
use ctx_history_core::platform_security::establish_private_data_root;
use serde_json::json;

use crate::{
    analytics::ProviderRefreshTrigger,
    compact_json,
    progress::ProgressReporter,
    provider_sources::{discovered_sources_for_provider_report, manual_path_guidance},
    ImportArgs,
};

use super::{
    core_refresh::{wait_for_import_core_refresh, ImportCoreRefreshRequest},
    ImportReport, ImportRunOptions, ImportTotals, ProviderRefreshCollector,
};

const MAX_REPORTED_SOURCE_FAILURES: usize = 3;

fn published_request_scanned_routes(scanned_routes: Option<usize>) -> Result<usize> {
    scanned_routes.context("published daemon source refresh omitted its scanned route count")
}

pub(super) struct AutomaticSourceRefreshImportContext<'a> {
    pub(super) args: &'a ImportArgs,
    pub(super) data_root: PathBuf,
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
    validate_selected_provider(context.args)?;

    let progress = ProgressReporter::new(
        context.options.progress,
        context.options.json,
        context.options.operation,
        0,
    );
    progress.message(
        "refreshing",
        "Refreshing the provider-authoritative source index through the ctx daemon.",
    )?;
    let home = crate::identity::home_dir()
        .context("resolve user home for provider-root safety preflight")?;
    let sources = discover_provider_sources(&home);
    validate_provider_source_roots_outside_data_root(&context.data_root, sources.iter())
        .context("validate provider roots before initializing ctx state")?;
    establish_private_data_root(&context.data_root)
        .context("protect ctx data root before provider refresh")?;
    let refresh = wait_for_import_core_refresh(
        &context.data_root,
        context.config,
        context.args.no_daemon,
        ImportCoreRefreshRequest::Automatic,
        &progress,
    )?;
    let receipt = refresh
        .receipt
        .clone()
        .context("daemon source refresh published without an authoritative terminal receipt")?;
    let request_previous_generation = refresh.request_previous_generation.clone();
    let request_generation_changed = refresh.request_generation_changed;
    let scanned_routes = published_request_scanned_routes(refresh.scanned_routes)?;
    let request_id = refresh.request_id.clone();
    let index = refresh.pin.into_index();
    let manifest = index.manifest();
    let current = receipt.current;
    let sources_completed_with_rejections = receipt
        .route_results
        .iter()
        .filter(|result| result.outcome.is_success() && result.rejected_record_total != 0)
        .count();
    let totals = ImportTotals {
        // Core receipts describe the committed current generation, not
        // synthetic per-run session/event/file totals.
        per_run_counts_available: false,
        terminal_route_counts_available: true,
        // Route-result counts are reported separately from per-run import
        // counts because the receipt certifies a whole Core generation.
        failed_sources: receipt.source_failure_total(),
        sources_completed_with_rejections,
        failed: usize::try_from(receipt.rejected_record_total()).unwrap_or(usize::MAX),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if request_generation_changed {
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

    let partial = receipt.source_failure_total() != 0 || receipt.rejected_record_total() != 0;
    let completion = if partial {
        format!(
            "Published valid history with {} source failure(s) and {} rejected record(s).",
            receipt.source_failure_total(),
            receipt.rejected_record_total()
        )
    } else if context.options.progress == crate::progress::ProgressArg::Json {
        format!(
            "Published Core generation {}.",
            receipt.published_generation
        )
    } else {
        "Finished refreshing local history.".to_owned()
    };
    progress.finish_line()?;
    // The receipt exposes retained corpus bytes, not work performed by this
    // invocation. Preserve the unknown per-run total instead of inventing a
    // 100% work-byte result on warm no-op imports.
    progress.done(if partial { "partial" } else { "published" }, completion, 0)?;

    let mut report_sources = vec![compact_json(json!({
        "status": if receipt.source_failure_total() != 0 || receipt.rejected_record_total() != 0 {
            "partial"
        } else {
            "published"
        },
        "failure_scope": match (
            receipt.source_failure_total() != 0,
            receipt.rejected_record_total() != 0,
        ) {
            (false, false) => "none",
            (false, true) => "record",
            (true, false) => "source",
            (true, true) => "record_and_source",
        },
        "failure_type": match (
            receipt.source_failure_total() != 0,
            receipt.rejected_record_total() != 0,
        ) {
            (false, false) => "none",
            (false, true) => "record_rejection",
            (true, false) => "source_failure",
            (true, true) => "record_rejection_and_source_failure",
        },
        "outcome": receipt.terminal_outcome(),
        "source_format": "provider_authoritative_all",
        "change": if request_generation_changed { "changed" } else { "no_op" },
        "previous_generation": request_previous_generation,
        "published_generation": receipt.published_generation,
        "generation_changed": request_generation_changed,
        "scanned_routes": scanned_routes,
        "successful_routes": receipt.successful_route_total(),
        "source_failure_total": receipt.source_failure_total(),
        "source_failures_omitted": receipt.source_failures_omitted()
            .saturating_add(receipt.source_failure_diagnostic_count()
                .saturating_sub(MAX_REPORTED_SOURCE_FAILURES)),
        "rejected_record_total": receipt.rejected_record_total(),
        "rejected_records": receipt.rejected_record_total(),
        "sources_completed_with_rejections": sources_completed_with_rejections,
        "rejections": {
            "rejected_records": receipt.rejected_record_total(),
            "sources_completed_with_rejections": sources_completed_with_rejections,
            "diagnostics_reported": receipt.rejection_diagnostic_count()
                .min(MAX_REPORTED_SOURCE_FAILURES),
            "diagnostics_omitted": receipt.rejection_diagnostics_omitted()
                .saturating_add(receipt.rejection_diagnostic_count()
                    .saturating_sub(MAX_REPORTED_SOURCE_FAILURES) as u64),
        },
        "rejection_diagnostics_omitted": receipt.rejection_diagnostics_omitted()
            .saturating_add(receipt.rejection_diagnostic_count()
                .saturating_sub(MAX_REPORTED_SOURCE_FAILURES) as u64),
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
            "operation": "import",
            "trigger": "import",
            "trigger_provenance": "automatic_provider_refresh",
        },
    }))];
    report_sources.extend(
        receipt
            .source_failures()
            .take(MAX_REPORTED_SOURCE_FAILURES)
            .map(|failure| {
                source_failure_report_row(
                    &failure.source_identity,
                    &failure.provider,
                    &failure.class,
                    failure.carried_forward,
                    &failure.source_selector,
                    &failure.detail,
                )
            }),
    );
    report_sources.extend(
        receipt
            .rejection_diagnostics()
            .take(MAX_REPORTED_SOURCE_FAILURES)
            .map(|rejection| {
                compact_json(json!({
                    "status": "rejection",
                    "failure_scope": "record",
                    "failure_type": "record_rejection",
                    "source_identity": rejection.source_identity,
                    "provider": rejection.provider,
                    "source_selector": rejection.source_selector,
                    "line": rejection.line,
                    "payload_type": rejection.payload_type,
                    "detail": rejection.detail,
                    "error": rejection.detail,
                    "source_files": 0,
                    "source_bytes": 0,
                }))
            }),
    );

    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources: report_sources,
    })
}

fn source_failure_report_row(
    source_identity: &str,
    provider: &str,
    class: &str,
    carried_forward: bool,
    source_selector: &str,
    detail: &str,
) -> serde_json::Value {
    let failure_type = if class == "incompatible" {
        "unsupported_schema"
    } else {
        "other"
    };
    compact_json(json!({
        "status": "failure",
        "failure_scope": "source",
        "failure_type": failure_type,
        "source_identity": source_identity,
        "provider": provider,
        "source_failure_class": class,
        "carried_forward": carried_forward,
        "source_selector": source_selector,
        "detail": detail,
        "error": detail,
        "source_files": 0,
        "source_bytes": 0,
    }))
}

fn validate_selected_provider(args: &ImportArgs) -> Result<()> {
    let Some(provider) = args.provider.map(|provider| provider.capture_provider()) else {
        return Ok(());
    };
    let report = discovered_sources_for_provider_report(provider);
    if report.sources.iter().any(|source| {
        source.status == ProviderSourceStatus::Available && source.import_support.is_importable()
    }) {
        return Ok(());
    }
    let provider_name = crate::provider_sources::provider_cli_name(provider);
    let guidance = manual_path_guidance(provider);
    if let Some(source) = report
        .sources
        .iter()
        .find(|source| source.status == ProviderSourceStatus::Unsupported)
    {
        bail!(
            "detected unsupported history at {}; current ctx cannot import that path for {provider_name}; use `{guidance}`",
            source.path.display()
        );
    }
    if let Some(issue) = report.issues.first() {
        let summary = match issue.kind {
            DiscoveryIssueKind::NoDiskHistory => {
                format!("{provider_name} has no disk history selected")
            }
            DiscoveryIssueKind::SelectorUnreconstructible => {
                format!("{provider_name} automatic history location cannot be safely reconstructed")
            }
            DiscoveryIssueKind::InsufficientOfficialEvidence => {
                format!("{provider_name} has no official automatic history location established")
            }
        };
        bail!("{summary}: {}; use `{guidance}`", issue.reason);
    }
    bail!(
        "no importable {provider_name} history source was discovered; use `{guidance}` to select one"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn source_failure_rows_follow_schema_v2_and_are_bounded() {
        let source_identity = "22".repeat(32);
        let row = source_failure_report_row(
            &source_identity,
            "codex",
            "source_changed",
            true,
            "/tmp/codex-history",
            "source changed during refresh",
        );
        assert_eq!(row["status"], "failure");
        assert_eq!(row["failure_scope"], "source");
        assert_eq!(row["failure_type"], "other");
        assert_eq!(row["source_failure_class"], "source_changed");
        assert_eq!(row["source_selector"], "/tmp/codex-history");
        assert_eq!(row["detail"], row["error"]);
        for unsupported in ["imported_sessions", "imported_events"] {
            assert!(row.get(unsupported).is_none(), "{row:#}");
        }
        assert_eq!(MAX_REPORTED_SOURCE_FAILURES, 3);
    }

    #[test]
    fn logical_publication_reports_zero_scans_without_receipt_count_fallback() {
        assert_eq!(published_request_scanned_routes(Some(0)).unwrap(), 0);
        assert!(published_request_scanned_routes(None).is_err());
    }
}
