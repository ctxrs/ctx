use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use crate::core_materialization::CoreStoreError;
use crate::graph::segment::{
    EventIndexEntry, EventIndexReader, EventIndexSource, SegmentManifest, SegmentRef, SegmentStore,
};
use crate::graph::segment_graph::{
    PRE_DIRECT_MATERIALIZATION_SEGMENT_SCHEMA_IDENTITY,
    PRE_STATIC_SHELL_QUOTING_SEGMENT_SCHEMA_IDENTITY, SEGMENT_EVIDENCE_IDENTITY,
    SEGMENT_ORDERING_IDENTITY, SEGMENT_SCHEMA_IDENTITY,
};
use crate::graph::segment_state::{
    EMPTY_PUBLICATION_SEMANTICS_SHA256, SegmentCompletedControl, SegmentCoreCoverage,
};
use crate::protocol::{CoreEventState, SourceKey, StableEntityId};

use super::super::SegmentMaterializerError;
use super::super::model::{
    ActiveSource, ManifestRoles, SourceStateSegment, source_map_from_mutations,
};
use super::io::{open_event_index, read_json_segment};

pub(crate) struct ActiveGeneration {
    pub(crate) manifest: SegmentManifest,
    pub(crate) completed: SegmentCompletedControl,
    pub(crate) sources: BTreeMap<String, ActiveSource>,
    pub(crate) requires_clean_rebuild: bool,
    event_indexes: Vec<CachedEventIndex>,
    event_index_readers: BTreeMap<usize, EventIndexReader>,
    event_index_reader_lru: VecDeque<usize>,
    #[cfg(test)]
    event_index_reader_open_count: usize,
}

pub(crate) struct ActiveControlSnapshot {
    pub(crate) manifest_generation: String,
    pub(crate) requires_clean_rebuild: bool,
    pub(crate) completed: SegmentCompletedControl,
}

struct CachedEventIndex {
    publication_generation: u64,
    reference: SegmentRef,
}

#[cfg(test)]
pub(crate) fn event_index_reader_open_count(active: &ActiveGeneration) -> usize {
    active.event_index_reader_open_count
}

/// One opened high-cardinality EventIndex can retain a large checked
/// session dictionary. Bound both descriptors and resident dictionaries
/// independently of the manifest's segment count.
const MAX_ACTIVE_EVENT_INDEX_READERS: usize = 4;
/// Across the manifest's bounded 4,096 segment slots, retain at most roughly
/// 16K decoded merge entries while amortizing each checked reader open
/// across at least four sequential rows.
const MAX_ACTIVE_EVENT_MERGE_FRONTIER_ITEMS: usize = 16 * 1024;
const MIN_ACTIVE_EVENT_CURSOR_PAGE_ITEMS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedEvent {
    pub(crate) source_id: String,
    pub(crate) event_id: StableEntityId,
    pub(crate) direct_session_id: StableEntityId,
    pub(crate) root_session_id: Option<StableEntityId>,
    pub(crate) event_sequence: u64,
    pub(crate) core_record_sha256: String,
    pub(crate) event_output_root: String,
    pub(crate) coverage: SegmentCoreCoverage,
}

pub(crate) fn load_active(
    root: &Path,
) -> Result<Option<ActiveGeneration>, SegmentMaterializerError> {
    load_active_with_policy(root, false)
}

/// Checks a predecessor schema as control-only state so the current
/// writer can request a clean rebuild without opening incompatible EventIndex
/// containers.
pub(crate) fn load_active_for_materializer(
    root: &Path,
) -> Result<Option<ActiveGeneration>, SegmentMaterializerError> {
    load_active_with_policy(root, true)
}

fn load_active_with_policy(
    root: &Path,

    allow_previous_control: bool,
) -> Result<Option<ActiveGeneration>, SegmentMaterializerError> {
    let Some((manifest, completed, sources, roles, requires_clean_rebuild)) =
        load_active_metadata(root, allow_previous_control)?
    else {
        return Ok(None);
    };
    let event_indexes = if requires_clean_rebuild {
        Vec::new()
    } else {
        roles
            .event_indexes
            .iter()
            .map(|reference| CachedEventIndex {
                publication_generation: reference.publication_generation,
                reference: reference.clone(),
            })
            .collect()
    };
    // The predecessor controls were checked above, but their projection
    // is not a baseline for source reconciliation after its indexes are dropped.
    let sources = if requires_clean_rebuild {
        BTreeMap::new()
    } else {
        sources
    };
    Ok(Some(ActiveGeneration {
        manifest,
        completed,
        sources,
        requires_clean_rebuild,
        event_indexes,
        event_index_readers: BTreeMap::new(),
        event_index_reader_lru: VecDeque::new(),
        #[cfg(test)]
        event_index_reader_open_count: 0,
    }))
}

