use std::{cell::RefCell, fs, num::NonZeroUsize, path::Path, rc::Rc};

#[cfg(test)]
use std::path::PathBuf;

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::sqlite_logical_rows::{
    SqliteLogicalRowBatchProducer, SqliteLogicalRowsBatchError,
};
use crate::captured_batch::{
    SourceObservation, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, import_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::sqlite::{
    open_provider_sqlite_readonly, sqlite_schema_fingerprint, with_sqlite_read_snapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
};

const DEEPAGENTS_CAPTURE_REVISION: u32 = 4;
const DEEPAGENTS_POLICY_REVISION: u32 = 7;
const DEEPAGENTS_POSITION_KIND: &str = "deepagents-logical-rowid-v2";
const DEEPAGENTS_WRITE_LOCATOR_KIND: &str = "deepagents-write-rowid-v2";
const DEEPAGENTS_THREAD_LOCATOR_KIND: &str = "deepagents-thread-rowid-v2";
const DEEPAGENTS_WRITE_RECORD_KIND: &str = "deepagents-message-write-v1";
const DEEPAGENTS_REJECTED_WRITE_RECORD_KIND: &str = "deepagents-rejected-write-v1";
const DEEPAGENTS_THREAD_RECORD_KIND: &str = "deepagents-thread-summary-v1";
const DEEPAGENTS_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 32 * 16;

// Admission, certified resume, and safe-group publication stay visible here as the provider's
// orchestration contract. The modules below own the bounded responsibilities it coordinates.
mod complete_content;
mod cursor;
mod ledger;
mod message;
mod producer;
mod projector;
mod record;
mod source;

pub(crate) use complete_content::{
    decode_deepagents_content_address, resolve_deepagents_content,
    validate_deepagents_content_schema, DeepAgentsContentAddress, DEEPAGENTS_CONTENT_LOCATOR_KIND,
};
use cursor::{decode_deepagents_position, initial_deepagents_position};
use producer::DeepAgentsRowFetcher;
use projector::DeepAgentsCapturedBatchProjector;
use source::{deepagents_source_revision, deepagents_source_snapshot, deepagents_validate_schema};

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum DeepAgentsImportTraceEvent {
    BatchRequested(usize),
    GroupPublished(usize),
    WriteKeyHydrated(i64),
    WriteHydrated(i64),
    CheckpointMetadataPreflightQueried(String),
    CheckpointMetadataHydrated(String),
    ThreadMetadataHydrated(String),
    SourceExhausted,
}

#[cfg(test)]
thread_local! {
    static DEEPAGENTS_IMPORT_TRACE: RefCell<Option<Vec<DeepAgentsImportTraceEvent>>> = const {
        RefCell::new(None)
    };
}

#[cfg(test)]
fn deepagents_trace(event: DeepAgentsImportTraceEvent) {
    DEEPAGENTS_IMPORT_TRACE.with(|trace| {
        if let Some(events) = trace.borrow_mut().as_mut() {
            events.push(event);
        }
    });
}

fn deepagents_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Deep Agents byte limit exceeds u64"))
}

fn deepagents_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn deepagents_sqlite_batch_error(error: SqliteLogicalRowsBatchError<CaptureError>) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

pub(crate) fn import_deepagents_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = deepagents_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    deepagents_validate_schema(&conn, path)?;
    let user_version = conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        format!("deepagents-sqlite:{cursor_path}"),
        deepagents_source_revision(&snapshot, &schema_fingerprint),
        cursor_stream,
        DEEPAGENTS_CAPTURE_REVISION,
        DEEPAGENTS_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(deepagents_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let mut expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_deepagents_position()?;
    let mut start_position = initial_position.clone();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.matches_revisions(
                    source.source_revision(),
                    source.capture_revision(),
                    source.policy_revision(),
                ) =>
            {
                let _: () = certified.parser_checkpoint().deserialize()?;
                decode_deepagents_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let mut projector = DeepAgentsCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: context
            .source_path
            .as_ref()
            .map(|source_path| source_path.display().to_string()),
        user_version,
        schema_fingerprint,
        source_revision: source.source_revision().to_owned(),
        committed_store: Some(Store::open_read_only(store.path())?),
    };
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).ok_or(
        CaptureError::SystemInvariant("captured batch group limit must be nonzero"),
    )?;
    let mut merged = ProviderImportSummary::default();
    let committed_store = Store::open_read_only(store.path())?;
    let fetcher = Rc::new(RefCell::new(DeepAgentsRowFetcher::new(
        &conn,
        context.clone(),
        Some(committed_store),
    )?));
    let producer_fetcher = Rc::clone(&fetcher);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source.clone(),
        start_position.clone(),
        move |position| {
            producer_fetcher
                .try_borrow_mut()
                .map_err(|_| {
                    CaptureError::SystemInvariant(
                        "Deep Agents row fetcher is already mutably borrowed",
                    )
                })?
                .fetch(position)
        },
    );
    let mut batch_requests = 0_usize;
    loop {
        let outcome = import_captured_batches(
            store,
            &admission,
            import_options.clone(),
            &context.machine_id,
            context.imported_at,
            expected_store_cursor.as_ref(),
            &initial_position,
            cursor_mode,
            max_batches,
            &mut projector,
            || {
                if batch_requests > 0 {
                    fetcher
                        .try_borrow_mut()
                        .map_err(|_| {
                            CaptureError::SystemInvariant(
                                "Deep Agents row fetcher is already mutably borrowed",
                            )
                        })?
                        .reset_for_batch_request();
                }
                batch_requests =
                    batch_requests
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Deep Agents batch request count overflowed",
                        ))?;
                #[cfg(test)]
                deepagents_trace(DeepAgentsImportTraceEvent::BatchRequested(batch_requests));
                if !snapshot.revalidate(path)? {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                let batch = with_sqlite_read_snapshot(&conn, || {
                    producer.next_batch().map_err(deepagents_sqlite_batch_error)
                })?;
                if !snapshot.revalidate(path)? {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                Ok(batch)
            },
            || snapshot.revalidate(path),
        )?;
        merged.merge_from(outcome.summary);
        #[cfg(test)]
        if outcome.batches_imported > 0 {
            deepagents_trace(DeepAgentsImportTraceEvent::GroupPublished(
                outcome.batches_imported,
            ));
        }
        if outcome.source_exhausted || outcome.batches_imported == 0 {
            #[cfg(test)]
            deepagents_trace(DeepAgentsImportTraceEvent::SourceExhausted);
            return Ok(merged);
        }
        if import_options.capture_work_limit == crate::CaptureWorkLimit::OneSafeGroup {
            merged.work_remaining = true;
            return Ok(merged);
        }
        expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
        if expected_store_cursor.is_none() {
            return Err(CaptureError::SystemInvariant(
                "published Deep Agents captured-batch cursor could not be reloaded",
            ));
        }
        let certified = CertifiedProviderCursor::decode_if_certified(
            &expected_store_cursor
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "published Deep Agents captured-batch cursor disappeared",
                ))?
                .cursor,
        )?
        .ok_or(CaptureError::SystemInvariant(
            "published Deep Agents cursor is not certified",
        ))?;
        if producer.current_position() != certified.native_position() {
            return Err(CaptureError::SystemInvariant(
                "published Deep Agents cursor does not match the persistent producer position",
            ));
        }
        cursor_mode = CapturedBatchCursorMode::Resume;
    }
}

#[cfg(test)]
#[path = "deepagents/tests/mod.rs"]
mod tests;
