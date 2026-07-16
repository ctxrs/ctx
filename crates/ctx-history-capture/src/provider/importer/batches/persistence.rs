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
        ProviderSessionContentPolicy::RequireRealMessage,
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
        ProviderSessionContentPolicy::RequireRealMessage,
    )
}

fn provider_transaction_batch_size() -> Option<NonZeroUsize> {
    NonZeroUsize::new(IMPORT_TRANSACTION_BATCH_UNITS)
}

#[allow(clippy::too_many_arguments)]
fn import_provider_capture_lines_with_batch_size(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    mut captures: Vec<(usize, ProviderCaptureEnvelope)>,
    mut files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
    transaction_batch_size: Option<NonZeroUsize>,
    suppress_search_merges: bool,
    content_policy: ProviderSessionContentPolicy,
) -> Result<ProviderImportSummary> {
    let caches = ProviderImportCaches::default();
    if content_policy == ProviderSessionContentPolicy::RequireRealMessage {
        filter_provider_capture_lines_without_real_session_messages(
            &mut summary,
            &mut captures,
            &mut files_touched,
        );
    }
    let supplied_file_touch_lines = files_touched
        .iter()
        .map(|(line_number, _)| *line_number)
        .collect::<BTreeSet<_>>();
    if content_policy == ProviderSessionContentPolicy::RequireRealMessage
        && summary.failed == 0
        && !provider_capture_lines_have_real_message(&captures)
    {
        let line = captures
            .first()
            .map(|(line_number, _)| *line_number)
            .or_else(|| files_touched.first().map(|(line_number, _)| *line_number))
            .unwrap_or(0);
        summary.failed += 1;
        summary.sample_failure(ProviderImportFailure {
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
        None => Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Complete),
    };
    match import_result {
        Ok(mut summary) => {
            apply_event_search_finalization(&mut summary, finish_result)?;
            Ok(summary)
        }
        Err(error) => Err(error),
    }
}

fn apply_event_search_finalization(
    summary: &mut ProviderImportSummary,
    result: Result<ctx_history_store::EventSearchBulkMaintenanceOutcome>,
) -> Result<()> {
    match result {
        Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Complete) => Ok(()),
        Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending) => {
            summary.push_maintenance_warning(
                crate::ProviderImportMaintenanceKind::EventSearchFinalizationPending,
                "event search maintenance remains queued",
            );
            Ok(())
        }
        Err(error) if is_retryable_import_pressure(&error) => {
            summary.push_maintenance_warning(
                crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                error.to_string(),
            );
            Ok(())
        }
        Err(error) => Err(error),
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
    let cursor_spool = ProviderImportStateSpool::new()?;
    if options.persist_cursors && summary.failed == 0 {
        for cursor in captures
            .iter()
            .filter_map(|(_, capture)| provider_sync_cursor(capture))
        {
            cursor_spool.store_cursor(&cursor)?;
        }
    }
    let mut transaction = ProviderImportTransaction::begin(
        store,
        has_captures && options.wrap_transaction,
        transaction_batch_size,
    )?;
    let persist_result = (|| -> Result<()> {
        for (line_number, capture) in captures {
            let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &capture)?;
            require_transaction_continue(transaction.prepare_unit(store, unit_bytes)?)?;
            match import_provider_capture_line(store, &capture, options, line_number, &mut caches) {
                Ok(line_summary) => summary.merge(line_summary),
                Err(err @ CaptureError::Store(_)) => return Err(err),
                Err(err) => {
                    summary.failed += 1;
                    summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            }
            require_transaction_continue(transaction.record_unit(store, unit_bytes)?)?;
        }
        resolve_pending_provider_edges_batched(store, &mut summary, &mut caches, &mut transaction)?;
        for (line_number, file) in files_touched {
            let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &file)?;
            require_transaction_continue(transaction.prepare_unit(store, unit_bytes)?)?;
            match import_provider_file_touched_line(store, &file, options) {
                Ok(()) => summary.accepted_content_records += 1,
                Err(err @ CaptureError::Store(_)) => return Err(err),
                Err(err) => {
                    summary.failed += 1;
                    summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            }
            require_transaction_continue(transaction.record_unit(store, unit_bytes)?)?;
        }
        if summary.failed == 0 {
            let mut after = None;
            loop {
                let batch = cursor_spool
                    .load_batch::<ctx_history_core::SyncCursor>("cursors", after.as_deref())?;
                if batch.is_empty() {
                    break;
                }
                for (_, cursor) in &batch {
                    let unit_bytes = serialized_len_or_rollback(&mut transaction, store, cursor)?;
                    require_transaction_continue(transaction.prepare_unit(store, unit_bytes)?)?;
                    persist_provider_sync_cursor(store, cursor)?;
                    require_transaction_continue(transaction.record_unit(store, unit_bytes)?)?;
                }
                after = batch.last().map(|(id, _)| id.clone());
            }
        }
        transaction.commit(store)?;
        Ok(())
    })();
    match persist_result {
        Ok(()) => {
            transaction.apply_maintenance_warning(&mut summary);
            Ok(summary)
        }
        Err(error)
            if matches!(error, CaptureError::CommittedImportMaintenance)
                || transaction.record_interruption_after_commit(&error) =>
        {
            transaction.rollback(store);
            transaction.apply_maintenance_warning(&mut summary);
            Ok(summary)
        }
        Err(error) => {
            transaction.rollback(store);
            Err(error)
        }
    }
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
        require_transaction_continue(transaction.prepare_unit(store, unit_bytes)?)?;
        if let Err(err) = resolve_pending_provider_edge(store, summary, caches, edge_id, edge) {
            transaction.rollback(store);
            return Err(err);
        }
        require_transaction_continue(transaction.record_unit(store, unit_bytes)?)?;
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
    halted: bool,
    batch_size: Option<NonZeroUsize>,
    units: usize,
    bytes: usize,
    byte_limit: usize,
    durable_materialization: bool,
    maintenance_warning: Option<crate::ProviderImportMaintenanceWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum ProviderImportTransactionStep {
    Continue,
    Halted,
}

fn require_transaction_continue(step: ProviderImportTransactionStep) -> Result<()> {
    match step {
        ProviderImportTransactionStep::Continue => Ok(()),
        ProviderImportTransactionStep::Halted => Err(CaptureError::CommittedImportMaintenance),
    }
}

impl ProviderImportTransaction {
    fn begin(store: &Store, has_work: bool, batch_size: Option<NonZeroUsize>) -> Result<Self> {
        if has_work {
            store.begin_immediate_batch()?;
        }
        Ok(Self {
            active: has_work,
            halted: false,
            batch_size,
            units: 0,
            bytes: 0,
            byte_limit: crate::disk_io_pacing::current_disk_io_burst_bytes()
                .and_then(|bytes| usize::try_from(bytes).ok())
                .map_or(IMPORT_TRANSACTION_BATCH_BYTES, |bytes| {
                    bytes.clamp(1, IMPORT_TRANSACTION_BATCH_BYTES)
                }),
            durable_materialization: false,
            maintenance_warning: None,
        })
    }

    pub(crate) fn begin_bounded(store: &Store, has_work: bool) -> Result<Self> {
        Self::begin(store, has_work, provider_transaction_batch_size())
    }

    fn ensure_active(&mut self, store: &Store) -> Result<()> {
        if self.halted {
            return Err(CaptureError::CommittedImportMaintenance);
        }
        if self.active {
            return Ok(());
        }
        store.begin_immediate_batch()?;
        self.active = true;
        self.units = 0;
        self.bytes = 0;
        Ok(())
    }

    pub(crate) fn prepare_unit(
        &mut self,
        store: &Store,
        unit_bytes: usize,
    ) -> Result<ProviderImportTransactionStep> {
        if self.halted {
            return Ok(ProviderImportTransactionStep::Halted);
        }
        let result = if self.active
            && self.batch_size.is_some()
            && self.units > 0
            && self.bytes.saturating_add(unit_bytes) > self.byte_limit
        {
            self.rotate(store)
        } else {
            Ok(ProviderImportTransactionStep::Continue)
        };
        if result.is_err() {
            self.rollback(store);
        }
        if matches!(&result, Ok(ProviderImportTransactionStep::Continue)) {
            crate::pace_current_disk_io(unit_bytes as u64);
        }
        result
    }

    pub(crate) fn record_unit(
        &mut self,
        store: &Store,
        unit_bytes: usize,
    ) -> Result<ProviderImportTransactionStep> {
        if self.halted {
            return Ok(ProviderImportTransactionStep::Halted);
        }
        if !self.active {
            return Ok(ProviderImportTransactionStep::Continue);
        }
        self.units = self.units.saturating_add(1);
        self.bytes = self.bytes.saturating_add(unit_bytes);
        let below_unit_limit = self
            .batch_size
            .is_none_or(|batch_size| self.units < batch_size.get());
        let below_byte_limit = self.batch_size.is_none() || self.bytes < self.byte_limit;
        if below_unit_limit && below_byte_limit {
            return Ok(ProviderImportTransactionStep::Continue);
        }
        let result = self.rotate(store);
        if result.is_err() {
            self.rollback(store);
        }
        result
    }

    fn rotate(&mut self, store: &Store) -> Result<ProviderImportTransactionStep> {
        self.rotate_with_maintenance(store, || store.maintain_event_search_bulk_mode())
    }

    fn rotate_with_maintenance<F>(
        &mut self,
        store: &Store,
        maintain: F,
    ) -> Result<ProviderImportTransactionStep>
    where
        F: FnOnce() -> ctx_history_store::Result<
            ctx_history_store::EventSearchBulkMaintenanceOutcome,
        >,
    {
        crate::pace_current_disk_io(u64::try_from(self.bytes).unwrap_or(u64::MAX));
        store.commit_batch()?;
        self.active = false;
        self.durable_materialization = true;

        match maintain() {
            Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Complete) => {}
            Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending) => {
                self.set_maintenance_warning(
                    crate::ProviderImportMaintenanceKind::ImportInterruptedAfterCommit,
                    "event search maintenance paused further provider import admission".to_owned(),
                );
                return Ok(ProviderImportTransactionStep::Halted);
            }
            Err(error) => {
                return self.defer_rotation_error(
                    error,
                    crate::ProviderImportMaintenanceKind::EventSearchFinalization,
                );
            }
        }
        if let Err(error) = store.checkpoint_wal_truncate_required() {
            return self
                .defer_rotation_error(error, crate::ProviderImportMaintenanceKind::WalCheckpoint);
        }
        if let Err(error) = store.begin_immediate_batch() {
            return self.defer_rotation_error(
                error,
                crate::ProviderImportMaintenanceKind::TransactionContinuation,
            );
        }
        self.active = true;
        self.units = 0;
        self.bytes = 0;
        Ok(ProviderImportTransactionStep::Continue)
    }

    fn defer_rotation_error(
        &mut self,
        error: ctx_history_store::StoreError,
        kind: crate::ProviderImportMaintenanceKind,
    ) -> Result<ProviderImportTransactionStep> {
        if !is_retryable_import_pressure(&error) {
            return Err(error.into());
        }
        self.set_maintenance_warning(kind, error.to_string());
        Ok(ProviderImportTransactionStep::Halted)
    }

    pub(crate) fn commit(&mut self, store: &Store) -> Result<()> {
        let result = if self.active {
            crate::pace_current_disk_io(u64::try_from(self.bytes).unwrap_or(u64::MAX));
            store.commit_batch().map_err(CaptureError::from)
        } else {
            Ok(())
        };
        if result.is_ok() {
            self.active = false;
            self.durable_materialization |= self.units > 0;
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

    pub(crate) fn apply_maintenance_warning(&mut self, summary: &mut ProviderImportSummary) {
        if let Some(warning) = self.maintenance_warning.take() {
            summary.maintenance_warnings.push(warning);
        }
    }

    pub(crate) fn record_interruption_after_commit(&mut self, error: &CaptureError) -> bool {
        let resumable = matches!(error, CaptureError::CommittedImportMaintenance)
            || is_retryable_import_pressure(error);
        if !self.durable_materialization || !resumable {
            return false;
        }
        self.set_maintenance_warning(
            crate::ProviderImportMaintenanceKind::ImportInterruptedAfterCommit,
            error.to_string(),
        );
        true
    }

    fn set_maintenance_warning(
        &mut self,
        kind: crate::ProviderImportMaintenanceKind,
        error: String,
    ) {
        self.halted = true;
        self.maintenance_warning
            .get_or_insert(crate::ProviderImportMaintenanceWarning { kind, error });
    }
}

fn is_retryable_import_pressure(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if matches!(
            error.downcast_ref::<ctx_history_store::StoreError>(),
            Some(
                ctx_history_store::StoreError::WalCheckpointBusy { .. }
                    | ctx_history_store::StoreError::BulkSearchImportBusy
            )
        ) {
            return true;
        }
        if matches!(
            error
                .downcast_ref::<rusqlite::Error>()
                .and_then(rusqlite::Error::sqlite_error_code),
            Some(rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked)
        ) {
            return true;
        }
        if matches!(
            error.downcast_ref::<rusqlite::ffi::Error>(),
            Some(rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::DatabaseBusy
                    | rusqlite::ffi::ErrorCode::DatabaseLocked,
                ..
            })
        ) {
            return true;
        }
        current = error.source();
    }
    false
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use crate::{install_disk_io_pacer, DiskIoPacer};

    fn sqlite_store_error(code: i32) -> ctx_history_store::StoreError {
        ctx_history_store::StoreError::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    }

    #[test]
    fn durable_transaction_batch_charges_actual_serialized_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let pacer = DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = install_disk_io_pacer(pacer.clone());
        let mut transaction = ProviderImportTransaction::begin_bounded(&store, true).unwrap();

        assert_eq!(
            transaction
                .record_unit(&store, IMPORT_TRANSACTION_BATCH_BYTES)
                .unwrap(),
            ProviderImportTransactionStep::Continue
        );
        transaction.commit(&store).unwrap();

        assert_eq!(pacer.charged_bytes(), IMPORT_TRANSACTION_BATCH_BYTES as u64);
    }

    #[test]
    fn retryable_pressure_is_classified_through_error_chains() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = CaptureError::Store(sqlite_store_error(code));
            assert!(is_retryable_import_pressure(&error));
        }
        let direct_sqlite_error = CaptureError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY_RECOVERY),
            None,
        ));
        assert!(is_retryable_import_pressure(&direct_sqlite_error));
        assert!(is_retryable_import_pressure(
            &ctx_history_store::StoreError::WalCheckpointBusy {
                log_frames: 2,
                checkpointed_frames: 1,
            }
        ));
        assert!(is_retryable_import_pressure(
            &ctx_history_store::StoreError::BulkSearchImportBusy
        ));

        for code in [rusqlite::ffi::SQLITE_FULL, rusqlite::ffi::SQLITE_CORRUPT] {
            let error = CaptureError::Store(sqlite_store_error(code));
            assert!(!is_retryable_import_pressure(&error));
        }
        assert!(!is_retryable_import_pressure(
            &ctx_history_store::StoreError::Io(std::io::Error::other("fatal test I/O failure"))
        ));
        assert!(!is_retryable_import_pressure(
            &ctx_history_store::StoreError::InvalidBulkSearchGuard
        ));
    }

    #[test]
    fn rotation_defers_only_retryable_pressure() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let errors = [
            ctx_history_store::StoreError::WalCheckpointBusy {
                log_frames: 2,
                checkpointed_frames: 1,
            },
            ctx_history_store::StoreError::BulkSearchImportBusy,
            sqlite_store_error(rusqlite::ffi::SQLITE_BUSY),
            sqlite_store_error(rusqlite::ffi::SQLITE_LOCKED),
        ];

        for error in errors {
            let mut transaction = ProviderImportTransaction::begin_bounded(&store, true).unwrap();
            transaction.units = 1;
            assert_eq!(
                transaction
                    .rotate_with_maintenance(&store, || Err(error))
                    .unwrap(),
                ProviderImportTransactionStep::Halted
            );
            assert!(transaction.durable_materialization);
            assert!(!transaction.active);
            assert!(transaction.halted);
            assert!(transaction.maintenance_warning.is_some());
        }
    }

    #[test]
    fn rotation_propagates_disk_full_and_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        for (sqlite_code, expected_code) in [
            (
                rusqlite::ffi::SQLITE_FULL,
                rusqlite::ffi::ErrorCode::DiskFull,
            ),
            (
                rusqlite::ffi::SQLITE_CORRUPT,
                rusqlite::ffi::ErrorCode::DatabaseCorrupt,
            ),
        ] {
            let mut transaction = ProviderImportTransaction::begin_bounded(&store, true).unwrap();
            transaction.units = 1;
            let error = transaction
                .rotate_with_maintenance(&store, || Err(sqlite_store_error(sqlite_code)))
                .unwrap_err();
            assert!(matches!(
                &error,
                CaptureError::Store(ctx_history_store::StoreError::Sql(error))
                    if error.sqlite_error_code() == Some(expected_code)
            ));
            assert!(!transaction.record_interruption_after_commit(&error));
        }
    }

    #[test]
    fn fts_pending_stops_further_admission() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let pacer = DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = install_disk_io_pacer(pacer.clone());
        let mut transaction = ProviderImportTransaction::begin_bounded(&store, true).unwrap();
        transaction.units = 1;
        transaction.bytes = 256;

        assert_eq!(
            transaction
                .rotate_with_maintenance(&store, || {
                    Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending)
                })
                .unwrap(),
            ProviderImportTransactionStep::Halted
        );
        assert!(transaction.durable_materialization);
        assert!(!transaction.active);
        assert_eq!(pacer.charged_bytes(), 256);
        assert_eq!(
            transaction.prepare_unit(&store, 128).unwrap(),
            ProviderImportTransactionStep::Halted
        );
        assert_eq!(pacer.charged_bytes(), 256);
        assert!(matches!(
            transaction.maintenance_warning.as_ref(),
            Some(crate::ProviderImportMaintenanceWarning {
                kind: crate::ProviderImportMaintenanceKind::ImportInterruptedAfterCommit,
                ..
            })
        ));
    }

    #[test]
    fn finalization_defers_busy_but_propagates_fatal_sqlite_errors() {
        let mut summary = ProviderImportSummary::default();
        for error in [
            ctx_history_store::StoreError::BulkSearchImportBusy,
            sqlite_store_error(rusqlite::ffi::SQLITE_BUSY),
            sqlite_store_error(rusqlite::ffi::SQLITE_LOCKED),
        ] {
            apply_event_search_finalization(&mut summary, Err(CaptureError::Store(error))).unwrap();
        }
        assert_eq!(summary.maintenance_warnings.len(), 3);
        assert!(summary.maintenance_warnings.iter().all(|warning| {
            warning.kind == crate::ProviderImportMaintenanceKind::EventSearchFinalization
        }));

        for code in [rusqlite::ffi::SQLITE_FULL, rusqlite::ffi::SQLITE_CORRUPT] {
            let error = apply_event_search_finalization(
                &mut ProviderImportSummary::default(),
                Err(CaptureError::Store(sqlite_store_error(code))),
            )
            .unwrap_err();
            assert!(matches!(error, CaptureError::Store(_)));
        }
    }
}
