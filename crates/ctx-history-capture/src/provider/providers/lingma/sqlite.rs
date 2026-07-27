use std::collections::VecDeque;

use rusqlite::{Connection, OptionalExtension, Statement};

use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, CapturedRecord, CapturedSqliteValue, NativeLocator,
    NativePosition, ProviderRecordKind, SourceObservation, StructuralRejectionKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
    CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

use super::text::{decode_lingma_encoded_row, LingmaEncodedRow, LingmaSqliteEncoding};
use super::{
    lingma_captured_error, LINGMA_LOCATOR_KIND, LINGMA_MALFORMED_RECORD_KIND, LINGMA_POSITION_KIND,
    LINGMA_RECORD_KIND, LINGMA_SKIPPED_RECORD_KIND,
};

const LINGMA_POSITION_BYTES: usize = 1 + 8 + 8 + 1;

fn lingma_sqlite_encoding(conn: &Connection) -> Result<LingmaSqliteEncoding> {
    let encoding = conn.pragma_query_value(None, "encoding", |row| row.get::<_, String>(0))?;
    match encoding.as_str() {
        "UTF-8" => Ok(LingmaSqliteEncoding::Utf8),
        "UTF-16le" => Ok(LingmaSqliteEncoding::Utf16Le),
        "UTF-16be" => Ok(LingmaSqliteEncoding::Utf16Be),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma SQLite source uses unsupported text encoding {encoding}"
        ))),
    }
}

