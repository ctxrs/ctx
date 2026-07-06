#[allow(unused_imports)]
use super::*;

#[derive(Debug, Subcommand)]
pub(crate) enum LocateTarget {
    #[command(about = "Locate provider/source metadata for a session")]
    Session(LocateSessionArgs),
    #[command(about = "Locate provider/source metadata for an event")]
    Event(LocateEventArgs),
}

pub(crate) type SourceInfo = ProviderSource;

pub(crate) fn source_visible_by_default(source: &SourceInfo) -> bool {
    source.exists
        || source.status != ProviderSourceStatus::Missing
        || DEFAULT_VISIBLE_SOURCE_PROVIDERS.contains(&source.provider)
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

pub(crate) fn source_format(metadata: &Value) -> Option<String> {
    for pointer in [
        "/source_format",
        "/format",
        "/provider/source_format",
        "/source/source_format",
    ] {
        if let Some(value) = metadata.pointer(pointer).and_then(|value| value.as_str()) {
            return Some(value.to_owned());
        }
    }
    None
}

pub(crate) fn filter_cli_supported_sources(sources: Vec<SourceInfo>) -> Vec<SourceInfo> {
    sources
        .into_iter()
        .filter(|source| cli_supported_provider(source.provider))
        .collect()
}

pub(crate) fn source_for_path(provider: CaptureProvider, path: PathBuf) -> SourceInfo {
    provider_source_for_path(provider, path)
}

pub(crate) fn sources_json(sources: &[SourceInfo]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "provider": source.provider.as_str(),
                "path": source.path,
                "exists": source.exists,
                "source_format": source.source_format,
                "status": source.status.as_str(),
                "import_support": import_support_json(source.import_support),
                "native_import": source.import_support.is_auto_importable(),
                "importable": source.status == ProviderSourceStatus::Available
                    && source.import_support.is_importable(),
                "raw_retention": raw_retention_json(source.raw_retention),
                "unsupported_reason": source.unsupported_reason,
            })
        })
        .collect()
}
