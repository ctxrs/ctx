use std::path::Path;

use ctx_history_store::Store;

use crate::native_source::NativeSqliteValue;
use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod capture;
mod native_path;
mod projection;
mod source;

#[cfg(test)]
mod tests;

pub(super) const CRUSH_CAPTURE_REVISION: u32 = 3;
pub(super) const CRUSH_POLICY_REVISION: u32 = 6;

pub(crate) fn load_crush_message_values(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    let session_columns = source::session_columns(conn)?;
    let message_columns = source::message_columns(conn)?;
    let parent_created_at = source::optional_session_column(&session_columns, "created_at");
    let parent_updated_at = source::optional_session_column(&session_columns, "updated_at");
    let projection = source::message_projection(&message_columns, "m");
    let sql = format!(
        "select s.rowid, cast({parent_created_at} as integer), \
                cast({parent_updated_at} as integer), {projection} \
         from messages m left join sessions s on s.id = m.session_id where m.rowid = ?1"
    );
    conn.query_row(&sql, [rowid], capture::message_child_values)
        .map_err(Into::into)
}

pub(crate) fn load_crush_message_values_schema(conn: &rusqlite::Connection) -> Result<()> {
    source::session_columns(conn)?;
    source::message_columns(conn)?;
    Ok(())
}

pub(crate) fn crush_complete_message(
    values: &[NativeSqliteValue],
) -> Result<(String, String, String, String)> {
    projection::crush_complete_message(values)
}

pub(crate) fn import_crush_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_crush_native_path(path, store, context, import_options)
}
