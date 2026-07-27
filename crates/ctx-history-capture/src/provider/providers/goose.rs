use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_store::Store;

use crate::native_source::NativeSqliteValue;
use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod content;
mod lifecycle;
mod metrics;
mod native_path;
mod normalization;
mod position;
mod production;
mod schema;
mod source;
mod stream;

pub(crate) fn goose_result_record(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Option<crate::complete_content::sqlite::SqliteResultRecord>> {
    content::result_record(conn, rowid)
}

pub(crate) fn load_goose_message_values_schema(conn: &rusqlite::Connection) -> Result<()> {
    content::load_schema(conn)
}

pub(crate) fn load_goose_message_values(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    content::load_message_values(conn, rowid)
}

pub(crate) fn goose_complete_message(
    values: &[NativeSqliteValue],
) -> Result<(String, String, String)> {
    content::complete_message(values)
}

pub(crate) fn import_goose_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    production::import_goose_nativepath(path, store, context, import_options)
}

pub(crate) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    normalization::goose_timestamp(raw, fallback)
}

#[cfg(test)]
mod tests;
