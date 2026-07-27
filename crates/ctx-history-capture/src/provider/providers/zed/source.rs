use std::{collections::BTreeSet, path::Path};

use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, optional_column_expr, sqlite_table_columns, sqlite_table_exists,
    ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

use super::{
    ZED_CAPTURE_REVISION, ZED_LOCATOR_KIND, ZED_MALFORMED_RECORD_KIND, ZED_POLICY_REVISION,
    ZED_POSITION_BYTES, ZED_POSITION_KIND, ZED_SQLITE_VALUE_OVERHEAD_BYTES,
};

pub(super) fn zed_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Zed SQLite source must be a regular non-symlink file",
        "Zed SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn zed_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "zed-sqlite-snapshot-v1:capture={ZED_CAPTURE_REVISION};policy={ZED_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(super) struct ZedRowFetcher<'connection> {
    conn: &'connection Connection,
    first_indexed_candidate: Statement<'connection>,
    next_indexed_candidate: Statement<'connection>,
    hydration: Statement<'connection>,
    record_kind: ProviderRecordKind,
    malformed_record_kind: ProviderRecordKind,
}

impl<'connection> ZedRowFetcher<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        columns: &BTreeSet<String>,
        record_kind: ProviderRecordKind,
    ) -> Result<Self> {
        zed_require_id_index(conn)?;
        let parent_id = optional_column_expr(columns, "parent_id", "NULL");
        let folder_paths = optional_column_expr(columns, "folder_paths", "NULL");
        let folder_paths_order = optional_column_expr(columns, "folder_paths_order", "NULL");
        let created_at = optional_column_expr(columns, "created_at", "NULL");
        let retained_bytes = zed_retained_length_expr(&[
            "id".to_owned(),
            parent_id.to_owned(),
            folder_paths.to_owned(),
            folder_paths_order.to_owned(),
            "summary".to_owned(),
            "updated_at".to_owned(),
            "data_type".to_owned(),
            "data".to_owned(),
            created_at.to_owned(),
        ]);
        let storage_error =
            zed_storage_class_error_expr(parent_id, folder_paths, folder_paths_order, created_at);
        Ok(Self {
            conn,
            first_indexed_candidate: conn
                .prepare(&zed_first_candidate_sql(&retained_bytes, &storage_error))?,
            next_indexed_candidate: conn
                .prepare(&zed_next_candidate_sql(&retained_bytes, &storage_error))?,
            hydration: conn.prepare(&format!(
                "select rowid, CAST(id AS TEXT), CAST({parent_id} AS TEXT), \
                        CAST({folder_paths} AS TEXT), CAST({folder_paths_order} AS TEXT), \
                        CAST(summary AS TEXT), CAST(updated_at AS TEXT), \
                        CAST(data_type AS TEXT), data, CAST({created_at} AS TEXT) \
                 from threads where rowid = ?1"
            ))?,
            record_kind,
            malformed_record_kind: ProviderRecordKind::new(ZED_MALFORMED_RECORD_KIND)
                .map_err(zed_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_zed_position(&after)?;
        let ordinal = keyset.as_ref().map_or(0, |value| value.next_ordinal);
        let candidate = match keyset.as_ref().map(|value| value.phase) {
            Some(ZedCapturePhase::Exhausted) => return Ok(None),
            None => zed_fetch_candidate(
                self.conn,
                &mut self.first_indexed_candidate,
                None,
                ZedCapturePhase::Rows,
            )?,
            Some(ZedCapturePhase::Rows) => zed_fetch_candidate(
                self.conn,
                &mut self.next_indexed_candidate,
                keyset.as_ref().map(|value| value.rowid),
                ZedCapturePhase::Rows,
            )?,
        };
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let next_position = encode_zed_position(ZedKeyset {
            phase: if candidate.terminal {
                ZedCapturePhase::Exhausted
            } else {
                candidate.phase
            },
            next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Zed captured row ordinal overflowed",
            ))?,
            rowid: candidate.rowid,
        })?;
        let locator = NativeLocator::new(ZED_LOCATOR_KIND, candidate.rowid.to_be_bytes().to_vec())
            .map_err(zed_captured_error)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > zed_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.record_kind.clone(),
                observed_bytes,
            )
            .map(Some)
            .map_err(zed_captured_error);
        }
        if candidate.storage_error_code != 0 {
            ZedStorageClassError::from_code(candidate.storage_error_code)?;
            return SqliteLogicalRow::values(
                next_position,
                ordinal,
                locator,
                self.malformed_record_kind.clone(),
                vec![
                    CapturedSqliteValue::Integer(candidate.rowid),
                    CapturedSqliteValue::Integer(candidate.storage_error_code),
                ],
            )
            .map(Some)
            .map_err(zed_captured_error);
        }
        let values = self.hydration.query_row([candidate.rowid], |row| {
            Ok(vec![
                CapturedSqliteValue::Integer(row.get(0)?),
                CapturedSqliteValue::Text(row.get(1)?),
                zed_captured_optional_text(row.get(2)?),
                zed_captured_optional_text(row.get(3)?),
                zed_captured_optional_text(row.get(4)?),
                CapturedSqliteValue::Text(row.get(5)?),
                CapturedSqliteValue::Text(row.get(6)?),
                CapturedSqliteValue::Text(row.get(7)?),
                CapturedSqliteValue::Blob(row.get(8)?),
                zed_captured_optional_text(row.get(9)?),
            ])
        })?;

        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.record_kind.clone(),
            values,
        )
        .map(Some)
        .map_err(zed_captured_error)
    }
}