pub(crate) fn load_active_control(
    root: &Path,
) -> Result<Option<ActiveControlSnapshot>, SegmentMaterializerError> {
    Ok(load_active_metadata(root, true)?.map(
        |(manifest, completed, _sources, _roles, requires_clean_rebuild)| ActiveControlSnapshot {
            manifest_generation: manifest.generation_id,
            requires_clean_rebuild,
            completed,
        },
    ))
}

type ActiveMetadata = (
    SegmentManifest,
    SegmentCompletedControl,
    BTreeMap<String, ActiveSource>,
    ManifestRoles,
    bool,
);

fn load_active_metadata(
    root: &Path,

    allow_previous_control: bool,
) -> Result<Option<ActiveMetadata>, SegmentMaterializerError> {
    let active = SegmentStore::new(root).load_active()?;
    let Some(manifest) = active else {
        return Ok(None);
    };
    // A valid manifest remains the CAS predecessor even when its disposable
    // index schema is unsupported. Rebuild exclusively from retained Core;
    // never decode incompatible source/event containers as current data.
    if allow_previous_control
        && (manifest.schema_identity != SEGMENT_SCHEMA_IDENTITY
            || manifest.evidence_identity != SEGMENT_EVIDENCE_IDENTITY
            || manifest.ordering_identity != SEGMENT_ORDERING_IDENTITY)
        && !schema_identity_requires_clean_rebuild(&manifest.schema_identity)
    {
        let mut completed = super::super::model::initial_completed_control();
        completed.graph_generation = manifest.graph_generation;
        completed.event_count = manifest.core_receipt.event_count;
        completed.receipt = Some(manifest.core_receipt.clone());
        completed.schema_contract = manifest.schema_identity.clone();
        completed.evidence_contract = manifest.evidence_identity.clone();
        // Even ordering-only mismatches require rebuilding, including empty data.
        completed.materializer_revision = "incompatible-index".to_owned();
        return Ok(Some((
            manifest,
            completed,
            BTreeMap::new(),
            ManifestRoles::default(),
            true,
        )));
    }
    let requires_clean_rebuild = validate_manifest_identities(&manifest, allow_previous_control)?;
    let roles = ManifestRoles::classify(&manifest.segments).map_err(map_core)?;
    if roles.sources.is_empty() {
        return Err(SegmentMaterializerError::Corrupt(
            "active manifest metadata roles are incompatible",
        ));
    }

    // Source/control metadata is bounded by 16,384 sources and is the only
    // corpus metadata decoded eagerly. Event rows remain plain and are
    // reached lazily through fixed-width binary search and paging.
    let mut completed = None;
    let mut mutations = Vec::new();
    for reference in &roles.sources {
        let segment = read_json_segment::<SourceStateSegment>(root, reference)?;
        if completed
            .as_ref()
            .is_some_and(|prior| prior != &segment.completed)
        {
            return Err(SegmentMaterializerError::Corrupt(
                "active source controls disagree",
            ));
        }
        completed = Some(segment.completed.clone());
        mutations.extend(segment.mutations);
    }
    let completed = completed.ok_or(SegmentMaterializerError::Corrupt(
        "active source control is missing",
    ))?;
    validate_completed(&manifest, &completed)?;
    let sources = source_map_from_mutations(mutations).map_err(map_core)?;
    let states = sources
        .values()
        .map(|source| source.state.clone())
        .collect::<Vec<_>>();
    completed
        .head
        .as_ref()
        .ok_or(SegmentMaterializerError::Corrupt(
            "active source control has no Core head",
        ))?
        .validate_sources(&states)
        .map_err(|_| SegmentMaterializerError::Corrupt("active source metadata is incomplete"))?;
    Ok(Some((
        manifest,
        completed,
        sources,
        roles,
        requires_clean_rebuild,
    )))
}

