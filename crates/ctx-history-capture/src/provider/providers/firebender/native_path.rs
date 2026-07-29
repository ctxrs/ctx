//! Firebender source-backed SQLite capture.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{ProviderSourceDirectory, ProviderSourceRoot},
    native_source::NativeSqliteValue,
    provider::{
        provider_path_identity,
        native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES,
        sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    },
    CaptureError, Result,
};

use super::firebender_chat_history_db_path;

mod scan;
mod source_backed;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(crate) use source_backed::{
    hydrate_firebender_source_backed_row, prepare_firebender_source_backed,
    FirebenderHydratedSourceRow, FirebenderSourceBackedError, FirebenderSourceBackedPage,
    FirebenderSourceBackedPlan, FirebenderSourceBackedResult, FirebenderSourceBackedScanner,
};

const FIREBENDER_NATIVE_FRONTIER_VERSION: u32 = 1;
const FIREBENDER_NATIVE_PARSER_REVISION: u32 = 1;
const FIREBENDER_NATIVE_POLICY_REVISION: u32 = 1;
const FIREBENDER_SOURCE_BACKED_PAGE_MAX_MESSAGES: usize = 60;
const FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;
const FIREBENDER_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;
const FIREBENDER_INITIAL_PREFIX_DOMAIN: &[u8] = b"ctx-firebender-native-prefix-v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirebenderFrontier {
    version: u32,
    row_ordinal: u64,
    updated_at: i64,
    rowid: i64,
    next_message_index: u64,
    prefix_sha256: [u8; 32],
    terminal: bool,
}

impl FirebenderFrontier {
    fn initial() -> Self {
        Self {
            version: FIREBENDER_NATIVE_FRONTIER_VERSION,
            row_ordinal: 0,
            updated_at: 0,
            rowid: 0,
            next_message_index: 0,
            prefix_sha256: Sha256::digest(FIREBENDER_INITIAL_PREFIX_DOMAIN).into(),
            terminal: false,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != FIREBENDER_NATIVE_FRONTIER_VERSION
            || (self.terminal && self.next_message_index != 0)
        {
            return Err(CaptureError::InvalidPayload(
                "Firebender source-backed frontier is malformed".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct FirebenderSqliteDatabase {
    parent: ProviderSourceDirectory,
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    evidence: SqliteSourceEvidence,
}

impl FirebenderSqliteDatabase {
    fn open<T>(path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<(Self, T)> {
        let parent_path =
            path.parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Firebender SQLite source must have a parent directory",
                })?;
        let database_name = path
            .file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Firebender SQLite source must have a database leaf name",
            })?
            .to_os_string();
        let parent = ProviderSourceRoot::open(parent_path)?.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority = retain_sqlite_source_directory_authority(&authority_handle, parent_path)
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, &database_name)
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| firebender_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let database = Self {
            parent,
            authority,
            database_name,
            evidence,
        };
        database.revalidate()?;
        Ok((database, result?))
    }

    fn read<T>(&self, path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.revalidate()?;
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(|error| firebender_sqlite_source_error(path, error))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| firebender_sqlite_source_error(path, error))
                .and_then(query)
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        };
        let finished = snapshot
            .finish()
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        self.revalidate()?;
        if finished != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        result
    }

    fn revalidate(&self) -> Result<()> {
        self.parent.revalidate()?;
        self.parent.authority_root().revalidate()
    }

    fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }
}

fn firebender_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: CaptureProvider::Firebender.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
    }
}

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

#[derive(Debug)]
struct FirebenderPage {
    next: FirebenderFrontier,
    row: Option<FirebenderRow>,
    message_start: usize,
    message_end: usize,
    rejection: Option<String>,
    retained_bytes: usize,
}

#[derive(Debug)]
struct FirebenderRowCandidate {
    rowid: i64,
    updated_at: i64,
    id_bytes: i64,
    name_bytes: i64,
    messages_bytes: i64,
    metadata_bytes: i64,
}

impl FirebenderRowCandidate {
    fn retained_bytes(&self) -> Result<usize> {
        [
            self.id_bytes,
            self.name_bytes,
            self.messages_bytes,
            self.metadata_bytes,
        ]
        .into_iter()
        .try_fold(FIREBENDER_PAGE_OVERHEAD_BYTES, |total, value| {
            let value = usize::try_from(value).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Firebender SQLite text length must be nonnegative".to_owned(),
                )
            })?;
            total
                .checked_add(value)
                .ok_or(CaptureError::SystemInvariant(
                    "Firebender source-backed retained byte count overflowed",
                ))
        })
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

fn firebender_source_revision(evidence: &SqliteSourceEvidence, schema_fingerprint: &str) -> String {
    format!(
        "firebender-native-sqlite-v2:parser={FIREBENDER_NATIVE_PARSER_REVISION};policy={FIREBENDER_NATIVE_POLICY_REVISION};schema={schema_fingerprint};identity={};length={};revision={}",
        hex(evidence.identity()),
        evidence.length(),
        hex(evidence.revision()),
    )
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

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
