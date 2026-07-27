use std::{borrow::Cow, io::Read};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::captured_batch::CapturedSqliteValue;
use crate::common::time::parse_rfc3339_utc;
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

pub(crate) struct ZedThreadRow {
    pub(crate) rowid: i64,
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) folder_paths: Option<String>,
    pub(crate) folder_paths_order: Option<String>,
    pub(crate) summary: String,
    pub(crate) updated_at: String,
    pub(crate) data_type: String,
    pub(crate) data: Vec<u8>,
    pub(crate) created_at: Option<String>,
}

pub(super) fn decode_zed_thread(values: &[CapturedSqliteValue]) -> Result<ZedThreadRow> {
    let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Text(id), parent_id, folder_paths, folder_paths_order, CapturedSqliteValue::Text(summary), CapturedSqliteValue::Text(updated_at), CapturedSqliteValue::Text(data_type), CapturedSqliteValue::Blob(data), created_at] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "Zed logical row has an invalid value shape",
        ));
    };
    Ok(ZedThreadRow {
        rowid: *rowid,
        id: id.clone(),
        parent_id: zed_optional_text_value(parent_id)?,
        folder_paths: zed_optional_text_value(folder_paths)?,
        folder_paths_order: zed_optional_text_value(folder_paths_order)?,
        summary: summary.clone(),
        updated_at: updated_at.clone(),
        data_type: data_type.clone(),
        data: data.clone(),
        created_at: zed_optional_text_value(created_at)?,
    })
}

pub(crate) fn decode_zed_thread_for_complete(
    values: &[CapturedSqliteValue],
) -> Result<ZedThreadRow> {
    decode_zed_thread(values)
}

fn zed_optional_text_value(value: &CapturedSqliteValue) -> Result<Option<String>> {
    match value {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::SystemInvariant(
            "Zed logical row has an invalid optional text value",
        )),
    }
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
