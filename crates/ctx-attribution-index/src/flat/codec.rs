use std::io::Write as _;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::*;

pub fn tracked_read(
    file: &mut SegmentFile,
    read_bytes: &mut u64,
    range_reads: &mut u64,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>, SegmentFileError> {
    *read_bytes = read_bytes.saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
    *range_reads = range_reads.saturating_add(1);
    file.read_range(offset, length)
}

#[allow(clippy::too_many_arguments)]
pub fn read_posting_page(
    file: &mut SegmentFile,
    read_bytes: &mut u64,
    range_reads: &mut u64,
    layout: &Layout,
    posting_offset: u64,
    mut posting_index: usize,
    _key: &[u8],
    indexed_term: &str,
    selector: &QuerySelector<'_>,
    limit: usize,
    query: &mut QueryState,
) -> Result<Option<usize>, FlatSegmentError> {
    let count_end = posting_offset
        .checked_add(4)
        .ok_or(FlatSegmentError::Corrupt("posting offset overflow"))?;
    if count_end > layout.postings_bytes {
        return Err(FlatSegmentError::Corrupt("posting offset"));
    }
    let count_start = layout
        .postings_start
        .checked_add(posting_offset)
        .ok_or(FlatSegmentError::Corrupt("posting offset overflow"))?;
    let count_bytes = tracked_read(file, read_bytes, range_reads, count_start, 4)?;
    let count = usize::try_from(read_u32(count_bytes.as_slice(), 0)?)
        .map_err(|_| FlatSegmentError::Corrupt("posting count"))?;
    if count > MAX_RECORDS {
        return Err(FlatSegmentError::Corrupt("posting count bound"));
    }
    let offsets_bytes = count
        .checked_mul(8)
        .ok_or(FlatSegmentError::Corrupt("posting list length"))?;
    let posting_end = count_end
        .checked_add(
            u64::try_from(offsets_bytes)
                .map_err(|_| FlatSegmentError::Corrupt("posting list length"))?,
        )
        .ok_or(FlatSegmentError::Corrupt("posting list length"))?;
    if posting_end > layout.postings_bytes {
        return Err(FlatSegmentError::Corrupt("posting list range"));
    }
    if posting_index > count {
        return Err(FlatSegmentError::Corrupt(
            "query continuation posting index",
        ));
    }
    let offsets_start = count_start
        .checked_add(4)
        .ok_or(FlatSegmentError::Corrupt("posting list offset"))?;
    while posting_index < count && query.results.len() < limit {
        let chunk_count = (count - posting_index).min(MAX_QUERY_RESULTS);
        let chunk_byte_offset = posting_index
            .checked_mul(8)
            .ok_or(FlatSegmentError::Corrupt("posting record offset"))?;
        let chunk_start = offsets_start
            .checked_add(
                u64::try_from(chunk_byte_offset)
                    .map_err(|_| FlatSegmentError::Corrupt("posting record offset"))?,
            )
            .ok_or(FlatSegmentError::Corrupt("posting record offset"))?;
        let chunk_bytes = chunk_count
            .checked_mul(8)
            .ok_or(FlatSegmentError::Corrupt("posting list length"))?;
        let encoded_offsets =
            tracked_read(file, read_bytes, range_reads, chunk_start, chunk_bytes)?;
        for index in 0..chunk_count {
            if query.results.len() == limit {
                break;
            }
            query.postings_scanned = query
                .postings_scanned
                .checked_add(1)
                .ok_or(FlatSegmentError::Bound("query postings scanned"))?;
            if query.postings_scanned > MAX_QUERY_POSTINGS_SCANNED {
                return Err(FlatSegmentError::Bound("query postings scanned"));
            }
            let byte_offset = index
                .checked_mul(8)
                .ok_or(FlatSegmentError::Corrupt("posting record offset"))?;
            let record_offset = read_u64(encoded_offsets.as_slice(), byte_offset)?;
            let record = read_record(file, read_bytes, range_reads, layout, record_offset)?;
            if selector.selects_indexed_term(&record, indexed_term)? {
                query.consider(record);
            }
            posting_index = posting_index
                .checked_add(1)
                .ok_or(FlatSegmentError::Bound("query postings scanned"))?;
        }
    }
    Ok((posting_index < count).then_some(posting_index))
}

fn read_record(
    file: &mut SegmentFile,
    read_bytes: &mut u64,
    range_reads: &mut u64,
    layout: &Layout,
    record_offset: u64,
) -> Result<ServingRecord, FlatSegmentError> {
    let frame_header_end = record_offset
        .checked_add(4)
        .ok_or(FlatSegmentError::Corrupt("record offset overflow"))?;
    if frame_header_end > layout.records_bytes {
        return Err(FlatSegmentError::Corrupt("record offset"));
    }
    let frame_start = layout
        .records_start
        .checked_add(record_offset)
        .ok_or(FlatSegmentError::Corrupt("record offset overflow"))?;
    let length_bytes = tracked_read(file, read_bytes, range_reads, frame_start, 4)?;
    let length = usize::try_from(read_u32(length_bytes.as_slice(), 0)?)
        .map_err(|_| FlatSegmentError::Corrupt("record length"))?;
    if length == 0 || length > MAX_RECORD_BYTES {
        return Err(FlatSegmentError::Corrupt("record length bound"));
    }
    let frame_end = frame_header_end
        .checked_add(u64::try_from(length).map_err(|_| FlatSegmentError::Corrupt("record length"))?)
        .ok_or(FlatSegmentError::Corrupt("record length overflow"))?;
    if frame_end > layout.records_bytes {
        return Err(FlatSegmentError::Corrupt("record range"));
    }
    let payload_start = frame_start
        .checked_add(4)
        .ok_or(FlatSegmentError::Corrupt("record payload offset"))?;
    let encoded = tracked_read(file, read_bytes, range_reads, payload_start, length)?;
    let record = serde_json::from_slice::<ServingRecord>(encoded.as_slice())
        .map_err(|_| FlatSegmentError::Corrupt("record encoding"))?;
    record
        .validate()
        .map_err(|_| FlatSegmentError::Corrupt("record model"))?;
    let canonical = canonical_json(&record)?;
    if canonical.as_slice() != encoded.as_slice() {
        return Err(FlatSegmentError::Corrupt("non-canonical record"));
    }
    Ok(record)
}

pub fn canonical_record_order(left: &ServingRecord, right: &ServingRecord) -> Ordering {
    left.origin
        .rank()
        .cmp(&right.origin.rank())
        .then_with(|| optional_time_order(left.occurred_at_unix_ms, right.occurred_at_unix_ms))
        .then_with(|| {
            left.event_owner
                .event_sequence
                .cmp(&right.event_owner.event_sequence)
        })
        .then_with(|| left.event_owner.event_id.cmp(&right.event_owner.event_id))
        .then_with(|| left.record_id.cmp(&right.record_id))
        .then_with(|| left.cmp(right))
}

fn optional_time_order(left: Option<i64>, right: Option<i64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, FlatSegmentError> {
    Ok(serde_json::to_vec(value)?)
}

impl FlatSegmentWriter {
    /// Validates exactly the model and canonical payload accepted by the Flat
    /// writer and returns the retained/writer work charged by publication.
    pub fn record_work(record: &ServingRecord) -> Result<FlatRecordWork, FlatSegmentError> {
        record.validate()?;
        let encoded = canonical_json(record)?;
        validate_encoded_size(encoded.len(), MAX_RECORD_BYTES, "record bytes")?;
        let frame_bytes = encoded
            .len()
            .checked_add(4)
            .ok_or(FlatSegmentError::Bound("record frame bytes"))?;
        let index_associations = record
            .index_terms
            .len()
            .checked_mul(2)
            .ok_or(FlatSegmentError::Bound("index association count"))?;
        Ok(FlatRecordWork {
            frame_bytes,
            index_associations,
        })
    }

    /// Conservatively charges one tombstone as its exact encoded key entry
    /// plus a page header. Charging a header per key overcounts every
    /// multi-key page and therefore bounds the eventual tombstone section.
    pub fn tombstone_work_bytes(tombstone: &EventTombstone) -> Result<usize, FlatSegmentError> {
        tombstone.validate()?;
        8_usize
            .checked_add(tombstone.source_id.len())
            .and_then(|bytes| bytes.checked_add(tombstone.event_id.len()))
            .ok_or(FlatSegmentError::Bound("tombstone publication bytes"))
    }
}

pub fn write_canonical_frame<T: Serialize>(
    writer: &mut SegmentWriter,
    value: &T,
    maximum: usize,
) -> Result<(), FlatSegmentError> {
    let encoded = canonical_json(value)?;
    validate_encoded_size(encoded.len(), maximum, "canonical frame bytes")?;
    write_u32(
        writer,
        u32::try_from(encoded.len())
            .map_err(|_| FlatSegmentError::Bound("canonical frame bytes"))?,
    )?;
    writer.write_all(encoded.as_slice())?;
    Ok(())
}

#[cfg(test)]
pub fn add_frame_bytes(
    current: u64,
    encoded_len: usize,
    bound: &'static str,
) -> Result<u64, FlatSegmentError> {
    current
        .checked_add(4)
        .and_then(|bytes| {
            u64::try_from(encoded_len)
                .ok()
                .and_then(|len| bytes.checked_add(len))
        })
        .ok_or(FlatSegmentError::Bound(bound))
}

fn validate_encoded_size(
    length: usize,
    maximum: usize,
    bound: &'static str,
) -> Result<(), FlatSegmentError> {
    if length == 0 || length > maximum {
        Err(FlatSegmentError::Bound(bound))
    } else {
        Ok(())
    }
}

pub fn validate_limit(limit: usize) -> Result<(), FlatSegmentError> {
    if limit == 0 || limit > MAX_QUERY_RESULTS {
        Err(FlatSegmentError::Bound("query result count"))
    } else {
        Ok(())
    }
}

pub fn index_key(
    repository_id: &str,
    fact_family: &FactFamily,
    term: &str,
) -> Result<Vec<u8>, FlatSegmentError> {
    validate_query_term(term, false)?;
    index_key_prefix(repository_id, fact_family, term)
}

pub fn index_key_prefix(
    repository_id: &str,
    fact_family: &FactFamily,
    term_prefix: &str,
) -> Result<Vec<u8>, FlatSegmentError> {
    validate_repository_scope(repository_id)?;
    if fact_family.as_str().is_empty() || fact_family.as_str().len() > MAX_FACT_FAMILY_BYTES {
        return Err(FlatSegmentError::Bound("fact family bytes"));
    }
    validate_query_term(term_prefix, true)?;
    let family_length = u16::try_from(fact_family.as_str().len())
        .map_err(|_| FlatSegmentError::Bound("fact family bytes"))?;
    let repository_scope = repository_scope_digest(repository_id);
    let capacity = 4_usize
        .checked_add(repository_scope.len())
        .and_then(|bytes| bytes.checked_add(fact_family.as_str().len()))
        .and_then(|bytes| bytes.checked_add(term_prefix.len()))
        .ok_or(FlatSegmentError::Bound("FST key bytes"))?;
    let mut key = Vec::with_capacity(capacity);
    key.push(KEY_VERSION);
    key.push(SCOPED_KEY);
    key.extend_from_slice(&repository_scope);
    key.extend_from_slice(&family_length.to_be_bytes());
    key.extend_from_slice(fact_family.as_str().as_bytes());
    key.extend_from_slice(term_prefix.as_bytes());
    Ok(key)
}

pub fn unscoped_index_key(
    fact_family: &FactFamily,
    term: &str,
) -> Result<Vec<u8>, FlatSegmentError> {
    validate_query_term(term, false)?;
    unscoped_index_key_prefix(fact_family, term)
}

pub fn unscoped_index_key_prefix(
    fact_family: &FactFamily,
    term_prefix: &str,
) -> Result<Vec<u8>, FlatSegmentError> {
    if fact_family.as_str().is_empty() || fact_family.as_str().len() > MAX_FACT_FAMILY_BYTES {
        return Err(FlatSegmentError::Bound("fact family bytes"));
    }
    validate_query_term(term_prefix, true)?;
    let family_length = u16::try_from(fact_family.as_str().len())
        .map_err(|_| FlatSegmentError::Bound("fact family bytes"))?;
    let capacity = 4_usize
        .checked_add(fact_family.as_str().len())
        .and_then(|bytes| bytes.checked_add(term_prefix.len()))
        .ok_or(FlatSegmentError::Bound("FST key bytes"))?;
    let mut key = Vec::with_capacity(capacity);
    key.push(KEY_VERSION);
    key.push(UNSCOPED_KEY);
    key.extend_from_slice(&family_length.to_be_bytes());
    key.extend_from_slice(fact_family.as_str().as_bytes());
    key.extend_from_slice(term_prefix.as_bytes());
    Ok(key)
}

pub fn validate_repository_scope(repository_id: &str) -> Result<(), FlatSegmentError> {
    if repository_id.is_empty() || repository_id.len() > MAX_REPOSITORY_ID_BYTES {
        Err(FlatSegmentError::Bound("repository identity bytes"))
    } else {
        Ok(())
    }
}

fn repository_scope_digest(repository_id: &str) -> [u8; REPOSITORY_SCOPE_DIGEST_BYTES] {
    const DOMAIN: &[u8] = b"ctx-pro-flat-repository-scope-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((repository_id.len() as u64).to_be_bytes());
    digest.update(repository_id.as_bytes());
    digest.finalize().into()
}

fn validate_query_term(term: &str, allow_empty: bool) -> Result<(), FlatSegmentError> {
    if (!allow_empty && term.is_empty())
        || term.len() > MAX_INDEX_TERM_BYTES
        || term.chars().any(char::is_control)
    {
        Err(FlatSegmentError::Bound("query term bytes"))
    } else {
        Ok(())
    }
}
