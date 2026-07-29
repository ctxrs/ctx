use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::{EventSearchBulkGuard, ProviderSourceRouteRetirementReason, Store};

use super::vertical::{
    publication::CodexNativeRootPublication, CodexNativeCommittedDelta, CodexNativeTerminalReport,
    CodexNativeVerticalError,
};
use super::{
    discover_codex_catalog_sources, finish_pending_codex_native_retirement,
    prepare_codex_native_output_replay, prepare_codex_native_producer_task,
    retire_codex_native_source_route, retire_replaced_codex_native_source_route,
    run_codex_bounded_producers, CodexCatalogSource, CodexNativeOutputReplay,
    CodexNativeProducerStep, CodexNativeRootGroup, CodexNativeStoreOptions,
    CodexOrderedProducerItem, CodexProducerConfig,
};
use crate::{
    common::io::ProviderSourceRoot,
    provider::codex::catalog::{
        catalog_codex_session_files, catalog_codex_session_tree_retained,
        ensure_catalog_source_bound,
    },
    summaries::MAX_RETAINED_PROVIDER_FAILURES,
    CaptureError, CatalogSummary, CodexSessionCatalogOptions, CodexSessionImportOptions,
    CodexSessionImportProgress, ImportProfile, ProOutputSinkError, ProviderImportFailure,
    ProviderImportSummary, Result,
};

mod ordering;

use ordering::parent_first;

const MAX_CODEX_REJECTION_SOURCE_LABEL_BYTES: usize = 192;
struct PendingCodexRootWindow {
    source: CodexCatalogSource,
    source_done: bool,
    delta: CodexNativeCommittedDelta,
    report: Option<CodexNativeTerminalReport>,
}

pub(crate) fn import_codex_native_session_root(
    root: &Path,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    import_codex_native_session_root_with_catalog(root, store, options).map(|(_, summary)| summary)
}

pub(crate) fn import_codex_native_session_root_with_catalog(
    root: &Path,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<(CatalogSummary, ProviderImportSummary)> {
    let source_root = options.source_path.as_deref().unwrap_or(root).to_path_buf();
    let retained_catalog = match catalog_codex_session_tree_retained(
        root,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(source_root.clone()),
            cataloged_at: options.imported_at,
            max_session_files: options.max_session_files,
            max_total_bytes: options.max_total_bytes,
            ..CodexSessionCatalogOptions::default()
        },
    ) {
        Ok(catalog) => catalog,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return retire_missing_root(&source_root, store, &options)
                .map(|summary| (CatalogSummary::default(), summary));
        }
        Err(error) => return Err(error),
    };
    let catalog = retained_catalog.summary;
    let mut summary = ProviderImportSummary {
        skipped_sessions: catalog.skipped_sessions,
        skipped: catalog.skipped_sessions,
        failed: catalog.failed_sessions,
        failures: catalog.failures.clone(),
        ..ProviderImportSummary::default()
    };
    import_cataloged_root(
        &source_root,
        store,
        &options,
        &mut summary,
        &retained_catalog.live_paths,
        Some((&retained_catalog.root, root)),
    )?;
    Ok((catalog, summary))
}

pub(crate) fn import_codex_native_session_files(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    import_codex_native_session_files_with_catalog(paths, store, options)
        .map(|(_, summary)| summary)
}

pub(crate) fn import_codex_native_session_files_with_catalog(
    paths: Vec<PathBuf>,
    store: &mut Store,
    mut options: CodexSessionImportOptions,
) -> Result<(CatalogSummary, ProviderImportSummary)> {
    if paths.is_empty() {
        return Ok((CatalogSummary::default(), ProviderImportSummary::default()));
    }
    let source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| common_root(&paths));
    options.source_path = Some(source_root.clone());
    let live_paths = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<BTreeSet<_>>();
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
        failures: catalog.failures.clone(),
        ..ProviderImportSummary::default()
    };
    import_cataloged_root(
        &source_root,
        store,
        &options,
        &mut summary,
        &live_paths,
        None,
    )?;
    Ok((catalog, summary))
}

