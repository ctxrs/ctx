use std::path::{Path, PathBuf};

use ctx_history_core::CaptureProvider;
use ctx_history_store::{ProviderSourceRouteRetirementReason, Store};

use super::{
    discover_codex_catalog_sources, prepare_codex_native_output_replay,
    prepare_codex_native_source, retire_codex_native_source_route,
    retire_replaced_codex_native_source_route, CodexCatalogSource, CodexNativeOutputReplay,
    CodexNativePreparedSource, CodexNativeRootGroup, CodexNativeSourceAdmission,
    CodexNativeStoreOptions,
};
use crate::{
    provider::codex::catalog::{catalog_codex_session_files, catalog_codex_session_tree},
    summaries::MAX_RETAINED_PROVIDER_FAILURES,
    CaptureError, CodexSessionCatalogOptions, CodexSessionImportOptions,
    CodexSessionImportProgress, ImportProfile, ProOutputSinkError, ProviderImportFailure,
    ProviderImportSummary, Result,
};

const MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES: usize = 192;

pub(crate) fn import_codex_native_session_root(
    root: &Path,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = options.source_path.as_deref().unwrap_or(root).to_path_buf();
    if !root.exists() {
        return retire_missing_root(&source_root, store, &options);
    }
    let catalog = catalog_codex_session_tree(
        root,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(source_root.clone()),
            cataloged_at: options.imported_at,
            max_session_files: options.max_session_files,
            max_total_bytes: options.max_total_bytes,
            ..CodexSessionCatalogOptions::default()
        },
    )?;
    let mut summary = ProviderImportSummary {
        skipped_sessions: catalog.skipped_sessions,
        skipped: catalog.skipped_sessions,
        failed: catalog.failed_sessions,
        failures: catalog.failures,
        ..ProviderImportSummary::default()
    };
    import_cataloged_root(&source_root, store, &options, &mut summary)?;
    Ok(summary)
}

pub(crate) fn import_codex_native_session_files(
    paths: Vec<PathBuf>,
    store: &mut Store,
    mut options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    if paths.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| common_root(&paths));
    options.source_path = Some(source_root.clone());
    let catalog = catalog_codex_session_files(
        paths,
        &source_root,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(source_root.clone()),
            cataloged_at: options.imported_at,
            max_session_files: options.max_session_files,
            max_total_bytes: options.max_total_bytes,
            ..CodexSessionCatalogOptions::default()
        },
    )?;
    let mut summary = ProviderImportSummary {
        skipped_sessions: catalog.skipped_sessions,
        skipped: catalog.skipped_sessions,
        failed: catalog.failed_sessions,
        failures: catalog.failures,
        ..ProviderImportSummary::default()
    };
    import_cataloged_root(&source_root, store, &options, &mut summary)?;
    Ok(summary)
}

