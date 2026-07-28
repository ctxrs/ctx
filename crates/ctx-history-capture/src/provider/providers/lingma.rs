use std::path::Path;

use ctx_history_core::EventType;
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::Value;

use crate::native_source::NativeSqliteValue;
use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod native_path;

pub(crate) use native_path::{
    scan_lingma_source_backed_v0, LingmaDatabaseScanV0, LingmaDatabaseSourceV0,
    LingmaExactContentCapabilityV0, LingmaExactContentFailureKindV0, LingmaExactContentFailureV0,
    LingmaHydratedContentV0, LingmaSourceBackedErrorV0, LingmaSourceBackedRecordV0,
    LingmaSourceBackedResolverV0, LingmaSourceBackedResultV0, LingmaSourceBackedScanV0,
    LingmaSourceInventoryV0,
};

pub(crate) fn import_lingma_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_lingma_native_path(path, store, context, import_options)
}

// Kept solely for the already-released verified-content resolver. Ingestion no
// longer uses the traditional SQLite producer/projector.
pub(crate) fn lingma_complete_values(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    native_path::lingma_complete_values(conn, rowid)
}

pub(crate) fn lingma_complete_user_message(
    values: &[NativeSqliteValue],
) -> Result<(LingmaCompleteEvent, String)> {
    let (event, text) = native_path::lingma_complete_user_message(values)?;
    let _ = &event.idempotency_key;
    Ok((
        LingmaCompleteEvent {
            provider_event_index: event.provider_event_index,
            provider_event_hash: event.provider_event_hash,
            released_provider_event_hash: event.released_provider_event_hash,
            cursor: event.cursor,
            event_type: event.event_type,
            payload: event.payload,
        },
        text,
    ))
}

/// Fields needed to verify current fallback and released provider-authority locators.
pub(crate) struct LingmaCompleteEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: String,
    pub(crate) released_provider_event_hash: String,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) payload: Value,
}

#[cfg(test)]
#[path = "lingma/tests.rs"]
mod tests;