fn with_event_index_reader<T>(
    active: &mut ActiveGeneration,
    reader_index: usize,
    root: &Path,

    operation: impl FnOnce(&mut EventIndexReader) -> Result<T, SegmentMaterializerError>,
) -> Result<T, SegmentMaterializerError> {
    if !active.event_index_readers.contains_key(&reader_index) {
        if active.event_index_readers.len() == MAX_ACTIVE_EVENT_INDEX_READERS {
            let evicted = active.event_index_reader_lru.pop_front().ok_or(
                SegmentMaterializerError::Corrupt("active event reader LRU is empty"),
            )?;
            if active.event_index_readers.remove(&evicted).is_none() {
                return Err(SegmentMaterializerError::Corrupt(
                    "active event reader LRU is inconsistent",
                ));
            }
        }
        let reference = active
            .event_indexes
            .get(reader_index)
            .ok_or(SegmentMaterializerError::Corrupt(
                "active event reader index is invalid",
            ))?
            .reference
            .clone();
        let reader = open_event_index(root, &reference)?;
        active.event_index_readers.insert(reader_index, reader);
        #[cfg(test)]
        {
            active.event_index_reader_open_count =
                active.event_index_reader_open_count.saturating_add(1);
        }
    }
    active
        .event_index_reader_lru
        .retain(|cached| *cached != reader_index);
    active.event_index_reader_lru.push_back(reader_index);
    let reader = active.event_index_readers.get_mut(&reader_index).ok_or(
        SegmentMaterializerError::Corrupt("active event reader cache did not initialize"),
    )?;
    operation(reader)
}

pub(crate) fn active_event_page(
    active: &mut ActiveGeneration,
    root: &Path,

    requested_source: &SourceKey,
    after: Option<StableEntityId>,
    maximum: usize,
    force_replacement: bool,
) -> Result<(Vec<CoreEventState>, Vec<ObservedEvent>, bool), SegmentMaterializerError> {
    let source_id = super::super::model::source_storage_id(requested_source);
    let active_source = active
        .sources
        .get(&source_id)
        .ok_or(SegmentMaterializerError::Conflict)?;
    if active_source.state.source.identity() != requested_source.identity() {
        return Err(SegmentMaterializerError::Conflict);
    }
    let mut cursors = Vec::new();
    // Keep the decoded merge frontier bounded independently of corpus size,
    // while amortizing reader reopen/authentication when a source spans more
    // layers than the resident-reader limit. One head per layer is the minimum
    // required for a correct ordered merge; the four-row floor prevents the
    // maximum valid manifest fanout from degenerating into per-event opens.
    let cursor_page_items = MAX_ACTIVE_EVENT_MERGE_FRONTIER_ITEMS
        .checked_div(active.event_indexes.len().max(1))
        .unwrap_or(MIN_ACTIVE_EVENT_CURSOR_PAGE_ITEMS)
        .clamp(
            MIN_ACTIVE_EVENT_CURSOR_PAGE_ITEMS,
            ctx_attribution_index::MAX_EVENT_INDEX_PAGE_ITEMS,
        );
    for reader_index in 0..active.event_indexes.len() {
        let publication_generation = active
            .event_indexes
            .get(reader_index)
            .ok_or(SegmentMaterializerError::Corrupt(
                "event reader cache index is invalid",
            ))?
            .publication_generation;
        let cursor = with_event_index_reader(active, reader_index, root, |reader| {
            let Some(stored) = reader.source_by_storage_key(&source_id)?.cloned() else {
                return Ok(None);
            };
            if stored.source.identity() != requested_source.identity() {
                return Err(SegmentMaterializerError::Corrupt(
                    "event index source identity does not match its storage key",
                ));
            }
            let mut cursor = EventCursor::new(
                reader_index,
                publication_generation,
                stored,
                after,
                cursor_page_items,
            )?;
            cursor.ensure_head(reader)?;
            Ok(Some(cursor))
        })?;
        if let Some(cursor) = cursor {
            cursors.push(cursor);
        }
    }

    let mut states = Vec::with_capacity(maximum);
    let mut observed = Vec::with_capacity(maximum);
    let mut has_more = false;
    loop {
        for cursor in &mut cursors {
            if cursor.requires_read() {
                with_event_index_reader(active, cursor.reader_index, root, |reader| {
                    cursor.ensure_head(reader)
                })?;
            }
        }
        let Some(next_digest) = cursors.iter().filter_map(EventCursor::head_digest).min() else {
            break;
        };
        let highest_layer = cursors
            .iter()
            .filter(|cursor| cursor.head_digest() == Some(next_digest))
            .map(|cursor| cursor.publication_generation)
            .max()
            .ok_or(SegmentMaterializerError::Corrupt(
                "event merge has no publication layer",
            ))?;
        let mut winning_state = None;
        let mut same_layer_tombstone = false;
        for cursor in &cursors {
            if cursor.head_digest() != Some(next_digest)
                || cursor.publication_generation != highest_layer
            {
                continue;
            }
            match cursor.head().ok_or(SegmentMaterializerError::Corrupt(
                "event merge winner has no entry",
            ))? {
                EventIndexEntry::State {
                    state,
                    shadows_older,
                } => {
                    if winning_state.as_ref().is_some_and(|prior| prior != state) {
                        return Err(SegmentMaterializerError::Corrupt(
                            "same-layer event states conflict",
                        ));
                    }
                    winning_state = Some(state.clone());
                    same_layer_tombstone |= *shadows_older;
                }
                EventIndexEntry::Tombstone(_) => same_layer_tombstone = true,
            }
        }
        for cursor in &mut cursors {
            if cursor.head_digest() == Some(next_digest) {
                cursor.pop_head();
            }
        }
        if let Some(state) = winning_state {
            if states.len() == maximum {
                has_more = true;
                break;
            }
            let descriptor_changed = state.event_id.source_descriptor_digest()
                != requested_source.exact_descriptor_digest();
            let event_id = remap_event_identity(state.event_id, requested_source)?;
            states.push(CoreEventState {
                event_id,
                core_record_sha256: state.core_record_sha256.clone(),
                requires_replacement: force_replacement || descriptor_changed,
            });
            observed.push(ObservedEvent {
                source_id: source_id.clone(),
                event_id,
                direct_session_id: state.lineage.session_id,
                root_session_id: state.lineage.root_session_id,
                event_sequence: state.event_sequence,
                core_record_sha256: state.core_record_sha256,
                event_output_root: state.event_output_root,
                coverage: state.coverage,
            });
        } else if !same_layer_tombstone {
            return Err(SegmentMaterializerError::Corrupt(
                "highest event layer has no state or tombstone",
            ));
        }
    }
    Ok((states, observed, !has_more))
}

