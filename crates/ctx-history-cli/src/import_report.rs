use ctx_history_ingest_application::{ImportTotals, IngestReport, IngestSourceOutcome};
use serde_json::{json, Value};

use ctx_history_capture::{
    CaptureError, ProviderSourceFailureKind, SourceBackedRouteError, SourceBackedRouteErrorKind,
};

use crate::{import_presentation::source_json, output::compact_json};
use ctx_terminal::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Outcome, OutcomeState,
    RenderContext,
};

pub fn import_report_outcome(totals: &ImportTotals) -> (&'static str, &'static str) {
    let (outcome, scope) = totals.outcome();
    (outcome.as_str(), scope.as_str())
}

pub fn import_report_failure_type(totals: &ImportTotals) -> &'static str {
    totals.failure_type().as_str()
}

pub fn resume_mode_name(resume: bool) -> &'static str {
    if resume {
        "idempotent_rescan"
    } else {
        "normal_scan"
    }
}

pub fn import_completion_error(report: &IngestReport) -> Option<anyhow::Error> {
    (import_report_outcome(&report.totals).0 == "failure").then(|| {
        if report.totals.failed_sources == 0 && !report.totals.has_usable_source_result() {
            return anyhow::anyhow!("No usable history was imported");
        }
        let detail = report
            .first_failure_detail()
            .map(|(selector, failure_type, error)| {
                if failure_type
                    == ctx_history_ingest_application::IngestFailureType::UnsupportedSchema
                {
                    format!("{selector} is not importable: {error}")
                } else {
                    error.to_owned()
                }
            })
            .map(|error| format!("; first failure: {error}"))
            .unwrap_or_default();
        anyhow::anyhow!("all import sources failed{detail}")
    })
}

pub fn import_report_json(report: &IngestReport) -> Value {
    let (outcome, failure_scope) = import_report_outcome(&report.totals);
    let sources = report
        .sources
        .iter()
        .map(|source| source_json(source, "import"))
        .collect::<Vec<_>>();
    json!({
        "schema_version": 2,
        "outcome": outcome,
        "failure_scope": failure_scope,
        "failure_type": import_report_failure_type(&report.totals),
        "resume": report.resume,
        "resume_mode": resume_mode_name(report.resume),
        "totals": import_totals_json(&report.totals),
        "sources": sources,
    })
}

fn import_totals_json(totals: &ImportTotals) -> Value {
    let mut value = compact_json(json!({
        "current_source_count": totals.current_source_count,
        "current_indexed_sessions": totals.current_indexed_sessions,
        "current_indexed_documents": totals.current_indexed_documents,
        "index_delta": totals.index_delta.map(|delta| json!({
            "sessions": delta.sessions,
            "searchable_events": delta.searchable_events,
        })),
        "current_complete_records": totals.current_complete_records,
        "current_retained_records": totals.current_retained_records,
        "current_rejected_records": totals.current_rejected_records,
        "current_ignored_records": totals.current_ignored_records,
        "current_certified_source_bytes": totals.current_certified_source_bytes,
        "current_sources_with_rejections": totals.current_sources_with_rejections,
        "removed_source_count": totals.removed_source_count,
        "change": totals.work_result.as_str(),
    }));
    if totals.per_run_counts_available {
        let Value::Object(output) = &mut value else {
            unreachable!("import totals are always an object")
        };
        let Value::Object(per_run) = json!({
            "source_files": totals.source_files,
            "source_bytes": totals.source_bytes,
            "imported_sources": totals.imported_sources,
            "sources_completed_with_rejections": totals.sources_completed_with_rejections,
            "imported_sessions": totals.imported_sessions,
            "imported_events": totals.imported_events,
            "imported_edges": totals.imported_edges,
            "skipped_sessions": totals.skipped_sessions,
            "skipped_events": totals.skipped_events,
            "skipped_edges": totals.skipped_edges,
            "skipped": totals.skipped,
            "rejected_records": totals.failed,
        }) else {
            unreachable!("per-run import totals are always an object")
        };
        output.extend(per_run);
    } else if totals.terminal_route_counts_available {
        let Value::Object(output) = &mut value else {
            unreachable!("import totals are always an object")
        };
        output.insert(
            "sources_completed_with_rejections".to_owned(),
            json!(totals.sources_completed_with_rejections),
        );
        output.insert("failed_sources".to_owned(), json!(totals.failed_sources));
        output.insert("rejected_records".to_owned(), json!(totals.failed));
        output.insert(
            "rejections".to_owned(),
            json!({
                "rejected_records": totals.failed,
                "sources_completed_with_rejections": totals.sources_completed_with_rejections,
            }),
        );
    } else if totals.reported_source_failures() > 0 {
        let Value::Object(output) = &mut value else {
            unreachable!("import totals are always an object")
        };
        output.insert(
            "failed_sources".to_owned(),
            json!(totals.reported_source_failures()),
        );
    }
    value
}

