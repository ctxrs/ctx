use super::*;

fn source_import_status(summary: &ProviderImportSummary) -> &'static str {
    if summary.failed > 0 && !provider_summary_has_imported_content(summary) {
        "failure"
    } else if summary.failed > 0 {
        "completed_with_rejections"
    } else {
        "success"
    }
}

fn source_failure_scope(summary: &ProviderImportSummary) -> &'static str {
    if summary.failed > 0 && !provider_summary_has_imported_content(summary) {
        "source"
    } else if summary.failed > 0 {
        "record"
    } else {
        "none"
    }
}

fn source_failure_type_for_summary(summary: &ProviderImportSummary) -> &'static str {
    if summary.failed > 0 {
        "record_rejection"
    } else {
        "none"
    }
}

pub(crate) fn print_import_report(report: &ImportReport, json_output: bool) -> Result<()> {
    if json_output {
        print_json(import_report_json(report))
    } else {
        print_import_report_human(report);
        Ok(())
    }
}

pub(crate) fn import_report_json(report: &ImportReport) -> Value {
    let (outcome, failure_scope) = import_report_analytics_outcome(&report.totals);
    let failure_type = import_report_failure_type(&report.totals);
    json!({
        "schema_version": 2,
        "outcome": outcome,
        "failure_scope": failure_scope,
        "failure_type": failure_type,
        "resume": report.resume,
        "resume_mode": report.resume_mode(),
        "totals": import_totals_json(&report.totals),
        "sources": report.sources.clone(),
    })
}

pub(crate) fn import_totals_json(totals: &ImportTotals) -> Value {
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
    })
}

pub(crate) fn print_import_report_human(report: &ImportReport) {
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
    println!("resume: {}", report.resume);
    println!("resume_mode: {}", report.resume_mode());
}

pub(crate) fn source_import_json(
    source: &SourceInfo,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": source_import_status(summary),
        "failure_scope": source_failure_scope(summary),
        "failure_type": source_failure_type_for_summary(summary),
        "provider": source.provider.as_str(),
        "path": source.path,
        "source_format": source.source_format,
        "import_support": import_support_json(source.import_support),
        "native_import": source.import_support.is_auto_importable(),
        "importable": source.import_support.is_importable()
            && source.status == ProviderSourceStatus::Available,
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "imported_sessions": summary.imported_sessions,
        "imported_events": summary.imported_events,
        "imported_edges": summary.imported_edges,
        "skipped_sessions": summary.skipped_sessions,
        "skipped_events": summary.skipped_events,
        "skipped_edges": summary.skipped_edges,
        "skipped": summary.skipped,
        "rejected_records": summary.failed,
        "rejections": provider_failures_json(summary),
    })
}

pub(crate) fn custom_format_import_json(
    format: ImportFormatArg,
    path: &Path,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": source_import_status(summary),
        "failure_scope": source_failure_scope(summary),
        "failure_type": source_failure_type_for_summary(summary),
        "provider": CaptureProvider::Custom.as_str(),
        "path": path,
        "format": format.as_str(),
        "source_format": format.as_str(),
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "imported_sessions": summary.imported_sessions,
        "imported_events": summary.imported_events,
        "imported_edges": summary.imported_edges,
        "skipped_sessions": summary.skipped_sessions,
        "skipped_events": summary.skipped_events,
        "skipped_edges": summary.skipped_edges,
        "skipped": summary.skipped,
        "rejected_records": summary.failed,
        "rejections": provider_failures_json(summary),
    })
}

pub(crate) fn custom_format_failure_json(
    format: ImportFormatArg,
    path: &Path,
    stats: &SourceStats,
    error: &str,
    failure_type: ImportFailureType,
) -> Value {
    json!({
        "status": "failure",
        "failure_scope": "source",
        "failure_type": failure_type.as_str(),
        "provider": CaptureProvider::Custom.as_str(),
        "path": path,
        "format": format.as_str(),
        "source_format": format.as_str(),
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "imported_sessions": 0,
        "imported_events": 0,
        "imported_edges": 0,
        "skipped_sessions": 0,
        "skipped_events": 0,
        "skipped_edges": 0,
        "skipped": 0,
        "error": one_line_error(error),
        "rejected_records": 0,
        "rejections": [],
    })
}

