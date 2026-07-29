//! Frozen SQLite observation, schema admission, and bounded keyset reads.

use std::{collections::BTreeSet, path::Path};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, ProviderAdapterContext, Result};

const DEEPAGENTS_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 32 * 16;

pub(super) fn deepagents_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Deep Agents SQLite source must be a regular non-symlink file",
        "Deep Agents SQLite sidecar must be a regular non-symlink file",
    )
}

#[derive(Clone, Debug)]
pub(super) struct DeepAgentsThread {
    pub(super) thread_id: String,
    pub(super) agent_name: Option<String>,
    pub(super) created_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) latest_checkpoint_id: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) cwd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) struct DeepAgentsWriteKey {
    pub(super) thread_id: String,
    pub(super) checkpoint_id: String,
    pub(super) task_id: String,
    pub(super) idx: i64,
}

#[derive(Clone, Debug)]
pub(super) struct DeepAgentsThreadSummary {
    pub(super) thread: DeepAgentsThread,
}

#[derive(Clone, Debug)]
pub(super) struct DeepAgentsWriteCandidate {
    pub(super) rowid: i64,
    pub(super) key: Option<DeepAgentsWriteKey>,
    pub(super) retained_bytes: i64,
    pub(super) rejection_reason: Option<String>,
}

pub(super) type DeepAgentsWritePreflight = (i64, [i64; 5], [String; 6]);

impl DeepAgentsWriteCandidate {
    pub(super) fn observed_bytes(&self) -> Result<u64> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Deep Agents SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        DEEPAGENTS_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Deep Agents SQLite retained byte count overflowed",
            ))
    }
}

pub(super) fn deepagents_oversize_limit() -> Result<u64> {
    let bounded = crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES
        .saturating_sub(256 * 1024);
    u64::try_from(bounded)
        .map_err(|_| CaptureError::SystemInvariant("Deep Agents byte limit exceeds u64"))
}

pub(super) struct DeepAgentsThreadCandidate {
    pub(super) rowid: i64,
    pub(super) thread_id: Option<String>,
    pub(super) rejection_reason: Option<String>,
}

pub(super) fn with_deepagents_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH can reject an index walk even when a query returns only rowids,
    // storage classes, and integer byte counts. Lift it only for those bounded preflights, then
    // restore the caller's limit before any raw key, metadata, or payload hydration can execute.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) fn deepagents_next_write_candidate(
    conn: &Connection,
    after_rowid: Option<i64>,
) -> Result<Option<DeepAgentsWriteCandidate>> {
    deepagents_next_write_candidate_scoped(conn, None, after_rowid)
}