fn zed_candidate_terminal_expr() -> &'static str {
    "case when not exists (\
         select 1 from threads later \
         where later.id > threads.id \
         order by later.id limit 1\
     ) then 1 else 0 end"
}

fn zed_first_candidate_sql(retained_bytes: &str, storage_error: &str) -> String {
    format!(
        "select rowid, {retained_bytes}, {}, {storage_error} \
         from threads order by id limit 1",
        zed_candidate_terminal_expr()
    )
}

fn zed_next_candidate_sql(retained_bytes: &str, storage_error: &str) -> String {
    format!(
        "select rowid, {retained_bytes}, {}, {storage_error} \
         from threads \
         where id > (select previous.id from threads previous where previous.rowid = ?1) \
         order by id limit 1",
        zed_candidate_terminal_expr()
    )
}

fn zed_fetch_candidate(
    conn: &Connection,
    statement: &mut Statement<'_>,
    after_rowid: Option<i64>,
    phase: ZedCapturePhase,
) -> Result<Option<ZedRowCandidate>> {
    with_zed_length_preflight(conn, || {
        let read_candidate = |row: &rusqlite::Row<'_>| {
            Ok(ZedRowCandidate {
                phase,
                rowid: row.get(0)?,
                retained_bytes: row.get(1)?,
                terminal: row.get::<_, i64>(2)? != 0,
                storage_error_code: row.get(3)?,
            })
        };
        match after_rowid {
            Some(rowid) => statement.query_row([rowid], read_candidate).optional(),
            None => statement.query_row([], read_candidate).optional(),
        }
    })
}

fn with_zed_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH can reject an index walk or octet_length inspection even when a
    // candidate query returns only integers. Lift it only around that integer-only preflight;
    // the guard restores the provider cap before ordering keys or raw values can be hydrated.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

struct ZedRowCandidate {
    phase: ZedCapturePhase,
    rowid: i64,
    retained_bytes: i64,
    terminal: bool,
    storage_error_code: i64,
}