pub fn render_import_report_human(context: &RenderContext, report: &IngestReport) -> Document {
    let totals = &report.totals;
    let (state, title, detail) = import_outcome_copy(totals);
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail: Some(&detail),
        },
    );

    if let Some(delta) = totals.index_delta {
        let net_change = [
            ("Sessions", signed_count(delta.sessions)),
            ("Searchable events", signed_count(delta.searchable_events)),
        ];
        document.push_blank();
        document.append(section(
            "Net index change",
            fields_from_owned(context, &net_change),
        ));
    }

    let mut imported = Vec::new();
    if totals.per_run_counts_available {
        imported.push(("Sources", totals.imported_sources.to_string()));
        push_nonzero(&mut imported, "Sessions", totals.imported_sessions);
        push_nonzero(&mut imported, "Events", totals.imported_events);
        push_nonzero(&mut imported, "Edges", totals.imported_edges);
        push_nonzero(&mut imported, "Failed sources", totals.failed_sources);
    }
    imported.push((
        "Skipped records",
        totals.skipped.saturating_add(totals.failed).to_string(),
    ));
    document.push_blank();
    document.append(section("Imported", fields_from_owned(context, &imported)));

    let mut current = Vec::new();
    push_optional(&mut current, "Sources", totals.current_source_count);
    push_optional(&mut current, "Sessions", totals.current_indexed_sessions);
    push_optional(
        &mut current,
        "Searchable events",
        totals.current_indexed_documents,
    );
    push_optional(&mut current, "Removed sources", totals.removed_source_count);
    if !current.is_empty() {
        document.push_blank();
        document.append(section(
            "Current index",
            fields_from_owned(context, &current),
        ));
    }

    let source_failures = source_failure_fields(report);
    if !source_failures.is_empty() {
        let source_failure_fields = source_failures
            .iter()
            .map(|(label, value)| Field::new(label, value))
            .collect::<Vec<_>>();
        document.push_blank();
        document.append(section(
            "Source failures",
            fields(context, &source_failure_fields),
        ));
    }

    if totals.failed_sources > 0 {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Inspect source availability and import support.",
            },
            Some(Action {
                command: "ctx sources",
            }),
        ));
    }
    document
}

fn signed_count(value: i64) -> String {
    if value > 0 {
        format!("+{value}")
    } else {
        value.to_string()
    }
}

const MAX_HUMAN_SOURCE_FAILURES: usize = 3;

struct HumanSourceFailure<'a> {
    selector: &'a str,
    provider: &'a str,
    class: &'a str,
    carried_forward: bool,
    detail: &'a str,
    unsupported_schema: bool,
}

