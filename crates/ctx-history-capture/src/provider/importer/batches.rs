use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    num::NonZeroUsize,
};

use rusqlite::{params, Connection};
use serde::{de::DeserializeOwned, Serialize};

use crate::common::scratch::CaptureScratchSpace;

use super::*;

pub(crate) const IMPORT_TRANSACTION_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const IMPORT_TRANSACTION_BATCH_UNITS: usize = 64;
pub(crate) const PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS: usize = 64;
const IMPORT_STATE_SPOOL_BATCH: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProviderNormalizationStreamMetrics {
    pub(crate) path_inventory_entries: usize,
    pub(crate) max_path_inventory_batch: usize,
    pub(crate) normalization_batches: usize,
    pub(crate) normalization_captures: usize,
    pub(crate) normalization_files_touched: usize,
    pub(crate) max_batch_captures: usize,
    pub(crate) max_batch_files_touched: usize,
    pub(crate) max_transaction_units: usize,
    pub(crate) max_transaction_bytes: usize,
    pub(crate) max_pending_cursors: usize,
    pub(crate) max_cache_entries: usize,
    pub(crate) max_pi_identity_load_batch: usize,
}

struct ProviderImportStateSpool {
    connection: Connection,
    _scratch: CaptureScratchSpace,
}

impl ProviderImportStateSpool {
    fn new() -> Result<Self> {
        let scratch = CaptureScratchSpace::create("provider-import-state")?;
        drop(scratch.create_file("state.sqlite")?);
        let connection = Connection::open(scratch.path().join("state.sqlite"))?;
        connection.execute_batch(
            "CREATE TABLE cursors (
                 id TEXT PRIMARY KEY NOT NULL,
                 payload TEXT NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE pending_edges (
                 id TEXT PRIMARY KEY NOT NULL,
                 payload TEXT NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE observed_entities (
                 kind TEXT NOT NULL,
                 id TEXT NOT NULL,
                 PRIMARY KEY (kind, id)
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            connection,
            _scratch: scratch,
        })
    }

    fn store_cursor(&self, cursor: &ctx_history_core::SyncCursor) -> Result<()> {
        self.connection.execute(
            "INSERT INTO cursors (id, payload) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
            params![cursor.id.to_string(), serde_json::to_string(cursor)?],
        )?;
        Ok(())
    }

    fn store_pending_edges(&self, pending: BTreeMap<Uuid, PendingProviderEdge>) -> Result<()> {
        for (id, edge) in pending {
            self.connection.execute(
                "INSERT INTO pending_edges (id, payload) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
                params![id.to_string(), serde_json::to_string(&edge)?],
            )?;
        }
        Ok(())
    }

    fn first_entity_observation(&self, kind: &'static str, id: Uuid) -> Result<bool> {
        Ok(self.connection.execute(
            "INSERT OR IGNORE INTO observed_entities (kind, id) VALUES (?1, ?2)",
            params![kind, id.to_string()],
        )? == 1)
    }

    fn load_batch<T: DeserializeOwned>(
        &self,
        table: &str,
        after: Option<&str>,
    ) -> Result<Vec<(String, T)>> {
        let sql = match table {
            "cursors" => {
                "SELECT id, payload FROM cursors WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2"
            }
            "pending_edges" => {
                "SELECT id, payload FROM pending_edges WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2"
            }
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "unknown provider import state spool table",
                ));
            }
        };
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(params![
            after,
            i64::try_from(IMPORT_STATE_SPOOL_BATCH).unwrap_or(i64::MAX)
        ])?;
        let mut batch = Vec::with_capacity(IMPORT_STATE_SPOOL_BATCH);
        while let Some(row) = rows.next()? {
            let id = row.get::<_, String>(0)?;
            let payload = serde_json::from_str::<T>(&row.get::<_, String>(1)?)?;
            batch.push((id, payload));
        }
        Ok(batch)
    }
}