pub(super) fn deepagents_next_write_candidate_scoped(
    conn: &Connection,
    thread_id: Option<&str>,
    after_rowid: Option<i64>,
) -> Result<Option<DeepAgentsWriteCandidate>> {
    if let Some(rowid) = after_rowid {
        let prior_exists = conn.query_row(
            "select exists(select 1 from writes \
             where rowid = ?1 and checkpoint_ns = '' and channel = 'messages')",
            [rowid],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !prior_exists {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    let has_after = i64::from(after_rowid.is_some());
    let prior_rowid = after_rowid.unwrap_or(0);
    // Keep prior keys inside direct column comparisons. Returning an oversized TEXT key from a
    // scalar subquery would materialize it and trip SQLITE_LIMIT_LENGTH before we can reject it.
    let preflight = with_deepagents_length_preflight(conn, || {
        conn.query_row(
            "select candidate.rowid, \
                    coalesce(octet_length(candidate.thread_id), 0), \
                    coalesce(octet_length(candidate.checkpoint_id), 0), \
                    coalesce(octet_length(candidate.task_id), 0), \
                    coalesce(octet_length(candidate.type), 0), \
                    coalesce(length(candidate.value), 0), \
                    typeof(candidate.thread_id), typeof(candidate.checkpoint_id), \
                    typeof(candidate.task_id), typeof(candidate.idx), typeof(candidate.type), \
                    typeof(candidate.value) \
             from writes as candidate \
             where candidate.checkpoint_ns = '' and candidate.channel = 'messages' \
               and (?3 is null or candidate.thread_id = ?3) \
               and (?1 = 0 or exists( \
                   select 1 from writes as prior where prior.rowid = ?2 \
                     and (candidate.thread_id, candidate.checkpoint_id, \
                          candidate.task_id, candidate.idx) \
                         > (prior.thread_id, prior.checkpoint_id, prior.task_id, prior.idx) \
               )) \
            order by candidate.thread_id, candidate.checkpoint_id, \
                      candidate.task_id, candidate.idx limit 1",
            rusqlite::params![has_after, prior_rowid, thread_id],
            deepagents_write_preflight_from_row,
        )
        .optional()
    })?;
    preflight
        .map(|preflight| deepagents_candidate_from_preflight(conn, preflight))
        .transpose()
}

pub(super) fn deepagents_write_candidate_at(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<DeepAgentsWriteCandidate>> {
    let preflight = with_deepagents_length_preflight(conn, || {
        conn.query_row(
            "select candidate.rowid, \
                    coalesce(octet_length(candidate.thread_id), 0), \
                    coalesce(octet_length(candidate.checkpoint_id), 0), \
                    coalesce(octet_length(candidate.task_id), 0), \
                    coalesce(octet_length(candidate.type), 0), \
                    coalesce(length(candidate.value), 0), \
                    typeof(candidate.thread_id), typeof(candidate.checkpoint_id), \
                    typeof(candidate.task_id), typeof(candidate.idx), typeof(candidate.type), \
                    typeof(candidate.value) \
             from writes as candidate \
             where candidate.rowid = ?1 \
               and candidate.checkpoint_ns = '' and candidate.channel = 'messages'",
            [rowid],
            deepagents_write_preflight_from_row,
        )
        .optional()
    })?;
    preflight
        .map(|preflight| deepagents_candidate_from_preflight(conn, preflight))
        .transpose()
}

fn deepagents_candidate_from_preflight(
    conn: &Connection,
    preflight: DeepAgentsWritePreflight,
) -> Result<DeepAgentsWriteCandidate> {
    let (rowid, lengths, types) = preflight;
    let retained_bytes = lengths.into_iter().try_fold(0_i64, |total, value| {
        if value < 0 {
            return Err(CaptureError::InvalidPayload(
                "Deep Agents SQLite value length must be nonnegative".to_owned(),
            ));
        }
        total
            .checked_add(value)
            .ok_or(CaptureError::SystemInvariant(
                "Deep Agents SQLite retained byte count overflowed",
            ))
    })?;
    let preflight_observed = u64::try_from(retained_bytes)
        .ok()
        .and_then(|bytes| DEEPAGENTS_SQLITE_VALUE_OVERHEAD_BYTES.checked_add(bytes))
        .ok_or(CaptureError::SystemInvariant(
            "Deep Agents SQLite retained byte count overflowed",
        ))?;
    let valid_types = types[0] == "text"
        && types[1] == "text"
        && types[2] == "text"
        && types[3] == "integer"
        && matches!(types[4].as_str(), "null" | "text")
        && types[5] == "blob";
    let rejection_reason = (!valid_types).then(|| {
        "Deep Agents write key or payload has an unsupported SQLite storage class".to_owned()
    });
    let key = if preflight_observed > deepagents_oversize_limit()? || rejection_reason.is_some() {
        None
    } else {
        Some(conn.query_row(
            "select thread_id, checkpoint_id, task_id, idx from writes \
             where rowid = ?1 and checkpoint_ns = '' and channel = 'messages'",
            [rowid],
            |row| {
                Ok(DeepAgentsWriteKey {
                    thread_id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    task_id: row.get(2)?,
                    idx: row.get(3)?,
                })
            },
        )?)
    };
    Ok(DeepAgentsWriteCandidate {
        rowid,
        key,
        retained_bytes,
        rejection_reason,
    })
}

pub(super) fn deepagents_write_preflight_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<DeepAgentsWritePreflight> {
    Ok((
        row.get::<_, i64>(0)?,
        [
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ],
        [
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
        ],
    ))
}

pub(super) fn deepagents_hydrate_write(
    conn: &Connection,
    rowid: i64,
) -> Result<(Option<String>, Vec<u8>)> {
    conn.query_row(
        "select type, value from writes \
         where rowid = ?1 and checkpoint_ns = '' and channel = 'messages'",
        [rowid],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(CaptureError::from)
}

pub(super) fn deepagents_next_thread_candidate(
    conn: &Connection,
    after_rowid: Option<i64>,
) -> Result<Option<DeepAgentsThreadCandidate>> {
    if let Some(rowid) = after_rowid {
        let prior_exists = conn.query_row(
            "select exists(select 1 from checkpoints \
             where rowid = ?1 and checkpoint_ns = '')",
            [rowid],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !prior_exists {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    let has_after = i64::from(after_rowid.is_some());
    let prior_rowid = after_rowid.unwrap_or(0);
    let preflight = with_deepagents_length_preflight(conn, || {
        conn.query_row(
            "select min(candidate.rowid), coalesce(octet_length(candidate.thread_id), 0), \
                    typeof(candidate.thread_id) \
             from checkpoints as candidate where candidate.checkpoint_ns = '' \
               and (?1 = 0 or exists( \
                   select 1 from checkpoints as prior where prior.rowid = ?2 \
                     and candidate.thread_id > prior.thread_id \
               )) \
             group by candidate.thread_id order by candidate.thread_id limit 1",
            rusqlite::params![has_after, prior_rowid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
    })?;
    let Some((rowid, thread_id_bytes, thread_id_type)) = preflight else {
        return Ok(None);
    };
    let thread_id_bytes = u64::try_from(thread_id_bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents thread identifier length must be nonnegative".to_owned(),
        )
    })?;
    let observed_bytes = DEEPAGENTS_SQLITE_VALUE_OVERHEAD_BYTES
        .checked_add(thread_id_bytes)
        .ok_or(CaptureError::SystemInvariant(
            "Deep Agents thread retained byte count overflowed",
        ))?;
    let rejection_reason = if thread_id_type != "text" {
        Some("Deep Agents thread identifier has an unsupported SQLite storage class".to_owned())
    } else if observed_bytes > deepagents_oversize_limit()? {
        Some(format!(
            "Deep Agents thread identifier exceeds the bounded record limit ({observed_bytes} bytes)"
        ))
    } else {
        None
    };
    let thread_id = if rejection_reason.is_some() {
        None
    } else {
        Some(conn.query_row(
            "select thread_id from checkpoints where rowid = ?1 and checkpoint_ns = ''",
            [rowid],
            |row| row.get(0),
        )?)
    };
    Ok(Some(DeepAgentsThreadCandidate {
        rowid,
        thread_id,
        rejection_reason,
    }))
}

pub(super) fn deepagents_thread_summary(
    conn: &Connection,
    context: &ProviderAdapterContext,
    thread_id: &str,
    _current_checkpoint_id: Option<&str>,
) -> Result<Option<DeepAgentsThreadSummary>> {
    let mut after_rowid = None;
    let mut thread = None::<DeepAgentsThread>;
    loop {
        let has_after = i64::from(after_rowid.is_some());
        let prior_rowid = after_rowid.unwrap_or(0);
        let candidate = with_deepagents_length_preflight(conn, || {
            conn.query_row(
                "select candidate.rowid, coalesce(octet_length(candidate.checkpoint_id), 0), \
                        coalesce(length(candidate.metadata), 0), \
                        typeof(candidate.checkpoint_id), typeof(candidate.metadata) \
                 from checkpoints as candidate \
                 where candidate.checkpoint_ns = '' and candidate.thread_id = ?1 \
                   and (?2 = 0 or exists( \
                       select 1 from checkpoints as prior where prior.rowid = ?3 \
                         and candidate.checkpoint_id > prior.checkpoint_id \
                   )) \
                 order by candidate.checkpoint_id limit 1",
                rusqlite::params![thread_id, has_after, prior_rowid],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
        })?;
        let Some((rowid, checkpoint_id_bytes, metadata_bytes, checkpoint_type, metadata_type)) =
            candidate
        else {
            break;
        };
        after_rowid = Some(rowid);
        let retained_bytes =
            [checkpoint_id_bytes, metadata_bytes]
                .into_iter()
                .try_fold(0_u64, |total, value| {
                    let value = u64::try_from(value).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Deep Agents checkpoint length must be nonnegative".to_owned(),
                        )
                    })?;
                    total
                        .checked_add(value)
                        .ok_or(CaptureError::SystemInvariant(
                            "Deep Agents checkpoint retained byte count overflowed",
                        ))
                })?;
        if retained_bytes > deepagents_oversize_limit()?
            || checkpoint_type != "text"
            || !matches!(metadata_type.as_str(), "null" | "blob")
        {
            continue;
        }
        let (checkpoint_id, metadata_blob) = conn.query_row(
            "select checkpoint_id, metadata from checkpoints where rowid = ?1",
            [rowid],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
        )?;
        let metadata = deepagents_metadata_json(metadata_blob.as_deref());
        let updated_at =
            deepagents_metadata_time(&metadata, "updated_at").unwrap_or(context.imported_at);
        let entry = thread.get_or_insert_with(|| DeepAgentsThread {
            thread_id: thread_id.to_owned(),
            agent_name: deepagents_metadata_string(&metadata, "agent_name"),
            created_at: updated_at,
            updated_at,
            latest_checkpoint_id: Some(checkpoint_id.clone()),
            git_branch: deepagents_metadata_string(&metadata, "git_branch"),
            cwd: deepagents_metadata_string(&metadata, "cwd"),
        });
        if updated_at < entry.created_at {
            entry.created_at = updated_at;
        }
        if updated_at >= entry.updated_at {
            entry.updated_at = updated_at;
            entry.latest_checkpoint_id = Some(checkpoint_id.clone());
            entry.agent_name = deepagents_metadata_string(&metadata, "agent_name")
                .or_else(|| entry.agent_name.clone());
            entry.git_branch = deepagents_metadata_string(&metadata, "git_branch")
                .or_else(|| entry.git_branch.clone());
            entry.cwd = deepagents_metadata_string(&metadata, "cwd").or_else(|| entry.cwd.clone());
        }
    }
    let Some(thread) = thread else {
        return Ok(None);
    };
    Ok(Some(DeepAgentsThreadSummary { thread }))
}

pub(super) fn deepagents_checkpoint_time(
    conn: &Connection,
    context: &ProviderAdapterContext,
    thread_id: &str,
    checkpoint_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let metadata_preflight = with_deepagents_length_preflight(conn, || {
        conn.query_row(
            "select coalesce(length(metadata), 0), typeof(metadata) from checkpoints \
             where checkpoint_ns = '' and thread_id = ?1 and checkpoint_id = ?2",
            rusqlite::params![thread_id, checkpoint_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
    })?;
    let Some((metadata_bytes, metadata_type)) = metadata_preflight else {
        return Ok(None);
    };
    let metadata_bytes = u64::try_from(metadata_bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents checkpoint metadata length must be nonnegative".to_owned(),
        )
    })?;
    if metadata_bytes > deepagents_oversize_limit()?
        || !matches!(metadata_type.as_str(), "null" | "blob")
    {
        return Ok(Some(context.imported_at));
    }
    let metadata_blob = conn.query_row(
        "select metadata from checkpoints \
         where checkpoint_ns = '' and thread_id = ?1 and checkpoint_id = ?2",
        rusqlite::params![thread_id, checkpoint_id],
        |row| row.get::<_, Option<Vec<u8>>>(0),
    )?;
    let metadata = deepagents_metadata_json(metadata_blob.as_deref());
    Ok(Some(
        deepagents_metadata_time(&metadata, "updated_at").unwrap_or(context.imported_at),
    ))
}

pub(super) fn deepagents_validate_schema(conn: &Connection, path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "checkpoints")? {
        return Err(CaptureError::UnsupportedSchema(format!(
            "Deep Agents sessions.db at {} is missing required checkpoints table",
            path.display()
        )));
    }
    if !sqlite_table_exists(conn, "writes")? {
        return Err(CaptureError::UnsupportedSchema(format!(
            "Deep Agents sessions.db at {} is missing required writes table",
            path.display()
        )));
    }
    deepagents_require_columns(
        &sqlite_table_columns(conn, "checkpoints")?,
        "Deep Agents checkpoints table",
        &[
            "thread_id",
            "checkpoint_ns",
            "checkpoint_id",
            "checkpoint",
            "metadata",
        ],
    )?;
    deepagents_require_columns(
        &sqlite_table_columns(conn, "writes")?,
        "Deep Agents writes table",
        &[
            "thread_id",
            "checkpoint_ns",
            "checkpoint_id",
            "task_id",
            "idx",
            "channel",
            "type",
            "value",
        ],
    )?;
    Ok(())
}

fn deepagents_require_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    ensure_sqlite_table_columns(columns, label, required).map_err(|error| match error {
        CaptureError::InvalidPayload(reason) => CaptureError::UnsupportedSchema(reason),
        error => error,
    })
}

pub(super) fn deepagents_metadata_json(blob: Option<&[u8]>) -> Value {
    blob.and_then(|blob| serde_json::from_slice::<Value>(blob).ok())
        .unwrap_or_else(|| json!({}))
}

pub(super) fn deepagents_metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn deepagents_metadata_time(metadata: &Value, key: &str) -> Option<DateTime<Utc>> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}
