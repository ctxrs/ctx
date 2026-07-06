#[allow(unused_imports)]
use super::*;

pub(crate) fn catalog_available_sources(
    store: &Store,
    sources: &[SourceInfo],
) -> Result<(CatalogTotals, Vec<Value>)> {
    let mut totals = CatalogTotals::default();
    let mut catalog_sources = Vec::new();
    for source in sources {
        if source.provider != CaptureProvider::Codex
            || source.source_format != "codex_session_jsonl_tree"
            || !source.exists
            || source.status != ProviderSourceStatus::Available
        {
            continue;
        }
        let summary = catalog_codex_session_tree(
            &source.path,
            store,
            CodexSessionCatalogOptions {
                source_root: Some(source.path.clone()),
                allow_partial_failures: true,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .with_context(|| format!("catalog Codex sessions from {}", source.path.display()))?;
        totals.add(&summary);
        catalog_sources.push(json!({
            "provider": source.provider.as_str(),
            "path": source.path,
            "source_format": source.source_format,
            "source_files": summary.source_files,
            "source_bytes": summary.source_bytes,
            "cataloged_sessions": summary.cataloged_sessions,
            "cached_sessions": summary.cached_sessions,
            "parsed_sessions": summary.parsed_sessions,
            "skipped_sessions": summary.skipped_sessions,
            "failed_sessions": summary.failed_sessions,
        }));
    }
    Ok((totals, catalog_sources))
}

pub(crate) fn provider_resume_json(
    provider: CaptureProvider,
    provider_session_id: Option<&str>,
) -> Value {
    let (command, argv) = match (provider, provider_session_id) {
        (CaptureProvider::Codex, Some(session_id)) => (
            Some(format!("codex resume {}", shell_quote_arg(session_id))),
            Some(vec![
                "codex".to_owned(),
                "resume".to_owned(),
                session_id.to_owned(),
            ]),
        ),
        _ => (None, None),
    };
    compact_json(json!({
        "available": command.is_some(),
        "command": command,
        "argv": argv,
    }))
}

pub(crate) fn validate_import_args(args: &ImportArgs) -> Result<()> {
    if args.path.is_some() && args.format.is_none() && args.provider.is_none() {
        return Err(anyhow!(
            "ctx import --path requires --provider for native provider history; use `ctx import --provider codex --path <path>` or `ctx import --format ctx-history-jsonl-v1 --path <file>`"
        ));
    }
    Ok(())
}

pub(crate) fn import_one_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
) -> Result<ProviderImportSummary> {
    let event_search_needs_backfill = store.event_search_projection_needs_backfill()?;
    let refresh_search_after_import =
        event_search_needs_backfill || !source_uses_incremental_event_search(source);
    import_one_source_inner(
        store,
        source,
        progress,
        refresh_search_after_import,
        full_rescan,
    )
}

pub(crate) fn import_one_source_without_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
) -> Result<ProviderImportSummary> {
    import_one_source_inner(store, source, progress, false, full_rescan)
}
