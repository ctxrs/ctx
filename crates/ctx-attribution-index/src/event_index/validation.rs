#[allow(clippy::wildcard_imports)]
use super::*;

pub fn validate_input_counts(
    source_count: usize,
    record_count: usize,
    tombstone_count: usize,
) -> Result<(), EventIndexError> {
    let entry_count = record_count
        .checked_add(tombstone_count)
        .ok_or(EventIndexError::Bound("entry count"))?;
    if source_count > MAX_EVENT_INDEX_SOURCES {
        return Err(EventIndexError::Bound("source count"));
    }
    if entry_count > MAX_EVENT_INDEX_ENTRIES {
        return Err(EventIndexError::Bound("entry count"));
    }
    Ok(())
}

pub fn validate_record(
    record: &IndexedCoreEventState,
    sources: &[EventIndexSource],
) -> Result<(), EventIndexError> {
    validate_source_storage_key(&record.source_storage_key)?;
    let source = find_source(sources, &record.source_storage_key)?;
    validate_event_identity(record.event_id, source)?;
    validate_owned_session_identity(record.lineage.session_id, source)?;
    record.lineage.validate()?;
    validate_core_digest(&record.core_record_sha256)?;
    validate_core_digest(&record.core_record_leaf_sha256)?;
    validate_core_digest(&record.event_output_root)?;
    let _packed = pack_coverage(&record.coverage)?;
    Ok(())
}

pub fn validate_compact_record(
    record: &CompactIndexedCoreEventState,
    sources: &[EventIndexSource],
    lineage: &EventLineageTables,
) -> Result<(u32, usize), EventIndexError> {
    validate_source_storage_key(&record.source_storage_key)?;
    let source_ordinal = source_ordinal(sources, &record.source_storage_key)?;
    let source = sources
        .get(
            usize::try_from(source_ordinal)
                .map_err(|_| EventIndexError::Invalid("source dictionary reference"))?,
        )
        .ok_or(EventIndexError::Invalid("source dictionary reference"))?;
    validate_event_identity(record.event_id, source)?;
    let session_ordinal = session_ordinal(record.session_ref)?;
    if session_ordinal >= lineage.sessions.len() {
        return Err(EventIndexError::Invalid("session dictionary reference"));
    }
    validate_core_digest(&record.core_record_sha256)?;
    validate_core_digest(&record.core_record_leaf_sha256)?;
    validate_core_digest(&record.event_output_root)?;
    let _packed = pack_coverage(&record.coverage)?;
    Ok((source_ordinal, session_ordinal))
}

pub fn validate_owned_session_identity(
    session_id: StableEntityId,
    source: &EventIndexSource,
) -> Result<(), EventIndexError> {
    validate_identity_kind(session_id, StableEntityKind::Session)?;
    if session_id.source_digest() != source.source.identity().digest()
        || session_id.source_descriptor_digest() != source.source.exact_descriptor_digest()
    {
        return Err(EventIndexError::Invalid("session source identity"));
    }
    Ok(())
}

pub fn pack_coverage(coverage: &SegmentCoreCoverage) -> Result<u8, EventIndexError> {
    let counts = [
        coverage.repository_candidate_events,
        coverage.logical_binding_events,
        coverage.certified_live_root_access_events,
        coverage.file_evidence_events,
        coverage.exact_commit_evidence_events,
        coverage.exact_pull_request_evidence_events,
    ];
    if counts.into_iter().any(|count| count > 1) {
        return Err(EventIndexError::Invalid("per-event coverage"));
    }
    Ok(counts
        .into_iter()
        .enumerate()
        .fold(0_u8, |packed, (index, count)| {
            packed | (u8::from(count != 0) << index)
        }))
}

pub fn unpack_coverage(packed: u8) -> Result<SegmentCoreCoverage, EventIndexError> {
    if packed & !PACKED_COVERAGE_MASK != 0 {
        return Err(EventIndexError::Corrupt("packed per-event coverage"));
    }
    let bit = |index: u8| u64::from((packed >> index) & 1);
    Ok(SegmentCoreCoverage {
        repository_candidate_events: bit(0),
        logical_binding_events: bit(1),
        certified_live_root_access_events: bit(2),
        file_evidence_events: bit(3),
        exact_commit_evidence_events: bit(4),
        exact_pull_request_evidence_events: bit(5),
    })
}

