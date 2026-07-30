//! Frozen SQLite observation, schema admission, and bounded keyset reads.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    SqliteLengthPreflightGuard,
};
use crate::{CaptureError, ProviderAdapterContext, Result};

const DEEPAGENTS_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 32 * 16;

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
    pub(super) value_type: Option<String>,
    pub(super) value: Option<Vec<u8>>,
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

pub(super) struct DeepAgentsCheckpointContexts {
    pub(super) threads: BTreeMap<String, DeepAgentsThreadSummary>,
    pub(super) checkpoint_times: BTreeMap<(String, String), DateTime<Utc>>,
}

pub(super) fn deepagents_write_candidate_page(
    conn: &Connection,
    after_rowid: Option<i64>,
    page_rows: i64,
) -> Result<Vec<DeepAgentsWriteCandidate>> {
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
    let limit = i64::try_from(deepagents_oversize_limit()?)
        .map_err(|_| CaptureError::SystemInvariant("Deep Agents byte limit exceeds i64"))?;
    let admissible = "typeof(candidate.thread_id) = 'text' \
        and typeof(candidate.checkpoint_id) = 'text' \
        and typeof(candidate.task_id) = 'text' \
        and typeof(candidate.idx) = 'integer' \
        and typeof(candidate.type) in ('null', 'text') \
        and typeof(candidate.value) = 'blob' \
        and coalesce(octet_length(candidate.thread_id), 0) \
          + coalesce(octet_length(candidate.checkpoint_id), 0) \
          + coalesce(octet_length(candidate.task_id), 0) \
          + coalesce(octet_length(candidate.type), 0) \
          + coalesce(length(candidate.value), 0) <= ?3";
    let sql = format!(
        "select candidate.rowid, \
                coalesce(octet_length(candidate.thread_id), 0), \
                coalesce(octet_length(candidate.checkpoint_id), 0), \
                coalesce(octet_length(candidate.task_id), 0), \
                coalesce(octet_length(candidate.type), 0), \
                coalesce(length(candidate.value), 0), \
                typeof(candidate.thread_id), typeof(candidate.checkpoint_id), \
                typeof(candidate.task_id), typeof(candidate.idx), typeof(candidate.type), \
                typeof(candidate.value), \
                case when {admissible} then candidate.thread_id end, \
                case when {admissible} then candidate.checkpoint_id end, \
                case when {admissible} then candidate.task_id end, \
                case when {admissible} then candidate.idx end, \
                case when {admissible} then candidate.type end, \
                case when {admissible} then candidate.value end \
         from writes as candidate \
         where candidate.checkpoint_ns = '' and candidate.channel = 'messages' \
           and (?1 = 0 or exists( \
               select 1 from writes as prior where prior.rowid = ?2 \
                 and (candidate.thread_id, candidate.checkpoint_id, \
                      candidate.task_id, candidate.idx) \
                     > (prior.thread_id, prior.checkpoint_id, prior.task_id, prior.idx))) \
         order by candidate.thread_id, candidate.checkpoint_id, candidate.task_id, candidate.idx \
         limit ?4"
    );
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(rusqlite::params![has_after, prior_rowid, limit, page_rows])?;
    let mut candidates = Vec::with_capacity(usize::try_from(page_rows).unwrap_or_default());
    while let Some(row) = rows.next()? {
        let preflight = deepagents_write_preflight_from_row(row)?;
        let (rowid, lengths, types) = preflight;
        let retained_bytes = lengths.into_iter().try_fold(0_i64, |total, value| {
            total
                .checked_add(value)
                .ok_or(CaptureError::SystemInvariant(
                    "Deep Agents SQLite retained byte count overflowed",
                ))
        })?;
        let key = match (
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<i64>>(15)?,
        ) {
            (Some(thread_id), Some(checkpoint_id), Some(task_id), Some(idx)) => {
                Some(DeepAgentsWriteKey {
                    thread_id,
                    checkpoint_id,
                    task_id,
                    idx,
                })
            }
            _ => None,
        };
        let valid_types = types[0] == "text"
            && types[1] == "text"
            && types[2] == "text"
            && types[3] == "integer"
            && matches!(types[4].as_str(), "null" | "text")
            && types[5] == "blob";
        candidates.push(DeepAgentsWriteCandidate {
            rowid,
            key,
            retained_bytes,
            rejection_reason: (!valid_types).then(|| {
                "Deep Agents write key or payload has an unsupported SQLite storage class"
                    .to_owned()
            }),
            value_type: row.get(16)?,
            value: row.get(17)?,
        });
    }
    Ok(candidates)
}

