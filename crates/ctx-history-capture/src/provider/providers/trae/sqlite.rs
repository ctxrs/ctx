use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::captured_batch::sqlite_logical_rows::{SqliteLogicalRow, SqliteLogicalRowsBatchError};
use crate::captured_batch::{
    CapturedSqliteValue, NativeLocator, NativePosition, ProviderRecordKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

use super::{
    TRAE_CAPTURE_REVISION, TRAE_CHAT_KEYS, TRAE_CHAT_ROW_LOCATOR_KIND, TRAE_CHAT_ROW_RECORD_KIND,
    TRAE_CHAT_VALUE_RECORD_KIND, TRAE_FRONTIER_LOCATOR_KIND, TRAE_FRONTIER_RECORD_KIND,
    TRAE_INVALID_VALUE_RECORD_KIND, TRAE_POLICY_REVISION, TRAE_POSITION_BYTES, TRAE_POSITION_KIND,
    TRAE_SQLITE_VALUE_OVERHEAD_BYTES,
};

pub(super) fn trae_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Trae SQLite source must be a regular non-symlink file",
        "Trae SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn trae_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
    workspace_ordinal: usize,
) -> String {
    format!(
        "trae-sqlite-snapshot-v1:capture={TRAE_CAPTURE_REVISION};policy={TRAE_POLICY_REVISION};workspace={workspace_ordinal};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TraePosition {
    pub(super) key_index: u16,
    pub(super) session_index: u32,
    pub(super) message_index: u32,
    pub(super) next_ordinal: u64,
}

impl TraePosition {
    fn next_key(self) -> Result<Self> {
        Ok(Self {
            key_index: self
                .key_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant("Trae key index overflowed"))?,
            session_index: 0,
            message_index: 0,
            next_ordinal: self.next_ordinal,
        })
    }
}

#[derive(Clone)]
enum TraeChatCandidate {
    Missing,
    Oversize { observed_bytes: u64 },
    InvalidType { value_type: String },
    Text { retained_bytes: usize },
}

struct TraeCachedCandidate {
    key_index: u16,
    line_key_index: usize,
    candidate: TraeChatCandidate,
}

pub(super) struct TraeRowFetcher<'connection> {
    conn: &'connection Connection,
    workspace_ordinal: usize,
    cached: Option<TraeCachedCandidate>,
    chat_value_record_kind: ProviderRecordKind,
    chat_row_record_kind: ProviderRecordKind,
    invalid_value_record_kind: ProviderRecordKind,
    frontier_record_kind: ProviderRecordKind,
    #[cfg(test)]
    pub(super) hydrated_chat_values: usize,
    #[cfg(test)]
    pub(super) candidate_queries: usize,
}

