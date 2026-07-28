//! Crush-owned SQLite row decoding used by the NativePath scanner.

use sha2::{Digest, Sha256};

use crate::complete_content::CompleteContentBodyDigest;
use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{CaptureError, Result};

pub(crate) const CRUSH_LOCATOR_KIND: &str = "crush-sqlite-row-v1";
pub(super) const CRUSH_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 13;

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
        optional_integer_value(row.get(offset + 9)?),
    ])
}

fn optional_text_value(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

fn optional_integer_value(value: Option<i64>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)
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
    let digest = message_record_digest_bytes(values);
    CompleteContentBodyDigest::parse(hex_digest(&digest)).ok_or(CaptureError::SystemInvariant(
        "Crush SQLite record digest is not valid SHA-256",
    ))
}

pub(super) fn message_record_digest_bytes(values: &[NativeSqliteValue]) -> [u8; 32] {
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

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}
