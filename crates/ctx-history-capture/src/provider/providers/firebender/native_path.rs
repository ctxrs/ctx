//! Firebender source-backed SQLite capture.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    native_source::NativeSqliteValue,
    provider::{
        native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES,
        provider_path_identity,
        sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    },
    CaptureError, Result,
};

use super::firebender_chat_history_db_path;

mod source_backed;
#[cfg(test)]
mod tests;

pub(crate) use source_backed::register_source_backed_route;

const FIREBENDER_NATIVE_PARSER_REVISION: u32 = 1;
const FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;
const FIREBENDER_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;

#[derive(Debug)]
struct FirebenderPathIdentity {
    canonical_database_path: PathBuf,
    route_identity: String,
}

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

fn firebender_path_identity(path: &Path) -> Result<FirebenderPathIdentity> {
    let canonical_database_path = absolute_path(&firebender_chat_history_db_path(path)?)?;
    let route_identity = provider_path_identity(&canonical_database_path)?;
    Ok(FirebenderPathIdentity {
        canonical_database_path,
        route_identity,
    })
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
