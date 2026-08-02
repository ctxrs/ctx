use anyhow::Result;
use serde_json::{json, Value};

use ctx_history_capture::{
    CaptureError, ProviderSourceFailureKind, SourceBackedRouteError, SourceBackedRouteErrorKind,
};

use crate::commands::import::totals::ImportTotals;
use crate::commands::import::{
    import_report_analytics_outcome, import_report_failure_type, ImportReport,
};
use crate::compact_json;
use crate::output::print_json;
use crate::ui::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Outcome, OutcomeState,
    RenderContext, Ui,
};

pub(crate) fn print_import_report(
    report: &ImportReport,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    if json_output {
        print_json(import_report_json(report))
    } else {
        let document = render_import_report_human(ui.stdout_context(), report);
        ui.write_stdout(&document)?;
        Ok(())
    }
}

fn import_report_json(report: &ImportReport) -> Value {
    let (outcome, failure_scope) = import_report_analytics_outcome(&report.totals);
    json!({
        "schema_version": 2,
        "outcome": outcome,
        "failure_scope": failure_scope,
        "failure_type": import_report_failure_type(&report.totals),
        "resume": report.resume,
        "resume_mode": report.resume_mode(),
        "totals": import_totals_json(&report.totals),
        "sources": report.sources,
    })
}

fn import_totals_json(totals: &ImportTotals) -> Value {
    let mut value = compact_json(json!({
        "current_source_count": totals.current_source_count,
        "current_indexed_documents": totals.current_indexed_documents,
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
            "failed_sources": totals.failed_sources,
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
    } else if totals.failed_sources > 0 {
        let Value::Object(output) = &mut value else {
            unreachable!("import totals are always an object")
        };
        output.insert("failed_sources".to_owned(), json!(totals.failed_sources));
    }
    value
}

fn render_import_report_human(context: &RenderContext, report: &ImportReport) -> Document {
    let totals = &report.totals;
    let rejected_records = rejected_record_count(totals);
    let (state, title, detail) = import_outcome_copy(totals);
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail: Some(&detail),
        },
    );

    if totals.per_run_counts_available {
        let mut imported = vec![("Sources", totals.imported_sources.to_string())];
        push_nonzero(&mut imported, "Sessions", totals.imported_sessions);
        push_nonzero(&mut imported, "Events", totals.imported_events);
        push_nonzero(&mut imported, "Edges", totals.imported_edges);
        push_nonzero(&mut imported, "Skipped records", totals.skipped);
        push_nonzero(&mut imported, "Rejected records", totals.failed);
        push_nonzero(&mut imported, "Failed sources", totals.failed_sources);
        document.push_blank();
        document.append(section("Imported", fields_from_owned(context, &imported)));
    }

    let mut current = Vec::new();
    push_optional(&mut current, "Sources", totals.current_source_count);
    push_optional(
        &mut current,
        "Searchable events",
        totals.current_indexed_documents,
    );
    push_optional(
        &mut current,
        "Rejected records",
        totals.current_rejected_records,
    );
    push_optional(
        &mut current,
        "Sources with rejections",
        totals.current_sources_with_rejections,
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

    if totals.failed_sources > 0 || rejected_records > 0 {
        document.push_blank();
        let fully_failed = !totals.has_usable_source_result() && totals.failed_sources > 0;
        let (text, command) = if !fully_failed && rejected_records > 0 {
            (
                "Diagnose rejected records while keeping the imported history available.",
                "ctx doctor",
            )
        } else {
            (
                "Inspect source availability and import support.",
                "ctx sources",
            )
        };
        document.append(hint(context, Hint { text }, Some(Action { command })));
    }
    document
}

fn source_failure_fields(report: &ImportReport) -> Vec<(String, String)> {
    let mut fields = report
        .sources
        .iter()
        .filter(|source| {
            source.get("status").and_then(Value::as_str) == Some("failure")
                && source.get("failure_scope").and_then(Value::as_str) == Some("source")
                && source
                    .get("source_identity")
                    .and_then(Value::as_str)
                    .is_some()
        })
        .enumerate()
        .map(|(index, source)| {
            let selector = source
                .get("source_selector")
                .and_then(Value::as_str)
                .or_else(|| source.get("source_identity").and_then(Value::as_str))
                .unwrap_or("unknown source");
            let provider = source
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("unknown provider");
            let class = source
                .get("source_failure_class")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let retained = if source
                .get("carried_forward")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                ", retained prior data"
            } else {
                ""
            };
            let detail = source
                .get("detail")
                .and_then(Value::as_str)
                .or_else(|| source.get("error").and_then(Value::as_str))
                .unwrap_or("no detail reported");
            (
                format!("Source {}", index.saturating_add(1)),
                format!("{selector} ({provider}, {class}{retained}): {detail}"),
            )
        })
        .collect::<Vec<_>>();
    let omitted = report
        .sources
        .iter()
        .find_map(|source| {
            source
                .get("source_failures_omitted")
                .and_then(Value::as_u64)
                .or_else(|| {
                    source
                        .get("source_failures")
                        .and_then(|failures| failures.get("omitted"))
                        .and_then(Value::as_u64)
                })
        })
        .unwrap_or_default();
    if omitted > 0 {
        fields.push((
            "Additional".to_owned(),
            counted_failure(
                omitted,
                "source failure was omitted",
                "source failures were omitted",
            ),
        ));
    }
    fields
}

