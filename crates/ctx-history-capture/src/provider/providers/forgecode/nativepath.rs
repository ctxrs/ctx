//! ForgeCode-owned NativePath discovery, scanning, and publication.
//!
//! This module is the only production ForgeCode ingestion path. It reads the
//! provider's SQLite source directly, commits bounded Core pages through the
//! typed Store transaction, and replays output-only Pro pages after Core.

use std::path::Path;

use ctx_history_store::Store;

use crate::{
    CaptureError, CaptureWorkLimit, ImportProfile, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result,
};

mod output;
mod publication;
pub(super) mod source;
pub(crate) mod source_backed;

use self::{
    output::ForgeCodeOutputReplay,
    publication::{
        generation_for_current_source, load_core_start, proposed_source_identity,
        publish_core_page, retire_missing_source, verify_core_page_committed,
    },
    source::{discover_forgecode_source, ForgeCodeDiscovery, ForgeCodeFrontier, ForgeCodeScanner},
};

pub(super) fn import_forgecode_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let discovery = discover_forgecode_source(path)?;
    match discovery {
        ForgeCodeDiscovery::Missing(missing) => {
            let bulk_guard = store.begin_event_search_bulk_mode()?;
            let operation = retire_missing_source(store, &bulk_guard, &context, &missing);
            finish_bulk(store, &bulk_guard, operation)
        }
        ForgeCodeDiscovery::Live(source) => {
            let committed_store = Store::open_read_only(store.path())?;
            let source_root = context.source_root_display().or_else(|| {
                source
                    .canonical_path
                    .parent()
                    .map(|path| path.display().to_string())
            });
            let core_start = load_core_start(store, &context.machine_id, &source)?;
            let core_generation =
                generation_for_current_source(&committed_store, &source, &context, &core_start)?;
            let output_identity = proposed_source_identity(
                source_root.as_deref(),
                &source.canonical_path.display().to_string(),
                &source.schema_fingerprint,
            )?;
            let mut output_behind = false;
            let mut output = match &import_options.import_profile {
                ImportProfile::CoreOnly => None,
                ImportProfile::CoreAndPro(sink) | ImportProfile::ProReplayOnly(sink) => {
                    match ForgeCodeOutputReplay::new(
                        sink.as_ref(),
                        &context.machine_id,
                        &output_identity,
                        &source.source_revision,
                    ) {
                        Ok(output) => Some(output),
                        Err(_) => {
                            sink.mark_behind(ProOutputSinkError::new(
                                "forgecode_output_progress",
                                "ForgeCode Pro output progress is unavailable",
                            ));
                            output_behind = true;
                            None
                        }
                    }
                }
            };
            let replay_only = import_options.import_profile.is_replay_only();
            let output_start = output.as_ref().map(|(_, start)| start);
            let start = select_scan_start(
                &core_start.frontier,
                core_start.terminal,
                output_start.map(|start| &start.frontier),
                output_start.is_some_and(|start| start.terminal),
                replay_only,
            );
            let Some(start) = start else {
                let mut summary = ProviderImportSummary::default();
                if output_behind {
                    record_output_behind(&mut summary);
                }
                summary.set_work_result(ProviderImportWorkResult::NoOp);
                return Ok(summary);
            };
            let mut output = output.take().map(|(output, _)| output);
            let mut scanner =
                ForgeCodeScanner::new(source.clone(), start, context.clone(), output.is_some())?;
            let bulk_guard = store.begin_event_search_bulk_mode()?;
            let mut summary = ProviderImportSummary::default();
            let mut changed_groups = 0_usize;
            let scan = (|| {
                while let Some(mut page) = scanner.next_page()? {
                    if replay_only {
                        verify_core_page_committed(store, &context.machine_id, &source, &page)?;
                    } else {
                        let core = publish_core_page(
                            store,
                            &committed_store,
                            &bulk_guard,
                            &context,
                            &import_options,
                            &source,
                            source_root.as_deref(),
                            &page,
                            core_generation,
                        )?;
                        if core.work_result() == ProviderImportWorkResult::Changed {
                            changed_groups = changed_groups.saturating_add(1);
                        }
                        summary.merge_from(core);
                    }
                    let output_result = output.as_mut().map(|output| output.materialize(&mut page));
                    if let Some(Err(_)) = output_result {
                        if let Some(sink) = import_options.import_profile.sink() {
                            sink.mark_behind(ProOutputSinkError::new(
                                "forgecode_output_page",
                                "ForgeCode Pro output page is behind committed Core",
                            ));
                        }
                        output_behind = true;
                        output = None;
                    }
                    if !replay_only
                        && import_options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        && changed_groups != 0
                    {
                        summary.work_remaining = !page.terminal;
                        break;
                    }
                }
                if output_behind {
                    record_output_behind(&mut summary);
                }
                Ok(summary)
            })();
            finish_bulk(store, &bulk_guard, scan)
        }
    }
}

fn record_output_behind(summary: &mut ProviderImportSummary) {
    summary.record_failure(ProviderImportFailure {
        line: 0,
        error: "ForgeCode Pro output is behind committed Core".to_owned(),
    });
}

fn select_scan_start(
    core: &ForgeCodeFrontier,
    core_terminal: bool,
    output: Option<&ForgeCodeFrontier>,
    output_terminal: bool,
    replay_only: bool,
) -> Option<ForgeCodeFrontier> {
    if replay_only {
        return (!output_terminal)
            .then(|| output.cloned().unwrap_or_else(ForgeCodeFrontier::initial));
    }
    match (core_terminal, output, output_terminal) {
        (true, None, _) => None,
        (false, None, _) => Some(core.clone()),
        (true, Some(_), true) => None,
        (true, Some(output), false) => Some(output.clone()),
        (false, Some(_), true) => Some(core.clone()),
        (false, Some(output), false) => Some(core.min(output).clone()),
    }
}

fn finish_bulk(
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    operation: Result<ProviderImportSummary>,
) -> Result<ProviderImportSummary> {
    let finish = store
        .finish_event_search_bulk_mode(bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}
