use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use sha2::{Digest, Sha256};

use crate::{
    native_source::NativeSqliteValue, provider::normalization::provider_timestamp_seconds,
};

use super::LingmaRow;

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