pub(super) fn deepagents_checkpoint_contexts(
    conn: &Connection,
    context: &ProviderAdapterContext,
    candidates: &[DeepAgentsWriteCandidate],
) -> Result<DeepAgentsCheckpointContexts> {
    let mut thread_ids = candidates
        .iter()
        .filter_map(|candidate| candidate.key.as_ref())
        .map(|key| key.thread_id.clone())
        .collect::<Vec<_>>();
    thread_ids.sort_unstable();
    thread_ids.dedup();
    let requested = candidates
        .iter()
        .filter_map(|candidate| candidate.key.as_ref())
        .map(|key| (key.thread_id.clone(), key.checkpoint_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut result = DeepAgentsCheckpointContexts {
        threads: BTreeMap::new(),
        checkpoint_times: BTreeMap::new(),
    };
    if thread_ids.is_empty() {
        return Ok(result);
    }
    let placeholders = std::iter::repeat_n("?", thread_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let limit = i64::try_from(deepagents_oversize_limit()?)
        .map_err(|_| CaptureError::SystemInvariant("Deep Agents byte limit exceeds i64"))?;
    let sql = format!(
        "select rowid, \
                case when typeof(thread_id) = 'text' and octet_length(thread_id) <= ?1 \
                     then thread_id end, \
                case when typeof(checkpoint_id) = 'text' and octet_length(checkpoint_id) <= ?1 \
                     then checkpoint_id end, \
                typeof(metadata), coalesce(length(metadata), 0), \
                case when typeof(metadata) in ('null', 'blob') \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(length(metadata), 0) <= ?1 \
                     then metadata end \
         from checkpoints where checkpoint_ns = '' and thread_id in ({placeholders}) \
         order by thread_id, checkpoint_id, rowid"
    );
    let mut parameters = Vec::with_capacity(thread_ids.len() + 1);
    parameters.push(rusqlite::types::Value::Integer(limit));
    parameters.extend(thread_ids.iter().cloned().map(rusqlite::types::Value::Text));
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
    let mut seen_pairs = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let Some(thread_id) = row.get::<_, Option<String>>(1)? else {
            continue;
        };
        let Some(checkpoint_id) = row.get::<_, Option<String>>(2)? else {
            continue;
        };
        let pair = (thread_id.clone(), checkpoint_id.clone());
        if !seen_pairs.insert(pair.clone()) {
            continue;
        }
        let metadata_type = row.get::<_, String>(3)?;
        let metadata_bytes = row.get::<_, i64>(4)?;
        let metadata_blob = row.get::<_, Option<Vec<u8>>>(5)?;
        let valid_metadata = matches!(metadata_type.as_str(), "null" | "blob")
            && metadata_bytes >= 0
            && metadata_blob
                .as_ref()
                .is_none_or(|blob| i64::try_from(blob.len()).ok() == Some(metadata_bytes));
        if requested.contains(&pair) {
            let occurred_at = if valid_metadata {
                let metadata = deepagents_metadata_json(metadata_blob.as_deref());
                deepagents_metadata_time(&metadata, "updated_at").unwrap_or(context.imported_at)
            } else {
                context.imported_at
            };
            result.checkpoint_times.insert(pair, occurred_at);
        }
        if !valid_metadata {
            continue;
        }
        let metadata = deepagents_metadata_json(metadata_blob.as_deref());
        let updated_at =
            deepagents_metadata_time(&metadata, "updated_at").unwrap_or(context.imported_at);
        let entry =
            result
                .threads
                .entry(thread_id.clone())
                .or_insert_with(|| DeepAgentsThreadSummary {
                    thread: DeepAgentsThread {
                        thread_id,
                        agent_name: deepagents_metadata_string(&metadata, "agent_name"),
                        created_at: updated_at,
                        updated_at,
                        latest_checkpoint_id: Some(checkpoint_id.clone()),
                        git_branch: deepagents_metadata_string(&metadata, "git_branch"),
                        cwd: deepagents_metadata_string(&metadata, "cwd"),
                    },
                });
        if updated_at < entry.thread.created_at {
            entry.thread.created_at = updated_at;
        }
        if updated_at >= entry.thread.updated_at {
            entry.thread.updated_at = updated_at;
            entry.thread.latest_checkpoint_id = Some(checkpoint_id);
            entry.thread.agent_name = deepagents_metadata_string(&metadata, "agent_name")
                .or_else(|| entry.thread.agent_name.clone());
            entry.thread.git_branch = deepagents_metadata_string(&metadata, "git_branch")
                .or_else(|| entry.thread.git_branch.clone());
            entry.thread.cwd =
                deepagents_metadata_string(&metadata, "cwd").or_else(|| entry.thread.cwd.clone());
        }
    }
    Ok(result)
}

pub(super) fn deepagents_oversize_limit() -> Result<u64> {
    let bounded = crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES
        .saturating_sub(256 * 1024);
    u64::try_from(bounded)
        .map_err(|_| CaptureError::SystemInvariant("Deep Agents byte limit exceeds u64"))
}

/// Hashes the exact logical rows that can affect Deep Agents projection without
/// decoding provider payloads. Oversized or unsupported rows are represented by
/// their storage classes and lengths, matching the scanner's rejection policy.
pub(super) fn deepagents_logical_fingerprint(
    conn: &Connection,
    schema_evidence: &[u8],
) -> Result<[u8; 32]> {
    let limit = i64::try_from(deepagents_oversize_limit()?)
        .map_err(|_| CaptureError::SystemInvariant("Deep Agents byte limit exceeds i64"))?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-deepagents-logical-snapshot-v1\0");
    digest.update((schema_evidence.len() as u64).to_be_bytes());
    digest.update(schema_evidence);
    hash_deepagents_writes(conn, limit, &mut digest)?;
    hash_deepagents_checkpoints(conn, limit, &mut digest)?;
    Ok(digest.finalize().into())
}

fn hash_deepagents_writes(conn: &Connection, limit: i64, digest: &mut Sha256) -> Result<()> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(
        "select typeof(thread_id), coalesce(octet_length(thread_id), 0), \
                typeof(checkpoint_id), coalesce(octet_length(checkpoint_id), 0), \
                typeof(task_id), coalesce(octet_length(task_id), 0), \
                typeof(idx), typeof(type), coalesce(octet_length(type), 0), \
                typeof(value), coalesce(length(value), 0), \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(task_id) = 'text' \
                           and typeof(idx) = 'integer' \
                           and typeof(type) in ('null', 'text') \
                           and typeof(value) = 'blob' \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(octet_length(task_id), 0) \
                             + coalesce(octet_length(type), 0) \
                             + coalesce(length(value), 0) <= ?1 \
                     then thread_id end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(task_id) = 'text' \
                           and typeof(idx) = 'integer' \
                           and typeof(type) in ('null', 'text') \
                           and typeof(value) = 'blob' \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(octet_length(task_id), 0) \
                             + coalesce(octet_length(type), 0) \
                             + coalesce(length(value), 0) <= ?1 \
                     then checkpoint_id end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(task_id) = 'text' \
                           and typeof(idx) = 'integer' \
                           and typeof(type) in ('null', 'text') \
                           and typeof(value) = 'blob' \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(octet_length(task_id), 0) \
                             + coalesce(octet_length(type), 0) \
                             + coalesce(length(value), 0) <= ?1 \
                     then task_id end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(task_id) = 'text' \
                           and typeof(idx) = 'integer' \
                           and typeof(type) in ('null', 'text') \
                           and typeof(value) = 'blob' \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(octet_length(task_id), 0) \
                             + coalesce(octet_length(type), 0) \
                             + coalesce(length(value), 0) <= ?1 \
                     then idx end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(task_id) = 'text' \
                           and typeof(idx) = 'integer' \
                           and typeof(type) in ('null', 'text') \
                           and typeof(value) = 'blob' \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(octet_length(task_id), 0) \
                             + coalesce(octet_length(type), 0) \
                             + coalesce(length(value), 0) <= ?1 \
                     then type end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(task_id) = 'text' \
                           and typeof(idx) = 'integer' \
                           and typeof(type) in ('null', 'text') \
                           and typeof(value) = 'blob' \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(octet_length(task_id), 0) \
                             + coalesce(octet_length(type), 0) \
                             + coalesce(length(value), 0) <= ?1 \
                     then value end \
         from writes where checkpoint_ns = '' and channel = 'messages' \
         order by thread_id, checkpoint_id, task_id, idx",
    )?;
    let mut rows = statement.query([limit])?;
    while let Some(row) = rows.next()? {
        digest.update(b"write\0");
        for column in [0_usize, 2, 4, 6, 7, 9] {
            hash_text(digest, &row.get::<_, String>(column)?);
        }
        for column in [1_usize, 3, 5, 8, 10] {
            digest.update(row.get::<_, i64>(column)?.to_be_bytes());
        }
        hash_optional_text(digest, row.get::<_, Option<String>>(11)?.as_deref());
        hash_optional_text(digest, row.get::<_, Option<String>>(12)?.as_deref());
        hash_optional_text(digest, row.get::<_, Option<String>>(13)?.as_deref());
        match row.get::<_, Option<i64>>(14)? {
            Some(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            None => digest.update([0]),
        }
        hash_optional_text(digest, row.get::<_, Option<String>>(15)?.as_deref());
        hash_optional_bytes(digest, row.get::<_, Option<Vec<u8>>>(16)?.as_deref());
    }
    Ok(())
}

fn hash_deepagents_checkpoints(conn: &Connection, limit: i64, digest: &mut Sha256) -> Result<()> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(
        "select typeof(thread_id), coalesce(octet_length(thread_id), 0), \
                typeof(checkpoint_id), coalesce(octet_length(checkpoint_id), 0), \
                typeof(metadata), coalesce(length(metadata), 0), \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(metadata) in ('null', 'blob') \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(length(metadata), 0) <= ?1 \
                     then thread_id end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(metadata) in ('null', 'blob') \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(length(metadata), 0) <= ?1 \
                     then checkpoint_id end, \
                case when typeof(thread_id) = 'text' \
                           and typeof(checkpoint_id) = 'text' \
                           and typeof(metadata) in ('null', 'blob') \
                           and coalesce(octet_length(thread_id), 0) \
                             + coalesce(octet_length(checkpoint_id), 0) \
                             + coalesce(length(metadata), 0) <= ?1 \
                     then metadata end \
         from checkpoints where checkpoint_ns = '' \
         order by thread_id, checkpoint_id",
    )?;
    let mut rows = statement.query([limit])?;
    while let Some(row) = rows.next()? {
        digest.update(b"checkpoint\0");
        for column in [0_usize, 2, 4] {
            hash_text(digest, &row.get::<_, String>(column)?);
        }
        for column in [1_usize, 3, 5] {
            digest.update(row.get::<_, i64>(column)?.to_be_bytes());
        }
        hash_optional_text(digest, row.get::<_, Option<String>>(6)?.as_deref());
        hash_optional_text(digest, row.get::<_, Option<String>>(7)?.as_deref());
        hash_optional_bytes(digest, row.get::<_, Option<Vec<u8>>>(8)?.as_deref());
    }
    Ok(())
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

fn hash_optional_bytes(digest: &mut Sha256, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        None => digest.update([0]),
    }
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