pub(super) fn lingma_complete_values(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<CapturedSqliteValue>>> {
    let encoding = lingma_sqlite_encoding(conn)?;
    let encoded = conn
        .query_row(
            "select c.rowid, cast(cast(c.session_id as text) as blob), \
                    cast(cast(c.request_id as text) as blob), \
                    cast(cast(c.chat_prompt as text) as blob), \
                    cast(cast(c.summary as text) as blob), \
                    cast(cast(c.error_result as text) as blob), \
                    cast(c.gmt_create as integer), cast(cast(c.extra as text) as blob) \
             from chat_record c where c.rowid = ?1",
            [rowid],
            lingma_encoded_row,
        )
        .optional()?;
    encoded
        .map(|encoded| {
            decode_lingma_encoded_row(encoded, encoding)
                .map(|(_, values)| values)
                .map_err(|_| {
                    CaptureError::InvalidPayload(
                        "Lingma complete-content row contains malformed text encoding".to_owned(),
                    )
                })
        })
        .transpose()
}

#[derive(Clone, Copy)]
pub(super) struct LingmaSchema {
    encoding: LingmaSqliteEncoding,
}

impl LingmaSchema {
    pub(super) fn detect(conn: &Connection) -> Result<Self> {
        lingma_chat_record_columns(conn)?;
        Ok(Self {
            encoding: lingma_sqlite_encoding(conn)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LingmaKeyset {
    pub(super) next_ordinal: u64,
    pub(super) rowid: i64,
    pub(super) exhausted: bool,
}

#[derive(Clone, Copy)]
pub(super) struct LingmaRowCandidate {
    pub(super) rowid: i64,
    estimated_bytes: u64,
    skip_projection: bool,
}

enum LingmaHydratedRow {
    Values {
        rowid: i64,
        values: Vec<CapturedSqliteValue>,
    },
    MalformedText {
        rowid: i64,
    },
    Skipped {
        rowid: i64,
    },
}

impl LingmaHydratedRow {
    fn rowid(&self) -> i64 {
        match self {
            Self::Values { rowid, .. }
            | Self::MalformedText { rowid }
            | Self::Skipped { rowid } => *rowid,
        }
    }
}

pub(super) struct LingmaBatchProducer<'connection> {
    source: SourceObservation,
    pub(super) current_position: NativePosition,
    first_candidate: Option<Statement<'connection>>,
    next_candidate: Option<Statement<'connection>>,
    conn: &'connection Connection,
    record_kind: ProviderRecordKind,
    malformed_record_kind: ProviderRecordKind,
    skipped_record_kind: ProviderRecordKind,
    encoding: LingmaSqliteEncoding,
    exhausted: bool,
    #[cfg(test)]
    pub(super) executed_queries: usize,
}

impl<'connection> LingmaBatchProducer<'connection> {
    pub(super) fn new(
        conn: &'connection Connection,
        source: SourceObservation,
        start_position: NativePosition,
        schema: LingmaSchema,
    ) -> Result<Self> {
        let exhausted =
            decode_lingma_position(&start_position)?.is_some_and(|keyset| keyset.exhausted);
        // Lingma does not guarantee an index compatible with its legacy
        // `gmt_create, rowid` presentation order. Rebuilding a source-sized rank
        // ledger before every resumable batch hid an unpaced corpus sort. Capture
        // revision 5 instead follows every row in SQLite's native rowid index and policy
        // revision 2 keeps only row-local session metadata. Public event
        // identity remains request/session/rowid-derived, so a revision reset
        // reimports the same durable events idempotently.
        Ok(Self {
            source,
            current_position: start_position,
            first_candidate: (!exhausted)
                .then(|| conn.prepare(&lingma_candidate_sql(false, schema.encoding)))
                .transpose()?,
            next_candidate: (!exhausted)
                .then(|| conn.prepare(&lingma_candidate_sql(true, schema.encoding)))
                .transpose()?,
            conn,
            record_kind: ProviderRecordKind::new(LINGMA_RECORD_KIND)
                .map_err(lingma_captured_error)?,
            malformed_record_kind: ProviderRecordKind::new(LINGMA_MALFORMED_RECORD_KIND)
                .map_err(lingma_captured_error)?,
            skipped_record_kind: ProviderRecordKind::new(LINGMA_SKIPPED_RECORD_KIND)
                .map_err(lingma_captured_error)?,
            encoding: schema.encoding,
            exhausted,
            #[cfg(test)]
            executed_queries: 0,
        })
    }

    pub(super) fn next_batch(&mut self) -> Result<Option<CapturedBatch>> {
        if self.exhausted {
            return Ok(None);
        }
        let start = decode_lingma_position(&self.current_position)?;
        let candidates = self.candidates(start)?;
        if candidates.is_empty() {
            self.exhausted = true;
            return Ok(None);
        }
        let selected_count = lingma_select_candidate_prefix(&candidates)?;
        let selected = &candidates[..selected_count];
        let source_exhausted = candidates.len() == selected_count;
        let oversize_limit = lingma_oversize_limit()?;
        let accepted_rowids = selected
            .iter()
            .filter(|candidate| {
                !candidate.skip_projection && candidate.estimated_bytes <= oversize_limit
            })
            .map(|candidate| candidate.rowid)
            .collect::<Vec<_>>();
        let mut hydrated = self.hydrate(&accepted_rowids)?;
        let next_ordinal = start.map_or(0, |keyset| keyset.next_ordinal);
        let mut builder =
            CapturedBatchBuilder::new(self.source.clone(), self.current_position.clone());
        let mut range_end = self.current_position.clone();

        for (offset, candidate) in selected.iter().enumerate() {
            let last_source_record = source_exhausted && offset + 1 == selected_count;
            let offset = u64::try_from(offset)
                .map_err(|_| CaptureError::SystemInvariant("Lingma batch offset exceeds u64"))?;
            let ordinal = next_ordinal
                .checked_add(offset)
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma captured row ordinal overflowed",
                ))?;
            let next_position = encode_lingma_position(LingmaKeyset {
                next_ordinal: ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Lingma captured row ordinal overflowed",
                ))?,
                rowid: candidate.rowid,
                exhausted: last_source_record,
            })?;
            let locator = lingma_locator(candidate.rowid)?;
            let record = if candidate.skip_projection {
                CapturedRecord::sqlite_logical(
                    ordinal,
                    locator,
                    self.skipped_record_kind.clone(),
                    vec![CapturedSqliteValue::Integer(candidate.rowid)],
                )
                .map_err(lingma_captured_error)?
            } else if candidate.estimated_bytes > oversize_limit {
                CapturedRecord::structural_rejection(
                    ordinal,
                    locator,
                    self.record_kind.clone(),
                    StructuralRejectionKind::OversizeRecord,
                    candidate.estimated_bytes,
                )
            } else {
                let hydrated_row = hydrated.pop_front().ok_or(CaptureError::SystemInvariant(
                    "Lingma hydrated page ended before its selected candidates",
                ))?;
                if hydrated_row.rowid() != candidate.rowid {
                    return Err(CaptureError::SystemInvariant(
                        "Lingma hydrated rows did not match the selected keyset order",
                    ));
                }
                let record = match hydrated_row {
                    LingmaHydratedRow::Values { values, .. } => CapturedRecord::sqlite_logical(
                        ordinal,
                        locator,
                        self.record_kind.clone(),
                        values,
                    ),
                    LingmaHydratedRow::MalformedText { rowid } => CapturedRecord::sqlite_logical(
                        ordinal,
                        locator,
                        self.malformed_record_kind.clone(),
                        vec![CapturedSqliteValue::Integer(rowid)],
                    ),
                    LingmaHydratedRow::Skipped { rowid } => CapturedRecord::sqlite_logical(
                        ordinal,
                        locator,
                        self.skipped_record_kind.clone(),
                        vec![CapturedSqliteValue::Integer(rowid)],
                    ),
                }
                .map_err(lingma_captured_error)?;
                if u64::try_from(record.retained_bytes())
                    .map_or(true, |bytes| bytes > candidate.estimated_bytes)
                {
                    return Err(CaptureError::SystemInvariant(
                        "Lingma retained-byte preflight underestimated a hydrated row",
                    ));
                }
                record
            };
            if !builder.can_accept(&record) {
                return Err(CaptureError::SystemInvariant(
                    "Lingma admitted candidate did not fit its bounded batch",
                ));
            }
            builder.push(record).map_err(lingma_captured_error)?;
            range_end = next_position;
        }
        if !hydrated.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "Lingma hydrated page contained unselected rows",
            ));
        }
        if source_exhausted {
            builder.mark_source_exhausted();
        }
        let batch = builder
            .finish(range_end.clone())
            .map_err(lingma_captured_error)?;
        self.current_position = range_end;
        self.exhausted = source_exhausted;
        Ok(Some(batch))
    }

    pub(super) fn candidates(
        &mut self,
        after: Option<LingmaKeyset>,
    ) -> Result<Vec<LingmaRowCandidate>> {
        #[cfg(test)]
        {
            self.executed_queries = self.executed_queries.saturating_add(1);
        }
        let rows = match after {
            Some(keyset) => self
                .next_candidate
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma exhausted producer retained no next-candidate statement",
                ))?
                .query_map([keyset.rowid], lingma_candidate_row)?,
            None => self
                .first_candidate
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma exhausted producer retained no first-candidate statement",
                ))?
                .query_map([], lingma_candidate_row)?,
        };
        rows.map(|row| {
            let (rowid, estimated_bytes, skip_projection) = row?;
            Ok(LingmaRowCandidate {
                rowid,
                estimated_bytes: u64::try_from(estimated_bytes).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "Lingma retained-byte preflight must be nonnegative".to_owned(),
                    )
                })?,
                skip_projection,
            })
        })
        .collect()
    }

    fn hydrate(&mut self, rowids: &[i64]) -> Result<VecDeque<LingmaHydratedRow>> {
        if rowids.is_empty() {
            return Ok(VecDeque::new());
        }
        let selected = rowids
            .iter()
            .enumerate()
            .map(|(ordinal, rowid)| format!("({rowid}, {ordinal})"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "with selected(rowid, selected_ordinal) as (values {selected}) \
             select c.rowid, cast(cast(c.session_id as text) as blob), \
                    cast(cast(c.request_id as text) as blob), \
                    cast(cast(c.chat_prompt as text) as blob), \
                    cast(cast(c.summary as text) as blob), \
                    cast(cast(c.error_result as text) as blob), \
                    cast(c.gmt_create as integer), cast(cast(c.extra as text) as blob) \
             from selected s join chat_record c on c.rowid = s.rowid \
             order by s.selected_ordinal"
        );
        #[cfg(test)]
        {
            self.executed_queries = self.executed_queries.saturating_add(1);
        }
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([], lingma_encoded_row)?;
        let rows = rows
            .collect::<std::result::Result<Vec<LingmaEncodedRow>, _>>()
            .map_err(CaptureError::from)?;
        let mut hydrated = VecDeque::with_capacity(rows.len());
        for row in rows {
            match decode_lingma_encoded_row(row, self.encoding) {
                Ok((rowid, values)) => {
                    let blank_prompt = matches!(values.get(3), Some(CapturedSqliteValue::Text(prompt)) if prompt.trim().is_empty());
                    if blank_prompt {
                        hydrated.push_back(LingmaHydratedRow::Skipped { rowid });
                    } else {
                        hydrated.push_back(LingmaHydratedRow::Values { rowid, values });
                    }
                }
                Err(rowid) => {
                    hydrated.push_back(LingmaHydratedRow::MalformedText { rowid });
                }
            }
        }
        Ok(hydrated)
    }
}