pub(crate) fn history_source_plugin_import_json(
    source: &HistorySourcePluginSource,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": source_import_status(summary),
        "failure_scope": source_failure_scope(summary),
        "failure_type": source_failure_type_for_summary(summary),
        "provider": CaptureProvider::Custom.as_str(),
        "kind": "history_source_plugin",
        "plugin": source.plugin_name,
        "history_source": source.label(),
        "provider_key": source.provider_key,
        "source_id": source.source_id,
        "source_format": source.source_format,
        "manifest_path": source.manifest_path,
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "imported_sessions": summary.imported_sessions,
        "imported_events": summary.imported_events,
        "imported_edges": summary.imported_edges,
        "skipped_sessions": summary.skipped_sessions,
        "skipped_events": summary.skipped_events,
        "skipped_edges": summary.skipped_edges,
        "skipped": summary.skipped,
        "rejected_records": summary.failed,
        "rejections": provider_failures_json(summary),
    })
}

pub(crate) fn provider_failures_json(summary: &ProviderImportSummary) -> Vec<Value> {
    summary
        .failures
        .iter()
        .take(5)
        .map(|failure| {
            json!({
                "line": failure.line,
                "error": failure.error,
            })
        })
        .collect()
}

pub(crate) fn source_failure_json(failure: &ImportSourceFailure) -> Value {
    let mut value = json!({
        "status": "failure",
        "failure_scope": "source",
        "failure_type": failure.failure_type.as_str(),
        "provider": failure.source.provider.as_str(),
        "path": failure.source.path,
        "source_format": failure.source.source_format,
        "import_support": import_support_json(failure.source.import_support),
        "native_import": failure.source.import_support.is_auto_importable(),
        "importable": failure.source.import_support.is_importable()
            && failure.source.status == ProviderSourceStatus::Available,
        "source_files": failure.stats.files,
        "source_bytes": failure.stats.bytes,
        "imported_sessions": 0,
        "imported_events": 0,
        "imported_edges": 0,
        "skipped_sessions": 0,
        "skipped_events": 0,
        "skipped_edges": 0,
        "skipped": 0,
        "error": source_error_reason(&failure.source, &failure.error),
        "rejected_records": 0,
        "rejections": [],
    });
    if let Some(summary) = failure.rejected_summary.as_ref() {
        value["skipped_sessions"] = json!(summary.skipped_sessions);
        value["skipped_events"] = json!(summary.skipped_events);
        value["skipped_edges"] = json!(summary.skipped_edges);
        value["skipped"] = json!(summary.skipped);
        value["rejected_records"] = json!(summary.failed);
        value["rejections"] = json!(provider_failures_json(summary));
    }
    value
}

pub(crate) fn history_source_plugin_failure_json(
    source: &HistorySourcePluginSource,
    error: &str,
    rejected_summary: Option<&ProviderImportSummary>,
    failure_type: ImportFailureType,
) -> Value {
    let mut value = json!({
        "status": "failure",
        "failure_scope": "source",
        "failure_type": failure_type.as_str(),
        "provider": CaptureProvider::Custom.as_str(),
        "kind": "history_source_plugin",
        "plugin": source.plugin_name,
        "history_source": source.label(),
        "provider_key": source.provider_key,
        "source_id": source.source_id,
        "source_format": source.source_format,
        "manifest_path": source.manifest_path,
        "source_files": 0,
        "source_bytes": 0,
        "imported_sessions": 0,
        "imported_events": 0,
        "imported_edges": 0,
        "skipped_sessions": 0,
        "skipped_events": 0,
        "skipped_edges": 0,
        "skipped": 0,
        "error": one_line_error(error),
        "rejected_records": 0,
        "rejections": [],
    });
    if let Some(summary) = rejected_summary {
        value["skipped_sessions"] = json!(summary.skipped_sessions);
        value["skipped_events"] = json!(summary.skipped_events);
        value["skipped_edges"] = json!(summary.skipped_edges);
        value["skipped"] = json!(summary.skipped);
        value["rejected_records"] = json!(summary.failed);
        value["rejections"] = json!(provider_failures_json(summary));
    }
    value
}

pub(crate) fn print_source_imported(source: &SourceInfo, summary: &ProviderImportSummary) {
    let outcome = if summary.failed > 0 {
        "completed with rejected records"
    } else {
        "imported"
    };
    println!(
        "{outcome} {}: sessions={} events={} edges={} skipped={} rejected={}",
        source.provider.as_str(),
        summary.imported_sessions,
        summary.imported_events,
        summary.imported_edges,
        summary.skipped,
        summary.failed
    );
    print_provider_failures(summary);
}

pub(crate) fn print_history_source_plugin_imported(
    source: &HistorySourcePluginSource,
    summary: &ProviderImportSummary,
) {
    let outcome = if summary.failed > 0 {
        "completed with rejected records"
    } else {
        "imported"
    };
    println!(
        "{outcome} history source plugin {}: sessions={} events={} edges={} skipped={} rejected={}",
        source.label(),
        summary.imported_sessions,
        summary.imported_events,
        summary.imported_edges,
        summary.skipped,
        summary.failed
    );
    print_provider_failures(summary);
}

