mod model;
pub(crate) mod native_path;
mod source;

#[cfg(test)]
mod tests;

use rusqlite::Connection;

use crate::native_source::NativeSqliteValue;
use crate::{CaptureError, Result};

pub(super) const ASTRBOT_CAPTURE_REVISION: u32 = 4;
pub(super) const ASTRBOT_POLICY_REVISION: u32 = 7;

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
