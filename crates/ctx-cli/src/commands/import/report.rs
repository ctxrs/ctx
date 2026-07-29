use anyhow::Result;
use serde_json::{json, Value};

use ctx_history_capture::{CaptureError, ProviderSourceFailureKind};

use crate::commands::import::totals::ImportTotals;
use crate::commands::import::{
    import_report_analytics_outcome, import_report_failure_type, ImportReport,
};
use crate::output::print_json;

pub(crate) fn print_import_report(report: &ImportReport, json_output: bool) -> Result<()> {
    if json_output {
        print_json(import_report_json(report))
    } else {
        print_import_report_human(report);
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
    json!({
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
        "change": totals.work_result.as_str(),
    })
}

fn print_import_report_human(report: &ImportReport) {
    let (outcome, failure_scope) = import_report_analytics_outcome(&report.totals);
    println!("outcome: {outcome}");
    println!("failure_scope: {failure_scope}");
    println!(
        "failure_type: {}",
        import_report_failure_type(&report.totals)
    );
    println!("source_files: {}", report.totals.source_files);
    println!("source_bytes: {}", report.totals.source_bytes);
    println!("imported_sources: {}", report.totals.imported_sources);
    println!(
        "sources_completed_with_rejections: {}",
        report.totals.sources_completed_with_rejections
    );
    println!("failed_sources: {}", report.totals.failed_sources);
    println!("imported_sessions: {}", report.totals.imported_sessions);
    println!("imported_events: {}", report.totals.imported_events);
    println!("imported_edges: {}", report.totals.imported_edges);
    println!("skipped_sessions: {}", report.totals.skipped_sessions);
    println!("skipped_events: {}", report.totals.skipped_events);
    println!("skipped_edges: {}", report.totals.skipped_edges);
    println!("skipped: {}", report.totals.skipped);
    println!("rejected_records: {}", report.totals.failed);
    println!("change: {}", report.totals.work_result.as_str());
    println!("resume: {}", report.resume);
    println!("resume_mode: {}", report.resume_mode());
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
    use std::path::Path;

    use super::*;

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
    fn ctx_owned_io_is_system_scoped() {
        let error = anyhow::Error::new(CaptureError::SystemIo {
            operation: "publish source generation",
            source: std::io::Error::other("disk failure"),
        });
        assert_eq!(import_error_scope(&error), ImportFailureScope::System);
        assert_eq!(import_failure_type(&error), ImportFailureType::SystemIo);
    }
}