fn import_cataloged_root(
    source_root: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    summary: &mut ProviderImportSummary,
    live_paths: &BTreeSet<String>,
    retained_root: Option<(&ProviderSourceRoot, &Path)>,
) -> Result<()> {
    let root = source_root.display().to_string();
    let sessions = store.list_catalog_sessions_for_source_bounded(
        CaptureProvider::Codex,
        &root,
        super::super::catalog::CODEX_CATALOG_MAX_SOURCES,
    )?;
    ensure_catalog_source_bound(sessions.len())?;
    let discovery = discover_codex_catalog_sources(&sessions);
    #[cfg(codex_nativepath_qualification)]
    super::qualification::observe_catalog_sources(source_root, &discovery.sources);
    let mut live_sources = discovery
        .sources
        .iter()
        .filter(|source| live_paths.contains(&source.source_path.display().to_string()))
        .cloned()
        .collect::<Vec<_>>();
    if let Some((authority, physical_root)) = retained_root {
        for source in &mut live_sources {
            let relative_path = source
                .source_path
                .strip_prefix(physical_root)
                .map_err(|_| {
                    CaptureError::SystemInvariant(
                        "Codex catalog source escaped its retained root authority",
                    )
                })?;
            source.authority_root = Some(authority.clone());
            source.authority_relative_path = Some(relative_path.to_path_buf());
        }
    }
    parent_first(&mut live_sources);
    let total_files = live_sources.len();
    let total_bytes = live_sources.iter().fold(0_u64, |total, source| {
        total.saturating_add(source.catalog_observation.len)
    });
    let missing_sources = discovery
        .sources
        .iter()
        .filter(|source| !live_paths.contains(&source.source_path.display().to_string()))
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
                Ok(replay) => replay_output(replay, sink.as_ref())?,
                Err(error) if native_error_is_fatal(&error) => return Err(native_error(error)),
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

        let mut tasks = Vec::with_capacity(live_sources.len());
        for source in live_sources {
            let source_bytes = source_bytes(&source);
            if let Err(error) =
                finish_pending_codex_native_retirement(store, &bulk_guard, &source, &native_options)
            {
                if native_error_is_fatal(&error) {
                    return Err(native_error(error));
                }
                summary.failed = summary.failed.saturating_add(1);
                summary.failures.push(ProviderImportFailure {
                    line: 0,
                    error: error.to_string(),
                });
                completed_files = completed_files.saturating_add(1);
                completed_bytes = completed_bytes.saturating_add(source_bytes);
                continue;
            }
            if let Err(error) = retire_replaced_codex_native_source_route(
                store,
                &bulk_guard,
                &source,
                &native_options,
            ) {
                if native_error_is_fatal(&error) {
                    return Err(native_error(error));
                }
                summary.failed = summary.failed.saturating_add(1);
                summary.failures.push(ProviderImportFailure {
                    line: 0,
                    error: error.to_string(),
                });
                completed_files = completed_files.saturating_add(1);
                completed_bytes = completed_bytes.saturating_add(source_bytes);
                continue;
            }
            match prepare_codex_native_producer_task(store, source, native_options.clone()) {
                Ok(task) => tasks.push(task),
                Err(error) => {
                    if native_error_is_fatal(&error) {
                        return Err(native_error(error));
                    }
                    summary.failed = summary.failed.saturating_add(1);
                    summary.failures.push(ProviderImportFailure {
                        line: 0,
                        error: error.to_string(),
                    });
                    completed_files = completed_files.saturating_add(1);
                    completed_bytes = completed_bytes.saturating_add(source_bytes);
                }
            }
        }

        let mut prior_chunk_publication = None;
        let mut completed_native_sessions = std::collections::BTreeSet::<String>::new();
        let mut pending_group = CodexNativeRootGroup::default();
        let mut pending_windows = Vec::<PendingCodexRootWindow>::new();
        let produced =
            run_codex_bounded_producers(tasks, CodexProducerConfig::for_host(), |item| {
                match item {
                    CodexOrderedProducerItem::Step { source, step, .. } => match step {
                        CodexNativeProducerStep::Noop(noop) => {
                            flush_pending_codex_root_group(
                                store,
                                &bulk_guard,
                                &native_options,
                                options,
                                total_files,
                                total_bytes,
                                &mut pending_group,
                                &mut pending_windows,
                                &mut prior_chunk_publication,
                                &mut completed_catalog_sources,
                                &mut completed_native_sessions,
                                &mut completed_files,
                                &mut completed_bytes,
                                summary,
                                output_sink,
                            )?;
                            prior_chunk_publication = None;
                            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                            summary.skipped = summary.skipped.saturating_add(1);
                            summary.skipped_events =
                                summary.skipped_events.saturating_add(noop.skipped_events);
                            summary.accepted_content_records =
                                summary.accepted_content_records.saturating_add(
                                    usize::try_from(noop.retained_events).unwrap_or(usize::MAX),
                                );
                            record_native_rejections(
                                summary,
                                &source,
                                noop.rejected_records,
                                noop.rejections,
                            );
                            if noop.terminal && noop.committed_authority {
                                completed_catalog_sources.insert(
                                    source.source_path.display().to_string(),
                                    noop.retained_events,
                                );
                                if let Some(native_session_id) =
                                    source.catalog_native_session_id.as_ref()
                                {
                                    completed_native_sessions.insert(native_session_id.clone());
                                }
                            }
                            if noop.terminal && noop.committed_authority {
                                if let Some(sink) = output_sink {
                                    match prepare_codex_native_output_replay(
                                        store,
                                        source.clone(),
                                        native_options.clone(),
                                        sink,
                                    ) {
                                        Ok(replay) => replay_output(replay, sink)?,
                                        Err(error) if native_error_is_fatal(&error) => {
                                            return Err(native_error(error));
                                        }
                                        Err(error) => sink.mark_behind(ProOutputSinkError::new(
                                            "codex_nativepath_output_prepare",
                                            error.to_string(),
                                        )),
                                    }
                                }
                            }
                            complete_source_progress(
                                options,
                                total_files,
                                total_bytes,
                                &source,
                                &mut completed_files,
                                &mut completed_bytes,
                                summary,
                            );
                        }
                        CodexNativeProducerStep::Window {
                            mut chunk,
                            source_done,
                            mut delta,
                            report,
                        } => {
                            let pending_parent = source
                                .catalog_parent_native_session_id
                                .as_ref()
                                .is_some_and(|parent| {
                                    pending_windows.iter().any(|pending| {
                                        pending.source_done
                                            && pending.source.catalog_native_session_id.as_ref()
                                                == Some(parent)
                                    })
                                });
                            if source
                                .catalog_parent_native_session_id
                                .as_ref()
                                .is_some_and(|parent| {
                                    !completed_native_sessions.contains(parent) && !pending_parent
                                })
                            {
                                chunk.detach_parent_lineage();
                                delta.imported_edges = 0;
                            }
                            if let Some(prior) = &prior_chunk_publication {
                                chunk
                                    .bind_exact_expected_cursor(prior)
                                    .map_err(native_error)?;
                                prior_chunk_publication = None;
                            }
                            let chunk = match pending_group.try_push(chunk) {
                                Ok(()) => None,
                                Err(chunk) => {
                                    flush_pending_codex_root_group(
                                        store,
                                        &bulk_guard,
                                        &native_options,
                                        options,
                                        total_files,
                                        total_bytes,
                                        &mut pending_group,
                                        &mut pending_windows,
                                        &mut prior_chunk_publication,
                                        &mut completed_catalog_sources,
                                        &mut completed_native_sessions,
                                        &mut completed_files,
                                        &mut completed_bytes,
                                        summary,
                                        output_sink,
                                    )?;
                                    Some(chunk)
                                }
                            };
                            if let Some(mut chunk) = chunk {
                                if let Some(prior) = &prior_chunk_publication {
                                    chunk
                                        .bind_exact_expected_cursor(prior)
                                        .map_err(native_error)?;
                                    prior_chunk_publication = None;
                                }
                                pending_group.try_push(chunk).map_err(|_| {
                                    CaptureError::SystemInvariant(
                                        "certified Codex window exceeds empty root group bounds",
                                    )
                                })?;
                            }
                            pending_windows.push(PendingCodexRootWindow {
                                source,
                                source_done,
                                delta,
                                report,
                            });
                        }
                    },
                    CodexOrderedProducerItem::Failed { source, error, .. } => {
                        flush_pending_codex_root_group(
                            store,
                            &bulk_guard,
                            &native_options,
                            options,
                            total_files,
                            total_bytes,
                            &mut pending_group,
                            &mut pending_windows,
                            &mut prior_chunk_publication,
                            &mut completed_catalog_sources,
                            &mut completed_native_sessions,
                            &mut completed_files,
                            &mut completed_bytes,
                            summary,
                            output_sink,
                        )?;
                        prior_chunk_publication = None;
                        if native_error_is_fatal(&error) {
                            return Err(native_error(error));
                        }
                        summary.failed = summary.failed.saturating_add(1);
                        summary.failures.push(ProviderImportFailure {
                            line: 0,
                            error: error.to_string(),
                        });
                        complete_source_progress(
                            options,
                            total_files,
                            total_bytes,
                            &source,
                            &mut completed_files,
                            &mut completed_bytes,
                            summary,
                        );
                    }
                }
                Ok(())
            });
        // A producer/consumer error may follow already ordered, prepared work.
        // Preserve the former per-window commit semantics by flushing that work,
        // but do not retry a group whose publication was already attempted and
        // failed after ownership moved into the Store.
        if !pending_group.is_empty() {
            flush_pending_codex_root_group(
                store,
                &bulk_guard,
                &native_options,
                options,
                total_files,
                total_bytes,
                &mut pending_group,
                &mut pending_windows,
                &mut prior_chunk_publication,
                &mut completed_catalog_sources,
                &mut completed_native_sessions,
                &mut completed_files,
                &mut completed_bytes,
                summary,
                output_sink,
            )?;
        } else if produced.is_ok() && !pending_windows.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "Codex pending publication metadata outlived its group",
            ));
        }
        produced.map(|stats| {
            #[cfg(codex_nativepath_qualification)]
            super::qualification::observe_producer_stats(stats);
            #[cfg(not(codex_nativepath_qualification))]
            let _ = stats;
        })?;
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
fn flush_pending_codex_root_group(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    native_options: &CodexNativeStoreOptions,
    options: &CodexSessionImportOptions,
    total_files: usize,
    total_bytes: u64,
    pending_group: &mut CodexNativeRootGroup,
    pending_windows: &mut Vec<PendingCodexRootWindow>,
    prior_chunk_publication: &mut Option<CodexNativeRootPublication>,
    completed_catalog_sources: &mut std::collections::BTreeMap<String, u64>,
    completed_native_sessions: &mut std::collections::BTreeSet<String>,
    completed_files: &mut usize,
    completed_bytes: &mut u64,
    summary: &mut ProviderImportSummary,
    output_sink: Option<&dyn crate::ProOutputSink>,
) -> Result<()> {
    if pending_windows.is_empty() {
        if !pending_group.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "Codex pending publication metadata is empty",
            ));
        }
        return Ok(());
    }
    if pending_group.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "Codex pending publication group is empty",
        ));
    }
    let continuation = pending_windows
        .last()
        .is_some_and(|pending| !pending.source_done);
    let publication = std::mem::take(pending_group)
        .publish(store, bulk_guard)
        .map_err(native_error)?;
    summary.imported_events = summary
        .imported_events
        .saturating_add(publication.imported_events);
    summary.skipped_events = summary
        .skipped_events
        .saturating_add(publication.skipped_events);

    for pending in pending_windows.drain(..) {
        summary.imported_sessions = summary
            .imported_sessions
            .saturating_add(pending.delta.imported_sessions);
        summary.imported = summary
            .imported
            .saturating_add(pending.delta.imported_sessions);
        summary.imported_edges = summary
            .imported_edges
            .saturating_add(pending.delta.imported_edges);
        if !pending.source_done {
            continue;
        }
        finish_pending_codex_native_retirement(store, bulk_guard, &pending.source, native_options)
            .map_err(native_error)?;
        let report = pending.report.ok_or(CaptureError::SystemInvariant(
            "terminal Codex producer window omitted its report",
        ))?;
        summary.skipped_events = summary
            .skipped_events
            .saturating_add(report.skipped_events)
            .saturating_add(report.rejected_records);
        record_native_rejections(
            summary,
            &pending.source,
            report.rejected_records,
            report.rejections,
        );
        if report.terminal {
            completed_catalog_sources.insert(
                pending.source.source_path.display().to_string(),
                report.retained_events,
            );
            if let Some(native_session_id) = pending.source.catalog_native_session_id.as_ref() {
                completed_native_sessions.insert(native_session_id.clone());
            }
            if let Some(sink) = output_sink {
                match prepare_codex_native_output_replay(
                    store,
                    pending.source.clone(),
                    native_options.clone(),
                    sink,
                ) {
                    Ok(replay) => replay_output(replay, sink)?,
                    Err(error) if native_error_is_fatal(&error) => {
                        return Err(native_error(error));
                    }
                    Err(error) => sink.mark_behind(ProOutputSinkError::new(
                        "codex_nativepath_output_prepare",
                        error.to_string(),
                    )),
                }
            }
        }
        complete_source_progress(
            options,
            total_files,
            total_bytes,
            &pending.source,
            completed_files,
            completed_bytes,
            summary,
        );
    }
    *prior_chunk_publication = continuation.then_some(publication);
    Ok(())
}

