//! Crush-owned SQLite row decoding used by the NativePath scanner.

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::complete_content::CompleteContentBodyDigest;
use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{CaptureError, Result};

use super::source::{message_projection, optional_session_column, session_columns};

pub(crate) const CRUSH_LOCATOR_KIND: &str = "crush-sqlite-row-v1";
pub(super) const CRUSH_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 13;

pub(super) fn session_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    let mut values = vec![NativeSqliteValue::Integer(row.get(0)?)];
    values.extend(session_values_at(row, 1)?);
    Ok(values)
}

fn session_values_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    Ok(vec![
        optional_text_value(row.get(offset)?),
        optional_text_value(row.get(offset + 1)?),
        optional_text_value(row.get(offset + 2)?),
        optional_integer_value(row.get(offset + 3)?),
        optional_integer_value(row.get(offset + 4)?),
        optional_integer_value(row.get(offset + 5)?),
        optional_integer_value(row.get(offset + 6)?),
        optional_real_value(row.get(offset + 7)?),
        optional_text_value(row.get(offset + 8)?),
    ])
}

pub(crate) fn message_child_values(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    let mut values = vec![
        optional_integer_value(row.get(0)?),
        optional_integer_value(row.get(1)?),
        optional_integer_value(row.get(2)?),
    ];
    values.extend(message_values_at(row, 3)?);
    Ok(values)
}

pub(super) fn crush_message_values_at_rowid(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let session_columns = session_columns(conn)?;
    let message_columns = super::source::message_columns(conn)?;
    let parent_created_at = optional_session_column(&session_columns, "created_at");
    let parent_updated_at = optional_session_column(&session_columns, "updated_at");
    let message_projection = message_projection(&message_columns, "m");
    let sql = format!(
        "select s.rowid, cast({parent_created_at} as integer), \
                cast({parent_updated_at} as integer), {message_projection} \
         from messages m left join sessions s on s.id = m.session_id where m.rowid = ?1"
    );
    conn.query_row(&sql, [rowid], message_child_values)
        .optional()
        .map_err(CaptureError::from)
}

fn message_values_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    Ok(vec![
        NativeSqliteValue::Integer(row.get(offset)?),
        NativeSqliteValue::Text(row.get(offset + 1)?),
        NativeSqliteValue::Text(row.get(offset + 2)?),
        NativeSqliteValue::Text(row.get(offset + 3)?),
        NativeSqliteValue::Text(row.get(offset + 4)?),
        optional_integer_value(row.get(offset + 5)?),
        optional_integer_value(row.get(offset + 6)?),
        optional_text_value(row.get(offset + 7)?),
        optional_text_value(row.get(offset + 8)?),
        NativeSqliteValue::Integer(row.get::<_, Option<i64>>(offset + 9)?.unwrap_or(0)),
    ])
}

pub(super) fn file_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    Ok(vec![
        NativeSqliteValue::Integer(row.get(0)?),
        optional_text_value(row.get(1)?),
        NativeSqliteValue::Text(row.get(2)?),
        optional_text_value(row.get(3)?),
        optional_integer_value(row.get(4)?),
        optional_integer_value(row.get(5)?),
    ])
}

pub(super) fn read_file_values(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    Ok(vec![
        NativeSqliteValue::Integer(row.get(0)?),
        NativeSqliteValue::Text(row.get(1)?),
        NativeSqliteValue::Text(row.get(2)?),
        optional_integer_value(row.get(3)?),
    ])
}

fn optional_text_value(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

fn optional_integer_value(value: Option<i64>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)
}

fn optional_real_value(value: Option<f64>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::from_real)
}

pub(super) fn message_locator(rowid: i64) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(1 + 8);
    value.push(2);
    value.extend_from_slice(&ordered_i64(rowid).to_be_bytes());
    NativeLocator::new(CRUSH_LOCATOR_KIND, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn message_record_digest(
    values: &[NativeSqliteValue],
) -> Result<CompleteContentBodyDigest> {
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
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("Crush SQLite record digest is not valid SHA-256"),
    )
}

fn ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}