fn import_outcome_copy(totals: &ImportTotals) -> (OutcomeState, &'static str, String) {
    if !totals.has_usable_source_result() && totals.failed_sources > 0 {
        return (
            OutcomeState::Error,
            "History import failed",
            counted_failure(
                u64::try_from(totals.failed_sources).unwrap_or(u64::MAX),
                "source failed",
                "sources failed",
            ),
        );
    }
    let rejected_records = rejected_record_count(totals);
    if totals.failed_sources > 0 || rejected_records > 0 {
        let mut details = Vec::new();
        if totals.failed_sources > 0 {
            details.push(counted_failure(
                u64::try_from(totals.failed_sources).unwrap_or(u64::MAX),
                "source failed",
                "sources failed",
            ));
        }
        if rejected_records > 0 {
            details.push(counted_failure(
                rejected_records,
                "record was rejected",
                "records were rejected",
            ));
        }
        return (
            OutcomeState::Warning,
            if rejected_records > 0 {
                "History import completed with rejections"
            } else {
                "History import completed with source failures"
            },
            format!(
                "{}; imported history remains available.",
                details.join("; ")
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

fn rejected_record_count(totals: &ImportTotals) -> u64 {
    if totals.failed > 0 {
        u64::try_from(totals.failed).unwrap_or(u64::MAX)
    } else {
        totals.current_rejected_records.unwrap_or(0)
    }
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
pub(crate) enum ImportFailureScope {
    Source,
    System,
}

impl ImportFailureScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportFailureType {
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
    pub(crate) fn as_str(self) -> &'static str {
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

pub(crate) fn import_error_scope(error: &anyhow::Error) -> ImportFailureScope {
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

pub(crate) fn import_failure_type(error: &anyhow::Error) -> ImportFailureType {
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
mod tests {
    use std::{io::Write as _, path::Path};

    use ctx_history_capture::ProviderImportWorkResult;
    use unicode_width::UnicodeWidthStr as _;

    use crate::ui::{ColorMode, StreamKind, TestContext};

    use super::*;

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn changed_report() -> ImportReport {
        ImportReport {
            resume: true,
            totals: ImportTotals {
                per_run_counts_available: true,
                source_files: 1,
                source_bytes: 4096,
                imported_sources: 1,
                imported_sessions: 2,
                imported_events: 7,
                imported_edges: 1,
                skipped: 1,
                current_source_count: Some(1),
                current_indexed_documents: Some(7),
                current_complete_records: Some(7),
                current_retained_records: Some(7),
                current_rejected_records: Some(0),
                current_ignored_records: Some(1),
                current_certified_source_bytes: Some(4096),
                current_sources_with_rejections: Some(0),
                removed_source_count: Some(0),
                work_result: ProviderImportWorkResult::Changed,
                ..ImportTotals::default()
            },
            sources: vec![json!({"status": "published"})],
        }
    }

    #[test]
    fn human_import_report_is_outcome_first_and_omits_internal_fields() {
        let report = changed_report();
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_import_report_human(&context, &report);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("✓ History import completed\nLocal history changed.\n"));
            assert!(rendered.contains("\nImported\n"));
            assert!(rendered.contains("\nCurrent index\n"));
            for internal in [
                "outcome:",
                "failure_scope",
                "failure_type",
                "published_generation",
                "previous_generation",
                "generation_changed",
                "resume_mode",
                "current_source_count",
                "source_files",
            ] {
                assert!(
                    !rendered.contains(internal),
                    "human output exposed {internal:?}: {rendered}"
                );
            }
            let available = context.content_width().unwrap();
            for line in rendered.lines() {
                assert!(
                    line.width() <= available,
                    "{line:?} exceeded {available} columns"
                );
            }
        }
    }

    #[test]
    fn human_import_report_has_stable_copy_and_warning_recovery() {
        let success = render_import_report_human(&context(80, ColorMode::Never), &changed_report())
            .render_plain();
        assert_eq!(
            success,
            "✓ History import completed\n\
             Local history changed.\n\
             \n\
             Imported\n\
             Sources          1\n\
             Sessions         2\n\
             Events           7\n\
             Edges            1\n\
             Skipped records  1\n\
             \n\
             Current index\n\
             Sources                  1\n\
             Searchable events        7\n\
             Rejected records         0\n\
             Sources with rejections  0\n\
             Removed sources          0\n"
        );

        let report = ImportReport {
            resume: false,
            totals: ImportTotals {
                per_run_counts_available: true,
                imported_sources: 1,
                failed_sources: 1,
                failed: 2,
                work_result: ProviderImportWorkResult::Changed,
                ..ImportTotals::default()
            },
            sources: Vec::new(),
        };
        let warning =
            render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
        assert!(warning.starts_with(
            "! History import completed with rejections\n\
             1 source failed; 2 records were rejected; imported history remains available.\n"
        ));
        assert!(
            warning.ends_with(concat!(
                "Hint: Diagnose rejected records while keeping the imported history available.\n",
                "\n",
                "Next\n",
                "  ctx doctor\n",
            )),
            "{warning:?}"
        );
    }

    #[test]
    fn persisted_rejections_remain_a_human_warning_with_diagnosis() {
        let report = ImportReport {
            resume: true,
            totals: ImportTotals {
                current_source_count: Some(1),
                current_indexed_documents: Some(7),
                current_rejected_records: Some(2),
                current_sources_with_rejections: Some(1),
                work_result: ProviderImportWorkResult::NoOp,
                ..ImportTotals::default()
            },
            sources: Vec::new(),
        };

        let rendered =
            render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
        assert!(
            rendered.starts_with("! History import completed with rejections\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("2 records were rejected; imported history remains available."),
            "{rendered}"
        );
        assert!(
            rendered.contains("Searchable events        7"),
            "{rendered}"
        );
        assert!(rendered.ends_with("Next\n  ctx doctor\n"), "{rendered}");
    }

    #[test]
    fn retained_generation_with_source_failures_is_partial_and_reports_each_source() {
        let source_identity = "ab".repeat(32);
        let report = ImportReport {
            resume: false,
            totals: ImportTotals {
                failed_sources: 2,
                current_source_count: Some(2),
                current_indexed_documents: Some(7),
                work_result: ProviderImportWorkResult::NoOp,
                ..ImportTotals::default()
            },
            sources: vec![
                json!({
                    "status": "published",
                    "outcome": "completed_with_source_failures",
                    "successful_routes": 0,
                    "source_failure_total": 2,
                    "source_failures_omitted": 1,
                }),
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
                }),
            ],
        };

        let json = import_report_json(&report);
        assert_eq!(json["outcome"], "completed_with_source_failures");
        assert_eq!(json["failure_scope"], "source");
        assert_eq!(json["totals"]["failed_sources"], 2);
        for unsupported in [
            "source_files",
            "source_bytes",
            "imported_sources",
            "imported_sessions",
            "imported_events",
            "imported_edges",
        ] {
            assert!(json["totals"].get(unsupported).is_none(), "{json:#}");
        }
        assert_eq!(json["sources"][1]["source_identity"], source_identity);
        assert_eq!(json["sources"][1]["failure_scope"], "source");
        assert_eq!(json["sources"][1]["failure_type"], "other");
        assert_eq!(json["sources"][1]["source_failure_class"], "source_changed");
        assert_eq!(json["sources"][1]["imported_events"], 0);
        assert_eq!(json["sources"][1]["rejections"], json!([]));

        let rendered =
            render_import_report_human(&context(120, ColorMode::Never), &report).render_plain();
        assert!(
            rendered.starts_with("! History import completed with source failures\n"),
            "{rendered}"
        );
        assert!(rendered.contains("Source failures\n"), "{rendered}");
        assert!(rendered.contains("/history/session.jsonl"), "{rendered}");
        assert!(rendered.contains("source_changed"), "{rendered}");
        assert!(rendered.contains("retained prior data"), "{rendered}");
        assert!(
            rendered.contains("source changed during refresh"),
            "{rendered}"
        );
        assert!(
            rendered.contains("1 source failure was omitted"),
            "{rendered}"
        );
    }

    #[test]
    fn source_failures_without_a_usable_generation_remain_failure() {
        let report = ImportReport {
            resume: false,
            totals: ImportTotals {
                per_run_counts_available: true,
                failed_sources: 1,
                current_source_count: Some(0),
                work_result: ProviderImportWorkResult::NoOp,
                ..ImportTotals::default()
            },
            sources: Vec::new(),
        };

        assert_eq!(import_report_json(&report)["outcome"], "failure");
        let rendered =
            render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
        assert!(
            rendered.starts_with("✗ History import failed\n"),
            "{rendered}"
        );
    }

    #[test]
    fn import_json_contract_is_unchanged_by_human_renderer() {
        let value = import_report_json(&changed_report());
        assert_eq!(
            value,
            json!({
                "schema_version": 2,
                "outcome": "success",
                "failure_scope": "none",
                "failure_type": "none",
                "resume": true,
                "resume_mode": "idempotent_rescan",
                "totals": {
                    "source_files": 1,
                    "source_bytes": 4096,
                    "imported_sources": 1,
                    "sources_completed_with_rejections": 0,
                    "failed_sources": 0,
                    "imported_sessions": 2,
                    "imported_events": 7,
                    "imported_edges": 1,
                    "skipped_sessions": 0,
                    "skipped_events": 0,
                    "skipped_edges": 0,
                    "skipped": 1,
                    "rejected_records": 0,
                    "current_source_count": 1,
                    "current_indexed_documents": 7,
                    "current_complete_records": 7,
                    "current_retained_records": 7,
                    "current_rejected_records": 0,
                    "current_ignored_records": 1,
                    "current_certified_source_bytes": 4096,
                    "current_sources_with_rejections": 0,
                    "removed_source_count": 0,
                    "change": "changed"
                },
                "sources": [{"status": "published"}],
            })
        );
    }

    #[test]
    fn import_plain_output_equals_ansi_stripped_styled_output() {
        let report = changed_report();
        let context = context(80, ColorMode::Always);
        let document = render_import_report_human(&context, &report);
        let mut stream = anstream::StripStream::new(Vec::new());
        stream
            .write_all(document.render(&context).as_bytes())
            .unwrap();
        assert_eq!(
            String::from_utf8(stream.into_inner()).unwrap(),
            document.render_plain()
        );
    }

    #[test]
    fn provider_database_lock_is_source_scoped() {
        let sqlite = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_owned()),
        );
        let error = anyhow::Error::new(CaptureError::Sqlite(sqlite));
        assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
        assert_eq!(
            import_failure_type(&error),
            ImportFailureType::SourceDatabase
        );
    }

    #[test]
    fn typed_native_source_failures_keep_stable_classification() {
        let cases = [
            (
                ProviderSourceFailureKind::NotFound,
                ImportFailureType::NotFound,
            ),
            (
                ProviderSourceFailureKind::Permission,
                ImportFailureType::Permission,
            ),
            (
                ProviderSourceFailureKind::Locked,
                ImportFailureType::SourceDatabase,
            ),
            (
                ProviderSourceFailureKind::SchemaIncompatible,
                ImportFailureType::UnsupportedSchema,
            ),
            (
                ProviderSourceFailureKind::InvalidSource,
                ImportFailureType::MalformedSource,
            ),
            (
                ProviderSourceFailureKind::SourceChanged,
                ImportFailureType::Other,
            ),
        ];
        for (kind, expected) in cases {
            let error = anyhow::Error::new(CaptureError::ProviderSource {
                provider: "test",
                path: Path::new("provider.sqlite").to_path_buf(),
                kind,
                detail: "typed failure".to_owned(),
            });
            assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
            assert_eq!(import_failure_type(&error), expected);
        }
    }

    #[test]
    fn typed_source_backed_route_failures_keep_stable_classification() {
        let cases = [
            (
                SourceBackedRouteErrorKind::Unsupported,
                ImportFailureType::UnsupportedSchema,
            ),
            (
                SourceBackedRouteErrorKind::InvalidSource,
                ImportFailureType::MalformedSource,
            ),
        ];
        for (kind, expected) in cases {
            let error = anyhow::Error::new(SourceBackedRouteError::new(kind, "typed failure"));
            assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
            assert_eq!(import_failure_type(&error), expected);
        }
    }

    #[test]
    fn ctx_owned_io_is_system_scoped() {
        let error = anyhow::Error::new(CaptureError::SystemIo {
            operation: "publish source generation",
            source: std::io::Error::other("disk failure"),
        });
        assert_eq!(import_error_scope(&error), ImportFailureScope::System);
        assert_eq!(import_failure_type(&error), ImportFailureType::SystemIo);
    }
}