fn source_failure_fields(report: &IngestReport) -> Vec<(String, String)> {
    let failures = report
        .sources
        .iter()
        .filter_map(human_source_failure)
        .collect::<Vec<_>>();
    let mut fields = failures
        .iter()
        .take(MAX_HUMAN_SOURCE_FAILURES)
        .enumerate()
        .map(|(index, source)| {
            let retained = if source.carried_forward {
                ", retained prior data"
            } else {
                ""
            };
            let disposition = if source.unsupported_schema {
                " is not importable"
            } else {
                ""
            };
            (
                format!("Source {}", index.saturating_add(1)),
                format!(
                    "{}{disposition} ({}, {}{retained}): {}",
                    source.selector, source.provider, source.class, source.detail
                ),
            )
        })
        .collect::<Vec<_>>();
    let displayed = fields.len();
    let summary_omitted = report
        .sources
        .iter()
        .find_map(|source| {
            let IngestSourceOutcome::Automatic(automatic) = source else {
                return None;
            };
            Some(automatic.source_failures_omitted)
        })
        .unwrap_or_default();
    let omitted = report
        .totals
        .reported_source_failures()
        .saturating_sub(displayed)
        .max(failures.len().saturating_sub(displayed))
        .max(summary_omitted);
    if omitted > 0 {
        fields.push((
            "Additional".to_owned(),
            counted_failure(
                u64::try_from(omitted).unwrap_or(u64::MAX),
                "source failure was omitted",
                "source failures were omitted",
            ),
        ));
    }
    fields
}

fn human_source_failure(source: &IngestSourceOutcome) -> Option<HumanSourceFailure<'_>> {
    match source {
        IngestSourceOutcome::SourceFailure(failure) => Some(HumanSourceFailure {
            selector: &failure.source_selector,
            provider: &failure.provider,
            class: &failure.source_failure_class,
            carried_forward: failure.carried_forward,
            detail: &failure.detail,
            unsupported_schema: failure.failure_type
                == ctx_history_ingest_application::IngestFailureType::UnsupportedSchema,
        }),
        IngestSourceOutcome::Exact(exact)
            if exact.status == ctx_history_ingest_application::IngestStatus::Failure
                && exact.failure_scope
                    == ctx_history_ingest_application::IngestFailureScope::Source
                && exact.route_source_failure_total != 0 =>
        {
            Some(HumanSourceFailure {
                selector: exact
                    .requested_failure
                    .as_ref()
                    .map(|failure| failure.source_selector.as_str())
                    .unwrap_or(""),
                provider: exact.provider.as_str(),
                class: exact
                    .requested_failure_class
                    .as_deref()
                    .unwrap_or("unknown"),
                carried_forward: exact
                    .requested_failure
                    .as_ref()
                    .is_some_and(|failure| failure.carried_forward),
                detail: exact
                    .requested_failure
                    .as_ref()
                    .map(|failure| failure.detail.as_str())
                    .unwrap_or("source failure detail omitted from bounded diagnostics"),
                unsupported_schema: exact.failure_type
                    == ctx_history_ingest_application::IngestFailureType::UnsupportedSchema,
            })
        }
        _ => None,
    }
}

fn import_outcome_copy(totals: &ImportTotals) -> (OutcomeState, &'static str, String) {
    if totals.outcome().0 == ctx_history_ingest_application::ImportOutcome::Failure {
        return (
            OutcomeState::Error,
            "History import failed",
            "No usable history was imported.".to_owned(),
        );
    }
    if totals.failed_sources > 0 {
        return (
            OutcomeState::Warning,
            "History import completed with source failures",
            format!(
                "{}; imported history remains available.",
                counted_failure(
                    u64::try_from(totals.failed_sources).unwrap_or(u64::MAX),
                    "source failed",
                    "sources failed",
                )
            ),
        );
    }
    (
        OutcomeState::Success,
        "History import completed",
        if totals.work_result == ctx_history_capture::ProviderImportWorkResult::Changed {
            "Local history changed.".to_owned()
        } else {
            "No source changes were found.".to_owned()
        },
    )
}

