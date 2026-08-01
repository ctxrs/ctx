//! Firebender source-backed SQLite capture.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    native_source::NativeSqliteValue,
    provider::{
        native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES,
        sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    },
    CaptureError, Result,
};

use super::firebender_chat_history_db_path;

mod source_backed;
#[cfg(test)]
mod tests;

pub(crate) use source_backed::register_source_backed_route;

const FIREBENDER_NATIVE_PARSER_REVISION: u32 = 2;
const FIREBENDER_SOURCE_IDENTITY_REVISION: u32 = 2;
const FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;
const FIREBENDER_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;

// SHA-256("ctx.firebender.selected-chat-history.default-catalog-lineage.v1").
// This is the logical discovery slot, not a digest or reversible encoding of
// the user-specific database path.
const FIREBENDER_SELECTED_CATALOG_LINEAGE_V1: [u8; 32] = [
    0xe4, 0x31, 0x55, 0xce, 0xe7, 0x9c, 0x55, 0x3e, 0x4a, 0xb6, 0x4a, 0xc7, 0xca, 0x41, 0xbf, 0x45,
    0x76, 0xbb, 0x44, 0x68, 0xcf, 0x3e, 0x38, 0xfd, 0x89, 0xa4, 0x30, 0xbb, 0x4a, 0xe9, 0x4f, 0x7f,
];

#[derive(Debug)]
struct FirebenderRow {
    rowid: i64,
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    messages_json: String,
    metadata_json: String,
    messages: Vec<Value>,
}

impl FirebenderRow {
    fn logical_values(&self) -> Vec<NativeSqliteValue> {
        vec![
            NativeSqliteValue::Text(self.id.clone()),
            NativeSqliteValue::Text(self.name.clone()),
            NativeSqliteValue::Integer(self.created_at),
            NativeSqliteValue::Integer(self.updated_at),
            NativeSqliteValue::Text(self.messages_json.clone()),
            NativeSqliteValue::Text(self.metadata_json.clone()),
        ]
    }
}

fn firebender_database_path(path: &Path) -> Result<PathBuf> {
    absolute_path(&firebender_chat_history_db_path(path)?)
}

fn validate_schema(conn: &Connection, _path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "chat_sessions")? {
        return Err(CaptureError::UnsupportedSchemaVersion(
            FIREBENDER_NATIVE_PARSER_REVISION,
        ));
    }
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Firebender chat_sessions table",
        &[
            "id",
            "name",
            "created_at",
            "updated_at",
            "messages_json",
            "metadata_json",
        ],
    )
    .map_err(|_| CaptureError::UnsupportedSchemaVersion(FIREBENDER_NATIVE_PARSER_REVISION))
}

fn firebender_raw_row_digest(values: &[NativeSqliteValue]) -> [u8; 32] {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
