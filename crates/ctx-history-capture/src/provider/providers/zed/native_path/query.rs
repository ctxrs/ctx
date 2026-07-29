use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{
    common::time::parse_rfc3339_utc,
    complete_content::sqlite::sqlite_logical_record_digest,
    native_source::NativeSqliteValue,
    provider::sqlite::{
        ensure_sqlite_table_columns, optional_column_expr, sqlite_table_columns,
        sqlite_table_exists, SqliteLengthPreflightGuard,
    },
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    decode::{decode_zed_native_payload, ZedDecodeOutcome},
    dto::{
        ZedNativeCounters, ZedNativeRejection, ZedNativeRejectionKind, ZedNativeSession,
        ZedNativeSink,
    },
    model::{hex_digest, ZedNativePageBuilder},
    ZedNativePathError, ZedNativeResult,
};
use crate::provider::providers::zed::thread::ZedThreadRow;

const ZED_CANDIDATE_PAGE_ROWS: i64 = 256;
pub(super) const ZED_THREAD_ID_MAX_BYTES: usize = 64 * 1024;
const ZED_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-zed-source-integrity-v1\0";
const ZED_RELATIONSHIP_MAX_DEPTH: usize = 1_024;

pub(crate) struct ZedNativeQueryResult {
    pub(crate) source_integrity_digest: String,
    pub(crate) counters: ZedNativeCounters,
}

pub(super) struct ZedThreadLineage {
    pub(super) parent_thread_id: Option<String>,
    pub(super) root_thread_id: String,
}

pub(super) struct ZedThreadLineageResolver<'connection> {
    connection: &'connection Connection,
    parent_statement: rusqlite::Statement<'connection>,
}

impl<'connection> ZedThreadLineageResolver<'connection> {
    pub(super) fn new(connection: &'connection Connection) -> ZedNativeResult<Self> {
        let schema = ZedNativeSchema::detect(connection)?;
        let parent_statement = connection.prepare(&format!(
            "select {} from threads where id = ?1 collate binary",
            schema.parent_id
        ))?;
        Ok(Self {
            connection,
            parent_statement,
        })
    }

    pub(super) fn resolve(&mut self, thread_id: &str) -> ZedNativeResult<Option<ZedThreadLineage>> {
        let mut visited = HashSet::new();
        let mut current = thread_id.to_owned();
        let mut root_thread_id = thread_id.to_owned();
        let mut parent_thread_id = None;
        let mut depth = 0_usize;

        loop {
            if !visited.insert(current.clone()) {
                return Err(ZedNativePathError::UnsupportedSchema(
                    "Zed thread relationships contain a cycle".to_owned(),
                ));
            }
            let parent = {
                let _guard = SqliteLengthPreflightGuard::new(self.connection);
                self.parent_statement
                    .query_row([current.as_str()], |row| row.get::<_, Option<String>>(0))
                    .optional()?
            };
            let Some(parent) = parent else {
                return if depth == 0 {
                    Ok(None)
                } else {
                    Ok(Some(ZedThreadLineage {
                        parent_thread_id,
                        root_thread_id,
                    }))
                };
            };
            if depth > ZED_RELATIONSHIP_MAX_DEPTH {
                return Err(ZedNativePathError::UnsupportedSchema(
                    "Zed thread relationships exceed the bounded depth".to_owned(),
                ));
            }
            if depth == 1 {
                parent_thread_id = Some(current.clone());
            }
            root_thread_id = current;
            let Some(next) = parent else {
                return Ok(Some(ZedThreadLineage {
                    parent_thread_id,
                    root_thread_id,
                }));
            };
            current = next;
            depth = depth.saturating_add(1);
        }
    }
}

struct ZedNativeSchema {
    parent_id: String,
    folder_paths: String,
    folder_paths_order: String,
    created_at: String,
}