fn import_cataloged_root(
    source_root: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let root = source_root.display().to_string();
    let sessions = store.list_catalog_sessions_for_source(CaptureProvider::Codex, &root)?;
    let discovery = discover_codex_catalog_sources(&sessions);
    let mut live_sources = discovery
        .sources
        .iter()
        .filter(|source| source.source_path.is_file())
        .cloned()
        .collect::<Vec<_>>();
    parent_first(&mut live_sources);
    let total_files = live_sources.len();
    let total_bytes = live_sources.iter().fold(0_u64, |total, source| {
        total.saturating_add(
            source
                .source_path
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        )
    });
    let missing_sources = discovery
        .sources
        .iter()
        .filter(|source| !source.source_path.is_file())
        .cloned()
        .collect::<Vec<_>>();
    summary.skipped = summary
        .skipped
        .saturating_add(discovery.ineligible)
        .saturating_add(discovery.rejections.len());
    summary.skipped_sessions = summary
        .skipped_sessions
        .saturating_add(discovery.ineligible)
        .saturating_add(discovery.rejections.len());
    summary
        .failures
        .extend(
            discovery
                .rejections
                .into_iter()
                .map(|rejection| ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {}", rejection.source_path, rejection.reason),
                }),
        );

    let mut output_replays = Vec::<CodexNativeOutputReplay>::new();
    let output_sink = match &options.import_profile {
        ImportProfile::CoreAndPro(sink) | ImportProfile::ProReplayOnly(sink) => Some(sink.as_ref()),
        ImportProfile::CoreOnly => None,
    };
    if let ImportProfile::ProReplayOnly(sink) = &options.import_profile {
        let native_options = CodexNativeStoreOptions {
            machine_id: options.machine_id.clone(),
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        };
        for source in live_sources {
            match prepare_codex_native_output_replay(
                store,
                source,
                native_options.clone(),
                sink.as_ref(),
            ) {
                Ok(replay) => replay_output(replay, sink.as_ref()),
                Err(error) => sink.mark_behind(ProOutputSinkError::new(
                    "codex_nativepath_output_prepare",
                    error.to_string(),
                )),
            }
        }
        return Ok(());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut completed_catalog_sources = std::collections::BTreeMap::<String, u64>::new();
    let mut completed_files = 0_usize;
    let mut completed_bytes = 0_u64;
    let import = (|| -> Result<()> {
        let mut pending_group = CodexNativeRootGroup::default();
        let native_options = CodexNativeStoreOptions {
            machine_id: options.machine_id.clone(),
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        };
        for source in missing_sources {
            if retire_codex_native_source_route(
                store,
                &bulk_guard,
                &source,
                &native_options,
                ProviderSourceRouteRetirementReason::SourceMissing,
            )
            .map_err(native_error)?
            {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            }
        }
        for source in live_sources {
            let source_bytes = source
                .source_path
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if retire_replaced_codex_native_source_route(
                store,
                &bulk_guard,
                &source,
                &native_options,
            )
            .map_err(native_error)?
            {
                publish_pending_group(store, &bulk_guard, &mut pending_group)?;
            }
            let replay_source = source.clone();
            let prepared = match prepare_codex_native_source(
                store,
                CodexNativeSourceAdmission::Live(source),
                native_options.clone(),
                output_sink,
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    summary.failed = summary.failed.saturating_add(1);
                    summary.failures.push(ProviderImportFailure {
                        line: 0,
                        error: error.to_string(),
                    });
                    completed_files = completed_files.saturating_add(1);
                    completed_bytes = completed_bytes.saturating_add(source_bytes);
                    report_import_progress(
                        options,
                        total_files,
                        total_bytes,
                        completed_files,
                        completed_bytes,
                        summary,
                        false,
                    );
                    continue;
                }
            };
            match prepared {
                CodexNativePreparedSource::Noop(noop) => {
                    summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                    summary.skipped_events =
                        summary.skipped_events.saturating_add(noop.skipped_events);
                    if noop.terminal {
                        completed_catalog_sources.insert(
                            replay_source.source_path.display().to_string(),
                            noop.retained_events,
                        );
                    }
                    if let Some(sink) = output_sink {
                        match prepare_codex_native_output_replay(
                            store,
                            replay_source,
                            native_options.clone(),
                            sink,
                        ) {
                            Ok(replay) => output_replays.push(replay),
                            Err(error) => sink.mark_behind(ProOutputSinkError::new(
                                "codex_nativepath_output_prepare",
                                error.to_string(),
                            )),
                        }
                    }
                }
                CodexNativePreparedSource::Publication(mut publication) => {
                    let rejected = publication.rejected_records;
                    let rejections = std::mem::take(&mut publication.rejections);
                    let imported_events = publication.imported_events;
                    let imported_edges = publication.imported_edges;
                    let skipped_events = publication.skipped_events;
                    let terminal = publication.terminal;
                    let retained_events = publication.retained_events;
                    let (chunks, output_replay) =
                        publication.into_root_parts().map_err(native_error)?;
                    if let Some(output_replay) = output_replay {
                        output_replays.push(output_replay);
                    }
                    let split_source = chunks.len() > 1;
                    for chunk in chunks {
                        if split_source || !chunk.terminal() {
                            publish_pending_group(store, &bulk_guard, &mut pending_group)?;
                            let mut isolated = CodexNativeRootGroup::default();
                            isolated.try_push(chunk).map_err(|_| {
                                CaptureError::SystemInvariant(
                                    "certified Codex source chunk exceeds root group bounds",
                                )
                            })?;
                            isolated.publish(store, &bulk_guard).map_err(native_error)?;
                            continue;
                        }
                        if let Err(chunk) = pending_group.try_push(chunk) {
                            publish_pending_group(store, &bulk_guard, &mut pending_group)?;
                            pending_group.try_push(chunk).map_err(|_| {
                                CaptureError::SystemInvariant(
                                    "certified Codex source chunk exceeds empty root group bounds",
                                )
                            })?;
                        }
                    }
                    summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                    summary.imported_events =
                        summary.imported_events.saturating_add(imported_events);
                    summary.imported_edges = summary.imported_edges.saturating_add(imported_edges);
                    summary.skipped_events = summary
                        .skipped_events
                        .saturating_add(skipped_events)
                        .saturating_add(rejected);
                    record_native_rejections(summary, &replay_source, rejected, rejections);
                    if terminal {
                        completed_catalog_sources.insert(
                            replay_source.source_path.display().to_string(),
                            retained_events,
                        );
                    }
                }
            }
            // Progress is a committed-state contract: when a caller asks for
            // it, flush the bounded source group before exposing its counters.
            // Imports without a callback retain cross-source group coalescing.
            if options.progress.is_some() {
                publish_pending_group(store, &bulk_guard, &mut pending_group)?;
            }
            completed_files = completed_files.saturating_add(1);
            completed_bytes = completed_bytes.saturating_add(source_bytes);
            report_import_progress(
                options,
                total_files,
                total_bytes,
                completed_files,
                completed_bytes,
                summary,
                false,
            );
        }
        publish_pending_group(store, &bulk_guard, &mut pending_group)?;
        Ok(())
    })();
    let finish = store.finish_event_search_bulk_mode(&bulk_guard);
    import?;
    finish?;
    for session in &sessions {
        if let Some(event_count) = completed_catalog_sources.get(&session.source_path) {
            store.mark_catalog_source_observation_indexed(
                session,
                None,
                Some(*event_count),
                options.imported_at.timestamp_millis(),
            )?;
        }
    }
    if let Some(sink) = output_sink {
        for replay in output_replays {
            replay_output(replay, sink);
        }
    }
    report_import_progress(
        options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        summary,
        true,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report_import_progress(
    options: &CodexSessionImportOptions,
    total_files: usize,
    total_bytes: u64,
    completed_files: usize,
    completed_bytes: u64,
    summary: &ProviderImportSummary,
    done: bool,
) {
    let Some(callback) = &options.progress else {
        return;
    };
    callback(CodexSessionImportProgress {
        source_path: options.source_path.clone(),
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        imported_sessions: summary.imported_sessions,
        imported_events: summary.imported_events,
        imported_edges: summary.imported_edges,
        skipped: summary.skipped,
        failed: summary.failed,
        done,
    });
}

fn replay_output(replay: CodexNativeOutputReplay, sink: &dyn crate::ProOutputSink) {
    match replay.replay(sink) {
        Ok(results) => {
            for result in results {
                if let Err(failure) = result {
                    sink.mark_behind(ProOutputSinkError::new(
                        "codex_nativepath_output_page",
                        format!("{failure:?}"),
                    ));
                }
            }
        }
        Err(error) => sink.mark_behind(ProOutputSinkError::new(
            "codex_nativepath_output_replay",
            error.to_string(),
        )),
    }
}

fn retire_missing_root(
    source_root: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let root = source_root.display().to_string();
    let sessions = store.list_catalog_sessions_for_source(CaptureProvider::Codex, &root)?;
    let discovery = discover_codex_catalog_sources(&sessions);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let native_options = CodexNativeStoreOptions {
        machine_id: options.machine_id.clone(),
        imported_at: options.imported_at,
        history_record_id: options.history_record_id,
    };
    let retire = (|| -> Result<ProviderImportSummary> {
        let mut summary = ProviderImportSummary::default();
        for source in discovery.sources {
            if retire_codex_native_source_route(
                store,
                &bulk_guard,
                &source,
                &native_options,
                ProviderSourceRouteRetirementReason::RootMissing,
            )
            .map_err(native_error)?
            {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            }
        }
        Ok(summary)
    })();
    let finish = store.finish_event_search_bulk_mode(&bulk_guard);
    let summary = retire?;
    finish?;
    Ok(summary)
}

fn parent_first(sources: &mut Vec<CodexCatalogSource>) {
    let mut remaining = std::mem::take(sources);
    remaining.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    let mut ordered = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let known = ordered
            .iter()
            .filter_map(|source: &CodexCatalogSource| source.catalog_native_session_id.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        let ready = remaining.iter().position(|source| {
            source
                .catalog_parent_native_session_id
                .as_deref()
                .is_none_or(|parent| {
                    known.contains(parent)
                        || !remaining.iter().any(|candidate| {
                            candidate.catalog_native_session_id.as_deref() == Some(parent)
                        })
                })
        });
        let index = ready.unwrap_or(0);
        ordered.push(remaining.remove(index));
    }
    *sources = ordered;
}

fn publish_pending_group(
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    pending: &mut CodexNativeRootGroup,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    std::mem::take(pending)
        .publish(store, bulk_guard)
        .map(|_| ())
        .map_err(native_error)
}

fn common_root(paths: &[PathBuf]) -> PathBuf {
    let mut root = paths[0]
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    while !paths.iter().all(|path| path.starts_with(&root)) {
        if !root.pop() {
            return PathBuf::from(".");
        }
    }
    root
}

fn record_native_rejections(
    summary: &mut ProviderImportSummary,
    source: &CodexCatalogSource,
    rejected: usize,
    rejections: Vec<super::reader::CodexRecordRejection>,
) {
    summary.failed = summary.failed.saturating_add(rejected);
    let source_label = bounded_rejection_source_label(source);
    let remaining = MAX_RETAINED_PROVIDER_FAILURES.saturating_sub(summary.failures.len());
    for rejection in rejections.into_iter().take(rejected.min(remaining)) {
        summary.failures.push(ProviderImportFailure {
            line: rejection_line(rejection.raw_ordinal),
            error: format!(
                concat!(
                    "Codex NativePath rejected record from source \"{}\" ",
                    "at raw ordinal {} (bytes {}..{}): {}"
                ),
                source_label,
                rejection.raw_ordinal,
                rejection.start_byte,
                rejection.end_byte,
                rejection.reason,
            ),
        });
    }
}

fn rejection_line(raw_ordinal: u64) -> usize {
    usize::try_from(raw_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .unwrap_or(usize::MAX)
}

fn bounded_rejection_source_label(source: &CodexCatalogSource) -> String {
    let root = Path::new(&source.source_root);
    let candidate = source
        .source_path
        .strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(Path::as_os_str)
        .or_else(|| source.source_path.file_name());
    let Some(candidate) = candidate else {
        return "<unnamed-codex-source>".to_owned();
    };
    let rendered = candidate.to_string_lossy();
    let mut label = String::new();
    let mut truncated = false;
    'characters: for character in rendered.chars() {
        for escaped in character.escape_default() {
            if label.len() >= MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES.saturating_sub(3) {
                truncated = true;
                break 'characters;
            }
            label.push(escaped);
        }
    }
    if truncated {
        label.push_str("...");
    }
    if label.is_empty() {
        "<unnamed-codex-source>".to_owned()
    } else {
        label
    }
}

fn native_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("Codex NativePath import failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejection_source_label_is_path_free_escaped_and_bounded() {
        let source = CodexCatalogSource {
            source_root: "/private/source/root".to_owned(),
            source_path: Path::new("/private/source/root/2026/07").join(format!(
                "rollout\n{}-secret.jsonl",
                "x".repeat(MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES * 2)
            )),
            cataloged_at_ms: 0,
            catalog_observation: super::super::CodexFileObservation {
                len: 0,
                modified_at_ms: 0,
                change_token: [0; 32],
            },
            catalog_native_session_id: None,
            catalog_parent_native_session_id: None,
            catalog_root_native_session_id: None,
        };
        let label = bounded_rejection_source_label(&source);

        assert!(label.len() <= MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES);
        assert!(!label.contains("/private/source/root"));
        assert!(label.contains("2026"));
        assert!(label.contains("rollout"));
        assert!(!label.contains('\n'));
        assert!(label.contains("\\n"));
        assert!(label.ends_with("..."));
    }
}
