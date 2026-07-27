use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use uuid::Uuid;

use crate::provider::importer::{provider_path_identity, provider_source_cursor_stream_for_path};
use crate::{
    CaptureError, CaptureWorkLimit, ImportProfile, ProOutputSink, ProOutputSinkError,
    ProviderImportSummary, Result,
};

use super::{
    decode_direct_jsonl_cursor, open_direct_jsonl_pages, publish_direct_jsonl_group,
    DirectJsonlCursorDecode, DirectJsonlPage, DirectJsonlPendingPage,
    DirectJsonlPublicationContext,
};

const DIRECT_GROUP_MAX_PAGES: usize = 32;
const DIRECT_GROUP_MAX_SOURCES: usize = 64;
const DIRECT_GROUP_MAX_BYTES: usize = 6 * 1024 * 1024;
const DIRECT_GROUP_MAX_ESTIMATED_MUTATIONS: usize = 3_000;
const DIRECT_GROUP_MAX_LOGICAL_UNITS: usize = 64;
const DIRECT_SOURCE_FAILURE_MAX_CHARS: usize = 1_024;

pub(crate) struct NativePathJsonlTreeImport<'a> {
    pub(crate) path: &'a Path,
    pub(crate) machine_id: String,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) source_root: Option<PathBuf>,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) capture_work_limit: CaptureWorkLimit,
    pub(crate) inventory_observation_token: Option<String>,
    pub(crate) import_profile: ImportProfile,
}

pub(super) fn import_direct_native_jsonl_tree_core(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
    provider: CaptureProvider,
    source_format: &'static str,
) -> Result<ProviderImportSummary> {
    super::super::dialect::validate_direct_native_jsonl_provider(provider)?;
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let context = DirectJsonlPublicationContext {
        provider,
        source_format,
        machine_id: &request.machine_id,
        source_root: &configured_source_root,
        imported_at: request.imported_at,
        history_record_id: request.history_record_id,
        inventory_observation_token: request.inventory_observation_token.as_deref(),
    };
    let collect_outputs = !matches!(request.import_profile, ImportProfile::CoreOnly);
    let mut accumulator = DirectGroupAccumulator::new(
        store,
        &committed_store,
        &bulk_guard,
        context,
        request.capture_work_limit,
    );
    let mut visited = 0_usize;
    let operation = super::super::traversal::visit_jsonl_tree_files(
        request.path,
        &|path| super::super::dialect::native_jsonl_file_is_selected(provider, path),
        &mut |path| {
            visited = visited.saturating_add(1);
            if accumulator.stopped() {
                return Ok(());
            }
            let source_result = (|| -> Result<()> {
                if let Some(token) = request.inventory_observation_token.as_deref() {
                    if crate::observe_ordinary_file(path)?.token_hex() != token {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                }
                let observation = super::super::native_path::reader::observe_file(path)?;
                let canonical_path = std::fs::canonicalize(path)?;
                let path_identity = provider_path_identity(&canonical_path)?;
                let stream =
                    provider_source_cursor_stream_for_path(provider, source_format, &path_identity);
                let stored =
                    accumulator
                        .store()
                        .get_sync_cursor(None, &request.machine_id, &stream)?;
                let decoded = stored
                    .as_ref()
                    .map(|cursor| {
                        decode_direct_jsonl_cursor(
                            &cursor.cursor,
                            provider,
                            source_format,
                            &canonical_path,
                            &observation,
                        )
                    })
                    .transpose()?;
                let migrated = matches!(&decoded, Some(DirectJsonlCursorDecode::Migrated(_)));
                let previous = decoded.and_then(|decoded| match decoded {
                    DirectJsonlCursorDecode::Native(checkpoint)
                    | DirectJsonlCursorDecode::Migrated(checkpoint) => Some(checkpoint),
                    DirectJsonlCursorDecode::Reset => None,
                });
                let mut reader = open_direct_jsonl_pages(
                    provider,
                    source_format,
                    &canonical_path,
                    Some(configured_source_root.clone()),
                    request.imported_at,
                    collect_outputs,
                    previous.as_ref(),
                )?;
                let mut emitted_page = false;
                while let Some(page) = reader.next_page()? {
                    emitted_page = true;
                    accumulator.push(DirectJsonlPendingPage {
                        path: canonical_path.clone(),
                        page,
                    })?;
                    if accumulator.stopped() {
                        break;
                    }
                }
                if !accumulator.stopped() && !emitted_page {
                    if let Some(outcome) = reader.outcome() {
                        if outcome.source_change == super::DirectJsonlSourceChange::Unchanged
                            && !migrated
                        {
                            accumulator.record_unchanged(outcome);
                        } else {
                            accumulator.push(DirectJsonlPendingPage {
                                path: canonical_path,
                                page: observation_only_page(outcome.checkpoint.clone()),
                            })?;
                        }
                    }
                }
                if !accumulator.stopped() {
                    accumulator.flush()?;
                }
                Ok(())
            })();
            if let Err(error) = source_result {
                accumulator.discard_pending();
                if capture_error_is_systemic(&error) {
                    return Err(error);
                }
                accumulator.record_source_failure(path, &error);
            }
            Ok(())
        },
    );
    let operation = operation.and_then(|_| accumulator.finish());
    let stopped = accumulator.stopped();
    drop(accumulator);
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(mut summary), Ok(())) => {
            if visited == 0 {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: request.path.to_path_buf(),
                    reason: super::super::dialect::native_jsonl_missing_reason(provider),
                });
            }
            if stopped {
                summary.work_remaining = true;
            }
            Ok(summary)
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn observation_only_page(checkpoint: super::DirectJsonlCheckpoint) -> DirectJsonlPage {
    DirectJsonlPage {
        expected_checkpoint: checkpoint.clone(),
        next_checkpoint: checkpoint.clone(),
        events: Vec::new(),
        outputs: Vec::new(),
        rejections: Vec::new(),
        logical_units: 1,
        conservative_serialized_bytes: 2 * 1024,
        terminal: checkpoint.terminal,
    }
}