impl ZedNativeSchema {
    fn detect(connection: &Connection) -> ZedNativeResult<Self> {
        if !sqlite_table_exists(connection, "threads")? {
            return Err(ZedNativePathError::UnsupportedSchema(
                "required `threads` table is missing".to_owned(),
            ));
        }
        let columns = sqlite_table_columns(connection, "threads")?;
        ensure_sqlite_table_columns(
            &columns,
            "Zed NativePath threads table",
            &["id", "summary", "updated_at", "data_type", "data"],
        )
        .map_err(|error| ZedNativePathError::UnsupportedSchema(error.to_string()))?;
        require_native_thread_identity(connection)?;
        Ok(Self {
            parent_id: optional_column_expr(&columns, "parent_id", "NULL").to_owned(),
            folder_paths: optional_column_expr(&columns, "folder_paths", "NULL").to_owned(),
            folder_paths_order: optional_column_expr(&columns, "folder_paths_order", "NULL")
                .to_owned(),
            created_at: optional_column_expr(&columns, "created_at", "NULL").to_owned(),
        })
    }

    fn storage_error_expression(&self) -> String {
        format!(
            "case \
             when typeof(id) != 'text' then 1 \
             when typeof(summary) != 'text' then 2 \
             when typeof(updated_at) != 'text' then 3 \
             when typeof(data_type) != 'text' then 4 \
             when typeof(data) != 'blob' then 5 \
             when typeof({}) not in ('null', 'text') then 6 \
             when typeof({}) not in ('null', 'text') then 7 \
             when typeof({}) not in ('null', 'text') then 8 \
             when typeof({}) not in ('null', 'text') then 9 \
             else 0 end",
            self.parent_id, self.folder_paths, self.folder_paths_order, self.created_at,
        )
    }

    fn retained_bytes_expression(&self) -> String {
        [
            "id",
            "summary",
            "updated_at",
            "data_type",
            "data",
            self.parent_id.as_str(),
            self.folder_paths.as_str(),
            self.folder_paths_order.as_str(),
            self.created_at.as_str(),
        ]
        .into_iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
    }

    fn invalid_identity_candidate_sql(&self, after_rowid: bool) -> String {
        let continuation = if after_rowid { "and rowid > ?1" } else { "" };
        format!(
            "select rowid, {}, {}, coalesce(octet_length(data), 0) \
             from threads \
             where (typeof(id) != 'text' \
                    or octet_length(id) > {ZED_THREAD_ID_MAX_BYTES}) \
               {continuation} \
             order by rowid limit {ZED_CANDIDATE_PAGE_ROWS}",
            self.storage_error_expression(),
            self.retained_bytes_expression(),
        )
    }

    fn candidate_sql(&self, after_id: bool) -> String {
        let continuation = if after_id {
            "and id collate binary > ?1 collate binary"
        } else {
            ""
        };
        format!(
            "select rowid, id, {}, {}, coalesce(octet_length(data), 0) \
             from threads \
             where typeof(id) = 'text' \
               and octet_length(id) <= {ZED_THREAD_ID_MAX_BYTES} \
               {continuation} \
             order by id collate binary asc limit {ZED_CANDIDATE_PAGE_ROWS}",
            self.storage_error_expression(),
            self.retained_bytes_expression(),
        )
    }

    fn hydration_sql(&self) -> String {
        format!(
            "select id, summary, updated_at, data_type, data, \
                    {}, {}, {}, {} \
             from threads where id = ?1 collate binary",
            self.parent_id, self.folder_paths, self.folder_paths_order, self.created_at,
        )
    }
}

