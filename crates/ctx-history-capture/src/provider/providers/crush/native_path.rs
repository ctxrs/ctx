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
    projection::{CrushFileRow, CrushMessageRow, CrushReadFileRow, CrushSessionRow},
    source::{optional_file_columns, optional_read_file_columns, session_columns},
};

const CRUSH_NATIVE_MAX_ROW_BYTES: u64 = 6 * 1024 * 1024;
const CRUSH_NATIVE_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;
const CRUSH_NATIVE_MAX_EVENT_TOUCHES: usize = 3_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrushNativePhase {
    Sessions,
    Messages,
    Files,
    ReadFiles,
}

impl CrushNativePhase {
    fn label(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Messages => "messages",
            Self::Files => "files",
            Self::ReadFiles => "read_files",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CrushNativeFrontier {
    phase: CrushNativePhase,
    after_rowid: Option<i64>,
    next_ordinal: u64,
}

// This is the per-row hydration result. Boxing the 400-byte message row merely
// to approach the 184-byte session variant would add an allocation per row.
#[allow(clippy::large_enum_variant)]
enum CrushHydratedRow {
    Session {
        row: CrushSessionRow,
        retained_bytes: usize,
    },
    Message {
        row: CrushMessageRow,
        session: Option<CrushSessionRow>,
        digest_values: Vec<NativeSqliteValue>,
        retained_bytes: usize,
    },
    File {
        row: CrushFileRow,
        retained_bytes: usize,
    },
    ReadFile {
        row: CrushReadFileRow,
        retained_bytes: usize,
    },
}

struct CrushNativeSchema {
    session_columns: BTreeSet<String>,
    message_columns: BTreeSet<String>,
    file_columns: Option<BTreeSet<String>>,
    read_file_columns: Option<BTreeSet<String>>,
    user_version: i64,
    schema_fingerprint: String,
}

fn read_native_schema(connection: &Connection) -> Result<CrushNativeSchema> {
    Ok(CrushNativeSchema {
        session_columns: session_columns(connection)?,
        message_columns: super::source::message_columns(connection)?,
        file_columns: optional_file_columns(connection)?,
        read_file_columns: optional_read_file_columns(connection)?,
        user_version: connection.pragma_query_value(None, "user_version", |row| row.get(0))?,
        schema_fingerprint: sqlite_schema_fingerprint(connection)?,
    })
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}
