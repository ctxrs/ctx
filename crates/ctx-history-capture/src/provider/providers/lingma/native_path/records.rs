use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use sha2::{Digest, Sha256};

use crate::{
    native_source::NativeSqliteValue, provider::normalization::provider_timestamp_seconds,
    CaptureError, Result,
};

use super::{LingmaCoreEvent, LingmaRow};

pub(super) fn lingma_timestamp(raw: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    raw.map(|timestamp| provider_timestamp_seconds(Some(timestamp as f64), fallback))
        .unwrap_or(fallback)
}

pub(super) fn assistant_text(row: &LingmaRow) -> Option<(String, &'static str, EventType)> {
    if let Some(summary) = row
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some((summary.to_owned(), "summary", EventType::Message));
    }
    row.error_result
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "{}")
        .map(|error| {
            (
                format!("Lingma error result: {error}"),
                "error_result",
                EventType::Notice,
            )
        })
}

pub(super) fn native_values(row: &LingmaRow) -> Vec<NativeSqliteValue> {
    vec![
        NativeSqliteValue::Integer(row.rowid),
        NativeSqliteValue::Text(row.session_id.clone()),
        optional_native_text(row.request_id.clone()),
        NativeSqliteValue::Text(row.chat_prompt.clone()),
        optional_native_text(row.summary.clone()),
        optional_native_text(row.error_result.clone()),
        row.gmt_create
            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer),
        optional_native_text(row.extra.clone()),
    ]
}

fn optional_native_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

/// Shared released complete-content dispatch still references this provider
/// symbol. The Store/canonical fallback itself has been removed.
pub(in super::super) fn lingma_complete_values(
    _conn: &rusqlite::Connection,
    _rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    Err(CaptureError::InvalidPayload(
        "Lingma canonical Store hydration was removed; use source-backed hydration".to_owned(),
    ))
}

/// Shared released complete-content dispatch still references this provider
/// symbol. The Store/canonical fallback itself has been removed.
pub(in super::super) fn lingma_complete_user_message(
    _values: &[NativeSqliteValue],
) -> Result<(LingmaCoreEvent, String)> {
    Err(CaptureError::InvalidPayload(
        "Lingma canonical Store hydration was removed; use source-backed hydration".to_owned(),
    ))
}

pub(super) fn row_from_native_values(values: &[NativeSqliteValue]) -> Result<LingmaRow> {
    if values.len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "Lingma logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(LingmaRow {
        rowid: native_integer(values, 0, "rowid")?,
        session_id: native_text(values, 1, "session_id")?,
        request_id: optional_native_text_value(values, 2, "request_id")?,
        chat_prompt: native_text(values, 3, "chat_prompt")?,
        summary: optional_native_text_value(values, 4, "summary")?,
        error_result: optional_native_text_value(values, 5, "error_result")?,
        gmt_create: optional_native_integer(values, 6, "gmt_create")?,
        extra: optional_native_text_value(values, 7, "extra")?,
    })
}

fn native_value<'a>(
    values: &'a [NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a NativeSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Lingma logical row is missing {field}"))
    })
}

fn native_text(values: &[NativeSqliteValue], index: usize, field: &str) -> Result<String> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text"
        ))),
    }
}

fn optional_native_text_value(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text or null"
        ))),
    }
}

fn native_integer(values: &[NativeSqliteValue], index: usize, field: &str) -> Result<i64> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer"
        ))),
    }
}

fn optional_native_integer(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer or null"
        ))),
    }
}

pub(super) fn lingma_logical_record_sha256(values: &[NativeSqliteValue]) -> [u8; 32] {
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

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

pub(super) fn hash_optional_bytes(hasher: &mut Sha256, bytes: Option<&[u8]>) {
    hasher.update([u8::from(bytes.is_some())]);
    if let Some(bytes) = bytes {
        hash_bytes(hasher, bytes);
    }
}

pub(super) fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

pub(super) fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}