impl<'connection> TraeRowFetcher<'connection> {
    pub(super) fn new(conn: &'connection Connection, workspace_ordinal: usize) -> Result<Self> {
        Ok(Self {
            conn,
            workspace_ordinal,
            cached: None,
            chat_value_record_kind: ProviderRecordKind::new(TRAE_CHAT_VALUE_RECORD_KIND)
                .map_err(trae_captured_error)?,
            chat_row_record_kind: ProviderRecordKind::new(TRAE_CHAT_ROW_RECORD_KIND)
                .map_err(trae_captured_error)?,
            invalid_value_record_kind: ProviderRecordKind::new(TRAE_INVALID_VALUE_RECORD_KIND)
                .map_err(trae_captured_error)?,
            frontier_record_kind: ProviderRecordKind::new(TRAE_FRONTIER_RECORD_KIND)
                .map_err(trae_captured_error)?,
            #[cfg(test)]
            hydrated_chat_values: 0,
            #[cfg(test)]
            candidate_queries: 0,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let initial_position = decode_trae_position(&after)?.unwrap_or(TraePosition {
            key_index: 0,
            session_index: 0,
            message_index: 0,
            next_ordinal: 0,
        });
        let mut position = initial_position;
        loop {
            let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(position.key_index)) else {
                return if position == initial_position {
                    Ok(None)
                } else {
                    self.frontier_row(position).map(Some)
                };
            };
            self.ensure_chat_row(position.key_index, chat_key)?;
            let cached = self.cached.as_ref().ok_or(CaptureError::SystemInvariant(
                "Trae chat candidate cache is unavailable",
            ))?;
            let line_key_index = cached.line_key_index;
            match cached.candidate.clone() {
                TraeChatCandidate::Missing => {
                    position = position.next_key()?;
                    continue;
                }
                TraeChatCandidate::Oversize { observed_bytes } => {
                    let next = position.next_key()?;
                    let next_position = encode_trae_position(TraePosition {
                        next_ordinal: position
                            .next_ordinal
                            .checked_add(1)
                            .ok_or(CaptureError::SystemInvariant("Trae ordinal overflowed"))?,
                        ..next
                    })?;
                    return SqliteLogicalRow::oversize(
                        next_position,
                        position.next_ordinal,
                        trae_chat_row_locator(position.key_index)?,
                        self.chat_row_record_kind.clone(),
                        observed_bytes,
                    )
                    .map(Some)
                    .map_err(trae_captured_error);
                }
                TraeChatCandidate::InvalidType { value_type } => {
                    let next = position.next_key()?;
                    let next_position = encode_trae_position(TraePosition {
                        next_ordinal: position
                            .next_ordinal
                            .checked_add(1)
                            .ok_or(CaptureError::SystemInvariant("Trae ordinal overflowed"))?,
                        ..next
                    })?;
                    return SqliteLogicalRow::values(
                        next_position,
                        position.next_ordinal,
                        trae_chat_row_locator(position.key_index)?,
                        self.invalid_value_record_kind.clone(),
                        vec![
                            CapturedSqliteValue::Integer(trae_line_integer(trae_line_base(
                                self.workspace_ordinal,
                                line_key_index,
                                0,
                            ))?),
                            CapturedSqliteValue::Text((*chat_key).to_owned()),
                            CapturedSqliteValue::Text(value_type),
                        ],
                    )
                    .map(Some)
                    .map_err(trae_captured_error);
                }
                TraeChatCandidate::Text { retained_bytes } => {
                    let raw_value = self.conn.query_row(
                        "select value from ItemTable where [key] = ?1",
                        [chat_key],
                        |row| row.get::<_, String>(0),
                    )?;
                    #[cfg(test)]
                    {
                        self.hydrated_chat_values = self.hydrated_chat_values.saturating_add(1);
                    }
                    if raw_value.len() != retained_bytes {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let next = position.next_key()?;
                    let next_position = encode_trae_position(TraePosition {
                        next_ordinal: position
                            .next_ordinal
                            .checked_add(1)
                            .ok_or(CaptureError::SystemInvariant("Trae ordinal overflowed"))?,
                        ..next
                    })?;
                    return SqliteLogicalRow::native_content(
                        next_position,
                        position.next_ordinal,
                        trae_chat_row_locator(position.key_index)?,
                        self.chat_value_record_kind.clone(),
                        raw_value.into_bytes(),
                    )
                    .map(Some)
                    .map_err(trae_captured_error);
                }
            }
        }
    }

    fn frontier_row(&self, position: TraePosition) -> Result<SqliteLogicalRow> {
        let next_ordinal = position
            .next_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant("Trae ordinal overflowed"))?;
        let next_position = encode_trae_position(TraePosition {
            next_ordinal,
            ..position
        })?;
        SqliteLogicalRow::native_content(
            next_position,
            position.next_ordinal,
            trae_frontier_locator(position)?,
            self.frontier_record_kind.clone(),
            Vec::new(),
        )
        .map_err(trae_captured_error)
    }

    fn ensure_chat_row(&mut self, key_index: u16, chat_key: &str) -> Result<()> {
        if self
            .cached
            .as_ref()
            .is_some_and(|cached| cached.key_index == key_index)
        {
            return Ok(());
        }
        #[cfg(test)]
        {
            self.candidate_queries = self.candidate_queries.saturating_add(1);
        }
        let candidate = with_trae_length_preflight(self.conn, || {
            self.conn
                .query_row(
                    "select case typeof(value) \
                        when 'text' then 1 when 'blob' then 2 when 'integer' then 3 \
                        when 'real' then 4 when 'null' then 5 else 0 end, \
                        coalesce(octet_length(value), 0) \
                     from ItemTable where [key] = ?1",
                    [chat_key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
        })?;
        let line_key_index = trae_present_key_ordinal(self.conn, key_index)?;
        let candidate = match candidate {
            None => TraeChatCandidate::Missing,
            Some((value_type, retained_bytes)) => {
                let retained_bytes = u64::try_from(retained_bytes).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "Trae ItemTable value length must be nonnegative".to_owned(),
                    )
                })?;
                let observed_bytes = retained_bytes
                    .checked_add(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
                    .and_then(|total| {
                        u64::try_from(chat_key.len())
                            .ok()
                            .and_then(|key_bytes| total.checked_add(key_bytes))
                    })
                    .ok_or(CaptureError::SystemInvariant(
                        "Trae ItemTable retained byte count overflowed",
                    ))?;
                if observed_bytes > trae_oversize_limit()? {
                    TraeChatCandidate::Oversize { observed_bytes }
                } else if value_type != 1 {
                    TraeChatCandidate::InvalidType {
                        value_type: trae_sqlite_type_name(value_type).to_owned(),
                    }
                } else {
                    TraeChatCandidate::Text {
                        retained_bytes: usize::try_from(retained_bytes).map_err(|_| {
                            CaptureError::SystemInvariant(
                                "Trae retained bytes exceed platform limits",
                            )
                        })?,
                    }
                }
            }
        };
        self.cached = Some(TraeCachedCandidate {
            key_index,
            line_key_index,
            candidate,
        });
        Ok(())
    }
}

pub(super) fn with_trae_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even an integer-only octet_length preflight
    // for an oversized stored value. This scope returns only integer type tags
    // and byte counts; restore the provider cap before any TEXT hydration.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

fn trae_sqlite_type_name(value_type: i64) -> &'static str {
    match value_type {
        1 => "text",
        2 => "blob",
        3 => "integer",
        4 => "real",
        5 => "null",
        _ => "unknown",
    }
}