pub(crate) fn print_provider_failures(summary: &ProviderImportSummary) {
    if summary.failed == 0 {
        return;
    }
    for failure in summary.failures.iter().take(5) {
        println!("  rejected line {}: {}", failure.line, failure.error);
    }
    if summary.failures.len() > 5 {
        println!(
            "  ... {} more rejected record(s)",
            summary.failures.len().saturating_sub(5)
        );
    }
}

pub(crate) fn print_source_failed(failure: &ImportSourceFailure) {
    println!(
        "skipped {}: {}",
        failure.source.provider.as_str(),
        source_error_reason(&failure.source, &failure.error)
    );
    println!("  path: {}", failure.source.path.display());
    if let Some(summary) = failure.rejected_summary.as_ref() {
        print_provider_failures(summary);
    }
}

pub(crate) fn print_history_source_plugin_failed(
    source: &HistorySourcePluginSource,
    error: &str,
    rejected_summary: Option<&ProviderImportSummary>,
) {
    println!(
        "skipped history source plugin {}: {}",
        source.label(),
        one_line_error(error)
    );
    println!("  manifest: {}", source.manifest_path.display());
    if let Some(summary) = rejected_summary {
        print_provider_failures(summary);
    }
}

pub(crate) fn source_error_reason(source: &SourceInfo, error: &str) -> String {
    let error = one_line_error(error);
    let prefix = format!(
        "import {} source {}: ",
        source.provider.as_str(),
        source.path.display()
    );
    error.strip_prefix(&prefix).unwrap_or(&error).to_owned()
}

pub(crate) fn one_line_error(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error")
        .to_owned()
}

pub(crate) fn error_summary(error: &anyhow::Error) -> String {
    let top = error.to_string();
    let root = error
        .chain()
        .next_back()
        .map(ToString::to_string)
        .unwrap_or_else(|| top.clone());
    if import_error_scope(error) == ImportFailureScope::System
        && (is_sqlite_busy_text(&top) || is_sqlite_busy_text(&root))
    {
        return "ctx index is busy because another ctx import or search refresh is writing to the local database; retry in a moment, or rerun the search with `--refresh off` to use the existing index".to_owned();
    }
    if root == top || top.contains(&root) {
        top
    } else {
        format!("{top}: {root}")
    }
}

pub(crate) fn is_sqlite_busy_text(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("database is locked") || lower.contains("database table is locked")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportFailureScope {
    Source,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportFailureType {
    RecordRejection,
    UnsupportedSchema,
    NotFound,
    Permission,
    SourceDatabase,
    MalformedSource,
    Store,
    WorkerPanic,
    SystemIo,
    System,
    Other,
}

impl ImportFailureType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RecordRejection => "record_rejection",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::SourceDatabase => "source_database",
            Self::MalformedSource => "malformed_source",
            Self::Store => "store",
            Self::WorkerPanic => "worker_panic",
            Self::SystemIo => "system_io",
            Self::System => "system",
            Self::Other => "other",
        }
    }
}

impl ImportFailureScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::System => "system",
        }
    }
}

pub(crate) fn import_error_scope(error: &anyhow::Error) -> ImportFailureScope {
    if error.chain().any(|cause| {
        cause.downcast_ref::<StoreError>().is_some()
            || matches!(
                cause.downcast_ref::<CaptureError>(),
                Some(CaptureError::Store(_) | CaptureError::WorkerPanicked(_))
                    | Some(CaptureError::SystemIo { .. } | CaptureError::SystemInvariant(_))
            )
    }) {
        ImportFailureScope::System
    } else {
        ImportFailureScope::Source
    }
}

