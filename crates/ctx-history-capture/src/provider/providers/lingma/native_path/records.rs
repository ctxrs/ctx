use super::{
    publication::{provider_event, EventDraft},
    *,
};

pub(super) fn native_source_id(canonical_source_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!(
            "native-path-provider-source-v1\0{}\0{}\0{}\0<database>",
            CaptureProvider::Lingma.as_str(),
            LINGMA_SQLITE_SOURCE_FORMAT,
            canonical_source_identity,
        ),
        "source",
    )
}

pub(super) fn event_base_index(row: &LingmaRow) -> u64 {
    let rowid = u64::try_from(row.rowid).unwrap_or_else(|_| text_id_index(&row.session_id, 0));
    rowid.saturating_sub(1).saturating_mul(2)
}

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

pub(super) fn released_lingma_event_hash(row: &LingmaRow, role_name: &str) -> String {
    let request_identity = row
        .request_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rowid-{}", row.rowid));
    format!("{}:{request_identity}:{role_name}", row.session_id)
}

pub(super) fn lingma_event_count(row: &LingmaRow) -> usize {
    1 + usize::from(assistant_text(row).is_some())
}

pub(in super::super) fn lingma_locator(rowid: i64) -> Result<NativeLocator> {
    NativeLocator::new(LOCATOR_KIND, ordered_i64(rowid).to_be_bytes().to_vec())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
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

pub(super) fn optional_native_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

pub(in super::super) fn lingma_complete_values(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let encoding = detect_schema(conn)?;
    conn.query_row(
        "select c.rowid, cast(cast(c.session_id as text) as blob), \
                cast(cast(c.request_id as text) as blob), \
                cast(cast(c.chat_prompt as text) as blob), \
                cast(cast(c.summary as text) as blob), \
                cast(cast(c.error_result as text) as blob), \
                cast(c.gmt_create as integer), cast(cast(c.extra as text) as blob) \
         from chat_record c where c.rowid = ?1",
        [rowid],
        |row| {
            Ok(RawRow {
                rowid: row.get(0)?,
                session_id: row.get(1)?,
                request_id: row.get(2)?,
                chat_prompt: row.get(3)?,
                summary: row.get(4)?,
                error_result: row.get(5)?,
                gmt_create: row.get(6)?,
                extra: row.get(7)?,
            })
        },
    )
    .optional()?
    .map(|raw| {
        decode_raw_row(raw, encoding)
            .map(|row| native_values(&row))
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "Lingma complete-content row contains malformed text encoding".to_owned(),
                )
            })
    })
    .transpose()
}

pub(in super::super) fn lingma_complete_user_message(
    values: &[NativeSqliteValue],
) -> Result<(LingmaCoreEvent, String)> {
    let row = row_from_native_values(values)?;
    let text = row.chat_prompt.clone();
    let event = provider_event(
        &row,
        EventDraft {
            provider_event_index: event_base_index(&row),
            role: EventRole::User,
            event_type: EventType::Message,
            occurred_at: lingma_timestamp(row.gmt_create, DateTime::<Utc>::UNIX_EPOCH),
            text: text.clone(),
            body_kind: "chat_prompt",
            fidelity: Fidelity::Imported,
        },
        false,
    )?;
    Ok((event, text))
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

pub(super) fn native_value<'a>(
    values: &'a [NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a NativeSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Lingma logical row is missing {field}"))
    })
}

pub(super) fn native_text(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<String> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text"
        ))),
    }
}

pub(super) fn optional_native_text_value(
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

pub(super) fn native_integer(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<i64> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer"
        ))),
    }
}

pub(super) fn optional_native_integer(
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

pub(super) fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
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
