use std::{
    collections::BTreeSet,
    fs,
    ops::Deref,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use tempfile::TempDir;
use url::Url;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::compute_payload_hash;

use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_schema where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

pub(crate) fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({})", sqlite_ident(table)))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn optional_column_expr<'a>(
    columns: &BTreeSet<String>,
    column: &'a str,
    fallback: &'a str,
) -> &'a str {
    if columns.contains(column) {
        column
    } else {
        fallback
    }
}

pub(crate) fn ensure_sqlite_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    let missing = required
        .iter()
        .copied()
        .filter(|column| !columns.contains(*column))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CaptureError::InvalidPayload(format!(
            "{label} missing required column(s): {}",
            missing.join(", ")
        )))
    }
}

pub(crate) fn sqlite_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn sqlite_is_too_big(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(ref fail, _)
            if fail.code == rusqlite::ErrorCode::TooBig
    )
}

pub(crate) struct ReadOnlySqliteConnection {
    conn: Connection,
    _snapshot_dir: Option<TempDir>,
}

impl Deref for ReadOnlySqliteConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

pub(crate) fn open_sqlite_readonly_source(path: &Path) -> Result<ReadOnlySqliteConnection> {
    ensure_regular_provider_transcript_file(path)?;
    let sidecars = sqlite_existing_regular_sidecar_paths(path)?;
    if sidecars.is_empty() {
        let uri = sqlite_immutable_uri(path)?;
        let conn = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        return Ok(ReadOnlySqliteConnection {
            conn,
            _snapshot_dir: None,
        });
    }

    // Read-only SQLite connections can still update live WAL shared-memory files.
    // Copy the DB plus sidecars first so imports see committed WAL content without
    // mutating provider-owned history.
    let snapshot_dir = tempfile::Builder::new()
        .prefix("ctx-provider-sqlite-")
        .tempdir()?;
    let snapshot_path = snapshot_dir.path().join(path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite path has no file name",
        }
    })?);
    fs::copy(path, &snapshot_path)?;
    for sidecar in sidecars {
        let sidecar_name =
            sidecar
                .file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: sidecar.clone(),
                    reason: "provider SQLite sidecar path has no file name",
                })?;
        fs::copy(&sidecar, snapshot_dir.path().join(sidecar_name))?;
    }
    let conn = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(ReadOnlySqliteConnection {
        conn,
        _snapshot_dir: Some(snapshot_dir),
    })
}

fn sqlite_existing_regular_sidecar_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let mut sidecars = Vec::new();
    for sidecar in sqlite_sidecar_paths(path) {
        match sidecar.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => sidecars.push(sidecar),
            Ok(_) => {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: sidecar,
                    reason: "provider SQLite sidecar is not a regular file",
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CaptureError::Io(error)),
        }
    }
    Ok(sidecars)
}

fn sqlite_sidecar_paths(path: &Path) -> Vec<PathBuf> {
    ["-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            PathBuf::from(sidecar)
        })
        .collect()
}

fn sqlite_immutable_uri(path: &Path) -> Result<String> {
    let absolute_path =
        path.canonicalize()
            .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "failed to resolve provider SQLite path",
            })?;
    let mut url = Url::from_file_path(&absolute_path).map_err(|()| {
        CaptureError::InvalidProviderTranscriptPath {
            path: absolute_path,
            reason: "provider SQLite path cannot be represented as a file URI",
        }
    })?;
    url.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    Ok(url.to_string())
}

pub(crate) fn sqlite_row_ids_with_oversized_value(
    path: &Path,
    table: &str,
    id_column: &str,
    value_column: &str,
) -> Result<BTreeSet<String>> {
    let conn = open_sqlite_readonly_source(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    // This prescan intentionally omits SQLITE_LIMIT_LENGTH: bounded connections
    // can raise SQLITE_TOOBIG before returning ids, and this query returns ids only.
    let mut stmt = conn.prepare(&format!(
        "select {} from {} where length(cast({} as blob)) > ?",
        sqlite_ident(id_column),
        sqlite_ident(table),
        sqlite_ident(value_column),
    ))?;
    let rows = stmt.query_map([MAX_PROVIDER_SQLITE_VALUE_BYTES as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn opencode_schema_fingerprint(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "select name, sql from sqlite_schema where type in ('table','index') order by name",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let sql: Option<String> = row.get(1)?;
        Ok(format!("{name}:{}", sql.unwrap_or_default()))
    })?;
    let schema = rows.collect::<std::result::Result<Vec<_>, _>>()?.join("\n");
    compute_payload_hash(&json!({ "schema": schema }))
}
