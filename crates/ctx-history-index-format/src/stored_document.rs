use ctx_history_core::{CoreRecord, MAX_ENCODED_CORE_RECORD_BYTES};
use tantivy::{
    columnar::Column, schema::Value as _, DocAddress, DocId, SegmentReader, TantivyDocument,
};
use uuid::Uuid;

use crate::{source_token, Fields, IndexError, Result};

const EVENT_ID_HIGH_FIELD: &str = "event_id_high";
const EVENT_ID_LOW_FIELD: &str = "event_id_low";
const SESSION_ID_HIGH_FIELD: &str = "session_id_high";
const SESSION_ID_LOW_FIELD: &str = "session_id_low";
const EVENT_IDENTITY_DIGEST_FIELD: &str = "event_identity_digest";
const SOURCE_KEY_FIELD: &str = "source_key";
const CORE_CONTENT_BYTES_FIELD: &str = "core_content_bytes";
const CORE_RECORD_ENCODED_BYTES_FIELD: &str = "core_record_encoded_bytes";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreDocumentFastFacts {
    pub event_id: Uuid,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
}

/// An owned stored document whose unique Core field and every indexed
/// acceptance projection were validated by the format authority.
#[derive(Debug)]
pub struct AcceptedCoreDocument {
    document: TantivyDocument,
    core_record_field: tantivy::schema::Field,
}

impl AcceptedCoreDocument {
    pub fn encoded_core_record(&self) -> &[u8] {
        self.document
            .get_first(self.core_record_field)
            .and_then(|value| value.as_bytes())
            .expect("accepted Core document retains its validated unique bytes")
    }
}

fn fast_uuid(
    segment: &SegmentReader,
    doc: DocId,
    high_field: &'static str,
    low_field: &'static str,
) -> Result<Uuid> {
    let high = unique_fast_u64(segment, doc, high_field)?;
    let low = unique_fast_u64(segment, doc, low_field)?;
    Ok(Uuid::from_u128((u128::from(high) << 64) | u128::from(low)))
}

fn unique_fast_u64(segment: &SegmentReader, doc: DocId, field_name: &'static str) -> Result<u64> {
    let column = segment.fast_fields().u64(field_name)?;
    unique_column_u64(&column, doc, field_name)
}

fn unique_column_u64(column: &Column<u64>, doc: DocId, field_name: &'static str) -> Result<u64> {
    let mut values = column.values_for_doc(doc);
    let value = values
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

fn fast_string(segment: &SegmentReader, doc: DocId, field_name: &'static str) -> Result<String> {
    let column = segment
        .fast_fields()
        .str(field_name)?
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    let mut term_ords = column.term_ords(doc);
    let term_ord = term_ords
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if term_ords.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    let mut value = String::new();
    if !column.ord_to_str(term_ord, &mut value)? {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

fn core_record_encoded_bytes(searcher: &tantivy::Searcher, address: DocAddress) -> Result<usize> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField(
            CORE_RECORD_ENCODED_BYTES_FIELD,
        ))?;
    checked_encoded_core_bytes(unique_fast_u64(
        segment,
        address.doc_id,
        CORE_RECORD_ENCODED_BYTES_FIELD,
    )?)
}

fn checked_encoded_core_bytes(encoded_core_bytes: u64) -> Result<usize> {
    let encoded_core_bytes =
        usize::try_from(encoded_core_bytes).map_err(|_| IndexError::CountOverflow)?;
    if encoded_core_bytes == 0 || encoded_core_bytes > MAX_ENCODED_CORE_RECORD_BYTES {
        return Err(IndexError::InvalidStoredDocumentField(
            CORE_RECORD_ENCODED_BYTES_FIELD,
        ));
    }
    Ok(encoded_core_bytes)
}

/// The fast-field columns behind [`core_document_fast_facts`], opened once per
/// segment so bulk callers do not reopen four columns for every document.
pub struct CoreDocumentFastFactColumns {
    event_id_high: Column<u64>,
    event_id_low: Column<u64>,
    encoded_core_bytes: Column<u64>,
    content_bytes: Column<u64>,
}

impl CoreDocumentFastFactColumns {
    pub fn open(segment: &SegmentReader) -> Result<Self> {
        let fast_fields = segment.fast_fields();
        Ok(Self {
            event_id_high: fast_fields.u64(EVENT_ID_HIGH_FIELD)?,
            event_id_low: fast_fields.u64(EVENT_ID_LOW_FIELD)?,
            encoded_core_bytes: fast_fields.u64(CORE_RECORD_ENCODED_BYTES_FIELD)?,
            content_bytes: fast_fields.u64(CORE_CONTENT_BYTES_FIELD)?,
        })
    }

    pub fn facts(&self, doc: DocId) -> Result<CoreDocumentFastFacts> {
        let high = unique_column_u64(&self.event_id_high, doc, EVENT_ID_HIGH_FIELD)?;
        let low = unique_column_u64(&self.event_id_low, doc, EVENT_ID_LOW_FIELD)?;
        let encoded_core_bytes = checked_encoded_core_bytes(unique_column_u64(
            &self.encoded_core_bytes,
            doc,
            CORE_RECORD_ENCODED_BYTES_FIELD,
        )?)?;
        let content_bytes = unique_column_u64(&self.content_bytes, doc, CORE_CONTENT_BYTES_FIELD)?;
        let content_bytes =
            usize::try_from(content_bytes).map_err(|_| IndexError::CountOverflow)?;
        Ok(CoreDocumentFastFacts {
            event_id: Uuid::from_u128((u128::from(high) << 64) | u128::from(low)),
            encoded_core_bytes,
            content_bytes,
        })
    }
}

/// Returns the exact indexed identity and size facts used to admit a stored
/// Core read without loading or decoding the stored document.
pub fn core_document_fast_facts(
    searcher: &tantivy::Searcher,
    address: DocAddress,
) -> Result<CoreDocumentFastFacts> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField(EVENT_ID_HIGH_FIELD))?;
    CoreDocumentFastFactColumns::open(segment)?.facts(address.doc_id)
}