pub(super) fn lingma_candidate_sql(after_rowid: bool, encoding: LingmaSqliteEncoding) -> String {
    let session_id_bytes = lingma_retained_text_byte_bound_sql("c.session_id", encoding);
    let request_id_bytes = lingma_retained_text_byte_bound_sql("c.request_id", encoding);
    let chat_prompt_bytes = lingma_retained_text_byte_bound_sql("c.chat_prompt", encoding);
    let summary_bytes = lingma_retained_text_byte_bound_sql("c.summary", encoding);
    let error_result_bytes = lingma_retained_text_byte_bound_sql("c.error_result", encoding);
    let extra_bytes = lingma_retained_text_byte_bound_sql("c.extra", encoding);
    let keyset = if after_rowid {
        "where c.rowid > ?1"
    } else {
        ""
    };
    format!(
        "select c.rowid, \
                9 + {session_id_bytes} + 5 + \
                case when c.request_id is null then 1 else {request_id_bytes} + 5 end + \
                {chat_prompt_bytes} + 5 + \
                case when c.summary is null then 1 else {summary_bytes} + 5 end + \
                case when c.error_result is null then 1 else {error_result_bytes} + 5 end + \
                case when c.gmt_create is null then 1 else 9 end + \
                case when c.extra is null then 1 else {extra_bytes} + 5 end, \
                case when c.chat_prompt is null then 1 else 0 end \
         from chat_record c \
         {keyset} \
         order by c.rowid limit 65"
    )
}

