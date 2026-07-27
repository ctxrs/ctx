use std::{borrow::Cow, io::Read};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::common::time::parse_rfc3339_utc;
use crate::native_source::NativeSqliteValue;
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

pub(crate) struct ZedThreadRow {
    pub(crate) rowid: i64,
    pub(crate) id: String,
    pub(crate) updated_at: String,
    pub(crate) data_type: String,
    pub(crate) data: Vec<u8>,
}

pub(super) fn decode_zed_thread(values: &[NativeSqliteValue]) -> Result<ZedThreadRow> {
    let [NativeSqliteValue::Integer(rowid), NativeSqliteValue::Text(id), _, _, _, NativeSqliteValue::Text(_), NativeSqliteValue::Text(updated_at), NativeSqliteValue::Text(data_type), NativeSqliteValue::Blob(data), _] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "Zed logical row has an invalid value shape",
        ));
    };
    Ok(ZedThreadRow {
        rowid: *rowid,
        id: id.clone(),
        updated_at: updated_at.clone(),
        data_type: data_type.clone(),
        data: data.clone(),
    })
}

pub(crate) fn decode_zed_thread_for_complete(values: &[NativeSqliteValue]) -> Result<ZedThreadRow> {
    decode_zed_thread(values)
}

pub(super) fn zed_decode_thread_json(row: &ZedThreadRow) -> Result<Value> {
    if row.data.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "Zed thread {} data exceeds {MAX_PROVIDER_SQLITE_VALUE_BYTES} encoded bytes",
            row.id
        )));
    }
    let json = match row.data_type.as_str() {
        "json" => Cow::Borrowed(row.data.as_slice()),
        "zstd" => Cow::Owned(zed_decode_zstd(&row.data)?),
        other => {
            return Err(CaptureError::InvalidPayload(format!(
                "Zed thread {} has unsupported data_type {other:?}",
                row.id
            )));
        }
    };
    serde_json::from_slice(&json).map_err(CaptureError::from)
}

fn zed_decode_zstd(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(data)?;
    let mut limited = decoder
        .by_ref()
        .take(MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 + 1);
    let mut out = Vec::new();
    limited.read_to_end(&mut out)?;
    if out.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "Zed compressed thread JSON exceeds {MAX_PROVIDER_SQLITE_VALUE_BYTES} decompressed bytes"
        )));
    }
    Ok(out)
}

pub(super) fn zed_required_timestamp(raw: &str, field: &'static str) -> Result<DateTime<Utc>> {
    parse_rfc3339_utc(raw)
        .ok_or_else(|| CaptureError::InvalidPayload(format!("{field} is not RFC3339: {raw:?}")))
}
