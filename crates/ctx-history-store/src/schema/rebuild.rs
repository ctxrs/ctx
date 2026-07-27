use std::{collections::BTreeSet, fs, path::Path};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::result_storage::compact_stored_result_event_payload;
use crate::schema::ddl::{table_exists, CREATE_TABLES_SQL};
use crate::{Result, StoreError};

// This transient table is committed with the logical migration. Physical file
// cleanup consumes it afterward, so a crash cannot strand an untracked result
// blob or delete a file before its database references are committed away.
const RESULT_BLOB_CLEANUP_TABLE: &str = "ctx_source_backed_result_blob_cleanup";

pub(crate) fn rebuild_v44_current_schema_tables(conn: &Connection) -> Result<()> {
    for table in [
        "capture_sources",
        "vcs_workspaces",
        "history_records",
        "artifacts",
        "sessions",
        "session_edges",
        "runs",
        "events",
        "vcs_changes",
        "history_record_links",
        "summaries",
        "files_touched",
        "record_edges",
        "sync_outbox",
    ] {
        rebuild_table_from_current_schema(conn, table)?;
    }
    sanitize_v44_result_event_payloads(conn)?;
    Ok(())
}

pub(super) fn sanitize_v44_result_event_payloads(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {RESULT_BLOB_CLEANUP_TABLE} (
           blob_hash TEXT PRIMARY KEY NOT NULL
         ) STRICT;"
    ))?;
    let result_artifacts = {
        let mut statement = conn.prepare(
            "SELECT DISTINCT artifacts.id, artifacts.blob_hash
             FROM artifacts
             JOIN events ON events.payload_blob_id = artifacts.id
             WHERE events.event_type IN ('tool_output', 'command_output')
             ORDER BY artifacts.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    conn.execute(
        "UPDATE events SET payload_blob_id = NULL
         WHERE event_type IN ('tool_output', 'command_output')
           AND payload_blob_id IS NOT NULL",
        [],
    )?;
    for (artifact_id, blob_hash) in result_artifacts {
        if artifact_is_referenced(conn, &artifact_id)? {
            continue;
        }
        if conn.execute("DELETE FROM artifacts WHERE id = ?1", [&artifact_id])? == 0 {
            continue;
        }
        let same_blob_remains = conn
            .query_row(
                "SELECT 1 FROM artifacts WHERE blob_hash = ?1 LIMIT 1",
                [&blob_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !same_blob_remains {
            conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO {RESULT_BLOB_CLEANUP_TABLE} (blob_hash) VALUES (?1)"
                ),
                [&blob_hash],
            )?;
        }
    }

    const BATCH_SIZE: i64 = 256;
    let mut last_rowid = 0_i64;
    loop {
        let rows = {
            let mut stmt = conn.prepare(
                "SELECT rowid, payload_json FROM events
                 WHERE rowid > ?1 AND event_type IN ('tool_output', 'command_output')
                 ORDER BY rowid LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![last_rowid, BATCH_SIZE], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut batch = Vec::new();
            for row in rows {
                batch.push(row?);
            }
            batch
        };
        if rows.is_empty() {
            break;
        }
        for (rowid, payload_json) in rows {
            let payload = serde_json::from_str::<Value>(&payload_json)?;
            let compact = compact_stored_result_event_payload(&payload);
            conn.execute(
                "UPDATE events SET payload_json = ?1 WHERE rowid = ?2",
                params![serde_json::to_string(&compact)?, rowid],
            )?;
            last_rowid = rowid;
        }
    }
    Ok(())
}

fn artifact_is_referenced(conn: &Connection, artifact_id: &str) -> Result<bool> {
    let referenced = conn.query_row(
        "SELECT
           EXISTS(SELECT 1 FROM sessions WHERE transcript_blob_id = ?1) OR
           EXISTS(SELECT 1 FROM runs WHERE input_blob_id = ?1 OR output_blob_id = ?1) OR
           EXISTS(SELECT 1 FROM events WHERE payload_blob_id = ?1) OR
           EXISTS(SELECT 1 FROM history_record_links
                  WHERE target_type = 'artifact' AND target_id = ?1) OR
           EXISTS(SELECT 1 FROM summaries WHERE instr(citations_json, ?1) > 0)",
        [artifact_id],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(referenced)
}

pub(super) fn finish_result_blob_cleanup(conn: &Connection, object_dir: &Path) -> Result<()> {
    if !table_exists(conn, RESULT_BLOB_CLEANUP_TABLE)? {
        return Ok(());
    }

    // Hold the writer lock across the final reference check and file removal.
    // A failed commit leaves the cleanup row retryable; a missing file is an
    // idempotent success on the next Store open.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let cleanup = (|| -> Result<()> {
        let blob_hashes = {
            let mut statement = conn.prepare(&format!(
                "SELECT blob_hash FROM {RESULT_BLOB_CLEANUP_TABLE} ORDER BY blob_hash"
            ))?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for blob_hash in blob_hashes {
            let still_referenced = conn
                .query_row(
                    "SELECT 1 FROM artifacts WHERE blob_hash = ?1 LIMIT 1",
                    [&blob_hash],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !still_referenced {
                remove_orphaned_blob(object_dir, &blob_hash)?;
            }
            conn.execute(
                &format!("DELETE FROM {RESULT_BLOB_CLEANUP_TABLE} WHERE blob_hash = ?1"),
                [&blob_hash],
            )?;
        }
        conn.execute_batch(&format!("DROP TABLE {RESULT_BLOB_CLEANUP_TABLE}"))?;
        Ok(())
    })();
    match cleanup {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            if let Err(rollback_error) = conn.execute_batch("ROLLBACK") {
                return Err(StoreError::Sql(rollback_error));
            }
            Err(error)
        }
    }
}

fn remove_orphaned_blob(object_dir: &Path, blob_hash: &str) -> Result<()> {
    if blob_hash.len() != 64
        || !blob_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StoreError::UnsafeBlobPath(blob_hash.to_owned()));
    }
    let shard = object_dir.join(&blob_hash[..2]);
    match fs::symlink_metadata(&shard) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(StoreError::UnsafeBlobPath(shard.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let path = shard.join(blob_hash);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path)?;
        }
        Ok(_) => return Err(StoreError::UnsafeBlobPath(path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn rebuild_table_from_current_schema(conn: &Connection, table: &str) -> Result<()> {
    if !table_exists(conn, table)? {
        return Ok(());
    }
    let new_table = format!("{table}_new");
    conn.execute(&format!("DROP TABLE IF EXISTS {new_table}"), [])?;
    conn.execute_batch(&create_table_rebuild_sql(table, &new_table)?)?;

    let old_columns = table_columns(conn, table)?;
    let old_column_set = old_columns.iter().cloned().collect::<BTreeSet<_>>();
    let new_columns = table_columns(conn, &new_table)?
        .into_iter()
        .filter(|column| old_column_set.contains(column))
        .collect::<Vec<_>>();
    if !new_columns.is_empty() {
        let column_list = new_columns.join(", ");
        let select_list = column_list.clone();
        conn.execute(
            &format!("INSERT INTO {new_table} ({column_list}) SELECT {select_list} FROM {table}"),
            [],
        )?;
    }
    conn.execute(&format!("DROP TABLE {table}"), [])?;
    conn.execute(&format!("ALTER TABLE {new_table} RENAME TO {table}"), [])?;
    Ok(())
}

fn create_table_rebuild_sql(table: &str, new_table: &str) -> Result<String> {
    let marker = format!("CREATE TABLE IF NOT EXISTS {table}");
    let start = CREATE_TABLES_SQL
        .find(&marker)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let rest = &CREATE_TABLES_SQL[start..];
    let end = rest.find("\n);").ok_or(rusqlite::Error::InvalidQuery)? + 3;
    Ok(rest[..end].replacen(&marker, &format!("CREATE TABLE {new_table}"), 1))
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}
