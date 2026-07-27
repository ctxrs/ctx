mod model;
mod native_path;
mod preferences;
mod source;

#[cfg(test)]
mod tests;

use std::path::Path;

use ctx_history_store::Store;
use rusqlite::Connection;

use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

pub(super) const ASTRBOT_CAPTURE_REVISION: u32 = 4;
pub(super) const ASTRBOT_POLICY_REVISION: u32 = 7;
const ASTRBOT_COMPLETE_MESSAGE_LOCATOR_KIND: &str = "astrbot-conversation-message-v1";

pub(crate) fn astrbot_complete_message_locator(
    physical_rowid: i64,
    item_index: usize,
) -> Result<NativeLocator> {
    let item_index = u32::try_from(item_index).map_err(|_| {
        CaptureError::InvalidPayload("AstrBot message index exceeds u32".to_owned())
    })?;
    let mut value = Vec::with_capacity(12);
    value.extend_from_slice(&model::ordered_i64(physical_rowid).to_be_bytes());
    value.extend_from_slice(&item_index.to_be_bytes());
    NativeLocator::new(ASTRBOT_COMPLETE_MESSAGE_LOCATOR_KIND, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

/// Compatibility seam owned by the existing complete-content resolver. It is
/// not part of ingestion and never publishes an ingestion page.
pub(crate) fn astrbot_complete_conversation_values(
    conn: &Connection,
    physical_rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let sql = source::AstrBotSql::new(conn)?;
    source::hydrate_conversation(conn, &sql.conversation_hydration, physical_rowid)
        .map(model::conversation_values)
        .map(Some)
        .or_else(|error| match error {
            CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            error => Err(error),
        })
}

/// Compatibility seam owned by the existing complete-content resolver.
pub(crate) fn astrbot_complete_conversation_message(
    values: &[NativeSqliteValue],
    item_index: u32,
) -> Result<Option<model::AstrBotCompleteMessage>> {
    model::complete_conversation_message(values, item_index)
}

/// The public registration keeps this historical symbol while its only
/// implementation is the provider-owned NativePath vertical.
pub(crate) fn import_astrbot_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_astrbot_native_path(path, store, context, import_options)
}
