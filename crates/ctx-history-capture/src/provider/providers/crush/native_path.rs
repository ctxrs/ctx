//! Crush-owned SQLite scanning and source-backed projection.

mod query;
pub(crate) mod source_backed;

use std::collections::BTreeSet;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::{
    native_source::NativeSqliteValue, provider::sqlite::sqlite_schema_fingerprint, Result,
};

use super::{
    projection::{CrushMessageRow, CrushSessionRow},
    source::session_columns,
};

const CRUSH_NATIVE_MAX_ROW_BYTES: u64 = 6 * 1024 * 1024;
const CRUSH_NATIVE_MAX_EVENT_TOUCHES: usize = 3_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CrushNativeFrontier {
    after_rowid: Option<i64>,
}

struct CrushLoadedRow {
    row: CrushMessageRow,
    session: Option<CrushSessionRow>,
    digest_values: Vec<NativeSqliteValue>,
}

struct CrushNativeSchema {
    session_columns: BTreeSet<String>,
    message_columns: BTreeSet<String>,
    schema_fingerprint: String,
}

fn read_native_schema(connection: &Connection) -> Result<CrushNativeSchema> {
    Ok(CrushNativeSchema {
        session_columns: session_columns(connection)?,
        message_columns: super::source::message_columns(connection)?,
        schema_fingerprint: sqlite_schema_fingerprint(connection)?,
    })
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}