fn trae_line_integer(line: usize) -> Result<i64> {
    i64::try_from(line)
        .map_err(|_| CaptureError::InvalidPayload("Trae line exceeds SQLite limits".to_owned()))
}

fn trae_present_key_ordinal(conn: &Connection, key_index: u16) -> Result<usize> {
    let mut ordinal = 0_usize;
    for key in TRAE_CHAT_KEYS.iter().take(usize::from(key_index)) {
        let present = conn.query_row(
            "select exists(select 1 from ItemTable where [key] = ?1)",
            [key],
            |row| row.get::<_, i64>(0),
        )?;
        if present != 0 {
            ordinal = ordinal.saturating_add(1);
        }
    }
    Ok(ordinal)
}

pub(super) fn trae_line_base(
    workspace_ordinal: usize,
    key_index: usize,
    session_index: usize,
) -> usize {
    workspace_ordinal
        .saturating_mul(1_000_000)
        .saturating_add(key_index.saturating_mul(10_000))
        .saturating_add(session_index.saturating_mul(1_000))
}

pub(super) fn decode_trae_chat_row_locator(locator: &NativeLocator) -> Result<u16> {
    if locator.kind() != TRAE_CHAT_ROW_LOCATOR_KIND || locator.value().len() != 2 {
        return Err(CaptureError::InvalidPayload(
            "Trae chat row locator is invalid".to_owned(),
        ));
    }
    Ok(u16::from_be_bytes(locator.value().try_into().map_err(
        |_| CaptureError::InvalidPayload("Trae chat row locator is invalid".to_owned()),
    )?))
}