pub(crate) struct ProviderNormalizationBatcher<F>
where
    F: FnMut(ProviderNormalizationResult) -> Result<()>,
{
    emit: F,
    current: ProviderNormalizationResult,
    records: usize,
}

pub(crate) struct ProviderNormalizationStreamImporter<'a> {
    store: &'a mut Store,
    options: NormalizedProviderImportOptions,
    summary: ProviderImportSummary,
    caches: ProviderImportCaches,
    transaction: ProviderImportTransaction,
    state_spool: ProviderImportStateSpool,
    bulk_search_guard: Option<ctx_history_store::EventSearchBulkGuard>,
    metrics: ProviderNormalizationStreamMetrics,
}

impl<'a> ProviderNormalizationStreamImporter<'a> {
    pub(crate) fn new(
        store: &'a mut Store,
        options: NormalizedProviderImportOptions,
    ) -> Result<Self> {
        if !options.wrap_transaction {
            return Err(CaptureError::InvalidPayload(
                "streamed provider import requires transaction wrapping".to_owned(),
            ));
        }
        let transaction = ProviderImportTransaction::begin_bounded(store, false)?;
        Ok(Self {
            store,
            options,
            summary: ProviderImportSummary::default(),
            caches: ProviderImportCaches::default(),
            transaction,
            state_spool: ProviderImportStateSpool::new()?,
            bulk_search_guard: None,
            metrics: ProviderNormalizationStreamMetrics::default(),
        })
    }

