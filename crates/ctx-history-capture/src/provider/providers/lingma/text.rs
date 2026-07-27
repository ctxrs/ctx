use crate::captured_batch::CapturedSqliteValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LingmaSqliteEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl LingmaSqliteEncoding {
    #[cfg(test)]
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16le",
            Self::Utf16Be => "UTF-16be",
        }
    }
}

pub(super) struct LingmaEncodedRow {
    pub(super) rowid: i64,
    pub(super) session_id: Option<Vec<u8>>,
    pub(super) request_id: Option<Vec<u8>>,
    pub(super) chat_prompt: Option<Vec<u8>>,
    pub(super) summary: Option<Vec<u8>>,
    pub(super) error_result: Option<Vec<u8>>,
    pub(super) gmt_create: Option<i64>,
    pub(super) extra: Option<Vec<u8>>,
}

pub(super) fn decode_lingma_encoded_row(
    row: LingmaEncodedRow,
    encoding: LingmaSqliteEncoding,
) -> std::result::Result<(i64, Vec<CapturedSqliteValue>), i64> {
    let rowid = row.rowid;
    let required = |value: Option<Vec<u8>>| {
        value
            .and_then(|bytes| decode_lingma_sqlite_text(encoding, &bytes))
            .ok_or(rowid)
    };
    let optional = |value: Option<Vec<u8>>| {
        value
            .map(|bytes| {
                decode_lingma_sqlite_text(encoding, &bytes)
                    .map(CapturedSqliteValue::Text)
                    .ok_or(rowid)
            })
            .unwrap_or(Ok(CapturedSqliteValue::Null))
    };
    Ok((
        rowid,
        vec![
            CapturedSqliteValue::Integer(rowid),
            CapturedSqliteValue::Text(required(row.session_id)?),
            optional(row.request_id)?,
            CapturedSqliteValue::Text(required(row.chat_prompt)?),
            optional(row.summary)?,
            optional(row.error_result)?,
            row.gmt_create
                .map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer),
            optional(row.extra)?,
        ],
    ))
}

pub(super) fn decode_lingma_sqlite_text(
    encoding: LingmaSqliteEncoding,
    bytes: &[u8],
) -> Option<String> {
    match encoding {
        LingmaSqliteEncoding::Utf8 => std::str::from_utf8(bytes).ok().map(str::to_owned),
        LingmaSqliteEncoding::Utf16Le | LingmaSqliteEncoding::Utf16Be => {
            if !bytes.len().is_multiple_of(2) {
                return None;
            }
            let mut text = String::with_capacity(bytes.len());
            let mut high_surrogate = None::<u16>;
            for pair in bytes.chunks_exact(2) {
                let unit = match encoding {
                    LingmaSqliteEncoding::Utf16Le => u16::from_le_bytes([pair[0], pair[1]]),
                    LingmaSqliteEncoding::Utf16Be => u16::from_be_bytes([pair[0], pair[1]]),
                    LingmaSqliteEncoding::Utf8 => return None,
                };
                let character = if let Some(high) = high_surrogate.take() {
                    if !(0xdc00..=0xdfff).contains(&unit) {
                        return None;
                    }
                    let codepoint =
                        0x1_0000 + (u32::from(high - 0xd800) << 10) + u32::from(unit - 0xdc00);
                    char::from_u32(codepoint)?
                } else if (0xd800..=0xdbff).contains(&unit) {
                    high_surrogate = Some(unit);
                    continue;
                } else if (0xdc00..=0xdfff).contains(&unit) {
                    return None;
                } else {
                    char::from_u32(u32::from(unit))?
                };
                text.push(character);
            }
            high_surrogate.is_none().then_some(text)
        }
    }
}
