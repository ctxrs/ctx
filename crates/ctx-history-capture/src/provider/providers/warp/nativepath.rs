//! Source-backed Warp parsing and task-message evidence.

use rusqlite::Connection;

use super::schema::WarpSqliteSchema;
use crate::{CaptureError, Result};

mod decode;
mod model;
mod query;

pub(super) use model::{
    WarpNativeCounters, WarpNativeEvent, WarpNativeMessageIdentity, WarpNativePage,
    WarpNativeSession, WarpNativeSink,
};
#[cfg(test)]
pub(super) use model::{WarpNativeEventIdentity, WarpNativeOrder};
use query::scan_warp_native_pinned_snapshot;

pub(in super::super) struct WarpNativeSourceBackedScan {
    pub(in super::super) source_integrity_digest: String,
    pub(in super::super) counters: WarpNativeCounters,
}

/// Runs the Warp parser against a caller-owned, certified SQLite read
/// connection. Publication and source lifecycle are owned by the shared
/// source-backed coordinator.
pub(in super::super) fn scan_warp_source_backed_connection(
    connection: &Connection,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeSourceBackedScan> {
    let schema = WarpSqliteSchema::detect(connection)?;
    validate_source_paging_compatibility(connection)?;
    let result = scan_warp_native_pinned_snapshot(connection, &schema, sink)?;
    Ok(WarpNativeSourceBackedScan {
        source_integrity_digest: result.source_integrity_digest,
        counters: result.counters,
    })
}

fn validate_source_paging_compatibility(connection: &Connection) -> Result<()> {
    let invalid_rowid: bool = connection.query_row(
        "select exists(select 1 from agent_conversations where rowid <= 0)
             or exists(select 1 from agent_tasks where rowid <= 0)",
        [],
        |row| row.get(0),
    )?;
    if invalid_rowid {
        return Err(CaptureError::InvalidPayload(
            "Warp source-backed paging requires positive 64-bit source rowids".to_owned(),
        ));
    }
    Ok(())
}
