use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    num::NonZeroUsize,
};

use serde::Serialize;

use super::*;

pub(crate) const IMPORT_TRANSACTION_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const IMPORT_TRANSACTION_BATCH_UNITS: usize = 64;

pub(super) fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let transaction_batch_size = provider_transaction_batch_size();
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    import_provider_capture_lines_with_batch_size(
        store,
        options,
        summary,
        captures,
        files_touched,
        transaction_batch_size,
        true,
    )
}

#[cfg(test)]
pub(crate) fn import_normalized_provider_captures_in_batches(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
    transaction_batch_size: usize,
) -> Result<ProviderImportSummary> {
    if !options.wrap_transaction {
        return Err(CaptureError::InvalidPayload(
            "batched provider import requires transaction wrapping".to_owned(),
        ));
    }
    let transaction_batch_size = NonZeroUsize::new(transaction_batch_size).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "provider import batch size must be greater than zero".to_owned(),
        )
    })?;
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    import_provider_capture_lines_with_batch_size(
        store,
        options,
        summary,
        captures,
        files_touched,
        Some(transaction_batch_size),
        true,
    )
}

pub(super) fn import_provider_capture_lines(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    summary: ProviderImportSummary,
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
) -> Result<ProviderImportSummary> {
    import_provider_capture_lines_with_batch_size(
        store,
        options,
        summary,
        captures,
        files_touched,
        provider_transaction_batch_size(),
        true,
    )
}

fn provider_transaction_batch_size() -> Option<NonZeroUsize> {
    NonZeroUsize::new(IMPORT_TRANSACTION_BATCH_UNITS)
}

fn import_provider_capture_lines_with_batch_size(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    mut captures: Vec<(usize, ProviderCaptureEnvelope)>,
    mut files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
    transaction_batch_size: Option<NonZeroUsize>,
    suppress_search_merges: bool,
) -> Result<ProviderImportSummary> {
    let caches = ProviderImportCaches::default();
    filter_provider_capture_lines_without_real_session_messages(
        &mut summary,
        &mut captures,
        &mut files_touched,
    );
    let supplied_file_touch_lines = files_touched
        .iter()
        .map(|(line_number, _)| *line_number)
        .collect::<BTreeSet<_>>();
    if summary.failed == 0 && !provider_capture_lines_have_real_message(&captures) {
        let line = captures
            .first()
            .map(|(line_number, _)| *line_number)
            .or_else(|| files_touched.first().map(|(line_number, _)| *line_number))
            .unwrap_or(0);
        summary.failed += 1;
        summary.failures.push(ProviderImportFailure {
            line,
            error: "provider source contained no real conversation message".to_owned(),
        });
        return Ok(summary);
    }
    for (line_number, capture) in &captures {
        if capture.provider == CaptureProvider::Codex
            || supplied_file_touch_lines.contains(line_number)
        {
            continue;
        }
        if let Some(event) = &capture.event {
            files_touched.extend(provider_file_touches_from_event(
                capture.provider,
                &capture.session.provider_session_id,
                &capture.source.source_format,
                capture.source.raw_source_path.as_deref(),
                capture.source.source_root.as_deref(),
                event,
                *line_number,
            ));
        }
    }
    let has_captures = !captures.is_empty() || !files_touched.is_empty();
    let bulk_search_mode = suppress_search_merges && has_captures && options.wrap_transaction;
    let bulk_search_guard = bulk_search_mode
        .then(|| store.begin_event_search_bulk_mode())
        .transpose()?;
    let import_result = persist_provider_capture_lines(
        store,
        &options,
        summary,
        captures,
        files_touched,
        has_captures,
        transaction_batch_size,
        caches,
    );
    let finish_result = match &bulk_search_guard {
        Some(guard) => store
            .finish_event_search_bulk_mode(guard)
            .map_err(CaptureError::from),
        None => Ok(()),
    };
    match (import_result, finish_result) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(err)) => Err(err),
        (Err(err), Ok(())) => Err(err),
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_provider_capture_lines(
    store: &mut Store,
    options: &NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
    has_captures: bool,
    transaction_batch_size: Option<NonZeroUsize>,
    mut caches: ProviderImportCaches,
) -> Result<ProviderImportSummary> {
    let pending_cursors = if options.persist_cursors && summary.failed == 0 {
        captures
            .iter()
            .filter_map(|(_, capture)| provider_sync_cursor(capture))
            .map(|cursor| (cursor.id, cursor))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let mut transaction = ProviderImportTransaction::begin(
        store,
        has_captures && options.wrap_transaction,
        transaction_batch_size,
    )?;
    for (line_number, capture) in captures {
        let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &capture)?;
        transaction.prepare_unit(store, unit_bytes)?;
        match import_provider_capture_line(store, &capture, options, line_number, &mut caches) {
            Ok(line_summary) => summary.merge(line_summary),
            Err(err @ CaptureError::Store(_)) => {
                transaction.rollback(store);
                return Err(err);
            }
            Err(err) => {
                summary.failed += 1;
                summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
            }
        }
        transaction.record_unit(store, unit_bytes)?;
    }
    resolve_pending_provider_edges_batched(store, &mut summary, &mut caches, &mut transaction)?;
    for (line_number, file) in files_touched {
        let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &file)?;
        transaction.prepare_unit(store, unit_bytes)?;
        match import_provider_file_touched_line(store, &file, options) {
            Ok(()) => summary.accepted_content_records += 1,
            Err(err) => match err {
                err @ CaptureError::Store(_) => {
                    transaction.rollback(store);
                    return Err(err);
                }
                err => {
                    summary.failed += 1;
                    summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            },
        }
        transaction.record_unit(store, unit_bytes)?;
    }
    if summary.failed == 0 {
        for cursor in pending_cursors.into_values() {
            let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &cursor)?;
            transaction.prepare_unit(store, unit_bytes)?;
            if let Err(err) = persist_provider_sync_cursor(store, &cursor) {
                transaction.rollback(store);
                return Err(err);
            }
            transaction.record_unit(store, unit_bytes)?;
        }
    }
    transaction.commit(store)?;
    Ok(summary)
}

