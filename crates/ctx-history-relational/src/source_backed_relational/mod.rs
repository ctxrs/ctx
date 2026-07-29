//! Independent relational compatibility projection for source-backed Core.
//!
//! This database is a disposable consumer of a committed Core generation. It
//! stores stable identities, relational metadata, and native locator/evidence
//! envelopes; it stores no event payload, provider body, search text, or
//! preview and never participates in Core lexical publication.
//!
//! Integration sequence:
//!
//! 1. Commit and reopen the source-backed lexical generation.
//! 2. Serialize the verified generation manifest and pair it with the exact
//!    Core commit receipt in [`CommittedCoreGeneration`].
//! 3. Stream source-grouped [`RelationalProjectionRecord`] values into
//!    [`SourceBackedRelationalProjection::catch_up`]. Use
//!    [`SourceBackedRelationalProjection::rebuild`] for first install, repair,
//!    or a consumer-contract change.
//! 4. Treat the returned frontier as SQL-owned state. A projection error leaves
//!    the prior SQL generation queryable and marks only this consumer behind.
//!
//! For the schema-v5 lexical seam, one source-backed event supplies event and
//! session identities, parent/root lineage, provider-session ID, branch,
//! source path, agent scope, workspace/cwd, event ordering/type/role, touched
//! paths, and locator evidence. The integration host emits one deduplicated
//! session record before its events and supplies deterministic file-relation
//! IDs plus any richer old-path/change metadata retained by the provider
//! projector. Rebuild obtains the same records by rereading certified provider
//! sources; it does not enumerate or hydrate bodies from SQLite.
//!
//! A normal catch-up stream contains only sources whose certificates changed.
//! A rebuild stream contains every source in the manifest. Confirmed deletion
//! is represented by omission from the new certified manifest, so no provider
//! body archive or relational tombstone row is required.

mod manifest;
mod materialization;
mod model;
mod publication;
mod raw_sql;
mod read;
mod schema;

pub use model::*;
pub use raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};

use std::path::PathBuf;

use rusqlite::Connection;

pub(super) const GENERATION_MANIFEST_VERSION: u32 = manifest::GENERATION_MANIFEST_VERSION;
pub(super) const REQUIRED_LEXICAL_SCHEMA_VERSION: u32 = manifest::REQUIRED_LEXICAL_SCHEMA_VERSION;
pub(super) const REQUIRED_SOURCE_GENERATION_POLICY_HASH: &str =
    manifest::REQUIRED_SOURCE_GENERATION_POLICY_HASH;

#[cfg(test)]
use ctx_history_core::{
    CertifiedSource, EventRole, FileChangeKind, SourceKey, StableEntityId, IDENTITY_VERSION,
};
#[cfg(test)]
use manifest::{GenerationManifest, GenerationRemoval, REQUIRED_LEXICAL_ANALYZER_VERSION};
#[cfg(test)]
use sha2::{Digest, Sha256};

pub struct SourceBackedRelationalProjection {
    path: PathBuf,
    conn: Connection,
    read_only: bool,
}

fn sqlite_i64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn sqlite_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn sqlite_u32(value: i64, field: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests;
