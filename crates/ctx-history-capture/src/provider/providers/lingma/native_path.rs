use std::path::Path;

use ctx_history_core::EventType;
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    provider::sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

mod records;
mod source_backed;

use records::{hash_optional_bytes, hash_optional_i64, hash_optional_u64};
pub(super) use records::{lingma_complete_user_message, lingma_complete_values};
pub(crate) use source_backed::{
    scan_lingma_source_backed_v0, LingmaDatabaseScanV0, LingmaDatabaseSourceV0,
    LingmaSourceBackedErrorV0, LingmaSourceBackedRecordV0, LingmaSourceBackedResolverV0,
    LingmaSourceBackedResultV0, LingmaSourceBackedScanV0, LingmaSourceInventoryV0,
};

const CORE_PAGE_LOOKAHEAD_ROWS: usize = 65;
const CORE_PAGE_MAX_SOURCE_BYTES: usize = 7 * 1024 * 1024;
const CORE_HASH_DOMAIN: &[u8] = b"ctx-lingma-nativepath-core-prefix-v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SqliteEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone)]
struct Candidate {
    rowid: i64,
    encoded_bytes: usize,
    field_bytes: [Option<usize>; 6],
    gmt_create: Option<i64>,
}

impl Candidate {
    fn required_fields_present(&self) -> bool {
        self.field_bytes[0].is_some() && self.field_bytes[2].is_some()
    }

    fn can_hydrate(&self) -> bool {
        self.required_fields_present() && self.encoded_bytes <= CORE_PAGE_MAX_SOURCE_BYTES
    }
}

#[derive(Clone)]
struct LingmaRow {
    rowid: i64,
    session_id: String,
    request_id: Option<String>,
    chat_prompt: String,
    summary: Option<String>,
    error_result: Option<String>,
    gmt_create: Option<i64>,
    extra: Option<String>,
}

/// Shape retained only so the untouched released complete-content wrapper can
/// compile. Provider-local fallback construction now rejects below.
pub(super) struct LingmaCoreEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) released_provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) idempotency_key: String,
    pub(super) payload: Value,
}

/// Rejects the released Store-ingestion entrypoint while shared dispatch still
/// carries its historical signature. Lingma ingestion is source-backed.
pub(super) fn import_lingma_native_path(
    _path: &Path,
    _store: &mut Store,
    _context: ProviderAdapterContext,
    _options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::InvalidPayload(
        "Lingma Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}

fn detect_schema(conn: &Connection) -> Result<SqliteEncoding> {
    if !sqlite_table_exists(conn, "chat_record")? {
        return Err(CaptureError::InvalidPayload(
            "Lingma local.db is missing required chat_record table".to_owned(),
        ));
    }
    let columns = sqlite_table_columns(conn, "chat_record")?;
    ensure_sqlite_table_columns(
        &columns,
        "Lingma chat_record table",
        &[
            "session_id",
            "request_id",
            "chat_prompt",
            "summary",
            "error_result",
            "gmt_create",
            "extra",
        ],
    )?;
    let encoding = conn.pragma_query_value(None, "encoding", |row| row.get::<_, String>(0))?;
    match encoding.as_str() {
        "UTF-8" => Ok(SqliteEncoding::Utf8),
        "UTF-16le" => Ok(SqliteEncoding::Utf16Le),
        "UTF-16be" => Ok(SqliteEncoding::Utf16Be),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma SQLite source uses unsupported text encoding {encoding}"
        ))),
    }
}