    pub(crate) fn import_batch(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> Result<()> {
        self.metrics.normalization_batches += 1;
        self.metrics.normalization_captures += normalization.captures.len();
        self.metrics.normalization_files_touched += normalization.files_touched.len();
        self.metrics.max_batch_captures = self
            .metrics
            .max_batch_captures
            .max(normalization.captures.len());
        self.metrics.max_batch_files_touched = self
            .metrics
            .max_batch_files_touched
            .max(normalization.files_touched.len());
        let ProviderNormalizationResult {
            summary,
            captures,
            mut files_touched,
        } = normalization;
        self.summary.merge(summary);
        if self.options.persist_cursors {
            let mut resident_cursors = BTreeMap::new();
            for cursor in captures
                .iter()
                .filter_map(|(_, capture)| provider_sync_cursor(capture))
            {
                resident_cursors.insert(cursor.id, cursor);
            }
            for cursor in resident_cursors.values() {
                self.state_spool.store_cursor(cursor)?;
            }
            self.metrics.max_pending_cursors =
                self.metrics.max_pending_cursors.max(resident_cursors.len());
        }
        self.observe_resident_state();

        let supplied_file_touch_lines = files_touched
            .iter()
            .map(|(line_number, _)| *line_number)
            .collect::<BTreeSet<_>>();
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

        if !captures.is_empty() || !files_touched.is_empty() {
            self.ensure_import_active()?;
        }
        for (line_number, capture) in captures {
            let unit_bytes =
                serialized_len_or_rollback(&mut self.transaction, self.store, &capture)?;
            require_transaction_continue(self.transaction.prepare_unit(self.store, unit_bytes)?)?;
            let sessions_before = self.caches.processed_sessions.clone();
            let edges_before = self.caches.processed_edges.clone();
            match import_provider_capture_line(
                self.store,
                &capture,
                &self.options,
                line_number,
                &mut self.caches,
            ) {
                Ok(mut line_summary) => {
                    for session_id in self.caches.processed_sessions.difference(&sessions_before) {
                        if !self
                            .state_spool
                            .first_entity_observation("session", *session_id)?
                        {
                            line_summary.skipped_sessions =
                                line_summary.skipped_sessions.saturating_sub(1);
                            line_summary.skipped = line_summary.skipped.saturating_sub(1);
                        }
                    }
                    for edge_id in self.caches.processed_edges.difference(&edges_before) {
                        if !self
                            .state_spool
                            .first_entity_observation("edge", *edge_id)?
                        {
                            line_summary.skipped_edges =
                                line_summary.skipped_edges.saturating_sub(1);
                            line_summary.skipped = line_summary.skipped.saturating_sub(1);
                        }
                    }
                    self.summary.merge(line_summary);
                }
                Err(err @ CaptureError::Store(_)) => {
                    self.transaction.rollback(self.store);
                    return Err(err);
                }
                Err(err) => {
                    self.summary.failed += 1;
                    self.summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            }
            require_transaction_continue(self.transaction.record_unit(self.store, unit_bytes)?)?;
            self.observe_resident_state();
        }
        for (line_number, file) in files_touched {
            let unit_bytes = serialized_len_or_rollback(&mut self.transaction, self.store, &file)?;
            require_transaction_continue(self.transaction.prepare_unit(self.store, unit_bytes)?)?;
            match import_provider_file_touched_line(self.store, &file, &self.options) {
                Ok(()) => self.summary.accepted_content_records += 1,
                Err(err @ CaptureError::Store(_)) => {
                    self.transaction.rollback(self.store);
                    return Err(err);
                }
                Err(err) => {
                    self.summary.failed += 1;
                    self.summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            }
            require_transaction_continue(self.transaction.record_unit(self.store, unit_bytes)?)?;
            self.observe_resident_state();
        }
        self.state_spool
            .store_pending_edges(std::mem::take(&mut self.caches.pending_edges))?;
        self.observe_resident_state();
        self.caches.clear_resident();
        Ok(())
    }

    pub(crate) fn finish_with_metrics(
        mut self,
    ) -> Result<(ProviderImportSummary, ProviderNormalizationStreamMetrics)> {
        match self.finish_import() {
            Ok(mut summary) => {
                if let Err(error) = self.finish_bulk_search() {
                    summary.push_maintenance_warning(
                        crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                        error.to_string(),
                    );
                }
                Ok((summary, self.metrics))
            }
            Err(error) if self.transaction.record_interruption_after_commit(&error) => {
                self.transaction.rollback(self.store);
                self.transaction
                    .apply_maintenance_warning(&mut self.summary);
                if let Err(finish_error) = self.finish_bulk_search() {
                    self.summary.push_maintenance_warning(
                        crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                        finish_error.to_string(),
                    );
                }
                Ok((std::mem::take(&mut self.summary), self.metrics))
            }
            Err(error) => {
                let _ = self.finish_bulk_search();
                Err(error)
            }
        }
    }

    pub(crate) fn abort(
        mut self,
        error: CaptureError,
    ) -> Result<(ProviderImportSummary, ProviderNormalizationStreamMetrics)> {
        self.transaction.rollback(self.store);
        let committed = matches!(error, CaptureError::CommittedImportMaintenance)
            || self.transaction.record_interruption_after_commit(&error);
        if committed {
            self.transaction
                .apply_maintenance_warning(&mut self.summary);
            if let Err(finish_error) = self.finish_bulk_search() {
                self.summary.push_maintenance_warning(
                    crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                    finish_error.to_string(),
                );
            }
            return Ok((std::mem::take(&mut self.summary), self.metrics));
        }
        let _ = self.finish_bulk_search();
        Err(error)
    }

    fn ensure_import_active(&mut self) -> Result<()> {
        if self.bulk_search_guard.is_none() {
            self.bulk_search_guard = Some(self.store.begin_event_search_bulk_mode()?);
        }
        self.transaction.ensure_active(self.store)
    }

    fn observe_resident_state(&mut self) {
        self.metrics.max_transaction_units = self
            .metrics
            .max_transaction_units
            .max(self.transaction.units);
        self.metrics.max_transaction_bytes = self
            .metrics
            .max_transaction_bytes
            .max(self.transaction.bytes);
        self.metrics.max_pi_identity_load_batch = self.metrics.max_pi_identity_load_batch.max(
            self.caches
                .pi_event_identities
                .as_ref()
                .map_or(0, ProviderPiEventIdentityInventory::max_load_batch),
        );
        let cache_entries = self.caches.imported_sessions.len()
            + self.caches.processed_sources.len()
            + self.caches.processed_sessions.len()
            + self.caches.imported_edges.len()
            + self.caches.processed_edges.len()
            + self.caches.session_exists.len()
            + self.caches.pending_edges.len();
        self.metrics.max_cache_entries = self.metrics.max_cache_entries.max(cache_entries);
    }

    fn finish_import(&mut self) -> Result<ProviderImportSummary> {
        let mut after = None;
        loop {
            let batch = self
                .state_spool
                .load_batch::<PendingProviderEdge>("pending_edges", after.as_deref())?;
            if batch.is_empty() {
                break;
            }
            self.ensure_import_active()?;
            for (id, edge) in &batch {
                let edge_id = Uuid::parse_str(id).map_err(|_| {
                    CaptureError::SystemInvariant(
                        "provider import state contains an invalid pending edge ID",
                    )
                })?;
                resolve_pending_provider_edge(
                    self.store,
                    &mut self.summary,
                    &mut self.caches,
                    edge_id,
                    edge.clone(),
                )?;
                self.caches.clear_resident();
            }
            after = batch.last().map(|(id, _)| id.clone());
        }
        if self.summary.failed == 0 {
            let mut after = None;
            loop {
                let batch = self
                    .state_spool
                    .load_batch::<ctx_history_core::SyncCursor>("cursors", after.as_deref())?;
                if batch.is_empty() {
                    break;
                }
                self.ensure_import_active()?;
                for (_, cursor) in &batch {
                    let unit_bytes =
                        serialized_len_or_rollback(&mut self.transaction, self.store, cursor)?;
                    require_transaction_continue(
                        self.transaction.prepare_unit(self.store, unit_bytes)?,
                    )?;
                    if let Err(err) = persist_provider_sync_cursor(self.store, cursor) {
                        self.transaction.rollback(self.store);
                        return Err(err);
                    }
                    require_transaction_continue(
                        self.transaction.record_unit(self.store, unit_bytes)?,
                    )?;
                    self.observe_resident_state();
                }
                after = batch.last().map(|(id, _)| id.clone());
            }
        }
        self.transaction.commit(self.store)?;
        self.transaction
            .apply_maintenance_warning(&mut self.summary);
        Ok(std::mem::take(&mut self.summary))
    }

    fn finish_bulk_search(&mut self) -> Result<()> {
        match self.bulk_search_guard.take() {
            Some(guard) => self
                .store
                .finish_event_search_bulk_mode(&guard)
                .map_err(CaptureError::from),
            None => Ok(()),
        }
    }
}

impl<F> ProviderNormalizationBatcher<F>
where
    F: FnMut(ProviderNormalizationResult) -> Result<()>,
{
    pub(crate) fn new(emit: F) -> Self {
        Self {
            emit,
            current: ProviderNormalizationResult::default(),
            records: 0,
        }
    }

    pub(crate) fn current_mut(&mut self) -> &mut ProviderNormalizationResult {
        &mut self.current
    }

    pub(crate) fn record_processed(&mut self) -> Result<()> {
        self.records = self.records.saturating_add(1);
        if self.records >= PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        self.flush()
    }

    fn flush(&mut self) -> Result<()> {
        if self.records == 0
            && self.current.captures.is_empty()
            && self.current.files_touched.is_empty()
            && self.current.summary.failures.is_empty()
            && self.current.summary.skipped == 0
            && self.current.summary.failed == 0
        {
            return Ok(());
        }
        self.records = 0;
        (self.emit)(std::mem::take(&mut self.current))
    }
}

pub(super) fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    import_normalized_provider_captures_with_content_policy(
        store,
        normalization,
        options,
        ProviderSessionContentPolicy::RequireRealMessage,
    )
}

pub(super) fn import_normalized_provider_captures_with_content_policy(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
    content_policy: ProviderSessionContentPolicy,
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
        content_policy,
    )
}

include!("batches/persistence.rs");