struct DirectGroupAccumulator<'a> {
    store: &'a mut Store,
    committed_store: &'a Store,
    bulk_guard: &'a ctx_history_store::EventSearchBulkGuard,
    context: DirectJsonlPublicationContext<'a>,
    work_limit: CaptureWorkLimit,
    pages: Vec<DirectJsonlPendingPage>,
    bytes: usize,
    estimated_mutations: usize,
    logical_units: usize,
    sources: std::collections::BTreeSet<PathBuf>,
    summary: ProviderImportSummary,
    published_groups: usize,
    stopped: bool,
}

impl<'a> DirectGroupAccumulator<'a> {
    fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a ctx_history_store::EventSearchBulkGuard,
        context: DirectJsonlPublicationContext<'a>,
        work_limit: CaptureWorkLimit,
    ) -> Self {
        Self {
            store,
            committed_store,
            bulk_guard,
            context,
            work_limit,
            pages: Vec::new(),
            bytes: 0,
            estimated_mutations: 0,
            logical_units: 0,
            sources: std::collections::BTreeSet::new(),
            summary: ProviderImportSummary::default(),
            published_groups: 0,
            stopped: false,
        }
    }

    fn store(&self) -> &Store {
        self.store
    }

    fn stopped(&self) -> bool {
        self.stopped
    }

    fn record_unchanged(&mut self, outcome: &super::DirectJsonlScanOutcome) {
        let sessions = usize::from(outcome.checkpoint.session.is_some());
        let events = usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX);
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(sessions);
        self.summary.skipped_events = self.summary.skipped_events.saturating_add(events);
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(sessions)
            .saturating_add(events);
    }

    fn record_source_failure(&mut self, path: &Path, error: &CaptureError) {
        let diagnostic = format!("{}: {error}", path.display())
            .chars()
            .take(DIRECT_SOURCE_FAILURE_MAX_CHARS)
            .collect();
        self.summary.record_failure(crate::ProviderImportFailure {
            line: 0,
            error: diagnostic,
        });
    }

    fn discard_pending(&mut self) {
        self.pages.clear();
        self.bytes = 0;
        self.estimated_mutations = 0;
        self.logical_units = 0;
        self.sources.clear();
    }

    fn push(&mut self, pending: DirectJsonlPendingPage) -> Result<()> {
        if pending.page.logical_units == 0
            || pending.page.logical_units > DIRECT_GROUP_MAX_LOGICAL_UNITS
        {
            return Err(CaptureError::SystemInvariant(
                "direct JSONL page has invalid normalization-unit accounting",
            ));
        }
        let next_sources = self.sources.len() + usize::from(!self.sources.contains(&pending.path));
        let next_bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        let page_mutations = pending.page.logical_units.saturating_add(4);
        if page_mutations > DIRECT_GROUP_MAX_ESTIMATED_MUTATIONS {
            return Err(CaptureError::InvalidPayload(format!(
                "{} expands past the certified direct JSONL Store mutation boundary",
                pending.path.display()
            )));
        }
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        let next_logical_units = self
            .logical_units
            .saturating_add(pending.page.logical_units);
        if !self.pages.is_empty()
            && (self.pages.len() >= DIRECT_GROUP_MAX_PAGES
                || next_sources > DIRECT_GROUP_MAX_SOURCES
                || next_bytes > DIRECT_GROUP_MAX_BYTES
                || next_mutations > DIRECT_GROUP_MAX_ESTIMATED_MUTATIONS
                || next_logical_units > DIRECT_GROUP_MAX_LOGICAL_UNITS)
        {
            self.flush()?;
            if self.stopped {
                return Ok(());
            }
        }
        self.bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        self.estimated_mutations = self.estimated_mutations.saturating_add(page_mutations);
        self.logical_units = self
            .logical_units
            .saturating_add(pending.page.logical_units);
        self.sources.insert(pending.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let pages = std::mem::take(&mut self.pages);
        self.bytes = 0;
        self.estimated_mutations = 0;
        self.logical_units = 0;
        self.sources.clear();
        let summary = publish_direct_jsonl_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
        self.published_groups = self.published_groups.saturating_add(1);
        if self.work_limit == CaptureWorkLimit::OneSafeGroup {
            self.stopped = true;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<ProviderImportSummary> {
        if !self.stopped {
            self.flush()?;
        }
        Ok(std::mem::take(&mut self.summary))
    }
}

pub(super) fn capture_error_is_systemic(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::Store(_)
            | CaptureError::Sqlite(_)
            | CaptureError::SystemIo { .. }
            | CaptureError::SystemInvariant(_)
            | CaptureError::ProviderCursorConflict
            | CaptureError::WorkerPanicked(_)
    )
}

pub(super) fn replay_selected_output_sources<F>(
    paths: &std::collections::BTreeSet<PathBuf>,
    sink: &dyn ProOutputSink,
    diagnostic_code: &'static str,
    mut replay: F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    for path in paths {
        if let Err(error) = replay(path) {
            if capture_error_is_systemic(&error) {
                return Err(error);
            }
            let diagnostic: String = format!("{}: {error}", path.display())
                .chars()
                .take(DIRECT_SOURCE_FAILURE_MAX_CHARS)
                .collect();
            sink.mark_behind(ProOutputSinkError::new(diagnostic_code, diagnostic));
        }
    }
    Ok(())
}