pub(crate) fn import_failure_type(error: &anyhow::Error) -> ImportFailureType {
    if rejected_source_summary(error).is_some() {
        return ImportFailureType::RecordRejection;
    }
    if error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(CaptureError::WorkerPanicked(_))
        )
    }) {
        return ImportFailureType::WorkerPanic;
    }
    if error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(CaptureError::SystemIo { .. })
        )
    }) {
        return ImportFailureType::SystemIo;
    }
    if error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(CaptureError::SystemInvariant(_))
        )
    }) {
        return ImportFailureType::System;
    }
    if import_error_scope(error) == ImportFailureScope::System {
        return ImportFailureType::Store;
    }
    for cause in error.chain() {
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
    for cause in error.chain() {
        if let Some(capture) = cause.downcast_ref::<CaptureError>() {
            return match capture {
                CaptureError::UnsupportedSchemaVersion(_) => ImportFailureType::UnsupportedSchema,
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
    }
    ImportFailureType::Other
}
pub(crate) fn low_disk_space_warning(db_path: &Path, planned_total_bytes: u64) -> Option<String> {
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let available = available_space_bytes(parent)?;
    let recommended = (planned_total_bytes / 4).clamp(1 << 30, 20 * (1 << 30));
    if available < recommended {
        Some(format!(
            "low disk space: {} available near {}, {} recommended before indexing {}",
            format_bytes(available),
            parent.display(),
            format_bytes(recommended),
            format_bytes(planned_total_bytes)
        ))
    } else {
        None
    }
}

#[cfg(unix)]
pub(crate) fn available_space_bytes(path: &Path) -> Option<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    fn statvfs_field_to_u64<T>(value: T) -> Option<u64>
    where
        T: TryInto<u64>,
    {
        value.try_into().ok()
    }

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let available_blocks = statvfs_field_to_u64(stat.f_bavail)?;
    let fragment_size = statvfs_field_to_u64(stat.f_frsize)?;
    Some(available_blocks.saturating_mul(fragment_size))
}

#[cfg(not(unix))]
pub(crate) fn available_space_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
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
        assert!(!error_summary(&error).contains("ctx index is busy"));
    }

    #[test]
    fn ctx_store_checkpoint_lock_is_system_scoped() {
        let error = anyhow::Error::new(StoreError::WalCheckpointBusy {
            log_frames: 2,
            checkpointed_frames: 1,
        });

        assert_eq!(import_error_scope(&error), ImportFailureScope::System);
        assert_eq!(import_failure_type(&error), ImportFailureType::Store);
    }

    #[test]
    fn rejected_records_have_stable_typed_classification() {
        let mut summary = ProviderImportSummary::default();
        summary.failed = 1;
        let error = rejected_source_error("records rejected".to_owned(), &summary);

        assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
        assert_eq!(
            import_failure_type(&error),
            ImportFailureType::RecordRejection
        );
    }

    #[test]
    fn worker_panics_have_stable_typed_classification() {
        let error = anyhow::Error::new(CaptureError::WorkerPanicked("provider import"));

        assert_eq!(import_error_scope(&error), ImportFailureScope::System);
        assert_eq!(import_failure_type(&error), ImportFailureType::WorkerPanic);
    }

    #[test]
    fn raw_source_io_and_sqlite_errors_have_stable_typed_classification() {
        let missing = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "source disappeared",
        ));
        assert_eq!(import_error_scope(&missing), ImportFailureScope::Source);
        assert_eq!(import_failure_type(&missing), ImportFailureType::NotFound);

        let sqlite = anyhow::Error::new(rusqlite::Error::InvalidQuery);
        assert_eq!(import_error_scope(&sqlite), ImportFailureScope::Source);
        assert_eq!(
            import_failure_type(&sqlite),
            ImportFailureType::SourceDatabase
        );
    }

    #[test]
    fn ctx_owned_io_is_system_scoped() {
        let error = anyhow::Error::new(CaptureError::SystemIo {
            operation: "write cursor temp file",
            source: std::io::Error::other("disk failure"),
        });

        assert_eq!(import_error_scope(&error), ImportFailureScope::System);
        assert_eq!(import_failure_type(&error), ImportFailureType::SystemIo);
    }

    #[test]
    fn run_outcomes_cover_clean_rejected_and_failed_combinations() {
        let cases = [
            (ImportTotals::default(), ("success", "none"), "none"),
            (
                ImportTotals {
                    imported_sources: 1,
                    failed: 1,
                    ..ImportTotals::default()
                },
                ("completed_with_rejections", "record"),
                "record_rejection",
            ),
            (
                ImportTotals {
                    imported_sources: 1,
                    failed_sources: 1,
                    ..ImportTotals::default()
                },
                ("completed_with_source_failures", "source"),
                "source_failure",
            ),
            (
                ImportTotals {
                    imported_sources: 1,
                    failed_sources: 1,
                    failed: 1,
                    ..ImportTotals::default()
                },
                (
                    "completed_with_rejections_and_source_failures",
                    "record_and_source",
                ),
                "record_rejection_and_source_failure",
            ),
            (
                ImportTotals {
                    failed_sources: 1,
                    failed: 1,
                    ..ImportTotals::default()
                },
                ("failure", "source"),
                "record_rejection_and_source_failure",
            ),
        ];

        for (totals, expected_outcome, expected_type) in cases {
            assert_eq!(import_report_analytics_outcome(&totals), expected_outcome);
            assert_eq!(import_report_failure_type(&totals), expected_type);
        }
    }
}