fn serialized_len(value: &impl Serialize) -> Result<usize> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}

fn serialized_len_or_rollback(
    transaction: &mut ProviderImportTransaction,
    store: &Store,
    value: &impl Serialize,
) -> Result<usize> {
    match serialized_len(value) {
        Ok(bytes) => Ok(bytes),
        Err(err) => {
            transaction.rollback(store);
            Err(err)
        }
    }
}

fn pending_edge_estimated_len(edge: &PendingProviderEdge) -> usize {
    edge.provider_session_id
        .len()
        .saturating_add(
            edge.parent_provider_session_id
                .as_deref()
                .map_or(0, str::len),
        )
        .saturating_add(edge.source_format.len())
        .saturating_add(256)
}

pub(crate) fn resolve_pending_provider_edges_batched(
    store: &mut Store,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
    transaction: &mut ProviderImportTransaction,
) -> Result<()> {
    let pending = std::mem::take(&mut caches.pending_edges);
    for (edge_id, edge) in pending {
        let unit_bytes = pending_edge_estimated_len(&edge);
        transaction.prepare_unit(store, unit_bytes)?;
        if let Err(err) = resolve_pending_provider_edge(store, summary, caches, edge_id, edge) {
            transaction.rollback(store);
            return Err(err);
        }
        transaction.record_unit(store, unit_bytes)?;
    }
    Ok(())
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) struct ProviderImportTransaction {
    active: bool,
    batch_size: Option<NonZeroUsize>,
    units: usize,
    bytes: usize,
}

impl ProviderImportTransaction {
    fn begin(store: &Store, has_work: bool, batch_size: Option<NonZeroUsize>) -> Result<Self> {
        if has_work {
            store.begin_immediate_batch()?;
        }
        Ok(Self {
            active: has_work,
            batch_size,
            units: 0,
            bytes: 0,
        })
    }

    pub(crate) fn begin_bounded(store: &Store, has_work: bool) -> Result<Self> {
        Self::begin(store, has_work, provider_transaction_batch_size())
    }

    pub(crate) fn prepare_unit(&mut self, store: &Store, unit_bytes: usize) -> Result<()> {
        let result = if self.active
            && self.batch_size.is_some()
            && self.units > 0
            && self.bytes.saturating_add(unit_bytes) > IMPORT_TRANSACTION_BATCH_BYTES
        {
            self.rotate(store)
        } else {
            Ok(())
        };
        if result.is_err() {
            self.rollback(store);
        }
        result
    }

    pub(crate) fn record_unit(&mut self, store: &Store, unit_bytes: usize) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.units = self.units.saturating_add(1);
        self.bytes = self.bytes.saturating_add(unit_bytes);
        let below_unit_limit = self
            .batch_size
            .is_none_or(|batch_size| self.units < batch_size.get());
        let below_byte_limit =
            self.batch_size.is_none() || self.bytes < IMPORT_TRANSACTION_BATCH_BYTES;
        if below_unit_limit && below_byte_limit {
            return Ok(());
        }
        let result = self.rotate(store);
        if result.is_err() {
            self.rollback(store);
        }
        result
    }

    fn rotate(&mut self, store: &Store) -> Result<()> {
        store.commit_batch()?;
        self.active = false;
        store.checkpoint_wal_truncate_required()?;
        store.begin_immediate_batch()?;
        self.active = true;
        self.units = 0;
        self.bytes = 0;
        Ok(())
    }

    pub(crate) fn commit(&mut self, store: &Store) -> Result<()> {
        let result = if self.active {
            store.commit_batch().map_err(CaptureError::from)
        } else {
            Ok(())
        };
        if result.is_ok() {
            self.active = false;
        } else {
            self.rollback(store);
        }
        result
    }

    pub(crate) fn rollback(&mut self, store: &Store) {
        if self.active {
            let _ = store.rollback_batch();
            self.active = false;
        }
    }
}
