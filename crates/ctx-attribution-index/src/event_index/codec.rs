use super::*;

pub struct SparseLineageRead {
    pub tables: EventLineageTables,
    #[cfg(test)]
    pub work: SparseLineageOpenWork,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SparseLineageOpenWork {
    pub session_rows: usize,
    pub copied_origin_rows: usize,
    pub copied_event_ref_reads: usize,
    pub ordinary_event_ref_reads: usize,
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, EventIndexError> {
    Ok(serde_json::to_vec(value)?)
}

pub fn validate_frame_length(length: usize) -> Result<(), EventIndexError> {
    if length == 0 || length > MAX_SOURCE_FRAME_BYTES {
        Err(EventIndexError::Bound("source frame bytes"))
    } else {
        Ok(())
    }
}

pub fn add_frame_bytes(
    current: u64,
    frame_length: usize,
    bound: &'static str,
) -> Result<u64, EventIndexError> {
    current
        .checked_add(4)
        .and_then(|bytes| {
            u64::try_from(frame_length)
                .ok()
                .and_then(|length| bytes.checked_add(length))
        })
        .ok_or(EventIndexError::Bound(bound))
}

pub fn fixed_section_bytes(
    count: usize,
    item_bytes: usize,
    bound: &'static str,
) -> Result<u64, EventIndexError> {
    u64::try_from(count)
        .ok()
        .and_then(|count| {
            u64::try_from(item_bytes)
                .ok()
                .and_then(|item_bytes| count.checked_mul(item_bytes))
        })
        .ok_or(EventIndexError::Bound(bound))
}

pub fn fixed_entry_offset(
    index: usize,
    item_bytes: usize,
    bound: &'static str,
) -> Result<u64, EventIndexError> {
    fixed_section_bytes(index, item_bytes, bound)
}

pub fn write_source_frame(
    writer: &mut SegmentWriter,
    source: &EventIndexSource,
) -> Result<(), EventIndexError> {
    let encoded = canonical_json(source)?;
    validate_frame_length(encoded.len())?;
    write_u32(
        writer,
        u32::try_from(encoded.len()).map_err(|_| EventIndexError::Bound("source frame bytes"))?,
    )?;
    writer.write_all(encoded.as_slice())?;
    Ok(())
}

pub fn write_record(
    writer: &mut SegmentWriter,
    record: &CompactIndexedCoreEventState,
    source_ordinal: u32,
    session_ref: u32,
) -> Result<(), EventIndexError> {
    writer.write_all(&source_ordinal.to_le_bytes())?;
    writer.write_all(&record.event_id.digest())?;
    writer.write_all(&record.event_sequence.to_le_bytes())?;
    writer.write_all(record.core_record_sha256.as_bytes())?;
    writer.write_all(record.core_record_leaf_sha256.as_bytes())?;
    writer.write_all(&record.flat_record_count.to_le_bytes())?;
    writer.write_all(record.event_output_root.as_bytes())?;
    writer.write_all(&[pack_coverage(&record.coverage)?])?;
    writer.write_all(&session_ref.to_le_bytes())?;
    Ok(())
}

pub fn write_tombstone(
    writer: &mut SegmentWriter,
    tombstone: &IndexedCoreEventTombstone,
    source_ordinal: u32,
) -> Result<(), EventIndexError> {
    writer.write_all(&source_ordinal.to_le_bytes())?;
    writer.write_all(&tombstone.event_id.digest())?;
    writer.write_all(tombstone.prior_event_state_sha256.as_bytes())?;
    Ok(())
}

pub fn read_sources(
    file: &mut SegmentFile,
    layout: &Layout,
    expected_count: u32,
) -> Result<Vec<EventIndexSource>, EventIndexError> {
    let expected_count =
        usize::try_from(expected_count).map_err(|_| EventIndexError::Corrupt("source count"))?;
    let mut sources = Vec::with_capacity(expected_count);
    let mut cursor = 0_u64;
    for _ in 0..expected_count {
        let length_end = cursor
            .checked_add(4)
            .ok_or(EventIndexError::Corrupt("source frame offset"))?;
        if length_end > layout.sources_bytes {
            return Err(EventIndexError::Corrupt("source frame length range"));
        }
        let length_offset = layout
            .sources_offset
            .checked_add(cursor)
            .ok_or(EventIndexError::Corrupt("source frame offset"))?;
        let encoded_length = file.read_range(length_offset, 4)?;
        let length = usize::try_from(read_u32(encoded_length.as_slice(), 0)?)
            .map_err(|_| EventIndexError::Corrupt("source frame length"))?;
        if length == 0 || length > MAX_SOURCE_FRAME_BYTES {
            return Err(EventIndexError::Corrupt("source frame length bound"));
        }
        cursor = length_end;
        let payload_end = cursor
            .checked_add(
                u64::try_from(length)
                    .map_err(|_| EventIndexError::Corrupt("source frame length"))?,
            )
            .ok_or(EventIndexError::Corrupt("source frame range"))?;
        if payload_end > layout.sources_bytes {
            return Err(EventIndexError::Corrupt("source frame range"));
        }
        let payload_offset = layout
            .sources_offset
            .checked_add(cursor)
            .ok_or(EventIndexError::Corrupt("source payload offset"))?;
        let encoded = file.read_range(payload_offset, length)?;
        let source = serde_json::from_slice::<EventIndexSource>(encoded.as_slice())
            .map_err(|_| EventIndexError::Corrupt("source encoding"))?;
        source
            .validate()
            .map_err(|_| EventIndexError::Corrupt("source identity"))?;
        if canonical_json(&source)?.as_slice() != encoded.as_slice() {
            return Err(EventIndexError::Corrupt("non-canonical source"));
        }
        if sources
            .last()
            .is_some_and(|prior: &EventIndexSource| prior.storage_key >= source.storage_key)
        {
            return Err(EventIndexError::Corrupt("source ordering or duplicate"));
        }
        sources.push(source);
        cursor = payload_end;
    }
    if cursor != layout.sources_bytes {
        return Err(EventIndexError::Corrupt("source section length"));
    }
    Ok(sources)
}

pub fn read_record_at(
    file: &mut SegmentFile,
    layout: &Layout,
    sources: &[EventIndexSource],
    lineage_tables: &EventLineageTables,
    offset: u64,
) -> Result<IndexedCoreEventState, EventIndexError> {
    validate_fixed_offset(offset, RECORD_BYTES, layout.records_bytes, "record offset")?;
    let absolute = layout
        .records_offset
        .checked_add(offset)
        .ok_or(EventIndexError::Corrupt("record offset"))?;
    let encoded = file.read_range(absolute, RECORD_BYTES)?;
    let source_ordinal = read_u32(encoded.as_slice(), 0)?;
    let source = source_by_ordinal(sources, source_ordinal)?;
    let event_digest = read_array::<EVENT_DIGEST_BYTES>(encoded.as_slice(), 4)?;
    let event_sequence = read_u64(encoded.as_slice(), 4 + EVENT_DIGEST_BYTES)?;
    let digest_offset = 4 + EVENT_DIGEST_BYTES + 8;
    let core_record_sha256 = read_digest(encoded.as_slice(), digest_offset, "Core record digest")?;
    let core_leaf_offset = digest_offset + CORE_DIGEST_BYTES;
    let core_record_leaf_sha256 = read_digest(
        encoded.as_slice(),
        core_leaf_offset,
        "Core record leaf digest",
    )?;
    let flat_count_offset = core_leaf_offset + CORE_DIGEST_BYTES;
    let flat_record_count = read_u32(encoded.as_slice(), flat_count_offset)?;
    let output_root_offset = flat_count_offset + 4;
    let event_output_root =
        read_digest(encoded.as_slice(), output_root_offset, "event output root")?;
    let coverage_offset = output_root_offset + CORE_DIGEST_BYTES;
    let packed_coverage = *encoded
        .get(coverage_offset)
        .ok_or(EventIndexError::Corrupt("packed per-event coverage range"))?;
    let coverage = unpack_coverage(packed_coverage)?;
    let session_ref = read_u32(encoded.as_slice(), coverage_offset + 1)?;
    let event_id = event_identity(source, event_digest)?;
    let record_ordinal = usize::try_from(
        offset
            .checked_div(RECORD_BYTES as u64)
            .ok_or(EventIndexError::Corrupt("record lineage ordinal"))?,
    )
    .map_err(|_| EventIndexError::Corrupt("record lineage ordinal"))?;
    let lineage = lineage_tables.resolve(session_ref, record_ordinal)?;
    let record = IndexedCoreEventState {
        source_storage_key: source.storage_key.clone(),
        event_id,
        lineage,
        event_sequence,
        core_record_sha256,
        core_record_leaf_sha256,
        flat_record_count,
        event_output_root,
        coverage,
    };
    validate_record(&record, sources).map_err(|_| EventIndexError::Corrupt("record model"))?;
    Ok(record)
}

pub fn read_tombstone_at(
    file: &mut SegmentFile,
    layout: &Layout,
    sources: &[EventIndexSource],
    offset: u64,
) -> Result<IndexedCoreEventTombstone, EventIndexError> {
    validate_fixed_offset(
        offset,
        TOMBSTONE_BYTES,
        layout.tombstones_bytes,
        "tombstone offset",
    )?;
    let absolute = layout
        .tombstones_offset
        .checked_add(offset)
        .ok_or(EventIndexError::Corrupt("tombstone offset"))?;
    let encoded = file.read_range(absolute, TOMBSTONE_BYTES)?;
    let source_ordinal = read_u32(encoded.as_slice(), 0)?;
    let source = source_by_ordinal(sources, source_ordinal)?;
    let event_digest = read_array::<EVENT_DIGEST_BYTES>(encoded.as_slice(), 4)?;
    let prior_event_state_sha256 = read_digest(
        encoded.as_slice(),
        4 + EVENT_DIGEST_BYTES,
        "prior event state digest",
    )?;
    let tombstone = IndexedCoreEventTombstone {
        source_storage_key: source.storage_key.clone(),
        event_id: event_identity(source, event_digest)?,
        prior_event_state_sha256,
    };
    validate_tombstone(&tombstone, sources)
        .map_err(|_| EventIndexError::Corrupt("tombstone model"))?;
    Ok(tombstone)
}

pub fn read_sparse_lineage_tables(
    file: &mut SegmentFile,
    layout: &Layout,
    record_count: usize,
) -> Result<SparseLineageRead, EventIndexError> {
    let mut sessions = Vec::with_capacity(layout.session_count);
    let mut prior_session = None;
    for ordinal in 0..layout.session_count {
        let offset = fixed_entry_offset(
            ordinal,
            SESSION_DICTIONARY_ROW_BYTES,
            "session dictionary offset",
        )?;
        let absolute = layout
            .session_dictionary_offset
            .checked_add(offset)
            .ok_or(EventIndexError::Corrupt("session dictionary offset"))?;
        let encoded = file.read_range(absolute, SESSION_DICTIONARY_ROW_BYTES)?;
        let session = decode_session_lineage(encoded.as_slice())?;
        let key = session
            .session_id
            .encode_canonical()
            .map_err(|_| EventIndexError::Corrupt("session dictionary identity"))?;
        if prior_session.as_ref().is_some_and(|prior| prior >= &key) {
            return Err(EventIndexError::Corrupt(
                "session dictionary ordering or duplicate",
            ));
        }
        prior_session = Some(key);
        sessions.push(session);
    }

    let mut copied_origins = Vec::with_capacity(layout.copied_origin_count);
    let mut prior_event_ordinal = None;
    for ordinal in 0..layout.copied_origin_count {
        let offset = fixed_entry_offset(ordinal, COPIED_ORIGIN_ROW_BYTES, "copied origin offset")?;
        let absolute = layout
            .copied_origin_offset
            .checked_add(offset)
            .ok_or(EventIndexError::Corrupt("copied origin offset"))?;
        let encoded = file.read_range(absolute, COPIED_ORIGIN_ROW_BYTES)?;
        let row = decode_copied_origin(encoded.as_slice())?;
        let event_ordinal = usize::try_from(row.event_ordinal)
            .map_err(|_| EventIndexError::Corrupt("copied event ordinal"))?;
        if event_ordinal >= record_count
            || prior_event_ordinal.is_some_and(|prior| prior >= row.event_ordinal)
        {
            return Err(EventIndexError::Corrupt(
                "copied origin ordering or reference",
            ));
        }
        let record_offset = fixed_entry_offset(event_ordinal, RECORD_BYTES, "record offset")?;
        let session_ref_offset = layout
            .records_offset
            .checked_add(record_offset)
            .and_then(|offset| offset.checked_add((RECORD_BYTES - 4) as u64))
            .ok_or(EventIndexError::Corrupt("copied session reference offset"))?;
        let encoded_ref = file.read_range(session_ref_offset, 4)?;
        let session_ref = read_u32(encoded_ref.as_slice(), 0)?;
        let session_ordinal = session_ordinal(session_ref)?;
        if decode_event_origin_kind(session_ref)? != IndexedCoreEventOriginKind::CopiedFromAncestor
            || session_ordinal >= layout.session_count
        {
            return Err(EventIndexError::Corrupt("copied origin marker"));
        }
        prior_event_ordinal = Some(row.event_ordinal);
        copied_origins.push(row);
    }
    Ok(SparseLineageRead {
        tables: EventLineageTables {
            sessions,
            copied_origins,
        },
        #[cfg(test)]
        work: SparseLineageOpenWork {
            session_rows: layout.session_count,
            copied_origin_rows: layout.copied_origin_count,
            copied_event_ref_reads: layout.copied_origin_count,
            ordinary_event_ref_reads: 0,
        },
    })
}

fn read_digest(
    encoded: &[u8],
    offset: usize,
    label: &'static str,
) -> Result<String, EventIndexError> {
    let end = offset
        .checked_add(CORE_DIGEST_BYTES)
        .ok_or(EventIndexError::Corrupt(label))?;
    let digest = std::str::from_utf8(
        encoded
            .get(offset..end)
            .ok_or(EventIndexError::Corrupt(label))?,
    )
    .map_err(|_| EventIndexError::Corrupt(label))?
    .to_owned();
    validate_core_digest(&digest).map_err(|_| EventIndexError::Corrupt(label))?;
    Ok(digest)
}

pub fn find_record(
    file: &mut SegmentFile,
    layout: &Layout,
    sources: &[EventIndexSource],
    lineage: &EventLineageTables,
    count: usize,
    target: (u32, [u8; EVENT_DIGEST_BYTES]),
) -> Result<Option<IndexedCoreEventState>, EventIndexError> {
    let Some(index) = find_fixed_key(file, layout, count, target, true)? else {
        return Ok(None);
    };
    let offset = fixed_entry_offset(index, RECORD_BYTES, "record offset")?;
    read_record_at(file, layout, sources, lineage, offset).map(Some)
}

pub fn find_tombstone(
    file: &mut SegmentFile,
    layout: &Layout,
    sources: &[EventIndexSource],
    count: usize,
    target: (u32, [u8; EVENT_DIGEST_BYTES]),
) -> Result<Option<IndexedCoreEventTombstone>, EventIndexError> {
    let Some(index) = find_fixed_key(file, layout, count, target, false)? else {
        return Ok(None);
    };
    let offset = fixed_entry_offset(index, TOMBSTONE_BYTES, "tombstone offset")?;
    read_tombstone_at(file, layout, sources, offset).map(Some)
}

pub fn lower_bound_records(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    source_ordinal: u32,
    after: Option<[u8; EVENT_DIGEST_BYTES]>,
) -> Result<usize, EventIndexError> {
    lower_bound_fixed(file, layout, count, source_ordinal, after, true)
}

pub fn lower_bound_tombstones(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    source_ordinal: u32,
    after: Option<[u8; EVENT_DIGEST_BYTES]>,
) -> Result<usize, EventIndexError> {
    lower_bound_fixed(file, layout, count, source_ordinal, after, false)
}

pub fn record_key_at(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    index: usize,
) -> Result<Option<(u32, [u8; EVENT_DIGEST_BYTES])>, EventIndexError> {
    checked_fixed_key(file, layout, count, index, true)
}

pub fn tombstone_key_at(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    index: usize,
) -> Result<Option<(u32, [u8; EVENT_DIGEST_BYTES])>, EventIndexError> {
    checked_fixed_key(file, layout, count, index, false)
}

fn find_fixed_key(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    target: (u32, [u8; EVENT_DIGEST_BYTES]),
    records: bool,
) -> Result<Option<usize>, EventIndexError> {
    let mut low = 0_usize;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        let key = checked_fixed_key(file, layout, count, middle, records)?
            .ok_or(EventIndexError::Corrupt("fixed-row binary search range"))?;
        match key.cmp(&target) {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Ok(Some(middle)),
        }
    }
    Ok(None)
}

fn lower_bound_fixed(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    source_ordinal: u32,
    after: Option<[u8; EVENT_DIGEST_BYTES]>,
    records: bool,
) -> Result<usize, EventIndexError> {
    let target = (source_ordinal, after.unwrap_or([0_u8; EVENT_DIGEST_BYTES]));
    let mut low = 0_usize;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        let key = checked_fixed_key(file, layout, count, middle, records)?
            .ok_or(EventIndexError::Corrupt("fixed-row lower-bound range"))?;
        let before = key < target || (after.is_some() && key == target);
        if before {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    Ok(low)
}

fn checked_fixed_key(
    file: &mut SegmentFile,
    layout: &Layout,
    count: usize,
    index: usize,
    records: bool,
) -> Result<Option<(u32, [u8; EVENT_DIGEST_BYTES])>, EventIndexError> {
    if index >= count {
        return Ok(None);
    }
    let current = raw_fixed_key(file, layout, index, records)?;
    if index > 0 && raw_fixed_key(file, layout, index - 1, records)? >= current {
        return Err(EventIndexError::Corrupt("fixed-row ordering or duplicate"));
    }
    if index + 1 < count && current >= raw_fixed_key(file, layout, index + 1, records)? {
        return Err(EventIndexError::Corrupt("fixed-row ordering or duplicate"));
    }
    Ok(Some(current))
}

fn raw_fixed_key(
    file: &mut SegmentFile,
    layout: &Layout,
    index: usize,
    records: bool,
) -> Result<(u32, [u8; EVENT_DIGEST_BYTES]), EventIndexError> {
    let item_bytes = if records {
        RECORD_BYTES
    } else {
        TOMBSTONE_BYTES
    };
    let section_offset = if records {
        layout.records_offset
    } else {
        layout.tombstones_offset
    };
    let offset = fixed_entry_offset(index, item_bytes, "fixed-row key offset")?;
    let absolute = section_offset
        .checked_add(offset)
        .ok_or(EventIndexError::Corrupt("fixed-row key offset"))?;
    let encoded = file.read_range(absolute, 4 + EVENT_DIGEST_BYTES)?;
    Ok((
        read_u32(encoded.as_slice(), 0)?,
        read_array::<EVENT_DIGEST_BYTES>(encoded.as_slice(), 4)?,
    ))
}

pub fn source_by_ordinal(
    sources: &[EventIndexSource],
    source_ordinal: u32,
) -> Result<&EventIndexSource, EventIndexError> {
    sources
        .get(
            usize::try_from(source_ordinal)
                .map_err(|_| EventIndexError::Corrupt("source ordinal"))?,
        )
        .ok_or(EventIndexError::Corrupt("source ordinal"))
}

pub fn event_identity(
    source: &EventIndexSource,
    digest: [u8; EVENT_DIGEST_BYTES],
) -> Result<StableEntityId, EventIndexError> {
    owned_identity(source, StableEntityKind::Event, digest)
}

pub fn owned_identity(
    source: &EventIndexSource,
    kind: StableEntityKind,
    digest: [u8; EVENT_DIGEST_BYTES],
) -> Result<StableEntityId, EventIndexError> {
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let identity = serde_json::from_value::<StableEntityId>(serde_json::json!({
        "contract_version": 1,
        "entity_kind": kind,
        "digest": digest,
        "source_digest": source.source.identity().digest(),
        "source_descriptor_digest": source.source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))
    .map_err(|_| EventIndexError::Corrupt("owned identity encoding"))?;
    match kind {
        StableEntityKind::Event => validate_event_identity(identity, source),
        StableEntityKind::Session => validate_owned_session_identity(identity, source),
        StableEntityKind::Source => Err(EventIndexError::Invalid("owned identity kind")),
    }
    .map_err(|_| EventIndexError::Corrupt("owned identity"))?;
    Ok(identity)
}

fn validate_fixed_offset(
    offset: u64,
    item_bytes: usize,
    section_bytes: u64,
    label: &'static str,
) -> Result<(), EventIndexError> {
    let item_bytes = u64::try_from(item_bytes).map_err(|_| EventIndexError::Corrupt(label))?;
    let end = offset
        .checked_add(item_bytes)
        .ok_or(EventIndexError::Corrupt(label))?;
    if !offset.is_multiple_of(item_bytes) || end > section_bytes {
        return Err(EventIndexError::Corrupt(label));
    }
    Ok(())
}

pub fn validate_page_limit(limit: usize) -> Result<(), EventIndexError> {
    if limit == 0 || limit > MAX_EVENT_INDEX_PAGE_ITEMS {
        Err(EventIndexError::Bound("page item count"))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub source_count: u32,
    pub record_count: u32,
    pub tombstone_count: u32,
    pub index_count: u32,
    pub session_count: u32,
    pub copied_origin_count: u32,
    pub sources_offset: u64,
    pub sources_bytes: u64,
    pub records_offset: u64,
    pub records_bytes: u64,
    pub tombstones_offset: u64,
    pub tombstones_bytes: u64,
    pub session_dictionary_offset: u64,
    pub session_dictionary_bytes: u64,
    pub copied_origin_offset: u64,
    pub copied_origin_bytes: u64,
    pub end_offset: u64,
}

impl Header {
    #[allow(clippy::too_many_arguments)]
    pub fn for_sections(
        source_count: usize,
        record_count: usize,
        tombstone_count: usize,
        sources_bytes: u64,
        records_bytes: u64,
        tombstones_bytes: u64,
        session_count: usize,
        session_dictionary_bytes: u64,
        copied_origin_count: usize,
        copied_origin_bytes: u64,
    ) -> Result<Self, EventIndexError> {
        let index_count = record_count
            .checked_add(tombstone_count)
            .ok_or(EventIndexError::Bound("index count"))?;
        let sources_offset =
            u64::try_from(HEADER_BYTES).map_err(|_| EventIndexError::Bound("header bytes"))?;
        let records_offset = sources_offset
            .checked_add(sources_bytes)
            .ok_or(EventIndexError::Bound("record offset"))?;
        let tombstones_offset = records_offset
            .checked_add(records_bytes)
            .ok_or(EventIndexError::Bound("tombstone offset"))?;
        let session_dictionary_offset = tombstones_offset
            .checked_add(tombstones_bytes)
            .ok_or(EventIndexError::Bound("session dictionary offset"))?;
        let copied_origin_offset = session_dictionary_offset
            .checked_add(session_dictionary_bytes)
            .ok_or(EventIndexError::Bound("copied origin offset"))?;
        let end_offset = copied_origin_offset
            .checked_add(copied_origin_bytes)
            .ok_or(EventIndexError::Bound("event index end offset"))?;
        Ok(Self {
            source_count: u32::try_from(source_count)
                .map_err(|_| EventIndexError::Bound("source count"))?,
            record_count: u32::try_from(record_count)
                .map_err(|_| EventIndexError::Bound("record count"))?,
            tombstone_count: u32::try_from(tombstone_count)
                .map_err(|_| EventIndexError::Bound("tombstone count"))?,
            index_count: u32::try_from(index_count)
                .map_err(|_| EventIndexError::Bound("index count"))?,
            session_count: u32::try_from(session_count)
                .map_err(|_| EventIndexError::Bound("session count"))?,
            copied_origin_count: u32::try_from(copied_origin_count)
                .map_err(|_| EventIndexError::Bound("copied origin count"))?,
            sources_offset,
            sources_bytes,
            records_offset,
            records_bytes,
            tombstones_offset,
            tombstones_bytes,
            session_dictionary_offset,
            session_dictionary_bytes,
            copied_origin_offset,
            copied_origin_bytes,
            end_offset,
        })
    }

    pub fn encode(self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0_u8; HEADER_BYTES];
        bytes[..8].copy_from_slice(&FORMAT_MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&128_u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.source_count.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.record_count.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.tombstone_count.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.index_count.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.session_count.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.copied_origin_count.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.sources_offset.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.sources_bytes.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.records_offset.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.records_bytes.to_le_bytes());
        bytes[72..80].copy_from_slice(&self.tombstones_offset.to_le_bytes());
        bytes[80..88].copy_from_slice(&self.tombstones_bytes.to_le_bytes());
        bytes[88..96].copy_from_slice(&self.session_dictionary_offset.to_le_bytes());
        bytes[96..104].copy_from_slice(&self.session_dictionary_bytes.to_le_bytes());
        bytes[104..112].copy_from_slice(&self.copied_origin_offset.to_le_bytes());
        bytes[112..120].copy_from_slice(&self.copied_origin_bytes.to_le_bytes());
        bytes[120..128].copy_from_slice(&self.end_offset.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EventIndexError> {
        if bytes.len() != HEADER_BYTES
            || bytes.get(..8) != Some(FORMAT_MAGIC.as_slice())
            || read_u16(bytes, 8)? != FORMAT_VERSION
            || usize::from(read_u16(bytes, 10)?) != HEADER_BYTES
            || bytes.get(36..40) != Some([0_u8; 4].as_slice())
        {
            return Err(EventIndexError::Corrupt("header"));
        }
        Ok(Self {
            source_count: read_u32(bytes, 12)?,
            record_count: read_u32(bytes, 16)?,
            tombstone_count: read_u32(bytes, 20)?,
            index_count: read_u32(bytes, 24)?,
            session_count: read_u32(bytes, 28)?,
            copied_origin_count: read_u32(bytes, 32)?,
            sources_offset: read_u64(bytes, 40)?,
            sources_bytes: read_u64(bytes, 48)?,
            records_offset: read_u64(bytes, 56)?,
            records_bytes: read_u64(bytes, 64)?,
            tombstones_offset: read_u64(bytes, 72)?,
            tombstones_bytes: read_u64(bytes, 80)?,
            session_dictionary_offset: read_u64(bytes, 88)?,
            session_dictionary_bytes: read_u64(bytes, 96)?,
            copied_origin_offset: read_u64(bytes, 104)?,
            copied_origin_bytes: read_u64(bytes, 112)?,
            end_offset: read_u64(bytes, 120)?,
        })
    }

    pub fn validate_bounds(self) -> Result<(), EventIndexError> {
        let source_count = usize::try_from(self.source_count)
            .map_err(|_| EventIndexError::Corrupt("source count"))?;
        let record_count = usize::try_from(self.record_count)
            .map_err(|_| EventIndexError::Corrupt("record count"))?;
        let tombstone_count = usize::try_from(self.tombstone_count)
            .map_err(|_| EventIndexError::Corrupt("tombstone count"))?;
        let index_count = usize::try_from(self.index_count)
            .map_err(|_| EventIndexError::Corrupt("index count"))?;
        let session_count = usize::try_from(self.session_count)
            .map_err(|_| EventIndexError::Corrupt("session count"))?;
        let copied_origin_count = usize::try_from(self.copied_origin_count)
            .map_err(|_| EventIndexError::Corrupt("copied origin count"))?;
        if source_count > MAX_EVENT_INDEX_SOURCES
            || record_count
                .checked_add(tombstone_count)
                .is_none_or(|count| count > MAX_EVENT_INDEX_ENTRIES || count != index_count)
            || self.sources_bytes > MAX_SOURCE_SECTION_BYTES
            || self.records_bytes > MAX_RECORD_SECTION_BYTES
            || self.tombstones_bytes > MAX_TOMBSTONE_SECTION_BYTES
            || session_count > record_count
            || copied_origin_count > record_count
            || self.session_dictionary_bytes > MAX_SESSION_DICTIONARY_SECTION_BYTES
            || self.copied_origin_bytes > MAX_COPIED_ORIGIN_SECTION_BYTES
        {
            return Err(EventIndexError::Corrupt("header bounds or counts"));
        }
        let expected_record_bytes =
            fixed_section_bytes(record_count, RECORD_BYTES, "record section accounting")
                .map_err(|_| EventIndexError::Corrupt("record section accounting"))?;
        let expected_tombstone_bytes = fixed_section_bytes(
            tombstone_count,
            TOMBSTONE_BYTES,
            "tombstone section accounting",
        )
        .map_err(|_| EventIndexError::Corrupt("tombstone section accounting"))?;
        let expected_session_dictionary_bytes = fixed_section_bytes(
            session_count,
            SESSION_DICTIONARY_ROW_BYTES,
            "session dictionary section accounting",
        )
        .map_err(|_| EventIndexError::Corrupt("session dictionary section accounting"))?;
        let expected_copied_origin_bytes = fixed_section_bytes(
            copied_origin_count,
            COPIED_ORIGIN_ROW_BYTES,
            "copied origin section accounting",
        )
        .map_err(|_| EventIndexError::Corrupt("copied origin section accounting"))?;
        if self.records_bytes != expected_record_bytes
            || self.tombstones_bytes != expected_tombstone_bytes
            || self.session_dictionary_bytes != expected_session_dictionary_bytes
            || self.copied_origin_bytes != expected_copied_origin_bytes
            || (record_count == 0 && (session_count != 0 || copied_origin_count != 0))
            || (record_count != 0 && session_count == 0)
            || (source_count == 0 && self.sources_bytes != 0)
            || (source_count != 0 && self.sources_bytes == 0)
        {
            return Err(EventIndexError::Corrupt("header section accounting"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub sources_offset: u64,
    pub sources_bytes: u64,
    pub records_offset: u64,
    pub records_bytes: u64,
    pub tombstones_offset: u64,
    pub tombstones_bytes: u64,
    pub session_dictionary_offset: u64,
    #[allow(dead_code)]
    pub session_dictionary_bytes: u64,
    pub session_count: usize,
    pub copied_origin_offset: u64,
    #[allow(dead_code)]
    pub copied_origin_bytes: u64,
    pub copied_origin_count: usize,
}

impl Layout {
    pub fn new(header: &Header, plaintext_bytes: u64) -> Result<Self, EventIndexError> {
        let expected_sources_offset =
            u64::try_from(HEADER_BYTES).map_err(|_| EventIndexError::Corrupt("header length"))?;
        let expected_records_offset = header
            .sources_offset
            .checked_add(header.sources_bytes)
            .ok_or(EventIndexError::Corrupt("source section range"))?;
        let expected_tombstones_offset = header
            .records_offset
            .checked_add(header.records_bytes)
            .ok_or(EventIndexError::Corrupt("record section range"))?;
        let expected_session_dictionary_offset = header
            .tombstones_offset
            .checked_add(header.tombstones_bytes)
            .ok_or(EventIndexError::Corrupt("tombstone section range"))?;
        let expected_copied_origin_offset = header
            .session_dictionary_offset
            .checked_add(header.session_dictionary_bytes)
            .ok_or(EventIndexError::Corrupt("session dictionary section range"))?;
        let expected_end_offset = header
            .copied_origin_offset
            .checked_add(header.copied_origin_bytes)
            .ok_or(EventIndexError::Corrupt("copied origin section range"))?;
        if header.sources_offset != expected_sources_offset
            || header.records_offset != expected_records_offset
            || header.tombstones_offset != expected_tombstones_offset
            || header.session_dictionary_offset != expected_session_dictionary_offset
            || header.copied_origin_offset != expected_copied_origin_offset
            || header.end_offset != expected_end_offset
        {
            return Err(EventIndexError::Corrupt("section offsets"));
        }
        if plaintext_bytes != header.end_offset {
            return Err(EventIndexError::Corrupt("trailing index bytes"));
        }
        Ok(Self {
            sources_offset: header.sources_offset,
            sources_bytes: header.sources_bytes,
            records_offset: header.records_offset,
            records_bytes: header.records_bytes,
            tombstones_offset: header.tombstones_offset,
            tombstones_bytes: header.tombstones_bytes,
            session_dictionary_offset: header.session_dictionary_offset,
            session_dictionary_bytes: header.session_dictionary_bytes,
            session_count: usize::try_from(header.session_count)
                .map_err(|_| EventIndexError::Corrupt("session count"))?,
            copied_origin_offset: header.copied_origin_offset,
            copied_origin_bytes: header.copied_origin_bytes,
            copied_origin_count: usize::try_from(header.copied_origin_count)
                .map_err(|_| EventIndexError::Corrupt("copied origin count"))?,
        })
    }
}

pub fn write_u32(writer: &mut SegmentWriter, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

pub fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, EventIndexError> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

pub fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, EventIndexError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

pub fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, EventIndexError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

pub fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], EventIndexError> {
    let end = offset
        .checked_add(N)
        .ok_or(EventIndexError::Corrupt("integer or array offset"))?;
    bytes
        .get(offset..end)
        .ok_or(EventIndexError::Corrupt("integer or array range"))?
        .try_into()
        .map_err(|_| EventIndexError::Corrupt("integer or array encoding"))
}
