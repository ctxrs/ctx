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
    semantic::{
        SourceBackedRefreshCurrent, SourceBackedRefreshSourceFailure,
        SourceBackedRefreshSourceFailureClass,
    },
    ImportArgs,
};

use super::{
    core_refresh::{wait_for_import_core_refresh, ImportCoreRefreshRequest},
    ImportReport, ImportRunOptions, ImportTotals, ProviderRefreshCollector,
};

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
    super::validate_explicit_source_catalog_roots(&context.data_root)
        .context("validate explicit provider roots before initializing ctx state")?;
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
    let request_id = refresh.request_id.clone();
    let index = refresh.pin.into_index();
    let manifest = index.manifest();
    let current = receipt.current;
    context
        .provider_refreshes
        .record_core_publication(ProviderRefreshTrigger::Import, receipt.generation_changed);

    let completion = if context.options.progress == crate::progress::ProgressArg::Json {
        format!(
            "Published Core generation {}.",
            receipt.published_generation
        )
    } else {
        "Finished refreshing local history.".to_owned()
    };
    progress.finish_line()?;
    progress.done("published", completion, current.certified_source_bytes)?;

    let totals = automatic_refresh_totals(
        receipt.generation_changed,
        receipt.source_failures.total(),
        current,
    );
    let mut sources = vec![compact_json(json!({
        "status": "published",
        "outcome": receipt.terminal_outcome(),
        "source_format": "provider_authoritative_all",
        "change": if receipt.generation_changed { "changed" } else { "no_op" },
        "previous_generation": receipt.previous_generation,
        "published_generation": receipt.published_generation,
            "generation_changed": receipt.generation_changed,
            "scanned_routes": receipt.scanned_routes,
            "successful_routes": receipt.successful_routes,
            "source_failure_total": receipt.source_failures.total(),
            "source_failures_omitted": receipt.source_failures.omitted,
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
    }))];
    sources.extend(
        receipt
            .source_failures
            .failures
            .iter()
            .map(source_failure_report_row),
    );
    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources,
    })
}

fn automatic_refresh_totals(
    generation_changed: bool,
    failed_sources: usize,
    current: SourceBackedRefreshCurrent,
) -> ImportTotals {
    ImportTotals {
        // Daemon receipts certify current-generation state and route outcomes,
        // but do not attribute record or byte deltas to this import invocation.
        per_run_counts_available: false,
        failed_sources,
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if generation_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    }
}

fn source_failure_report_row(failure: &SourceBackedRefreshSourceFailure) -> serde_json::Value {
    let failure_type = match failure.class {
        SourceBackedRefreshSourceFailureClass::Incompatible => "unsupported_schema",
        SourceBackedRefreshSourceFailureClass::Unavailable
        | SourceBackedRefreshSourceFailureClass::SourceChanged
        | SourceBackedRefreshSourceFailureClass::Unreadable => "other",
    };
    compact_json(json!({
        "status": "failure",
        "failure_scope": "source",
        "failure_type": failure_type,
        "source_identity": failure.source_identity,
        "provider": failure.provider,
        "source_failure_class": failure.class.as_str(),
        "carried_forward": failure.carried_forward,
        "source_selector": failure.source_selector,
        "detail": failure.detail,
        "error": failure.detail,
        "source_files": 0,
        "source_bytes": 0,
        "imported_sessions": 0,
        "imported_events": 0,
        "imported_edges": 0,
        "skipped_sessions": 0,
        "skipped_events": 0,
        "skipped_edges": 0,
        "skipped": 0,
        "rejected_records": 0,
        "rejections": [],
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
    fn automatic_import_reports_bounded_source_result_contract() {
        let source = include_str!("automatic_source_refresh.rs");
        for required in [
            "receipt.terminal_outcome()",
            "receipt.scanned_routes",
            "receipt.successful_routes",
            "receipt.source_failures.total()",
            "receipt.source_failures.omitted",
            "failure.source_identity",
            "failure.class.as_str()",
            "failure.carried_forward",
            "failure.source_selector",
            "failure.detail",
        ] {
            assert!(
                source.contains(required),
                "automatic source report omitted `{required}`"
            );
        }
        for obsolete in [
            ["successful_route", "_ids"].concat(),
            ["route_", "identity"].concat(),
        ] {
            assert!(
                !source.contains(&obsolete),
                "automatic source report retained obsolete `{obsolete}`"
            );
        }
    }

    #[test]
    fn daemon_receipt_totals_do_not_invent_per_run_counts() {
        let totals = automatic_refresh_totals(
            true,
            2,
            SourceBackedRefreshCurrent {
                source_count: 3,
                indexed_documents: 11,
                certified_source_bytes: 4096,
                ..SourceBackedRefreshCurrent::default()
            },
        );

        assert!(!totals.per_run_counts_available);
        assert_eq!(totals.imported_sources, 0);
        assert_eq!(totals.imported_events, 0);
        assert_eq!(totals.source_bytes, 0);
        assert_eq!(totals.failed_sources, 2);
        assert_eq!(totals.current_source_count, Some(3));
        assert_eq!(totals.current_indexed_documents, Some(11));
    }

    #[test]
    fn source_failure_row_uses_schema_v2_shape() {
        let source_identity = "ab".repeat(32);
        let row = source_failure_report_row(&SourceBackedRefreshSourceFailure {
            source_identity: source_identity.clone(),
            provider: "codex".to_owned(),
            class: SourceBackedRefreshSourceFailureClass::SourceChanged,
            carried_forward: true,
            source_selector: "/history/session.jsonl".to_owned(),
            detail: "source changed during refresh".to_owned(),
        });

        assert_eq!(
            row,
            json!({
                "status": "failure",
                "failure_scope": "source",
                "failure_type": "other",
                "source_identity": source_identity,
                "provider": "codex",
                "source_failure_class": "source_changed",
                "carried_forward": true,
                "source_selector": "/history/session.jsonl",
                "detail": "source changed during refresh",
                "error": "source changed during refresh",
                "source_files": 0,
                "source_bytes": 0,
                "imported_sessions": 0,
                "imported_events": 0,
                "imported_edges": 0,
                "skipped_sessions": 0,
                "skipped_events": 0,
                "skipped_edges": 0,
                "skipped": 0,
                "rejected_records": 0,
                "rejections": [],
            })
        );
    }
}