pub(crate) fn lookup_event_metadata(
    active: &mut ActiveGeneration,
    root: &Path,

    requested_source: &SourceKey,
    event_id: StableEntityId,
) -> Result<Option<ObservedEvent>, SegmentMaterializerError> {
    let source_id = super::super::model::source_storage_id(requested_source);
    let mut highest_layer = None;
    let mut winning_state = None;
    let mut tombstoned = false;
    for reader_index in 0..active.event_indexes.len() {
        let publication_generation = active
            .event_indexes
            .get(reader_index)
            .ok_or(SegmentMaterializerError::Corrupt(
                "event reader cache index is invalid",
            ))?
            .publication_generation;
        let entry = with_event_index_reader(active, reader_index, root, |reader| {
            let Some(stored) = reader.source_by_storage_key(&source_id)?.cloned() else {
                return Ok(None);
            };
            let stored_id = remap_event_identity(event_id, &stored.source)?;
            reader.lookup(&stored, stored_id).map_err(Into::into)
        })?;
        let Some(entry) = entry else {
            continue;
        };
        match highest_layer {
            Some(layer) if publication_generation < layer => continue,
            Some(layer) if publication_generation == layer => {}
            _ => {
                highest_layer = Some(publication_generation);
                winning_state = None;
                tombstoned = false;
            }
        }
        match entry {
            EventIndexEntry::State { state, .. } => {
                if winning_state.as_ref().is_some_and(|prior| prior != &state) {
                    return Err(SegmentMaterializerError::Corrupt(
                        "same-layer event lookup states conflict",
                    ));
                }
                winning_state = Some(state);
            }
            EventIndexEntry::Tombstone(_) => tombstoned = true,
        }
    }
    if let Some(state) = winning_state {
        return Ok(Some(ObservedEvent {
            source_id,
            event_id,
            direct_session_id: state.lineage.session_id,
            root_session_id: state.lineage.root_session_id,
            event_sequence: state.event_sequence,
            core_record_sha256: state.core_record_sha256,
            event_output_root: state.event_output_root,
            coverage: state.coverage,
        }));
    }
    if tombstoned || highest_layer.is_none() {
        Ok(None)
    } else {
        Err(SegmentMaterializerError::Corrupt(
            "highest event lookup layer is empty",
        ))
    }
}

fn validate_manifest_identities(
    manifest: &SegmentManifest,
    allow_previous_control: bool,
) -> Result<bool, SegmentMaterializerError> {
    let requires_clean_rebuild = schema_identity_requires_clean_rebuild(&manifest.schema_identity);
    if (manifest.schema_identity != SEGMENT_SCHEMA_IDENTITY
        && !(allow_previous_control && requires_clean_rebuild))
        || manifest.evidence_identity != SEGMENT_EVIDENCE_IDENTITY
        || manifest.ordering_identity != SEGMENT_ORDERING_IDENTITY
    {
        return Err(SegmentMaterializerError::RebuildRequired);
    }
    Ok(requires_clean_rebuild)
}