fn lingma_candidate_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, i64, bool)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

pub(super) fn lingma_retained_text_byte_bound_sql(
    column: &str,
    encoding: LingmaSqliteEncoding,
) -> String {
    // SQLite length(TEXT) counts characters while length(BLOB) counts bytes. A direct-column
    // octet_length() obtains encoded bytes lazily. UTF-16 uses at most three UTF-8 bytes per
    // code unit, so this remains an upper bound for the retained Rust String without loading it.
    match encoding {
        LingmaSqliteEncoding::Utf8 => format!("coalesce(octet_length({column}), 0)"),
        LingmaSqliteEncoding::Utf16Le | LingmaSqliteEncoding::Utf16Be => {
            format!("((coalesce(octet_length({column}), 0) + 1) / 2) * 3")
        }
    }
}

fn lingma_select_candidate_prefix(candidates: &[LingmaRowCandidate]) -> Result<usize> {
    let oversize_limit = lingma_oversize_limit()?;
    let payload_limit = u64::try_from(CAPTURE_BATCH_MAX_PAYLOAD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Lingma batch payload limit exceeds u64"))?;
    let mut selected = 0_usize;
    let mut retained = 0_u64;
    for candidate in candidates.iter().take(CAPTURE_BATCH_MAX_RECORDS) {
        let content_free = candidate.skip_projection || candidate.estimated_bytes > oversize_limit;
        let candidate_bytes = if content_free {
            0
        } else {
            candidate.estimated_bytes
        };
        if candidate_bytes > payload_limit {
            if selected == 0 {
                selected = 1;
            }
            break;
        }
        let Some(next_retained) = retained.checked_add(candidate_bytes) else {
            return Err(CaptureError::SystemInvariant(
                "Lingma candidate page retained-byte count overflowed",
            ));
        };
        if selected != 0 && next_retained > payload_limit {
            break;
        }
        retained = next_retained;
        selected = selected.saturating_add(1);
    }
    if selected == 0 {
        return Err(CaptureError::SystemInvariant(
            "Lingma nonempty candidate page selected no records",
        ));
    }
    Ok(selected)
}

fn lingma_encoded_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LingmaEncodedRow> {
    Ok(LingmaEncodedRow {
        rowid: row.get(0)?,
        session_id: row.get(1)?,
        request_id: row.get(2)?,
        chat_prompt: row.get(3)?,
        summary: row.get(4)?,
        error_result: row.get(5)?,
        gmt_create: row.get(6)?,
        extra: row.get(7)?,
    })
}

pub(super) fn initial_lingma_position() -> Result<NativePosition> {
    NativePosition::new(LINGMA_POSITION_KIND, vec![0]).map_err(lingma_captured_error)
}

pub(super) fn encode_lingma_position(keyset: LingmaKeyset) -> Result<NativePosition> {
    let mut bytes = Vec::with_capacity(LINGMA_POSITION_BYTES);
    bytes.push(1);
    bytes.extend_from_slice(&keyset.next_ordinal.to_be_bytes());
    bytes.extend_from_slice(&lingma_ordered_i64(keyset.rowid).to_be_bytes());
    bytes.push(u8::from(keyset.exhausted));
    NativePosition::new(LINGMA_POSITION_KIND, bytes).map_err(lingma_captured_error)
}

pub(super) fn decode_lingma_position(position: &NativePosition) -> Result<Option<LingmaKeyset>> {
    if position.kind() != LINGMA_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Lingma cursor has an unexpected native-position kind".to_owned(),
        ));
    }
    if position.value() == [0] {
        return Ok(None);
    }
    if position.value().len() != LINGMA_POSITION_BYTES || position.value()[0] != 1 {
        return Err(CaptureError::InvalidPayload(
            "Lingma cursor has an invalid native-position payload".to_owned(),
        ));
    }
    Ok(Some(LingmaKeyset {
        next_ordinal: lingma_decode_u64(&position.value()[1..9])?,
        rowid: lingma_unordered_i64(lingma_decode_u64(&position.value()[9..17])?),
        exhausted: match position.value()[17] {
            0 => false,
            1 => true,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "Lingma cursor has an invalid exhaustion flag".to_owned(),
                ));
            }
        },
    }))
}

pub(super) fn lingma_locator(rowid: i64) -> Result<NativeLocator> {
    NativeLocator::new(
        LINGMA_LOCATOR_KIND,
        lingma_ordered_i64(rowid).to_be_bytes().to_vec(),
    )
    .map_err(lingma_captured_error)
}

fn lingma_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        CaptureError::InvalidPayload("Lingma cursor integer has an invalid width".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn lingma_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn lingma_unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}

fn lingma_oversize_limit() -> Result<u64> {
    u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Lingma byte limit exceeds u64"))
}

fn lingma_chat_record_columns(conn: &Connection) -> Result<()> {
    if !sqlite_table_exists(conn, "chat_record")? {
        return Err(CaptureError::InvalidPayload(
            "Lingma local.db is missing required chat_record table".into(),
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
    )
}