fn load_candidates(
    conn: &Connection,
    encoding: SqliteEncoding,
    after_rowid: Option<i64>,
    through_rowid: Option<i64>,
) -> Result<Vec<Candidate>> {
    let after = if after_rowid.is_some() {
        "c.rowid > ?1"
    } else {
        "?1 is null"
    };
    let through = if through_rowid.is_some() {
        "and c.rowid <= ?2"
    } else {
        "and ?2 is null"
    };
    let sql = format!(
        "select c.rowid, octet_length(c.session_id), octet_length(c.request_id), \
                octet_length(c.chat_prompt), octet_length(c.summary), \
                octet_length(c.error_result), octet_length(c.extra), \
                cast(c.gmt_create as integer) \
         from chat_record c where {after} {through} \
         order by c.rowid limit {CORE_PAGE_LOOKAHEAD_ROWS}"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map((after_rowid, through_rowid), |row| {
        let raw_bytes = [
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
        ];
        Ok((row.get::<_, i64>(0)?, raw_bytes, row.get(7)?))
    })?;
    rows.map(|row| {
        let (rowid, raw, gmt_create) = row?;
        let mut field_bytes = [None; 6];
        for (index, raw) in raw.into_iter().enumerate() {
            field_bytes[index] = raw
                .map(|bytes| {
                    usize::try_from(bytes).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Lingma SQLite text length must be nonnegative".to_owned(),
                        )
                    })
                })
                .transpose()?
                .map(|bytes| retained_utf8_bound(bytes, encoding));
        }
        let encoded_bytes = field_bytes
            .iter()
            .flatten()
            .fold(128_usize, |total, bytes| total.saturating_add(*bytes));
        Ok(Candidate {
            rowid,
            encoded_bytes,
            field_bytes,
            gmt_create,
        })
    })
    .collect()
}

struct RawRow {
    rowid: i64,
    session_id: Option<Vec<u8>>,
    request_id: Option<Vec<u8>>,
    chat_prompt: Option<Vec<u8>>,
    summary: Option<Vec<u8>>,
    error_result: Option<Vec<u8>>,
    gmt_create: Option<i64>,
    extra: Option<Vec<u8>>,
}

fn load_raw_row(conn: &Connection, rowid: i64) -> Result<RawRow> {
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
    .map_err(CaptureError::from)
}

fn decode_raw_row(row: RawRow, encoding: SqliteEncoding) -> std::result::Result<LingmaRow, i64> {
    let rowid = row.rowid;
    let required = |value: Option<Vec<u8>>| {
        value
            .and_then(|bytes| decode_sqlite_text(encoding, &bytes))
            .ok_or(rowid)
    };
    let optional = |value: Option<Vec<u8>>| {
        value
            .map(|bytes| decode_sqlite_text(encoding, &bytes).ok_or(rowid))
            .transpose()
    };
    Ok(LingmaRow {
        rowid,
        session_id: required(row.session_id)?,
        request_id: optional(row.request_id)?,
        chat_prompt: required(row.chat_prompt)?,
        summary: optional(row.summary)?,
        error_result: optional(row.error_result)?,
        gmt_create: row.gmt_create,
        extra: optional(row.extra)?,
    })
}

fn decode_sqlite_text(encoding: SqliteEncoding, bytes: &[u8]) -> Option<String> {
    match encoding {
        SqliteEncoding::Utf8 => std::str::from_utf8(bytes).ok().map(str::to_owned),
        SqliteEncoding::Utf16Le | SqliteEncoding::Utf16Be => {
            if !bytes.len().is_multiple_of(2) {
                return None;
            }
            let little_endian = encoding == SqliteEncoding::Utf16Le;
            let units = bytes.chunks_exact(2).map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            });
            char::decode_utf16(units)
                .collect::<std::result::Result<String, _>>()
                .ok()
        }
    }
}

fn retained_utf8_bound(bytes: usize, encoding: SqliteEncoding) -> usize {
    match encoding {
        SqliteEncoding::Utf8 => bytes,
        SqliteEncoding::Utf16Le | SqliteEncoding::Utf16Be => bytes.div_ceil(2).saturating_mul(3),
    }
}

fn initial_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(CORE_HASH_DOMAIN);
    hasher
}

fn hash_candidate(hasher: &mut Sha256, candidate: &Candidate, raw: Option<&RawRow>) {
    hasher.update(candidate.rowid.to_le_bytes());
    for bytes in candidate.field_bytes {
        hash_optional_u64(hasher, bytes.and_then(|value| u64::try_from(value).ok()));
    }
    hash_optional_i64(hasher, candidate.gmt_create);
    if let Some(raw) = raw {
        hash_optional_bytes(hasher, raw.session_id.as_deref());
        hash_optional_bytes(hasher, raw.request_id.as_deref());
        hash_optional_bytes(hasher, raw.chat_prompt.as_deref());
        hash_optional_bytes(hasher, raw.summary.as_deref());
        hash_optional_bytes(hasher, raw.error_result.as_deref());
        hash_optional_bytes(hasher, raw.extra.as_deref());
    }
}