fn schema_identity_requires_clean_rebuild(identity: &str) -> bool {
    identity == PRE_DIRECT_MATERIALIZATION_SEGMENT_SCHEMA_IDENTITY
        || identity == crate::graph::segment_graph::PRE_OPTIONAL_ROOT_SEGMENT_SCHEMA_IDENTITY
        || identity == PRE_STATIC_SHELL_QUOTING_SEGMENT_SCHEMA_IDENTITY
}

fn validate_completed(
    manifest: &SegmentManifest,
    completed: &SegmentCompletedControl,
) -> Result<(), SegmentMaterializerError> {
    if completed.graph_generation != manifest.graph_generation
        || completed.receipt.as_ref() != Some(&manifest.core_receipt)
        || completed.schema_contract != manifest.schema_identity
        || completed.evidence_contract != SEGMENT_EVIDENCE_IDENTITY
        || (manifest.schema_identity == SEGMENT_SCHEMA_IDENTITY
            && manifest.core_receipt.event_count != 0
            && completed.publication_semantics_sha256 == EMPTY_PUBLICATION_SEMANTICS_SHA256)
    {
        return Err(SegmentMaterializerError::Corrupt(
            "active source control does not match its manifest",
        ));
    }
    Ok(())
}

struct EventCursor {
    reader_index: usize,
    publication_generation: u64,
    source: EventIndexSource,
    after: Option<StableEntityId>,
    page_items: usize,
    entries: VecDeque<EventIndexEntry>,
    terminal_after_buffer: bool,
    exhausted: bool,
}

impl EventCursor {
    fn new(
        reader_index: usize,
        publication_generation: u64,
        source: EventIndexSource,
        after: Option<StableEntityId>,
        page_items: usize,
    ) -> Result<Self, SegmentMaterializerError> {
        let after = after
            .map(|event| remap_event_identity(event, &source.source))
            .transpose()?;
        Ok(Self {
            reader_index,
            publication_generation,
            source,
            after,
            page_items,
            entries: VecDeque::new(),
            terminal_after_buffer: false,
            exhausted: false,
        })
    }

    fn ensure_head(
        &mut self,
        reader: &mut EventIndexReader,
    ) -> Result<(), SegmentMaterializerError> {
        if !self.entries.is_empty() || self.exhausted {
            return Ok(());
        }
        if self.terminal_after_buffer {
            self.exhausted = true;
            return Ok(());
        }
        let page = reader.page(&self.source, self.after, self.page_items)?;
        self.terminal_after_buffer = page.terminal;
        if let Some(last) = page.entries.last() {
            self.after = Some(last.event_id());
        }
        self.entries = page.entries.into();
        if self.entries.is_empty() && self.terminal_after_buffer {
            self.exhausted = true;
        }
        Ok(())
    }

    fn requires_read(&mut self) -> bool {
        if !self.entries.is_empty() || self.exhausted {
            return false;
        }
        if self.terminal_after_buffer {
            self.exhausted = true;
            return false;
        }
        true
    }

    fn head(&self) -> Option<&EventIndexEntry> {
        self.entries.front()
    }

    fn head_digest(&self) -> Option<[u8; 32]> {
        self.head().map(|entry| entry.event_id().digest())
    }

    fn pop_head(&mut self) {
        let _ = self.entries.pop_front();
    }
}

pub(super) fn remap_event_identity(
    event: StableEntityId,
    source: &SourceKey,
) -> Result<StableEntityId, SegmentMaterializerError> {
    if event.source_digest() != source.identity().digest() {
        return Err(SegmentMaterializerError::Conflict);
    }
    if event.source_descriptor_digest() == source.exact_descriptor_digest() {
        return Ok(event);
    }
    let mut encoded =
        serde_json::to_value(event).map_err(|_| SegmentMaterializerError::Encoding)?;
    encoded
        .as_object_mut()
        .ok_or(SegmentMaterializerError::Encoding)?
        .insert(
            "source_descriptor_digest".to_owned(),
            serde_json::to_value(source.exact_descriptor_digest())
                .map_err(|_| SegmentMaterializerError::Encoding)?,
        );
    serde_json::from_value(encoded).map_err(|_| SegmentMaterializerError::Encoding)
}

fn map_core(error: CoreStoreError) -> SegmentMaterializerError {
    match error {
        CoreStoreError::Conflict => SegmentMaterializerError::Conflict,
        CoreStoreError::Bounds => SegmentMaterializerError::Bounds,
        CoreStoreError::RebuildRequired => {
            SegmentMaterializerError::Corrupt("active segment requested an impossible rebuild")
        }
        CoreStoreError::Backend => {
            SegmentMaterializerError::Corrupt("active segment state is invalid")
        }
    }
}

#[cfg(test)]
#[path = "active_tests.rs"]
mod tests;
