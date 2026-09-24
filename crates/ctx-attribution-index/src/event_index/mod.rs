use std::cmp::Ordering;
use std::io::{self, Write as _};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{MAX_SEGMENT_CORE_EVENTS, SegmentCoreCoverage};
use ctx_attribution_model::{EventCopyProofKind, MAX_CORE_SOURCE_STATES, SessionRelationshipKind};
use ctx_history_core::{SourceAnchor, SourceKey, StableEntityId, StableEntityKind, TypedKey};

use super::segment_file::{SegmentFile, SegmentFileError, SegmentWriter};

mod codec;
mod lineage;
#[cfg(test)]
mod tests;
mod validation;

#[allow(clippy::wildcard_imports)]
use codec::*;
pub use lineage::{
    EventLineageAccumulator, EventLineageTables, IndexedCopiedOriginRow, decode_copied_origin,
    decode_session_lineage, decode_staged_session_ref, encode_copied_origin,
    encode_session_lineage, encode_staged_session_ref,
};
use lineage::{decode_event_origin_kind, session_ordinal};
#[allow(clippy::wildcard_imports)]
use validation::*;

/// Domain-separates event-state index bytes from every other segment role.
pub const EVENT_STATE_INDEX_ROLE: u32 = 0x45_56_49_58;

/// These are input and on-disk format bounds, not suggested batch sizes. They make
/// the writer's in-memory sort finite and keep an opened source dictionary small.
pub const MAX_EVENT_INDEX_SOURCES: usize = MAX_CORE_SOURCE_STATES;
pub const MAX_EVENT_INDEX_PAGE_ITEMS: usize = 256;
pub const MAX_EVENT_INDEX_ENTRIES: usize = MAX_SEGMENT_CORE_EVENTS;

const FORMAT_MAGIC: [u8; 8] = *b"CTXEVI06";
const FORMAT_VERSION: u16 = 6;
const HEADER_BYTES: usize = 128;
const EVENT_DIGEST_BYTES: usize = 32;
const CORE_DIGEST_BYTES: usize = 64;
const PACKED_COVERAGE_BYTES: usize = 1;
const PACKED_COVERAGE_MASK: u8 = 0b01_111111;
const RECORD_BYTES: usize = 4
    + EVENT_DIGEST_BYTES
    + 8
    + CORE_DIGEST_BYTES
    + CORE_DIGEST_BYTES
    + 4
    + CORE_DIGEST_BYTES
    + PACKED_COVERAGE_BYTES
    + 4;
const TOMBSTONE_BYTES: usize = 4 + EVENT_DIGEST_BYTES + CORE_DIGEST_BYTES;
pub const SESSION_DICTIONARY_ROW_BYTES: usize = (StableEntityId::CANONICAL_LEN * 3) + 3;
pub const COPIED_ORIGIN_ROW_BYTES: usize = 4 + (StableEntityId::CANONICAL_LEN * 2) + 1;
const SOURCE_STORAGE_PREFIX: &str = "core_source_";
const SOURCE_STORAGE_KEY_BYTES: usize = SOURCE_STORAGE_PREFIX.len() + 64;
const MAX_SOURCE_FRAME_BYTES: usize = 512 * 1024;
const MAX_SOURCE_SECTION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECORD_SECTION_BYTES: u64 = MAX_EVENT_INDEX_ENTRIES as u64 * RECORD_BYTES as u64;
const MAX_TOMBSTONE_SECTION_BYTES: u64 = MAX_EVENT_INDEX_ENTRIES as u64 * TOMBSTONE_BYTES as u64;
const MAX_SESSION_DICTIONARY_SECTION_BYTES: u64 =
    MAX_EVENT_INDEX_ENTRIES as u64 * SESSION_DICTIONARY_ROW_BYTES as u64;
const MAX_COPIED_ORIGIN_SECTION_BYTES: u64 =
    MAX_EVENT_INDEX_ENTRIES as u64 * COPIED_ORIGIN_ROW_BYTES as u64;