pub(super) fn trae_rejection_values<'a>(
    values: &'a [CapturedSqliteValue],
    label: &str,
) -> Result<(usize, &'a str, &'a str)> {
    let [line, first, second] = values else {
        return Err(CaptureError::InvalidPayload(format!(
            "Trae {label} logical row has an invalid value shape"
        )));
    };
    let line = usize::try_from(trae_required_integer(line, "rejection line")?).map_err(|_| {
        CaptureError::InvalidPayload("Trae rejection line exceeds platform limits".to_owned())
    })?;
    Ok((
        line,
        trae_required_text(first, label)?,
        trae_required_text(second, label)?,
    ))
}

fn trae_required_text<'a>(value: &'a CapturedSqliteValue, field: &str) -> Result<&'a str> {
    match value {
        CapturedSqliteValue::Text(value) => Ok(value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Trae logical {field} must be text"
        ))),
    }
}

fn trae_required_integer(value: &CapturedSqliteValue, field: &str) -> Result<i64> {
    match value {
        CapturedSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Trae logical {field} must be an integer"
        ))),
    }
}

pub(super) fn trae_validate_schema(conn: &Connection, path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "ItemTable")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae state.vscdb is missing ItemTable",
        });
    }
    ensure_sqlite_table_columns(
        &sqlite_table_columns(conn, "ItemTable")?,
        "Trae ItemTable",
        &["key", "value"],
    )
}

pub(super) fn initial_trae_position() -> Result<NativePosition> {
    NativePosition::new(TRAE_POSITION_KIND, vec![0]).map_err(trae_captured_error)
}

fn encode_trae_position(position: TraePosition) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(TRAE_POSITION_BYTES);
    value.push(1);
    value.extend_from_slice(&position.key_index.to_be_bytes());
    value.extend_from_slice(&position.session_index.to_be_bytes());
    value.extend_from_slice(&position.message_index.to_be_bytes());
    value.extend_from_slice(&position.next_ordinal.to_be_bytes());
    NativePosition::new(TRAE_POSITION_KIND, value).map_err(trae_captured_error)
}

pub(super) fn decode_trae_position(position: &NativePosition) -> Result<Option<TraePosition>> {
    if position.kind() != TRAE_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Trae cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != TRAE_POSITION_BYTES || position.value()[0] != 1 {
        return Err(CaptureError::InvalidPayload(
            "Trae cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(TraePosition {
        key_index: u16::from_be_bytes(
            position.value()[1..3]
                .try_into()
                .map_err(|_| CaptureError::InvalidPayload("invalid Trae key cursor".to_owned()))?,
        ),
        session_index: u32::from_be_bytes(
            position.value()[3..7].try_into().map_err(|_| {
                CaptureError::InvalidPayload("invalid Trae session cursor".to_owned())
            })?,
        ),
        message_index: u32::from_be_bytes(
            position.value()[7..11].try_into().map_err(|_| {
                CaptureError::InvalidPayload("invalid Trae message cursor".to_owned())
            })?,
        ),
        next_ordinal: u64::from_be_bytes(
            position.value()[11..19].try_into().map_err(|_| {
                CaptureError::InvalidPayload("invalid Trae ordinal cursor".to_owned())
            })?,
        ),
    }))
}

fn trae_chat_row_locator(key_index: u16) -> Result<NativeLocator> {
    NativeLocator::new(TRAE_CHAT_ROW_LOCATOR_KIND, key_index.to_be_bytes().to_vec())
        .map_err(trae_captured_error)
}

fn trae_frontier_locator(position: TraePosition) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(2 + 4 + 4);
    value.extend_from_slice(&position.key_index.to_be_bytes());
    value.extend_from_slice(&position.session_index.to_be_bytes());
    value.extend_from_slice(&position.message_index.to_be_bytes());
    NativeLocator::new(TRAE_FRONTIER_LOCATOR_KIND, value).map_err(trae_captured_error)
}

fn trae_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Trae byte limit exceeds u64"))
}

pub(super) fn trae_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn trae_sqlite_batch_error(
    error: SqliteLogicalRowsBatchError<CaptureError>,
) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