impl ZedRowCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        let payload = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "Zed SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        ZED_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(payload)
            .ok_or(CaptureError::SystemInvariant(
                "Zed SQLite retained byte count overflowed",
            ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ZedCapturePhase {
    Rows,
    Exhausted,
}

impl ZedCapturePhase {
    fn tag(self) -> u8 {
        match self {
            Self::Rows => 1,
            Self::Exhausted => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Rows),
            2 => Ok(Self::Exhausted),
            _ => Err(CaptureError::InvalidPayload(
                "Zed cursor has an unknown capture phase".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ZedKeyset {
    pub(super) phase: ZedCapturePhase,
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
}

pub(super) fn initial_zed_position() -> Result<NativePosition> {
    NativePosition::new(ZED_POSITION_KIND, vec![0]).map_err(zed_captured_error)
}

fn encode_zed_position(keyset: ZedKeyset) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(ZED_POSITION_BYTES);
    value.push(2);
    value.push(keyset.phase.tag());
    value.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    value.extend_from_slice(&zed_ordered_i64(keyset.rowid).to_be_bytes());
    NativePosition::new(ZED_POSITION_KIND, value).map_err(zed_captured_error)
}

pub(super) fn decode_zed_position(position: &NativePosition) -> Result<Option<ZedKeyset>> {
    if position.kind() != ZED_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Zed cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != ZED_POSITION_BYTES || position.value()[0] != 2 {
        return Err(CaptureError::InvalidPayload(
            "Zed cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(ZedKeyset {
        phase: ZedCapturePhase::from_tag(position.value()[1])?,
        next_ordinal: zed_decode_u64(&position.value()[2..10])?,
        rowid: zed_unordered_i64(zed_decode_u64(&position.value()[10..18])?),
    }))
}

fn zed_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Zed cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn zed_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn zed_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn zed_captured_optional_text(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

fn zed_storage_class_error_expr(
    parent_id: &str,
    folder_paths: &str,
    folder_paths_order: &str,
    created_at: &str,
) -> String {
    format!(
        "case \
         when typeof(id) != 'text' then 1 \
         when typeof({parent_id}) not in ('null', 'text') then 2 \
         when typeof({folder_paths}) not in ('null', 'text') then 3 \
         when typeof({folder_paths_order}) not in ('null', 'text') then 4 \
         when typeof(summary) != 'text' then 5 \
         when typeof(updated_at) != 'text' then 6 \
         when typeof(data_type) != 'text' then 7 \
         when typeof(data) != 'blob' then 8 \
         when typeof({created_at}) not in ('null', 'text') then 9 \
         else 0 end"
    )
}

fn zed_retained_length_expr(expressions: &[String]) -> String {
    // octet_length returns an integer without materializing large TEXT/BLOB values into Rust.
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn zed_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Zed byte limit exceeds u64"))
}

pub(super) fn zed_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn zed_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ZedStorageClassError {
    Id,
    ParentId,
    FolderPaths,
    FolderPathsOrder,
    Summary,
    UpdatedAt,
    DataType,
    Data,
    CreatedAt,
}

impl ZedStorageClassError {
    fn from_code(code: i64) -> Result<Self> {
        match code {
            1 => Ok(Self::Id),
            2 => Ok(Self::ParentId),
            3 => Ok(Self::FolderPaths),
            4 => Ok(Self::FolderPathsOrder),
            5 => Ok(Self::Summary),
            6 => Ok(Self::UpdatedAt),
            7 => Ok(Self::DataType),
            8 => Ok(Self::Data),
            9 => Ok(Self::CreatedAt),
            _ => Err(CaptureError::SystemInvariant(
                "Zed SQLite storage-class rejection code is invalid",
            )),
        }
    }

    pub(super) fn rejection_reason(self) -> &'static str {
        match self {
            Self::Id => "Zed thread id must have SQLite TEXT storage class",
            Self::ParentId => "Zed thread parent_id must have SQLite NULL or TEXT storage class",
            Self::FolderPaths => {
                "Zed thread folder_paths must have SQLite NULL or TEXT storage class"
            }
            Self::FolderPathsOrder => {
                "Zed thread folder_paths_order must have SQLite NULL or TEXT storage class"
            }
            Self::Summary => "Zed thread summary must have SQLite TEXT storage class",
            Self::UpdatedAt => "Zed thread updated_at must have SQLite TEXT storage class",
            Self::DataType => "Zed thread data_type must have SQLite TEXT storage class",
            Self::Data => "Zed thread data must have SQLite BLOB storage class",
            Self::CreatedAt => "Zed thread created_at must have SQLite NULL or TEXT storage class",
        }
    }
}

pub(super) fn decode_zed_storage_rejection(
    values: &[CapturedSqliteValue],
) -> Result<(i64, ZedStorageClassError)> {
    let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Integer(error_code)] = values
    else {
        return Err(CaptureError::SystemInvariant(
            "Zed malformed logical row has an invalid value shape",
        ));
    };
    Ok((*rowid, ZedStorageClassError::from_code(*error_code)?))
}

pub(super) fn zed_thread_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "threads")? {
        return Err(CaptureError::InvalidPayload(
            "Zed threads.db is missing required threads table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "threads")?;
    ensure_sqlite_table_columns(
        &columns,
        "Zed threads table",
        &["id", "summary", "updated_at", "data_type", "data"],
    )?;
    Ok(columns)
}

fn zed_require_id_index(conn: &Connection) -> Result<()> {
    let mut indexes = conn.prepare(
        "select name from pragma_index_list('threads') \
         where partial = 0 and \"unique\" = 1 order by name",
    )?;
    let index_names = indexes
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut index_columns = conn.prepare(
        "select name, desc, coll from pragma_index_xinfo(?1) \
         where key = 1 order by seqno",
    )?;
    for index_name in index_names {
        let columns = index_columns
            .query_map([index_name], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if columns.len() == 1
            && columns[0].0.as_deref() == Some("id")
            && columns[0].1 == 0
            && columns[0]
                .2
                .as_deref()
                .is_some_and(|collation| collation.eq_ignore_ascii_case("binary"))
        {
            return Ok(());
        }
    }
    Err(CaptureError::InvalidPayload(
        "Zed threads table requires a non-partial unique ascending BINARY index on (id)".to_owned(),
    ))
}

#[cfg(test)]
#[path = "source/tests.rs"]
mod tests;
