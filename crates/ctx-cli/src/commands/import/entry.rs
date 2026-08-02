use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::analytics::{
    ImportFailureScope as AnalyticsImportFailureScope,
    ImportFailureType as AnalyticsImportFailureType, ImportOutcome as AnalyticsImportOutcome,
    ImportTelemetry, ProviderRefreshTrigger,
};
use crate::ui::{diagnostic, Diagnostic, DiagnosticLevel, Ui};
use crate::ImportArgs;

use super::provider_refresh::ProviderRefreshCollector;
use super::report::{import_error_scope, import_failure_type, print_import_report};
use super::{run_import_internal, ImportReport, ImportRunOptions, ImportTotals};

pub(crate) fn run_import(
    args: ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    config: &crate::config::AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    let json = args.format.is_json();
    if args.partial && !json {
        let document = diagnostic(
            ui.stderr_context(),
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: "--partial is deprecated",
                detail: Some(
                    "It no longer changes import behavior because tolerant import is always enabled.",
                ),
                fields: &[],
                action: None,
            },
        );
        ui.write_stderr(&document)?;
    }
    let progress = args.progress;
    provider_refreshes.start_timing();
    let report = run_import_internal(
        &args,
        data_root,
        telemetry,
        provider_refreshes,
        ProviderRefreshTrigger::Import,
        config,
        ImportRunOptions {
            progress,
            json,
            operation: "import",
        },
    );
    provider_refreshes.stop_timing();
    let report = match report {
        Ok(report) => report,
        Err(err) => {
            insert_import_error_analytics(telemetry, &err);
            return Err(err);
        }
    };
    insert_import_report_analytics(telemetry, &report);
    let (outcome, _) = import_report_analytics_outcome(&report.totals);
    print_import_report(&report, json, ui)?;
    if outcome == "failure" {
        let detail = report
            .sources
            .iter()
            .find_map(|source| source.get("error").and_then(Value::as_str))
            .map(|error| format!("; first failure: {error}"))
            .unwrap_or_default();
        return Err(anyhow!("all import sources failed{detail}"));
    }
    Ok(())
}

pub(crate) fn insert_import_report_analytics(
    telemetry: &mut ImportTelemetry,
    report: &ImportReport,
) {
    let (outcome, failure_scope) = import_report_analytics_outcome(&report.totals);
    telemetry.outcome = Some(match outcome {
        "success" => AnalyticsImportOutcome::Success,
        "failure" => AnalyticsImportOutcome::Failure,
        "completed_with_rejections" => AnalyticsImportOutcome::CompletedWithRejections,
        "completed_with_source_failures" => AnalyticsImportOutcome::CompletedWithSourceFailures,
        _ => AnalyticsImportOutcome::CompletedWithRejectionsAndSourceFailures,
    });
    telemetry.failure_scope = Some(match failure_scope {
        "none" => AnalyticsImportFailureScope::None,
        "record" => AnalyticsImportFailureScope::Record,
        "source" => AnalyticsImportFailureScope::Source,
        _ => AnalyticsImportFailureScope::RecordAndSource,
    });
    telemetry.failure_type = Some(match import_report_failure_type(&report.totals) {
        "none" => AnalyticsImportFailureType::None,
        "record_rejection" => AnalyticsImportFailureType::RecordRejection,
        "source_failure" => AnalyticsImportFailureType::SourceFailure,
        _ => AnalyticsImportFailureType::RecordRejectionAndSourceFailure,
    });
}

pub(crate) fn insert_import_error_analytics(
    telemetry: &mut ImportTelemetry,
    error: &anyhow::Error,
) {
    telemetry.outcome = Some(AnalyticsImportOutcome::Failure);
    telemetry.failure_scope = Some(match import_error_scope(error).as_str() {
        "record" => AnalyticsImportFailureScope::Record,
        "source" => AnalyticsImportFailureScope::Source,
        "record_and_source" => AnalyticsImportFailureScope::RecordAndSource,
        _ => AnalyticsImportFailureScope::Invocation,
    });
    telemetry.failure_type = Some(match import_failure_type(error).as_str() {
        "invalid_request" => AnalyticsImportFailureType::InvalidRequest,
        "io" => AnalyticsImportFailureType::Io,
        _ => AnalyticsImportFailureType::Other,
    });
}

pub(crate) fn import_report_analytics_outcome(
    totals: &ImportTotals,
) -> (&'static str, &'static str) {
    if !totals.has_usable_source_result() && totals.failed_sources > 0 {
        return ("failure", "source");
    }
    match (totals.failed_sources > 0, totals.failed > 0) {
        (false, false) => ("success", "none"),
        (false, true) => ("completed_with_rejections", "record"),
        (true, false) => ("completed_with_source_failures", "source"),
        (true, true) => (
            "completed_with_rejections_and_source_failures",
            "record_and_source",
        ),
    }
}

pub(crate) fn import_report_failure_type(totals: &ImportTotals) -> &'static str {
    match (totals.failed_sources > 0, totals.failed > 0) {
        (false, false) => "none",
        (false, true) => "record_rejection",
        (true, false) => "source_failure",
        (true, true) => "record_rejection_and_source_failure",
    }
}
