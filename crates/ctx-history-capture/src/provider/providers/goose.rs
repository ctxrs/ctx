use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_store::Store;

use crate::native_source::NativeSqliteValue;
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

mod content;
mod lifecycle;
mod metrics;
mod native_path;
mod normalization;
mod position;
mod schema;
mod source;
mod source_backed;
mod stream;

pub(crate) use source_backed::{
    GooseSourceBackedAdapterV0, GooseSourceBackedResolverV0, GooseSourceBackedSelectionV0,
    GooseSourceBackedSnapshotV0, GooseSourceRouteV0,
};

pub(crate) fn load_goose_message_values_schema(conn: &rusqlite::Connection) -> Result<()> {
    content::load_schema(conn)
}

pub(crate) fn load_goose_message_values(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    content::load_message_values(conn, rowid)
}

pub(crate) fn goose_complete_message_with_normalized_hash(
    conn: &rusqlite::Connection,
    values: &[NativeSqliteValue],
) -> Result<(String, String, String, String)> {
    content::complete_message_with_normalized_hash(conn, values)
}

/// Rejects the released Store-ingestion entrypoint while shared dispatch still
/// carries its historical signature. Goose ingestion is source-backed.
pub(crate) fn import_goose_nativepath(
    _path: &Path,
    _store: &mut Store,
    _context: ProviderAdapterContext,
    _import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::InvalidPayload(
        "Goose Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}

pub(crate) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    normalization::goose_timestamp(raw, fallback)
}

#[cfg(test)]
pub(crate) mod tests;