const MAX_SEGMENT_PLAINTEXT_BYTES: u64 = MAX_SOURCE_SECTION_BYTES
    + MAX_RECORD_SECTION_BYTES
    + MAX_TOMBSTONE_SECTION_BYTES
    + MAX_SESSION_DICTIONARY_SECTION_BYTES
    + MAX_COPIED_ORIGIN_SECTION_BYTES
    + HEADER_BYTES as u64;

#[derive(Debug, Error)]
pub enum EventIndexError {
    #[error(transparent)]
    File(#[from] SegmentFileError),
    #[error("event-state index I/O failed")]
    Io(#[from] io::Error),
    #[error("event-state index serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("event-state index bound exceeded: {0}")]
    Bound(&'static str),
    #[error("event-state index input is invalid: {0}")]
    Invalid(&'static str),
    #[error("event-state index contains conflicting event keys")]
    Conflict,
    #[error("event-state index source does not match the requested exact source")]
    WrongSource,
    #[error("event-state index is corrupt: {0}")]
    Corrupt(&'static str),
}

/// Exact Core source identity plus the stable graph storage key used to group it.
///
/// The format stores this once per source. Event rows refer to its sorted ordinal,
/// so provider keys and descriptors are not repeated for every event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventIndexSource {
    pub source: SourceKey,
    pub storage_key: String,
}

impl EventIndexSource {
    pub fn new(source: SourceKey) -> Result<Self, EventIndexError> {
        source
            .validate_contract()
            .map_err(|_| EventIndexError::Invalid("source identity"))?;
        let storage_key = source_storage_key(&source);
        Ok(Self {
            source,
            storage_key,
        })
    }

    pub fn validate(&self) -> Result<(), EventIndexError> {
        self.source
            .validate_contract()
            .map_err(|_| EventIndexError::Invalid("source identity"))?;
        validate_source_storage_key(&self.storage_key)?;
        if self.storage_key != source_storage_key(&self.source) {
            return Err(EventIndexError::Invalid("source storage key"));
        }
        Ok(())
    }

    #[must_use]
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.storage_key == other.storage_key && self.source.exact_descriptor_eq(&other.source)
    }
}

impl PartialEq for EventIndexSource {
    fn eq(&self, other: &Self) -> bool {
        self.exact_eq(other)
    }
}

impl Eq for EventIndexSource {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedCoreEventState {
    pub source_storage_key: String,
    pub event_id: StableEntityId,
    pub lineage: IndexedCoreEventLineage,
    pub event_sequence: u64,
    pub core_record_sha256: String,
    pub core_record_leaf_sha256: String,
    pub flat_record_count: u32,
    pub event_output_root: String,
    /// Six checked per-event 0/1 counters packed into one fixed byte on disk.
    pub coverage: SegmentCoreCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedCopiedEventOrigin {
    pub ancestor_session_id: StableEntityId,
    pub ancestor_event_id: StableEntityId,
    pub proof: EventCopyProofKind,
}

/// Publication-owned fixed event row. Session identity and ancestry are held
/// once in the companion lineage dictionary rather than repeated in every
/// retained event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactIndexedCoreEventState {
    pub source_storage_key: String,
    pub event_id: StableEntityId,
    pub session_ref: u32,
    pub event_sequence: u64,
    pub core_record_sha256: String,
    pub core_record_leaf_sha256: String,
    pub flat_record_count: u32,
    pub event_output_root: String,
    pub coverage: SegmentCoreCoverage,
}

impl CompactIndexedCoreEventState {
    #[must_use]
    pub fn key(&self) -> EventIndexKey {
        EventIndexKey {
            source_storage_key: self.source_storage_key.clone(),
            event_digest: self.event_id.digest(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedCoreEventLineage {
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: Option<StableEntityId>,
    pub session_relationship: SessionRelationshipKind,
    pub origin_kind: IndexedCoreEventOriginKind,
    pub copied_from: Option<IndexedCopiedEventOrigin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexedCoreEventOriginKind {
    Unknown,
    UniqueToSession,
    CopiedFromAncestor,
}

impl IndexedCoreEventState {
    #[must_use]
    pub fn key(&self) -> EventIndexKey {
        EventIndexKey {
            source_storage_key: self.source_storage_key.clone(),
            event_digest: self.event_id.digest(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedCoreEventTombstone {
    pub source_storage_key: String,
    pub event_id: StableEntityId,
    pub prior_event_state_sha256: String,
}

impl IndexedCoreEventTombstone {
    #[must_use]
    pub fn key(&self) -> EventIndexKey {
        EventIndexKey {
            source_storage_key: self.source_storage_key.clone(),
            event_digest: self.event_id.digest(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum EventIndexEntry {
    /// The state is visible in this segment. A same-key shadow applies only to
    /// older segments after a newest-first merger chooses this replacement.
    State {
        state: IndexedCoreEventState,
        shadows_older: bool,
    },
    Tombstone(IndexedCoreEventTombstone),
}

impl EventIndexEntry {
    #[must_use]
    pub fn key(&self) -> EventIndexKey {
        match self {
            Self::State { state, .. } => state.key(),
            Self::Tombstone(tombstone) => tombstone.key(),
        }
    }

    #[must_use]
    pub fn event_id(&self) -> StableEntityId {
        match self {
            Self::State { state, .. } => state.event_id,
            Self::Tombstone(tombstone) => tombstone.event_id,
        }
    }
}

/// Merge key exposed without a source-table ordinal, which is segment-local.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventIndexKey {
    pub source_storage_key: String,
    pub event_digest: [u8; EVENT_DIGEST_BYTES],
}

/// The continuation deliberately contains no generation, request, or page state.
/// Its caller can bind this small immutable cursor to those control-plane values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventIndexContinuation {
    pub event_id: StableEntityId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventIndexPage {
    pub entries: Vec<EventIndexEntry>,
    pub terminal: bool,
    pub continuation: Option<EventIndexContinuation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventIndexStats {
    pub source_count: u32,
    pub record_count: u32,
    pub tombstone_count: u32,
    pub session_count: u32,
    pub copied_event_count: u32,
    pub plaintext_bytes: u64,
    pub fst_bytes: u64,
}

pub struct EventIndexWriter;

impl EventIndexWriter {
    /// Returns the encoded-payload plus owned-input bytes charged while one
    /// sealed publication job is outstanding. The writer still performs its
    /// complete validation before emitting bytes; this accounting pass exists
    /// only for publication admission and does not prepare an alternate
    /// durable representation.
    pub fn publication_accounted_bytes(
        sources: &[EventIndexSource],
        records: &[CompactIndexedCoreEventState],
        tombstones: &[IndexedCoreEventTombstone],
        lineage: &EventLineageTables,
    ) -> Result<usize, EventIndexError> {
        validate_input_counts(sources.len(), records.len(), tombstones.len())?;
        let source_payload_bytes = sources.iter().try_fold(0_usize, |total, source| {
            total
                .checked_add(Self::publication_source_accounted_bytes(source)?)
                .ok_or(EventIndexError::Bound("publication source bytes"))
        })?;
        let record_payload_bytes = records.iter().try_fold(0_usize, |total, record| {
            total
                .checked_add(Self::publication_record_accounted_bytes(record)?)
                .ok_or(EventIndexError::Bound("publication record bytes"))
        })?;
        let tombstone_payload_bytes = tombstones.iter().try_fold(0_usize, |total, tombstone| {
            total
                .checked_add(Self::publication_tombstone_accounted_bytes(tombstone)?)
                .ok_or(EventIndexError::Bound("publication tombstone bytes"))
        })?;
        source_payload_bytes
            .checked_add(record_payload_bytes)
            .and_then(|bytes| bytes.checked_add(tombstone_payload_bytes))
            .and_then(|bytes| {
                lineage
                    .sessions
                    .len()
                    .checked_mul(Self::publication_session_accounted_bytes())
                    .and_then(|lineage_bytes| bytes.checked_add(lineage_bytes))
            })
            .and_then(|bytes| {
                lineage
                    .copied_origins
                    .len()
                    .checked_mul(Self::publication_copied_origin_accounted_bytes())
                    .and_then(|lineage_bytes| bytes.checked_add(lineage_bytes))
            })
            .ok_or(EventIndexError::Bound("publication retained bytes"))
    }

    pub fn publication_source_accounted_bytes(
        source: &EventIndexSource,
    ) -> Result<usize, EventIndexError> {
        let encoded = canonical_json(source)?;
        validate_frame_length(encoded.len())?;
        // One canonical frame conservatively covers all nested logical source
        // payload retained by the owned input. The second is the writer's
        // transient frame. Container capacities are charged separately because
        // compact composite keys can own more allocation than their JSON text.
        let container_capacity = source_container_capacity(&source.source)?;
        encoded
            .len()
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(4))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<EventIndexSource>()))
            .and_then(|bytes| bytes.checked_add(source.storage_key.capacity()))
            .and_then(|bytes| bytes.checked_add(container_capacity))
            .ok_or(EventIndexError::Bound("publication source bytes"))
    }

    pub fn publication_record_accounted_bytes(
        record: &CompactIndexedCoreEventState,
    ) -> Result<usize, EventIndexError> {
        RECORD_BYTES
            .checked_add(std::mem::size_of::<CompactIndexedCoreEventState>())
            .and_then(|bytes| bytes.checked_add(record.source_storage_key.capacity()))
            .and_then(|bytes| bytes.checked_add(record.core_record_sha256.capacity()))
            .and_then(|bytes| bytes.checked_add(record.core_record_leaf_sha256.capacity()))
            .and_then(|bytes| bytes.checked_add(record.event_output_root.capacity()))
            .ok_or(EventIndexError::Bound("publication record bytes"))
    }

    pub fn publication_staged_record_accounted_bytes(
        record: &IndexedCoreEventState,
    ) -> Result<usize, EventIndexError> {
        RECORD_BYTES
            .checked_add(std::mem::size_of::<CompactIndexedCoreEventState>())
            .and_then(|bytes| bytes.checked_add(record.source_storage_key.capacity()))
            .and_then(|bytes| bytes.checked_add(record.core_record_sha256.capacity()))
            .and_then(|bytes| bytes.checked_add(record.core_record_leaf_sha256.capacity()))
            .and_then(|bytes| bytes.checked_add(record.event_output_root.capacity()))
            .ok_or(EventIndexError::Bound("publication record bytes"))
    }

    #[must_use]
    pub const fn publication_session_accounted_bytes() -> usize {
        SESSION_DICTIONARY_ROW_BYTES + std::mem::size_of::<IndexedCoreEventLineage>()
    }

    #[must_use]
    pub const fn publication_copied_origin_accounted_bytes() -> usize {
        COPIED_ORIGIN_ROW_BYTES + std::mem::size_of::<IndexedCopiedOriginRow>()
    }

    pub fn publication_tombstone_accounted_bytes(
        tombstone: &IndexedCoreEventTombstone,
    ) -> Result<usize, EventIndexError> {
        TOMBSTONE_BYTES
            .checked_add(std::mem::size_of::<IndexedCoreEventTombstone>())
            .and_then(|bytes| bytes.checked_add(tombstone.source_storage_key.capacity()))
            .and_then(|bytes| bytes.checked_add(tombstone.prior_event_state_sha256.capacity()))
            .ok_or(EventIndexError::Bound("publication tombstone bytes"))
    }

    /// Sorts only the explicitly bounded input vectors and streams every encoded
    /// section directly into the segment writer. No plaintext temporary file or
    /// whole-segment plaintext buffer is created.
    pub fn write(
        writer: SegmentWriter,
        sources: Vec<EventIndexSource>,
        records: Vec<IndexedCoreEventState>,
        tombstones: Vec<IndexedCoreEventTombstone>,
    ) -> Result<EventIndexStats, EventIndexError> {
        let mut lineage = EventLineageAccumulator::new();
        let mut compact = Vec::with_capacity(records.len());
        for record in records {
            let (session_ref, _, _) = lineage.retain(record.key(), record.lineage)?;
            compact.push(CompactIndexedCoreEventState {
                source_storage_key: record.source_storage_key,
                event_id: record.event_id,
                session_ref,
                event_sequence: record.event_sequence,
                core_record_sha256: record.core_record_sha256,
                core_record_leaf_sha256: record.core_record_leaf_sha256,
                flat_record_count: record.flat_record_count,
                event_output_root: record.event_output_root,
                coverage: record.coverage,
            });
        }
        let lineage = lineage.finish(&mut compact)?;
        Self::write_compact(writer, sources, compact, tombstones, lineage)
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub fn write_compact(
        writer: SegmentWriter,
        mut sources: Vec<EventIndexSource>,
        mut records: Vec<CompactIndexedCoreEventState>,
        mut tombstones: Vec<IndexedCoreEventTombstone>,
        lineage_tables: EventLineageTables,
    ) -> Result<EventIndexStats, EventIndexError> {
        validate_input_counts(sources.len(), records.len(), tombstones.len())?;

        for source in &sources {
            source.validate()?;
        }
        sources.sort_by(|left, right| left.storage_key.cmp(&right.storage_key));
        if sources
            .windows(2)
            .any(|pair| pair[0].storage_key >= pair[1].storage_key)
        {
            return Err(EventIndexError::Conflict);
        }

        let session_refs = records
            .iter()
            .map(|record| record.session_ref)
            .collect::<Vec<_>>();
        lineage_tables.validate_refs(&session_refs)?;
        let mut session_sources = vec![None; lineage_tables.sessions.len()];
        for record in &records {
            let (source_ordinal, session_ordinal) =
                validate_compact_record(record, &sources, &lineage_tables)?;
            match session_sources
                .get_mut(session_ordinal)
                .ok_or(EventIndexError::Invalid("session dictionary reference"))?
            {
                slot @ None => {
                    let source =
                        sources
                            .get(usize::try_from(source_ordinal).map_err(|_| {
                                EventIndexError::Invalid("source dictionary reference")
                            })?)
                            .ok_or(EventIndexError::Invalid("source dictionary reference"))?;
                    validate_owned_session_identity(
                        lineage_tables.sessions[session_ordinal].session_id,
                        source,
                    )?;
                    *slot = Some(source_ordinal);
                }
                Some(expected) if *expected != source_ordinal => {
                    return Err(EventIndexError::Invalid("session source identity"));
                }
                Some(_) => {}
            }
        }
        for tombstone in &tombstones {
            validate_tombstone(tombstone, &sources)?;
        }
        records.sort_by(compare_compact_records);
        tombstones.sort_by(compare_tombstones);
        if has_duplicate_record_keys(&records) || has_duplicate_tombstone_keys(&tombstones) {
            return Err(EventIndexError::Conflict);
        }
        let sources_bytes = sources.iter().try_fold(0_u64, |total, source| {
            let encoded = canonical_json(source)?;
            validate_frame_length(encoded.len())?;
            add_frame_bytes(total, encoded.len(), "source section bytes")
        })?;
        if sources_bytes > MAX_SOURCE_SECTION_BYTES {
            return Err(EventIndexError::Bound("source section bytes"));
        }
        let records_bytes = fixed_section_bytes(records.len(), RECORD_BYTES, "record bytes")?;
        if records_bytes > MAX_RECORD_SECTION_BYTES {
            return Err(EventIndexError::Bound("record section bytes"));
        }
        let tombstones_bytes =
            fixed_section_bytes(tombstones.len(), TOMBSTONE_BYTES, "tombstone bytes")?;
        if tombstones_bytes > MAX_TOMBSTONE_SECTION_BYTES {
            return Err(EventIndexError::Bound("tombstone section bytes"));
        }
        let session_dictionary_bytes = fixed_section_bytes(
            lineage_tables.sessions.len(),
            SESSION_DICTIONARY_ROW_BYTES,
            "session dictionary bytes",
        )?;
        if session_dictionary_bytes > MAX_SESSION_DICTIONARY_SECTION_BYTES {
            return Err(EventIndexError::Bound("session dictionary section bytes"));
        }
        let copied_origin_bytes = fixed_section_bytes(
            lineage_tables.copied_origins.len(),
            COPIED_ORIGIN_ROW_BYTES,
            "copied origin bytes",
        )?;
        if copied_origin_bytes > MAX_COPIED_ORIGIN_SECTION_BYTES {
            return Err(EventIndexError::Bound("copied origin section bytes"));
        }

        let header = Header::for_sections(
            sources.len(),
            records.len(),
            tombstones.len(),
            sources_bytes,
            records_bytes,
            tombstones_bytes,
            lineage_tables.sessions.len(),
            session_dictionary_bytes,
            lineage_tables.copied_origins.len(),
            copied_origin_bytes,
        )?;
        let mut writer = writer;
        writer.write_all(&header.encode())?;
        for source in &sources {
            write_source_frame(&mut writer, source)?;
        }
        for record in &records {
            write_record(
                &mut writer,
                record,
                source_ordinal(&sources, &record.source_storage_key)?,
                record.session_ref,
            )?;
        }
        for tombstone in &tombstones {
            write_tombstone(
                &mut writer,
                tombstone,
                source_ordinal(&sources, &tombstone.source_storage_key)?,
            )?;
        }
        for session in &lineage_tables.sessions {
            writer.write_all(&encode_session_lineage(session)?)?;
        }
        for copied in &lineage_tables.copied_origins {
            writer.write_all(&encode_copied_origin(copied)?)?;
        }

        let plaintext_bytes = header.end_offset;
        if plaintext_bytes > MAX_SEGMENT_PLAINTEXT_BYTES {
            return Err(EventIndexError::Bound("segment plaintext bytes"));
        }
        writer.finish()?;
        Ok(EventIndexStats {
            source_count: header.source_count,
            record_count: header.record_count,
            tombstone_count: header.tombstone_count,
            session_count: header.session_count,
            copied_event_count: header.copied_origin_count,
            plaintext_bytes,
            fst_bytes: 0,
        })
    }
}

fn source_container_capacity(source: &SourceKey) -> Result<usize, EventIndexError> {
    match source.anchor() {
        SourceAnchor::ProviderNative { namespace, key } => namespace
            .capacity()
            .checked_add(typed_key_container_capacity(key)?)
            .ok_or(EventIndexError::Bound("publication source container bytes")),
        SourceAnchor::CatalogLineage(_) => Ok(0),
    }
}

fn typed_key_container_capacity(key: &TypedKey) -> Result<usize, EventIndexError> {
    match key {
        TypedKey::Bytes(value) => Ok(value.capacity()),
        TypedKey::Utf8(value) => Ok(value.capacity()),
        TypedKey::Composite(values) => values
            .capacity()
            .checked_mul(std::mem::size_of::<TypedKey>())
            .ok_or(EventIndexError::Bound(
                "publication typed-key container bytes",
            ))
            .and_then(|bytes| {
                values.iter().try_fold(bytes, |total, value| {
                    total
                        .checked_add(typed_key_container_capacity(value)?)
                        .ok_or(EventIndexError::Bound(
                            "publication typed-key container bytes",
                        ))
                })
            }),
        TypedKey::Null
        | TypedKey::I64(_)
        | TypedKey::U64(_)
        | TypedKey::F64Bits(_)
        | TypedKey::Bool(_) => Ok(0),
    }
}

pub struct EventIndexReader {
    file: SegmentFile,
    layout: Layout,
    sources: Vec<EventIndexSource>,
    lineage: EventLineageTables,
    #[cfg(test)]
    sparse_lineage_open_work: SparseLineageOpenWork,
    record_count: usize,
    tombstone_count: usize,
}

impl EventIndexReader {
    pub fn open(mut file: SegmentFile) -> Result<Self, EventIndexError> {
        let plaintext_bytes = file.plaintext_len();
        if plaintext_bytes
            < u64::try_from(HEADER_BYTES).map_err(|_| EventIndexError::Corrupt("header length"))?
        {
            return Err(EventIndexError::Corrupt("truncated header"));
        }
        if plaintext_bytes > MAX_SEGMENT_PLAINTEXT_BYTES {
            return Err(EventIndexError::Corrupt("segment plaintext bound"));
        }
        let encoded_header = file.read_range(0, HEADER_BYTES)?;
        let header = Header::decode(encoded_header.as_slice())?;
        header.validate_bounds()?;
        let layout = Layout::new(&header, plaintext_bytes)?;
        let sources = read_sources(&mut file, &layout, header.source_count)?;
        let record_count = usize::try_from(header.record_count)
            .map_err(|_| EventIndexError::Corrupt("record count"))?;
        let sparse_lineage = read_sparse_lineage_tables(&mut file, &layout, record_count)?;
        #[cfg(test)]
        let sparse_lineage_open_work = sparse_lineage.work;
        let lineage = sparse_lineage.tables;

        Ok(Self {
            file,
            layout,
            sources,
            lineage,
            #[cfg(test)]
            sparse_lineage_open_work,
            record_count,
            tombstone_count: usize::try_from(header.tombstone_count)
                .map_err(|_| EventIndexError::Corrupt("tombstone count"))?,
        })
    }

    #[must_use]
    pub fn sources(&self) -> &[EventIndexSource] {
        &self.sources
    }

    #[cfg(test)]
    fn sparse_lineage_open_work(&self) -> SparseLineageOpenWork {
        self.sparse_lineage_open_work
    }

    #[cfg(test)]
    fn open_checked_chunk_reads(&self) -> u64 {
        self.file.chunk_reads()
    }

    /// Checked physical rows in this immutable `EventIndex` container.
    /// Candidate validation uses this header-derived count to bound complete
    /// passes without imposing an unrelated interactive-query ceiling.
    pub fn entry_count(&self) -> Result<u64, EventIndexError> {
        u64::try_from(self.record_count)
            .ok()
            .and_then(|records| {
                u64::try_from(self.tombstone_count)
                    .ok()
                    .and_then(|tombstones| records.checked_add(tombstones))
            })
            .ok_or(EventIndexError::Bound("entry count"))
    }

    /// Resolves a compact `core_source_storage_id` to the exact source
    /// descriptor stored once in this segment's bounded source dictionary.
    pub fn source_by_storage_key(
        &self,
        storage_key: &str,
    ) -> Result<Option<&EventIndexSource>, EventIndexError> {
        validate_source_storage_key(storage_key)?;
        match self
            .sources
            .binary_search_by(|source| source.storage_key.as_str().cmp(storage_key))
        {
            Ok(index) => Ok(self.sources.get(index)),
            Err(_) => Ok(None),
        }
    }

    pub fn lookup(
        &mut self,
        source: &EventIndexSource,
        event_id: StableEntityId,
    ) -> Result<Option<EventIndexEntry>, EventIndexError> {
        let source_ordinal = self.require_exact_source(source)?;
        validate_event_identity(event_id, source)?;
        let target = (source_ordinal, event_id.digest());
        let record = find_record(
            &mut self.file,
            &self.layout,
            &self.sources,
            &self.lineage,
            self.record_count,
            target,
        )?;
        let tombstone = find_tombstone(
            &mut self.file,
            &self.layout,
            &self.sources,
            self.tombstone_count,
            target,
        )?;
        let entry = if let Some(state) = record {
            EventIndexEntry::State {
                state,
                shadows_older: tombstone.is_some(),
            }
        } else if let Some(tombstone) = tombstone {
            EventIndexEntry::Tombstone(tombstone)
        } else {
            return Ok(None);
        };
        if entry.key().event_digest != event_id.digest()
            || entry.key().source_storage_key != source.storage_key
        {
            return Err(EventIndexError::Corrupt("lookup locator key"));
        }
        Ok(Some(entry))
    }

    pub fn page(
        &mut self,
        source: &EventIndexSource,
        after_event_id: Option<StableEntityId>,
        limit: usize,
    ) -> Result<EventIndexPage, EventIndexError> {
        validate_page_limit(limit)?;
        let source_ordinal = self.require_exact_source(source)?;
        if let Some(after) = after_event_id {
            validate_event_identity(after, source)?;
        }
        let after_digest = after_event_id.map(StableEntityId::digest);
        let mut record_index = lower_bound_records(
            &mut self.file,
            &self.layout,
            self.record_count,
            source_ordinal,
            after_digest,
        )?;
        let mut tombstone_index = lower_bound_tombstones(
            &mut self.file,
            &self.layout,
            self.tombstone_count,
            source_ordinal,
            after_digest,
        )?;
        let mut entries = Vec::with_capacity(limit);
        let mut has_more = false;
        loop {
            let record_key = record_key_at(
                &mut self.file,
                &self.layout,
                self.record_count,
                record_index,
            )?
            .filter(|key| key.0 == source_ordinal);
            let tombstone_key = tombstone_key_at(
                &mut self.file,
                &self.layout,
                self.tombstone_count,
                tombstone_index,
            )?
            .filter(|key| key.0 == source_ordinal);
            let Some(next_key) = record_key.into_iter().chain(tombstone_key).min() else {
                break;
            };
            if entries.len() == limit {
                has_more = true;
                break;
            }
            let has_record = record_key == Some(next_key);
            let has_tombstone = tombstone_key == Some(next_key);
            let entry = if has_record {
                let offset = fixed_entry_offset(record_index, RECORD_BYTES, "record offset")?;
                let state = read_record_at(
                    &mut self.file,
                    &self.layout,
                    &self.sources,
                    &self.lineage,
                    offset,
                )?;
                record_index = record_index
                    .checked_add(1)
                    .ok_or(EventIndexError::Bound("record page cursor"))?;
                if has_tombstone {
                    tombstone_index = tombstone_index
                        .checked_add(1)
                        .ok_or(EventIndexError::Bound("tombstone page cursor"))?;
                }
                EventIndexEntry::State {
                    state,
                    shadows_older: has_tombstone,
                }
            } else {
                let offset =
                    fixed_entry_offset(tombstone_index, TOMBSTONE_BYTES, "tombstone offset")?;
                let tombstone =
                    read_tombstone_at(&mut self.file, &self.layout, &self.sources, offset)?;
                tombstone_index = tombstone_index
                    .checked_add(1)
                    .ok_or(EventIndexError::Bound("tombstone page cursor"))?;
                EventIndexEntry::Tombstone(tombstone)
            };
            if entry.key().source_storage_key != source.storage_key
                || entry.key().event_digest != next_key.1
            {
                return Err(EventIndexError::Corrupt("page fixed-row key"));
            }
            entries.push(entry);
        }
        let continuation = if has_more {
            entries.last().map(|entry| EventIndexContinuation {
                event_id: entry.event_id(),
            })
        } else {
            None
        };
        let page = EventIndexPage {
            entries,
            terminal: !has_more,
            continuation,
        };
        Ok(page)
    }

    fn require_exact_source(&self, requested: &EventIndexSource) -> Result<u32, EventIndexError> {
        requested.validate()?;
        let ordinal = source_ordinal(&self.sources, &requested.storage_key)
            .map_err(|_| EventIndexError::WrongSource)?;
        let stored = self
            .sources
            .get(usize::try_from(ordinal).map_err(|_| EventIndexError::WrongSource)?)
            .ok_or(EventIndexError::WrongSource)?;
        if !stored.exact_eq(requested) {
            return Err(EventIndexError::WrongSource);
        }
        Ok(ordinal)
    }
}
