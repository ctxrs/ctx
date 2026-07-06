use super::*;

pub(crate) fn print_import_report(report: &ImportReport, json_output: bool) -> Result<()> {
    if json_output {
        print_json(import_report_json(report))
    } else {
        print_import_report_human(report);
        Ok(())
    }
}

pub(crate) fn import_report_json(report: &ImportReport) -> Value {
    json!({
        "schema_version": 1,
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
        "failed_sources": totals.failed_sources,
        "imported_sessions": totals.imported_sessions,
        "imported_events": totals.imported_events,
        "imported_edges": totals.imported_edges,
        "skipped_sessions": totals.skipped_sessions,
        "skipped_events": totals.skipped_events,
        "skipped_edges": totals.skipped_edges,
        "skipped": totals.skipped,
        "failed": totals.failed,
    })
}

pub(crate) fn print_import_report_human(report: &ImportReport) {
    println!("source_files: {}", report.totals.source_files);
    println!("source_bytes: {}", report.totals.source_bytes);
    println!("imported_sources: {}", report.totals.imported_sources);
    println!("failed_sources: {}", report.totals.failed_sources);
    println!("imported_sessions: {}", report.totals.imported_sessions);
    println!("imported_events: {}", report.totals.imported_events);
    println!("imported_edges: {}", report.totals.imported_edges);
    println!("skipped_sessions: {}", report.totals.skipped_sessions);
    println!("skipped_events: {}", report.totals.skipped_events);
    println!("skipped_edges: {}", report.totals.skipped_edges);
    println!("skipped: {}", report.totals.skipped);
    println!("failed: {}", report.totals.failed);
    println!("resume: {}", report.resume);
    println!("resume_mode: {}", report.resume_mode());
}

pub(crate) fn source_import_json(
    source: &SourceInfo,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": "imported",
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
        "failed": summary.failed,
        "failures": provider_failures_json(summary),
    })
}

pub(crate) fn custom_format_import_json(
    format: ImportFormatArg,
    path: &Path,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": "imported",
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
        "failed": summary.failed,
        "failures": provider_failures_json(summary),
    })
}

pub(crate) fn history_source_plugin_import_json(
    source: &HistorySourcePluginSource,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": "imported",
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
        "failed": summary.failed,
        "failures": provider_failures_json(summary),
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
    json!({
        "status": "failed",
        "provider": failure.source.provider.as_str(),
        "path": failure.source.path,
        "source_format": failure.source.source_format,
        "import_support": import_support_json(failure.source.import_support),
        "native_import": failure.source.import_support.is_auto_importable(),
        "importable": failure.source.import_support.is_importable()
            && failure.source.status == ProviderSourceStatus::Available,
        "source_files": failure.stats.files,
        "source_bytes": failure.stats.bytes,
        "error": source_error_reason(&failure.source, &failure.error),
    })
}

pub(crate) fn history_source_plugin_failure_json(
    source: &HistorySourcePluginSource,
    error: &str,
) -> Value {
    json!({
        "status": "failed",
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
        "error": one_line_error(error),
    })
}

pub(crate) fn print_source_imported(source: &SourceInfo, summary: &ProviderImportSummary) {
    println!(
        "imported {}: sessions={} events={} edges={} skipped={} failed={}",
        source.provider.as_str(),
        summary.imported_sessions,
        summary.imported_events,
        summary.imported_edges,
        summary.skipped,
        summary.failed
    );
}

pub(crate) fn print_history_source_plugin_imported(
    source: &HistorySourcePluginSource,
    summary: &ProviderImportSummary,
) {
    println!(
        "imported history source plugin {}: sessions={} events={} edges={} skipped={} failed={}",
        source.label(),
        summary.imported_sessions,
        summary.imported_events,
        summary.imported_edges,
        summary.skipped,
        summary.failed
    );
}

pub(crate) fn print_source_failed(failure: &ImportSourceFailure) {
    println!(
        "skipped {}: {}",
        failure.source.provider.as_str(),
        source_error_reason(&failure.source, &failure.error)
    );
    println!("  path: {}", failure.source.path.display());
}

pub(crate) fn print_history_source_plugin_failed(source: &HistorySourcePluginSource, error: &str) {
    println!(
        "skipped history source plugin {}: {}",
        source.label(),
        one_line_error(error)
    );
    println!("  manifest: {}", source.manifest_path.display());
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
        .last()
        .map(ToString::to_string)
        .unwrap_or_else(|| top.clone());
    if is_sqlite_busy_text(&top) || is_sqlite_busy_text(&root) {
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

pub(crate) fn import_error_is_systemic(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("database or disk is full")
        || lower.contains("ctx index is busy")
        || lower.contains("database is locked")
        || lower.contains("readonly database")
        || lower.contains("disk i/o error")
        || lower.contains("out of memory")
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
