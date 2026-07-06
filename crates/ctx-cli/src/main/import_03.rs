#[allow(unused_imports)]
use super::*;

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

#[derive(Debug)]
pub(crate) struct ImportSourceOutcome {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) summary: ProviderImportSummary,
}

#[derive(Debug)]
pub(crate) struct ImportSourceFailure {
    pub(crate) index: usize,
    pub(crate) source: SourceInfo,
    pub(crate) stats: SourceStats,
    pub(crate) error: String,
}

#[derive(Debug)]
pub(crate) enum ImportSourceRun {
    Imported(ImportSourceOutcome),
    Failed(ImportSourceFailure),
}

impl ImportSourceRun {
    pub(crate) fn index(&self) -> usize {
        match self {
            Self::Imported(outcome) => outcome.index,
            Self::Failed(failure) => failure.index,
        }
    }
}

pub(crate) fn should_parallelize_import(planned_sources: &[(SourceInfo, SourceStats)]) -> bool {
    let _ = planned_sources;
    false
}

pub(crate) fn large_import_warning(
    planned_sources: &[(SourceInfo, SourceStats)],
    planned_total_bytes: u64,
) -> Option<String> {
    let planned_total_files = planned_sources
        .iter()
        .map(|(_, stats)| stats.files)
        .sum::<usize>();
    if planned_total_files < LARGE_IMPORT_SOURCE_FILES_WARNING
        && planned_total_bytes < LARGE_IMPORT_SOURCE_BYTES_WARNING
    {
        return None;
    }
    Some(format!(
        "large import: {} source file(s), {}; initial indexing may use sustained CPU and disk",
        planned_total_files,
        format_bytes(planned_total_bytes)
    ))
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

pub(crate) fn print_source_failed(failure: &ImportSourceFailure) {
    println!(
        "skipped {}: {}",
        failure.source.provider.as_str(),
        source_error_reason(&failure.source, &failure.error)
    );
    println!("  path: {}", failure.source.path.display());
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

pub(crate) fn import_error_is_systemic(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("database or disk is full")
        || lower.contains("ctx index is busy")
        || lower.contains("database is locked")
        || lower.contains("readonly database")
        || lower.contains("disk i/o error")
        || lower.contains("out of memory")
}

pub(crate) fn open_existing_store_read_only(db_path: &Path, command: &str) -> Result<Store> {
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx store is not initialized at {}; run `ctx setup` or `ctx import` first",
            db_path.display()
        ));
    }
    match Store::open_read_only(db_path) {
        Ok(store) => Ok(store),
        Err(StoreError::UnsupportedSchemaVersion(version)) => Err(anyhow!(
            "ctx store schema version {version} is not supported by this ctx binary; run `ctx status` once to migrate before using `{command}`"
        )),
        Err(err) => {
            Err(err).with_context(|| format!("open read-only ctx store {}", db_path.display()))
        }
    }
}

pub(crate) fn import_requests(args: &ImportArgs) -> Result<Vec<SourceInfo>> {
    if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(path) = &args.path {
        let provider = args
            .provider
            .context("ctx import --path requires --provider for native provider history")?
            .capture_provider();
        let source = explicit_path_source(provider, path.clone());
        if !source
            .path
            .try_exists()
            .with_context(|| format!("check import path {}", source.path.display()))?
        {
            return Err(anyhow!(
                "import path does not exist: {}",
                source.path.display()
            ));
        }
        validate_source_import_supported(&source)?;
        return Ok(vec![source]);
    }
    if args.all || args.provider.is_none() {
        return Ok(discovered_sources()
            .into_iter()
            .filter(|source| {
                source.exists
                    && source.import_support.is_auto_importable()
                    && source.status == ProviderSourceStatus::Available
            })
            .collect());
    }
    let provider = args.provider.expect("checked provider").capture_provider();
    let discovered = discovered_sources_for_provider(provider);
    let sources = discovered
        .iter()
        .filter(|source| {
            source.provider == provider
                && source.exists
                && source.import_support.is_importable()
                && source.status == ProviderSourceStatus::Available
        })
        .cloned()
        .collect::<Vec<_>>();
    if sources.is_empty() {
        let spec = provider_source_spec(provider);
        if spec
            .is_some_and(|spec| matches!(spec.import_support, ProviderImportSupport::Unsupported))
        {
            let reason = spec
                .and_then(|spec| spec.unsupported_reason)
                .unwrap_or("no native local-history parser is implemented");
            return Err(anyhow!(
                "{} native import is unsupported: {reason}",
                provider.as_str()
            ));
        }
        return Err(no_importable_provider_sources_error(provider, &discovered));
    }
    for source in &sources {
        validate_source_import_supported(source)?;
    }
    Ok(sources)
}

pub(crate) fn merge_provider_import_summary(
    summary: &mut ProviderImportSummary,
    other: ProviderImportSummary,
) {
    summary.imported += other.imported;
    summary.skipped += other.skipped;
    summary.failed += other.failed;
    summary.redacted += other.redacted;
    summary.imported_sessions += other.imported_sessions;
    summary.skipped_sessions += other.skipped_sessions;
    summary.imported_events += other.imported_events;
    summary.skipped_events += other.skipped_events;
    summary.imported_edges += other.imported_edges;
    summary.skipped_edges += other.skipped_edges;
    summary.failures.extend(other.failures);
}

pub(crate) fn source_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("stat import source {}", path.display()))?;
    if metadata.file_type().is_file() {
        return Ok(SourceStats {
            files: 1,
            bytes: metadata.len(),
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(SourceStats::default());
    }

    let mut stats = SourceStats::default();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("read import source directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("read import source entry under {}", dir.display()))?;
            let entry_path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", entry_path.display()))?;
            if file_type.is_dir() {
                stack.push(entry_path);
            } else if file_type.is_file() {
                let metadata = entry
                    .metadata()
                    .with_context(|| format!("stat import source file {}", entry_path.display()))?;
                stats.files += 1;
                stats.bytes = stats.bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(stats)
}

pub(crate) fn import_support_json(support: ProviderImportSupport) -> &'static str {
    match support {
        ProviderImportSupport::Native => "native",
        ProviderImportSupport::Explicit => "explicit",
        ProviderImportSupport::Unsupported => "unsupported",
    }
}