pub(crate) fn scan_zed_native_snapshot(
    connection: &Connection,
    _physical_locator: &str,
    _snapshot_revision: &str,
    sink: &mut dyn ZedNativeSink,
) -> ZedNativeResult<ZedNativeQueryResult> {
    let schema = ZedNativeSchema::detect(connection)?;
    let mut first_invalid_identity_statement =
        connection.prepare(&schema.invalid_identity_candidate_sql(false))?;
    let mut next_invalid_identity_statement =
        connection.prepare(&schema.invalid_identity_candidate_sql(true))?;
    let mut first_candidate_statement = connection.prepare(&schema.candidate_sql(false))?;
    let mut next_candidate_statement = connection.prepare(&schema.candidate_sql(true))?;
    let mut hydration_statement = connection.prepare(&schema.hydration_sql())?;
    let mut builder = ZedNativePageBuilder::new(sink);
    let mut source_hasher = Sha256::new();
    source_hasher.update(ZED_SOURCE_DIGEST_DOMAIN);
    let mut counters = ZedNativeCounters::default();

    let mut last_invalid_rowid = None;
    loop {
        counters.candidate_page_queries = counters.candidate_page_queries.saturating_add(1);
        let candidates = {
            let _guard = SqliteLengthPreflightGuard::new(connection);
            let mut rows = match last_invalid_rowid {
                Some(rowid) => next_invalid_identity_statement.query([rowid])?,
                None => first_invalid_identity_statement.query([])?,
            };
            let mut candidates = Vec::with_capacity(ZED_CANDIDATE_PAGE_ROWS as usize);
            while let Some(row) = rows.next()? {
                candidates.push(ZedCandidate {
                    rowid: row.get(0)?,
                    id: None,
                    storage_error: row.get(1)?,
                    retained_bytes: row.get(2)?,
                    data_bytes: row.get(3)?,
                });
            }
            candidates
        };
        if candidates.is_empty() {
            break;
        }

        for candidate in candidates {
            last_invalid_rowid = Some(candidate.rowid);
            counters.native_thread_rows = counters.native_thread_rows.saturating_add(1);
            counters.certified_logical_bytes = counters
                .certified_logical_bytes
                .saturating_add(u64::try_from(candidate.retained_bytes).unwrap_or(u64::MAX));
            hash_candidate(&mut source_hasher, &candidate);
            counters.rejected_threads = counters.rejected_threads.saturating_add(1);
            let (kind, reason) = if candidate.storage_error != 0 {
                (
                    ZedNativeRejectionKind::InvalidStorageClass,
                    storage_class_reason(candidate.storage_error).to_owned(),
                )
            } else {
                (
                    ZedNativeRejectionKind::OversizedEncodedCell,
                    format!(
                        "Zed SQLite row {} has a thread id exceeding the {}-byte limit",
                        candidate.rowid, ZED_THREAD_ID_MAX_BYTES
                    ),
                )
            };
            builder.push_rejection(ZedNativeRejection {
                sqlite_rowid: candidate.rowid,
                thread_id: None,
                kind,
                reason,
            })?;
        }
    }

    let mut last_thread_id: Option<String> = None;
    let mut thread_ordinal = 0_u64;
    loop {
        counters.candidate_page_queries = counters.candidate_page_queries.saturating_add(1);
        let candidates = {
            let _guard = SqliteLengthPreflightGuard::new(connection);
            let mut rows = match last_thread_id.as_deref() {
                Some(id) => next_candidate_statement.query([id])?,
                None => first_candidate_statement.query([])?,
            };
            let mut candidates = Vec::with_capacity(ZED_CANDIDATE_PAGE_ROWS as usize);
            while let Some(row) = rows.next()? {
                candidates.push(ZedCandidate {
                    rowid: row.get(0)?,
                    id: Some(row.get(1)?),
                    storage_error: row.get(2)?,
                    retained_bytes: row.get(3)?,
                    data_bytes: row.get(4)?,
                });
            }
            candidates
        };
        if candidates.is_empty() {
            break;
        }

        for candidate in candidates {
            let id = candidate.id.as_deref().ok_or_else(|| {
                ZedNativePathError::UnsupportedSchema(
                    "bounded Zed thread candidate is missing its text identity".to_owned(),
                )
            })?;
            last_thread_id = Some(id.to_owned());
            counters.native_thread_rows = counters.native_thread_rows.saturating_add(1);
            counters.certified_logical_bytes = counters
                .certified_logical_bytes
                .saturating_add(u64::try_from(candidate.retained_bytes).unwrap_or(u64::MAX));
            hash_candidate(&mut source_hasher, &candidate);
            if candidate.storage_error != 0 {
                counters.rejected_threads = counters.rejected_threads.saturating_add(1);
                builder.push_rejection(ZedNativeRejection {
                    sqlite_rowid: candidate.rowid,
                    thread_id: Some(id.to_owned()),
                    kind: ZedNativeRejectionKind::InvalidStorageClass,
                    reason: storage_class_reason(candidate.storage_error).to_owned(),
                })?;
                thread_ordinal = thread_ordinal.saturating_add(1);
                continue;
            }
            if candidate.retained_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES as i64
                || candidate.data_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES as i64
            {
                counters.rejected_threads = counters.rejected_threads.saturating_add(1);
                builder.push_rejection(ZedNativeRejection {
                    sqlite_rowid: candidate.rowid,
                    thread_id: Some(id.to_owned()),
                    kind: ZedNativeRejectionKind::OversizedEncodedCell,
                    reason: format!(
                        "Zed thread {id:?} exceeds the {} encoded-byte limit",
                        MAX_PROVIDER_SQLITE_VALUE_BYTES
                    ),
                })?;
                thread_ordinal = thread_ordinal.saturating_add(1);
                continue;
            }

            counters.hydration_queries = counters.hydration_queries.saturating_add(1);
            let mut row = hydration_statement
                .query_row([id], hydrate_zed_row)
                .optional()?
                .ok_or_else(|| {
                    ZedNativePathError::UnsupportedSchema(format!(
                        "Zed thread {id:?} disappeared from immutable snapshot"
                    ))
                })?;
            row.rowid = candidate.rowid;
            counters.encoded_payload_bytes = counters
                .encoded_payload_bytes
                .saturating_add(u64::try_from(row.data.len()).unwrap_or(u64::MAX));
            hash_hydrated_row(&mut source_hasher, &row);
            let record_digest = zed_logical_record_digest(&row);
            let row_updated_at = match parse_zed_timestamp(&row.updated_at) {
                Some(value) => value,
                None => {
                    counters.rejected_threads = counters.rejected_threads.saturating_add(1);
                    builder.push_rejection(ZedNativeRejection {
                        sqlite_rowid: row.rowid,
                        thread_id: Some(row.id.clone()),
                        kind: ZedNativeRejectionKind::MalformedThread,
                        reason: format!(
                            "Zed thread `{}` updated_at is not RFC3339: {:?}",
                            row.id, row.updated_at
                        ),
                    })?;
                    thread_ordinal = thread_ordinal.saturating_add(1);
                    continue;
                }
            };
            let created_at = match row.created_at.as_deref() {
                Some(raw) => match parse_zed_timestamp(raw) {
                    Some(value) => value,
                    None => {
                        counters.rejected_threads = counters.rejected_threads.saturating_add(1);
                        builder.push_rejection(ZedNativeRejection {
                            sqlite_rowid: row.rowid,
                            thread_id: Some(row.id.clone()),
                            kind: ZedNativeRejectionKind::MalformedThread,
                            reason: format!(
                                "Zed thread `{}` created_at is not RFC3339: {raw:?}",
                                row.id
                            ),
                        })?;
                        thread_ordinal = thread_ordinal.saturating_add(1);
                        continue;
                    }
                },
                None => row_updated_at,
            };

            match decode_zed_native_payload(&row.id, &row.data_type, &row.data, row_updated_at)? {
                ZedDecodeOutcome::Rejected(failure) => {
                    counters.rejected_threads = counters.rejected_threads.saturating_add(1);
                    builder.push_rejection(ZedNativeRejection {
                        sqlite_rowid: row.rowid,
                        thread_id: Some(row.id.clone()),
                        kind: failure.kind,
                        reason: failure.reason,
                    })?;
                }
                ZedDecodeOutcome::Decoded(decoded) => {
                    counters.decompressed_payload_bytes = counters
                        .decompressed_payload_bytes
                        .saturating_add(decoded.decoded_bytes);
                    let folder_paths = folder_paths(row.folder_paths.as_deref());
                    let cwd =
                        ordered_folder_paths(&folder_paths, row.folder_paths_order.as_deref())
                            .first()
                            .cloned();
                    let updated_at = decoded.updated_at.unwrap_or(row_updated_at);
                    let title = decoded
                        .title
                        .as_deref()
                        .filter(|title| !title.trim().is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| row.summary.clone());
                    builder.push_session(ZedNativeSession {
                        thread_id: row.id.clone(),
                        parent_thread_id: row.parent_id.clone(),
                        root_thread_id: row.parent_id.clone().unwrap_or_else(|| row.id.clone()),
                        title,
                        summary: row.summary.clone(),
                        created_at,
                        updated_at,
                        cwd,
                        folder_paths,
                        encoding: decoded.encoding,
                    })?;
                    counters.sessions_retained = counters.sessions_retained.saturating_add(1);
                    decoded.emit_events(thread_ordinal, &mut |draft| {
                        let event = super::dto::ZedNativeEvent::from_draft(
                            row.rowid,
                            &row.id,
                            draft,
                            record_digest.clone(),
                        )?;
                        counters.retained_events = counters.retained_events.saturating_add(1);
                        counters.retained_body_bytes = counters.retained_body_bytes.saturating_add(
                            u64::try_from(event.lexical_body.len()).unwrap_or(u64::MAX),
                        );
                        counters.retained_file_touches =
                            counters.retained_file_touches.saturating_add(
                                u64::try_from(event.safe_file_touches.len()).unwrap_or(u64::MAX),
                            );
                        match event.event_type {
                            ctx_history_core::EventType::ToolCall => {
                                counters.retained_tool_calls =
                                    counters.retained_tool_calls.saturating_add(1);
                            }
                            ctx_history_core::EventType::Summary => {
                                counters.retained_summaries =
                                    counters.retained_summaries.saturating_add(1);
                            }
                            ctx_history_core::EventType::Notice => {
                                counters.retained_notices =
                                    counters.retained_notices.saturating_add(1);
                            }
                            _ => {
                                counters.retained_messages =
                                    counters.retained_messages.saturating_add(1);
                            }
                        }
                        builder.push_event(event)
                    })?;
                }
            }
            thread_ordinal = thread_ordinal.saturating_add(1);
        }
    }

    let source_integrity_digest = hex_digest(source_hasher.finalize().into());
    builder.finish()?;
    Ok(ZedNativeQueryResult {
        source_integrity_digest,
        counters,
    })
}