pub fn validate_core_record_encoded_bytes(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    actual_encoded_core_bytes: usize,
) -> Result<()> {
    if core_record_encoded_bytes(searcher, address)? != actual_encoded_core_bytes {
        return Err(IndexError::InvalidStoredDocumentField(
            CORE_RECORD_ENCODED_BYTES_FIELD,
        ));
    }
    Ok(())
}

pub fn unique_required_bytes<'a>(
    document: &'a TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<&'a [u8]> {
    let mut values = document.get_all(field);
    let value = values
        .next()
        .and_then(|value| value.as_bytes())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

pub fn decode_core_document(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    document: &TantivyDocument,
    fields: Fields,
) -> Result<(CoreRecord, usize)> {
    let encoded_core_record = validated_core_record_bytes(searcher, address, document, fields)?;
    let stored_core_bytes = encoded_core_record.len();
    let core_record = decode_validated_core_record_bytes(searcher, address, encoded_core_record)?;
    Ok((core_record, stored_core_bytes))
}

pub fn decode_owned_core_document(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    document: TantivyDocument,
    fields: Fields,
) -> Result<(CoreRecord, usize, AcceptedCoreDocument)> {
    let encoded_core_record = validated_core_record_bytes(searcher, address, &document, fields)?;
    let stored_core_bytes = encoded_core_record.len();
    let core_record = decode_validated_core_record_bytes(searcher, address, encoded_core_record)?;
    Ok((
        core_record,
        stored_core_bytes,
        AcceptedCoreDocument {
            document,
            core_record_field: fields.core_record,
        },
    ))
}

pub fn validated_core_record_bytes<'a>(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    document: &'a TantivyDocument,
    fields: Fields,
) -> Result<&'a [u8]> {
    let encoded_core_record = unique_required_bytes(document, fields.core_record, "core_record")?;
    validate_core_record_encoded_bytes(searcher, address, encoded_core_record.len())?;
    Ok(encoded_core_record)
}

pub fn decode_core_record_bytes(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    encoded_core_record: &[u8],
) -> Result<CoreRecord> {
    validate_core_record_encoded_bytes(searcher, address, encoded_core_record.len())?;
    decode_validated_core_record_bytes(searcher, address, encoded_core_record)
}

pub fn decode_validated_core_record_bytes(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    encoded_core_record: &[u8],
) -> Result<CoreRecord> {
    let core_record = CoreRecord::decode_stored(encoded_core_record)?;
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
    if fast_uuid(
        segment,
        address.doc_id,
        EVENT_ID_HIGH_FIELD,
        EVENT_ID_LOW_FIELD,
    )? != core_record.event_id.as_uuid()
        || fast_uuid(
            segment,
            address.doc_id,
            SESSION_ID_HIGH_FIELD,
            SESSION_ID_LOW_FIELD,
        )? != core_record.session_id.as_uuid()
        || fast_string(segment, address.doc_id, EVENT_IDENTITY_DIGEST_FIELD)?
            != ctx_history_index_generation::hex(&core_record.event_id.digest())
        || fast_string(segment, address.doc_id, SOURCE_KEY_FIELD)?
            != source_token(&core_record.source)
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(core_record)
}