fn counted_failure(count: u64, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn push_nonzero(values: &mut Vec<(&'static str, String)>, label: &'static str, value: usize) {
    if value > 0 {
        values.push((label, value.to_string()));
    }
}

fn push_optional<T>(values: &mut Vec<(&'static str, String)>, label: &'static str, value: Option<T>)
where
    T: ToString,
{
    if let Some(value) = value {
        values.push((label, value.to_string()));
    }
}

fn fields_from_owned(context: &RenderContext, values: &[(&'static str, String)]) -> Document {
    let values = values
        .iter()
        .map(|(label, value)| Field::new(label, value))
        .collect::<Vec<_>>();
    fields(context, &values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailureScope {
    Source,
    System,
}

impl ImportFailureScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailureType {
    UnsupportedSchema,
    NotFound,
    Permission,
    SourceDatabase,
    MalformedSource,
    WorkerPanic,
    SystemIo,
    System,
    Other,
}

impl ImportFailureType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedSchema => "unsupported_schema",
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::SourceDatabase => "source_database",
            Self::MalformedSource => "malformed_source",
            Self::WorkerPanic => "worker_panic",
            Self::SystemIo => "system_io",
            Self::System => "system",
            Self::Other => "other",
        }
    }
}

pub fn import_error_scope(error: &anyhow::Error) -> ImportFailureScope {
    if error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(
                CaptureError::WorkerPanicked(_)
                    | CaptureError::SystemIo { .. }
                    | CaptureError::SystemInvariant(_)
            )
        )
    }) {
        ImportFailureScope::System
    } else {
        ImportFailureScope::Source
    }
}

pub fn import_failure_type(error: &anyhow::Error) -> ImportFailureType {
    for cause in error.chain() {
        if let Some(capture) = cause.downcast_ref::<CaptureError>() {
            return match capture {
                CaptureError::WorkerPanicked(_) => ImportFailureType::WorkerPanic,
                CaptureError::SystemIo { .. } => ImportFailureType::SystemIo,
                CaptureError::SystemInvariant(_) => ImportFailureType::System,
                CaptureError::ProviderSource { kind, .. } => match kind {
                    ProviderSourceFailureKind::NotFound => ImportFailureType::NotFound,
                    ProviderSourceFailureKind::Permission => ImportFailureType::Permission,
                    ProviderSourceFailureKind::Locked
                    | ProviderSourceFailureKind::Corrupt
                    | ProviderSourceFailureKind::SourceDatabase => {
                        ImportFailureType::SourceDatabase
                    }
                    ProviderSourceFailureKind::SchemaIncompatible => {
                        ImportFailureType::UnsupportedSchema
                    }
                    ProviderSourceFailureKind::InvalidSource => ImportFailureType::MalformedSource,
                    ProviderSourceFailureKind::SourceChanged | ProviderSourceFailureKind::Io => {
                        ImportFailureType::Other
                    }
                },
                CaptureError::UnsupportedSchemaVersion(_) | CaptureError::UnsupportedSchema(_) => {
                    ImportFailureType::UnsupportedSchema
                }
                CaptureError::Io(error) => match error.kind() {
                    std::io::ErrorKind::NotFound => ImportFailureType::NotFound,
                    std::io::ErrorKind::PermissionDenied => ImportFailureType::Permission,
                    _ => ImportFailureType::Other,
                },
                CaptureError::Sqlite(_) => ImportFailureType::SourceDatabase,
                CaptureError::Json(_)
                | CaptureError::InvalidPayload(_)
                | CaptureError::InvalidJsonLine { .. } => ImportFailureType::MalformedSource,
                _ => ImportFailureType::Other,
            };
        }
        if let Some(route) = cause.downcast_ref::<SourceBackedRouteError>() {
            return match route.kind {
                SourceBackedRouteErrorKind::Unsupported => ImportFailureType::UnsupportedSchema,
                SourceBackedRouteErrorKind::InvalidSource => ImportFailureType::MalformedSource,
                _ => ImportFailureType::Other,
            };
        }
        if let Some(error) = cause.downcast_ref::<std::io::Error>() {
            return match error.kind() {
                std::io::ErrorKind::NotFound => ImportFailureType::NotFound,
                std::io::ErrorKind::PermissionDenied => ImportFailureType::Permission,
                _ => ImportFailureType::Other,
            };
        }
        if cause.downcast_ref::<rusqlite::Error>().is_some() {
            return ImportFailureType::SourceDatabase;
        }
    }
    ImportFailureType::Other
}

#[cfg(test)]
mod tests;