fn zed_logical_record_digest(
    row: &ZedHydratedRow,
) -> crate::complete_content::CompleteContentBodyDigest {
    sqlite_logical_record_digest(&[
        NativeSqliteValue::Integer(row.rowid),
        NativeSqliteValue::Text(row.id.clone()),
        optional_native_text(row.parent_id.clone()),
        optional_native_text(row.folder_paths.clone()),
        optional_native_text(row.folder_paths_order.clone()),
        NativeSqliteValue::Text(row.summary.clone()),
        NativeSqliteValue::Text(row.updated_at.clone()),
        NativeSqliteValue::Text(row.data_type.clone()),
        NativeSqliteValue::Blob(row.data.clone()),
        optional_native_text(row.created_at.clone()),
    ])
}

pub(super) fn hydrate_zed_thread_row(
    connection: &Connection,
    thread_id: &str,
) -> ZedNativeResult<
    Option<(
        ZedThreadRow,
        crate::complete_content::CompleteContentBodyDigest,
    )>,
> {
    let schema = ZedNativeSchema::detect(connection)?;
    let sql = format!(
        "select rowid, id, summary, updated_at, data_type, data, \
                {}, {}, {}, {} \
         from threads where id = ?1 collate binary",
        schema.parent_id, schema.folder_paths, schema.folder_paths_order, schema.created_at,
    );
    let hydrated = {
        let _guard = SqliteLengthPreflightGuard::new(connection);
        connection
            .query_row(&sql, [thread_id], |row| {
                Ok(ZedHydratedRow {
                    rowid: row.get(0)?,
                    id: row.get(1)?,
                    summary: row.get(2)?,
                    updated_at: row.get(3)?,
                    data_type: row.get(4)?,
                    data: row.get(5)?,
                    parent_id: row.get(6)?,
                    folder_paths: row.get(7)?,
                    folder_paths_order: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })
            .optional()?
    };
    Ok(hydrated.map(|row| {
        let digest = zed_logical_record_digest(&row);
        let thread = ZedThreadRow {
            rowid: row.rowid,
            id: row.id,
            updated_at: row.updated_at,
            data_type: row.data_type,
            data: row.data,
        };
        (thread, digest)
    }))
}

fn optional_native_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

struct ZedCandidate {
    rowid: i64,
    id: Option<String>,
    storage_error: i64,
    retained_bytes: i64,
    data_bytes: i64,
}

struct ZedHydratedRow {
    rowid: i64,
    id: String,
    summary: String,
    updated_at: String,
    data_type: String,
    data: Vec<u8>,
    parent_id: Option<String>,
    folder_paths: Option<String>,
    folder_paths_order: Option<String>,
    created_at: Option<String>,
}

fn hydrate_zed_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ZedHydratedRow> {
    Ok(ZedHydratedRow {
        rowid: 0, // Set from the integer-only candidate after hydration succeeds.
        id: row.get(0)?,
        summary: row.get(1)?,
        updated_at: row.get(2)?,
        data_type: row.get(3)?,
        data: row.get(4)?,
        parent_id: row.get(5)?,
        folder_paths: row.get(6)?,
        folder_paths_order: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn require_native_thread_identity(connection: &Connection) -> ZedNativeResult<()> {
    let mut indexes = connection.prepare(
        "select name
         from pragma_index_list('threads')
         where \"unique\" = 1 and partial = 0
         order by seq",
    )?;
    let names = indexes.query_map([], |row| row.get::<_, String>(0))?;
    for name in names {
        let name = name?;
        let mut columns = connection.prepare(
            "select name, \"desc\", coll
             from pragma_index_xinfo(?1)
             where key = 1
             order by seqno",
        )?;
        let key_columns = columns
            .query_map([&name], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if key_columns.len() == 1
            && key_columns[0].0.as_deref() == Some("id")
            && key_columns[0].1 == 0
            && key_columns[0]
                .2
                .as_deref()
                .is_some_and(|collation| collation.eq_ignore_ascii_case("binary"))
        {
            return Ok(());
        }
    }
    Err(ZedNativePathError::UnsupportedSchema(
        "threads requires a non-partial unique ascending BINARY single-column index on id"
            .to_owned(),
    ))
}

fn parse_zed_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    parse_rfc3339_utc(raw)
}

fn folder_paths(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

fn ordered_folder_paths(paths: &[String], order: Option<&str>) -> Vec<String> {
    let Some(order) = order else {
        return paths.to_vec();
    };
    let indices = order
        .split(',')
        .filter_map(|index| index.trim().parse::<usize>().ok())
        .collect::<Vec<_>>();
    if indices.len() != paths.len() {
        return paths.to_vec();
    }
    let mut ordered = paths
        .iter()
        .cloned()
        .zip(indices)
        .collect::<Vec<(String, usize)>>();
    ordered.sort_by_key(|(_, index)| *index);
    ordered.into_iter().map(|(path, _)| path).collect()
}

fn storage_class_reason(code: i64) -> &'static str {
    match code {
        1 => "Zed threads.id must use SQLite TEXT storage",
        2 => "Zed threads.summary must use SQLite TEXT storage",
        3 => "Zed threads.updated_at must use SQLite TEXT storage",
        4 => "Zed threads.data_type must use SQLite TEXT storage",
        5 => "Zed threads.data must use SQLite BLOB storage",
        6 => "Zed threads.parent_id must use SQLite NULL or TEXT storage",
        7 => "Zed threads.folder_paths must use SQLite NULL or TEXT storage",
        8 => "Zed threads.folder_paths_order must use SQLite NULL or TEXT storage",
        9 => "Zed threads.created_at must use SQLite NULL or TEXT storage",
        _ => "Zed thread row has an unknown SQLite storage-class error",
    }
}

fn hash_candidate(hasher: &mut Sha256, candidate: &ZedCandidate) {
    hasher.update(b"candidate\0");
    hash_optional_text(hasher, candidate.id.as_deref());
    hasher.update(candidate.storage_error.to_le_bytes());
    hasher.update(candidate.retained_bytes.to_le_bytes());
    hasher.update(candidate.data_bytes.to_le_bytes());
}

fn hash_hydrated_row(hasher: &mut Sha256, row: &ZedHydratedRow) {
    hasher.update(b"thread\0");
    hash_text(hasher, &row.id);
    hash_text(hasher, &row.summary);
    hash_text(hasher, &row.updated_at);
    hash_text(hasher, &row.data_type);
    hash_bytes(hasher, &row.data);
    hash_optional_text(hasher, row.parent_id.as_deref());
    hash_optional_text(hasher, row.folder_paths.as_deref());
    hash_optional_text(hasher, row.folder_paths_order.as_deref());
    hash_optional_text(hasher, row.created_at.as_deref());
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}