pub fn validate_tombstone(
    tombstone: &IndexedCoreEventTombstone,
    sources: &[EventIndexSource],
) -> Result<(), EventIndexError> {
    validate_source_storage_key(&tombstone.source_storage_key)?;
    let source = find_source(sources, &tombstone.source_storage_key)?;
    validate_event_identity(tombstone.event_id, source)?;
    validate_core_digest(&tombstone.prior_event_state_sha256)
}

pub fn validate_event_identity(
    event_id: StableEntityId,
    source: &EventIndexSource,
) -> Result<(), EventIndexError> {
    event_id
        .validate_contract()
        .map_err(|_| EventIndexError::Invalid("event identity"))?;
    if event_id.entity_kind() != StableEntityKind::Event
        || event_id.source_digest() != source.source.identity().digest()
        || event_id.source_descriptor_digest() != source.source.exact_descriptor_digest()
    {
        return Err(EventIndexError::Invalid("event source identity"));
    }
    Ok(())
}

pub fn validate_identity_kind(
    identity: StableEntityId,
    kind: StableEntityKind,
) -> Result<(), EventIndexError> {
    identity
        .validate_contract()
        .map_err(|_| EventIndexError::Invalid("stable identity"))?;
    if identity.entity_kind() != kind {
        return Err(EventIndexError::Invalid("stable identity kind"));
    }
    Ok(())
}

pub fn validate_core_digest(value: &str) -> Result<(), EventIndexError> {
    if value.len() == CORE_DIGEST_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(EventIndexError::Invalid("Core record SHA-256"))
    }
}

pub fn source_storage_key(source: &SourceKey) -> String {
    format!(
        "{SOURCE_STORAGE_PREFIX}{}",
        hex::encode(source.identity().digest())
    )
}

pub fn validate_source_storage_key(value: &str) -> Result<(), EventIndexError> {
    if value.len() == SOURCE_STORAGE_KEY_BYTES
        && value.starts_with(SOURCE_STORAGE_PREFIX)
        && value[SOURCE_STORAGE_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(EventIndexError::Invalid("source storage key"))
    }
}

fn find_source<'a>(
    sources: &'a [EventIndexSource],
    storage_key: &str,
) -> Result<&'a EventIndexSource, EventIndexError> {
    let ordinal = source_ordinal(sources, storage_key)?;
    sources
        .get(usize::try_from(ordinal).map_err(|_| EventIndexError::Invalid("source ordinal"))?)
        .ok_or(EventIndexError::Invalid("source storage key"))
}

pub fn source_ordinal(
    sources: &[EventIndexSource],
    storage_key: &str,
) -> Result<u32, EventIndexError> {
    let index = sources
        .binary_search_by(|source| source.storage_key.as_str().cmp(storage_key))
        .map_err(|_| EventIndexError::Invalid("unknown source storage key"))?;
    u32::try_from(index).map_err(|_| EventIndexError::Bound("source ordinal"))
}

pub fn compare_compact_records(
    left: &CompactIndexedCoreEventState,
    right: &CompactIndexedCoreEventState,
) -> Ordering {
    left.source_storage_key
        .cmp(&right.source_storage_key)
        .then_with(|| left.event_id.digest().cmp(&right.event_id.digest()))
}

pub fn compare_tombstones(
    left: &IndexedCoreEventTombstone,
    right: &IndexedCoreEventTombstone,
) -> Ordering {
    left.source_storage_key
        .cmp(&right.source_storage_key)
        .then_with(|| left.event_id.digest().cmp(&right.event_id.digest()))
}

pub fn has_duplicate_record_keys(records: &[CompactIndexedCoreEventState]) -> bool {
    records
        .windows(2)
        .any(|pair| compare_compact_records(&pair[0], &pair[1]) != Ordering::Less)
}

pub fn has_duplicate_tombstone_keys(tombstones: &[IndexedCoreEventTombstone]) -> bool {
    tombstones
        .windows(2)
        .any(|pair| compare_tombstones(&pair[0], &pair[1]) != Ordering::Less)
}