fn source_bytes(source: &CodexCatalogSource) -> u64 {
    source.catalog_observation.len
}

#[allow(clippy::too_many_arguments)]
fn complete_source_progress(
    options: &CodexSessionImportOptions,
    total_files: usize,
    total_bytes: u64,
    source: &CodexCatalogSource,
    completed_files: &mut usize,
    completed_bytes: &mut u64,
    summary: &ProviderImportSummary,
) {
    *completed_files = (*completed_files).saturating_add(1);
    *completed_bytes = (*completed_bytes).saturating_add(source_bytes(source));
    report_import_progress(
        options,
        total_files,
        total_bytes,
        *completed_files,
        *completed_bytes,
        summary,
        false,
    );
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

fn replay_output(
    mut replay: CodexNativeOutputReplay,
    sink: &dyn crate::ProOutputSink,
) -> Result<()> {
    loop {
        match replay.next_page(sink) {
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(failure))) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "codex_nativepath_output_page",
                    format!("{failure:?}"),
                ));
                return Ok(());
            }
            Ok(None) => return Ok(()),
            Err(error) if native_error_is_fatal(&error) => return Err(native_error(error)),
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "codex_nativepath_output_replay",
                    error.to_string(),
                ));
                return Ok(());
            }
        }
    }
}

fn retire_missing_root(
    source_root: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let root = source_root.display().to_string();
    let sessions = store.list_catalog_sessions_for_source_bounded(
        CaptureProvider::Codex,
        &root,
        super::super::catalog::CODEX_CATALOG_MAX_SOURCES,
    )?;
    ensure_catalog_source_bound(sessions.len())?;
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

fn native_error(error: CodexNativeVerticalError) -> CaptureError {
    match error {
        CodexNativeVerticalError::Capture(error) => error,
        CodexNativeVerticalError::Store(error) => CaptureError::Store(error),
        error => CaptureError::InvalidPayload(format!("Codex NativePath import failed: {error}")),
    }
}

fn native_error_is_fatal(error: &CodexNativeVerticalError) -> bool {
    error.requires_immediate_propagation()
}
