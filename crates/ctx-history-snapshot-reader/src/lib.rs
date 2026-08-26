//! Immutable-data access to one exact committed Core generation.
//!
//! The selected generation is never mutated. Opening a snapshot does perform
//! one bounded control-plane mutation may initialize the owner-private lease
//! coordinator for an older index root. Ordinary opens only take shared OS
//! byte-range locks; they never refresh, migrate, repair, or select a replacement
//! generation.

use std::{collections::VecDeque, path::Path};

use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityId, CORE_RECORD_VERSION, IDENTITY_VERSION,
};
use ctx_history_index_format::{
    current_core_record_contract_fingerprint, current_source_generation_policy_hash,
    source_sort_key, GenerationManifest, SourceCoreRecordAggregate, GENERATION_MANIFEST_VERSION,
    LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
};
use ctx_history_index_generation::{
    acquire_generation_read_lease_from_root, acquire_retained_generation_read_lease_from_root,
    GenerationReadLease, GenerationReadRoot, GenerationRetentionLease,
};
use ctx_history_index_query::{
    CoreEventPageBudget, IndexError, SourceEventCursor, VerifiedIndex,
    DEFAULT_CORE_EVENT_PAGE_BUDGET, MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_SOURCE_MANIFEST_PAGE_ITEMS: usize = 256;
pub const MAX_SOURCE_DELTA_PAGE_ITEMS: usize = 256;
pub const MAX_SOURCE_DELTA_SCANNED_ITEMS: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSchema {
    pub manifest_version: u32,
    pub identity_version: u16,
    pub core_record_version: u32,
    pub lexical_schema_version: u32,
    pub lexical_analyzer_version: u32,
    pub policy_schema_hash: String,
}

impl SnapshotSchema {
    fn from_manifest(manifest: &GenerationManifest) -> Self {
        Self {
            manifest_version: manifest.manifest_version,
            identity_version: manifest.identity_version,
            core_record_version: manifest.core_record_version,
            lexical_schema_version: manifest.lexical_schema_version,
            lexical_analyzer_version: manifest.lexical_analyzer_version,
            policy_schema_hash: manifest.policy_schema_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotContract {
    pub schema: SnapshotSchema,
    pub core_record_fingerprint: String,
}

impl SnapshotContract {
    pub fn current() -> Result<Self> {
        Ok(Self {
            schema: SnapshotSchema {
                manifest_version: GENERATION_MANIFEST_VERSION,
                identity_version: IDENTITY_VERSION,
                core_record_version: CORE_RECORD_VERSION,
                lexical_schema_version: LEXICAL_SCHEMA_VERSION,
                lexical_analyzer_version: LEXICAL_ANALYZER_VERSION,
                policy_schema_hash: current_source_generation_policy_hash()
                    .map_err(|error| SnapshotError::Corrupt(error.to_string()))?,
            },
            core_record_fingerprint: current_core_record_contract_fingerprint(),
        })
    }
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("requested generation was not found: {0}")]
    NotFound(String),
    #[error("snapshot path is unsafe: {0}")]
    UnsafePath(String),
    #[error("generation read lease conflicts with reclamation: {0}")]
    LeaseConflict(String),
    #[error("snapshot changed while a verified reader was opening: {0}")]
    ConcurrentGenerationChange(String),
    #[error("snapshot schema mismatch")]
    SchemaMismatch {
        expected: SnapshotSchema,
        actual: SnapshotSchema,
    },
    #[error("Core record fingerprint mismatch: expected {expected}, got {actual}")]
    FingerprintMismatch { expected: String, actual: String },
    #[error("snapshot is corrupt: {0}")]
    Corrupt(String),
    #[error("snapshot request exceeds a bound: {0}")]
    Bounds(String),
}

pub type Result<T> = std::result::Result<T, SnapshotError>;

pub struct CoreSnapshot {
    index: VerifiedIndex,
    contract: SnapshotContract,
    _lease: GenerationReadLease,
}

impl CoreSnapshot {
    /// Opens only `generation_id` below `data_root/search/lexical`.
    ///
    /// This leaves the selected generation immutable. It may initialize one
    /// fixed lease coordinator for an older index root, then holds only the
    /// exact generation's shared OS lock ranges for the snapshot lifetime.
    pub fn open(
        data_root: impl AsRef<Path>,
        generation_id: &str,
        expected: &SnapshotContract,
    ) -> Result<Self> {
        let root =
            GenerationReadRoot::open_data_root(data_root).map_err(map_generation_root_error)?;
        let lease = acquire_generation_read_lease_from_root(root, generation_id)
            .map_err(|error| map_generation_error(error, generation_id))?;
        Self::open_leased(lease, expected)
    }

    /// Opens the exact generation named by a validated durable retention
    /// authority, even after it has advanced beyond active/previous. Because
    /// publication may legitimately change retained hard-link metadata, this
    /// path performs one full read-only physical audit before opening.
    pub fn open_retained(
        data_root: impl AsRef<Path>,
        authority: &GenerationRetentionLease,
        expected: &SnapshotContract,
    ) -> Result<Self> {
        let generation_id = authority.generation_id();
        let root =
            GenerationReadRoot::open_data_root(data_root).map_err(map_generation_root_error)?;
        let lease = acquire_retained_generation_read_lease_from_root(root, authority)
            .map_err(|error| map_generation_error(error, generation_id))?;
        Self::open_leased(lease, expected)
    }

    fn open_leased(lease: GenerationReadLease, expected: &SnapshotContract) -> Result<Self> {
        let generation_id = lease.generation_id().to_owned();
        let index = lease
            .with_root_access(|root| VerifiedIndex::open_generation_read_lease(root, &lease))
            .map_err(|error| map_generation_error(error, &generation_id))?
            .map_err(|error| map_index_error(error, &generation_id, expected))?;
        if index.generation_id() != generation_id {
            return Err(SnapshotError::Corrupt(
                "pinned generation identity changed".to_owned(),
            ));
        }
        let actual_schema = SnapshotSchema::from_manifest(index.manifest());
        if actual_schema != expected.schema {
            return Err(SnapshotError::SchemaMismatch {
                expected: expected.schema.clone(),
                actual: actual_schema,
            });
        }
        let actual_fingerprint = &index.manifest().core_record_contract_fingerprint;
        if actual_fingerprint != &expected.core_record_fingerprint {
            return Err(SnapshotError::FingerprintMismatch {
                expected: expected.core_record_fingerprint.clone(),
                actual: actual_fingerprint.clone(),
            });
        }
        Ok(Self {
            index,
            contract: expected.clone(),
            _lease: lease,
        })
    }

    pub fn generation_id(&self) -> &str {
        self.index.generation_id()
    }

    pub fn contract(&self) -> &SnapshotContract {
        &self.contract
    }

    pub fn indexed_documents(&self) -> u64 {
        self.index.manifest().indexed_documents
    }

    pub fn certified_source_bytes(&self) -> u64 {
        self.index.manifest().certified_source_bytes
    }

    pub fn source_count(&self) -> usize {
        self.index.manifest().sources.len()
    }

    pub fn source_manifest_page(
        &self,
        cursor: Option<&SourceManifestCursor>,
        limit: usize,
    ) -> Result<SourceManifestPage> {
        if !(1..=MAX_SOURCE_MANIFEST_PAGE_ITEMS).contains(&limit) {
            return Err(SnapshotError::Bounds(format!(
                "source manifest page size {limit} is outside 1..={MAX_SOURCE_MANIFEST_PAGE_ITEMS}"
            )));
        }
        let offset = match cursor {
            Some(cursor) if cursor.generation_id != self.generation_id() => {
                return Err(SnapshotError::Corrupt(
                    "source manifest cursor belongs to another generation".to_owned(),
                ));
            }
            Some(cursor) => cursor.offset,
            None => 0,
        };
        if offset > self.source_count() {
            return Err(SnapshotError::Bounds(
                "source manifest cursor is past the end".to_owned(),
            ));
        }
        let end = offset.saturating_add(limit).min(self.source_count());
        let items = (offset..end)
            .map(|index| self.source_state(index))
            .collect::<Result<Vec<_>>>()?;
        let terminal = end == self.source_count();
        Ok(SourceManifestPage {
            generation_id: self.generation_id().to_owned(),
            items,
            next_cursor: (!terminal).then(|| SourceManifestCursor {
                generation_id: self.generation_id().to_owned(),
                offset: end,
            }),
            terminal,
        })
    }

    pub fn source_delta_page(
        &self,
        base: &CoreSnapshot,
        cursor: Option<&SourceDeltaCursor>,
        limit: usize,
    ) -> Result<SourceDeltaPage> {
        if !(1..=MAX_SOURCE_DELTA_PAGE_ITEMS).contains(&limit) {
            return Err(SnapshotError::Bounds(format!(
                "source delta page size {limit} is outside 1..={MAX_SOURCE_DELTA_PAGE_ITEMS}"
            )));
        }
        let (mut base_offset, mut target_offset) = match cursor {
            Some(cursor)
                if cursor.base_generation_id != base.generation_id()
                    || cursor.target_generation_id != self.generation_id() =>
            {
                return Err(SnapshotError::Corrupt(
                    "source delta cursor belongs to another generation pair".to_owned(),
                ));
            }
            Some(cursor) => (cursor.base_offset, cursor.target_offset),
            None => (0, 0),
        };
        if base_offset > base.source_count() || target_offset > self.source_count() {
            return Err(SnapshotError::Bounds(
                "source delta cursor is past the end".to_owned(),
            ));
        }

        let mut items = Vec::new();
        let mut scanned = 0_usize;
        while items.len() < limit
            && scanned < MAX_SOURCE_DELTA_SCANNED_ITEMS
            && (base_offset < base.source_count() || target_offset < self.source_count())
        {
            scanned += 1;
            let previous = (base_offset < base.source_count())
                .then(|| base.source_state(base_offset))
                .transpose()?;
            let current = (target_offset < self.source_count())
                .then(|| self.source_state(target_offset))
                .transpose()?;
            match (previous, current) {
                (Some(previous), Some(current)) => {
                    let order =
                        source_sort_key(&previous.source).cmp(&source_sort_key(&current.source));
                    match order {
                        std::cmp::Ordering::Less => {
                            base_offset += 1;
                            items.push(SourceDelta::Removed(previous));
                        }
                        std::cmp::Ordering::Greater => {
                            target_offset += 1;
                            items.push(SourceDelta::Added(current));
                        }
                        std::cmp::Ordering::Equal => {
                            base_offset += 1;
                            target_offset += 1;
                            if previous != current {
                                items.push(SourceDelta::Replaced {
                                    previous: Box::new(previous),
                                    current,
                                });
                            }
                        }
                    }
                }
                (Some(previous), None) => {
                    base_offset += 1;
                    items.push(SourceDelta::Removed(previous));
                }
                (None, Some(current)) => {
                    target_offset += 1;
                    items.push(SourceDelta::Added(current));
                }
                (None, None) => break,
            }
        }
        let terminal = base_offset == base.source_count() && target_offset == self.source_count();
        Ok(SourceDeltaPage {
            base_generation_id: base.generation_id().to_owned(),
            target_generation_id: self.generation_id().to_owned(),
            items,
            scanned_sources: scanned,
            next_cursor: (!terminal).then(|| SourceDeltaCursor {
                base_generation_id: base.generation_id().to_owned(),
                target_generation_id: self.generation_id().to_owned(),
                base_offset,
                target_offset,
            }),
            terminal,
        })
    }

    /// Returns one replayable page of complete records and their exact stored
    /// Core JSON bytes from this generation.
    ///
    /// Reusing the same serialized cursor and bounds returns the same immutable
    /// page, allowing a consumer to durably acknowledge a complete page before
    /// advancing to `next_cursor` without rescanning prior records.
    pub fn record_page(
        &self,
        source: &SourceKey,
        cursor: Option<&CoreRecordPageCursor>,
        limit: usize,
        budget: CoreEventPageBudget,
    ) -> Result<CoreRecordPage> {
        if !(1..=MAX_SOURCE_EVENT_PAGE_ITEMS).contains(&limit) {
            return Err(SnapshotError::Bounds(format!(
                "record page size {limit} is outside 1..={MAX_SOURCE_EVENT_PAGE_ITEMS}"
            )));
        }
        let query_cursor = cursor
            .map(|cursor| {
                if cursor.generation_id != self.generation_id() {
                    return Err(SnapshotError::Corrupt(
                        "record cursor belongs to another generation".to_owned(),
                    ));
                }
                if !cursor.source.exact_descriptor_eq(source) {
                    return Err(SnapshotError::Corrupt(
                        "record cursor belongs to another source".to_owned(),
                    ));
                }
                Ok(SourceEventCursor::new(
                    cursor.generation_id.clone(),
                    cursor.source.clone(),
                    cursor.after,
                ))
            })
            .transpose()?;
        let page = self
            .index
            .stored_core_source_event_page_with_budget(source, query_cursor.as_ref(), limit, budget)
            .map_err(|error| map_index_error(error, self.generation_id(), self.contract()))?;
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let stored_json = item
                    .stored_json
                    .encoded_core_record()
                    .map_err(|error| map_index_error(error, self.generation_id(), self.contract()))?
                    .to_vec();
                Ok(CoreRecordPageItem {
                    core_record: item.core_record,
                    stored_json,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(CoreRecordPage {
            generation_id: page.generation_id,
            source: page.source,
            items,
            encoded_core_bytes: page.encoded_core_bytes,
            content_bytes: page.content_bytes,
            next_cursor: page.next_cursor.map(CoreRecordPageCursor::from_query),
            terminal: page.terminal,
        })
    }

    pub fn records<'a>(
        &'a self,
        source: SourceKey,
        page_items: usize,
        budget: CoreEventPageBudget,
    ) -> Result<CoreRecordStream<'a>> {
        if !(1..=MAX_SOURCE_EVENT_PAGE_ITEMS).contains(&page_items) {
            return Err(SnapshotError::Bounds(format!(
                "record page size {page_items} is outside 1..={MAX_SOURCE_EVENT_PAGE_ITEMS}"
            )));
        }
        Ok(CoreRecordStream {
            snapshot: self,
            source,
            page_items,
            budget,
            cursor: None,
            buffered: VecDeque::new(),
            terminal: false,
            failed: false,
        })
    }

    pub fn records_with_default_bounds(&self, source: SourceKey) -> Result<CoreRecordStream<'_>> {
        self.records(source, 64, DEFAULT_CORE_EVENT_PAGE_BUDGET)
    }

    fn source_state(&self, index: usize) -> Result<SourceState> {
        let source = self
            .index
            .manifest()
            .sources
            .get(index)
            .map(|certificate| certificate.observation().source().clone())
            .ok_or_else(|| SnapshotError::Bounds("source offset is past the end".to_owned()))?;
        let aggregate = self
            .index
            .manifest()
            .core_record_aggregates
            .get(index)
            .cloned()
            .ok_or_else(|| SnapshotError::Corrupt("source aggregate is missing".to_owned()))?;
        Ok(SourceState { source, aggregate })
    }
}

/// Minimal immutable source state needed to identify record ownership and
/// detect logical source replacement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceState {
    pub source: SourceKey,
    pub aggregate: SourceCoreRecordAggregate,
}

impl PartialEq for SourceState {
    fn eq(&self, other: &Self) -> bool {
        self.source.exact_descriptor_eq(&other.source) && self.aggregate == other.aggregate
    }
}

impl Eq for SourceState {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestCursor {
    generation_id: String,
    offset: usize,
}

impl SourceManifestCursor {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn offset(&self) -> usize {
        self.offset
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestPage {
    pub generation_id: String,
    pub items: Vec<SourceState>,
    pub next_cursor: Option<SourceManifestCursor>,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDeltaCursor {
    base_generation_id: String,
    target_generation_id: String,
    base_offset: usize,
    target_offset: usize,
}

impl SourceDeltaCursor {
    pub fn base_generation_id(&self) -> &str {
        &self.base_generation_id
    }

    pub fn target_generation_id(&self) -> &str {
        &self.target_generation_id
    }

    pub fn base_offset(&self) -> usize {
        self.base_offset
    }

    pub fn target_offset(&self) -> usize {
        self.target_offset
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDelta {
    Added(SourceState),
    Replaced {
        previous: Box<SourceState>,
        current: SourceState,
    },
    Removed(SourceState),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDeltaPage {
    pub base_generation_id: String,
    pub target_generation_id: String,
    pub items: Vec<SourceDelta>,
    pub scanned_sources: usize,
    pub next_cursor: Option<SourceDeltaCursor>,
    pub terminal: bool,
}

/// Exclusive cursor for one exact source in one exact immutable generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRecordPageCursor {
    generation_id: String,
    source: SourceKey,
    after: StableEntityId,
}

impl CoreRecordPageCursor {
    fn from_query(cursor: SourceEventCursor) -> Self {
        Self {
            generation_id: cursor.generation_id().to_owned(),
            source: cursor.source().clone(),
            after: cursor.after(),
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One decoded complete Core record and the byte-exact stored JSON that was
/// validated to produce it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRecordPageItem {
    pub core_record: CoreRecord,
    pub stored_json: Vec<u8>,
}

/// A bounded, serializable, replayable page from one exact generation/source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRecordPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<CoreRecordPageItem>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<CoreRecordPageCursor>,
    pub terminal: bool,
}

pub struct CoreRecordStream<'a> {
    snapshot: &'a CoreSnapshot,
    source: SourceKey,
    page_items: usize,
    budget: CoreEventPageBudget,
    cursor: Option<CoreRecordPageCursor>,
    buffered: VecDeque<CoreRecord>,
    terminal: bool,
    failed: bool,
}

impl Iterator for CoreRecordStream<'_> {
    type Item = Result<CoreRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(record) = self.buffered.pop_front() {
            return Some(Ok(record));
        }
        if self.terminal || self.failed {
            return None;
        }
        let page = match self.snapshot.record_page(
            &self.source,
            self.cursor.as_ref(),
            self.page_items,
            self.budget,
        ) {
            Ok(page) => page,
            Err(error) => {
                self.failed = true;
                return Some(Err(error));
            }
        };
        self.terminal = page.terminal;
        self.cursor = page.next_cursor;
        self.buffered
            .extend(page.items.into_iter().map(|item| item.core_record));
        if let Some(record) = self.buffered.pop_front() {
            return Some(Ok(record));
        }
        if self.terminal {
            return None;
        }
        self.failed = true;
        Some(Err(SnapshotError::Corrupt(
            "nonterminal record page made no progress".to_owned(),
        )))
    }
}

fn map_generation_root_error(
    error: ctx_history_index_generation::GenerationError,
) -> SnapshotError {
    SnapshotError::UnsafePath(error.to_string())
}

fn map_generation_error(
    error: ctx_history_index_generation::GenerationError,
    generation_id: &str,
) -> SnapshotError {
    use ctx_history_index_generation::GenerationError;
    match error {
        GenerationError::InvalidGenerationId
        | GenerationError::GenerationRetentionLeaseTargetNotRetained { .. }
        | GenerationError::MissingActiveGenerationPointer
        | GenerationError::MissingManifest(_) => SnapshotError::NotFound(generation_id.to_owned()),
        GenerationError::GenerationRetentionLeaseConflict { .. } => {
            SnapshotError::LeaseConflict(generation_id.to_owned())
        }
        error @ GenerationError::ConcurrentGenerationChange => {
            SnapshotError::ConcurrentGenerationChange(error.to_string())
        }
        GenerationError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            SnapshotError::UnsafePath("permission denied".to_owned())
        }
        error => SnapshotError::Corrupt(error.to_string()),
    }
}

fn map_index_error(
    error: IndexError,
    generation_id: &str,
    expected: &SnapshotContract,
) -> SnapshotError {
    match error {
        IndexError::MissingActiveGenerationPointer
        | IndexError::PinnedGenerationNotRetained { .. }
        | IndexError::MissingManifest(_) => SnapshotError::NotFound(generation_id.to_owned()),
        IndexError::GenerationRetentionLeaseConflict { .. } => {
            SnapshotError::LeaseConflict(generation_id.to_owned())
        }
        error @ IndexError::ConcurrentGenerationChange => {
            SnapshotError::ConcurrentGenerationChange(error.to_string())
        }
        IndexError::CoreRecordContractMismatch { actual, .. } => {
            SnapshotError::FingerprintMismatch {
                expected: expected.core_record_fingerprint.clone(),
                actual,
            }
        }
        IndexError::InvalidSourceEventPageSize { .. }
        | IndexError::InvalidCoreEventPageByteLimit { .. } => {
            SnapshotError::Bounds(error.to_string())
        }
        IndexError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            SnapshotError::UnsafePath("permission denied".to_owned())
        }
        error => SnapshotError::Corrupt(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
