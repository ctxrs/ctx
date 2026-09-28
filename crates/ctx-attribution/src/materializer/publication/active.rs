use std::path::Path;

use crate::core_materialization::CoreStoreError;
use crate::graph::segment::{SegmentManifest, SegmentRef, SegmentStore};
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
use super::super::model::{ManifestRoles, SourceStateSegment};
use super::super::source_inventory::{SourceInventory, SourceMutationRuns};
use super::io::{open_event_index, read_json_segment};
use ctx_attribution_index::materialization::event_proofs::EventProofs;

pub(crate) struct ActiveGeneration {
    pub(crate) manifest: SegmentManifest,
    pub(crate) completed: SegmentCompletedControl,
    pub(crate) sources: SourceInventory,
    pub(crate) requires_clean_rebuild: bool,
    event_indexes: Vec<CachedEventIndex>,
    event_proofs: Option<(String, EventProofs)>,
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
        SourceInventory::new(&std::env::temp_dir())?
    } else {
        sources
    };
    Ok(Some(ActiveGeneration {
        manifest,
        completed,
        sources,
        requires_clean_rebuild,
        event_indexes,
        event_proofs: None,
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
    SourceInventory,
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
            SourceInventory::new(&std::env::temp_dir())?,
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

    // Decode one metadata segment at a time into disposable indexed storage.
    // Event rows stay in their existing immutable, lazily opened segments.
    let mut completed = None;
    let mut source_runs = SourceMutationRuns::new(&std::env::temp_dir())?;
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
        source_runs.push_run(segment.mutations)?;
    }
    let sources = source_runs.finish(&std::env::temp_dir())?;
    let completed = completed.ok_or(SegmentMaterializerError::Corrupt(
        "active source control is missing",
    ))?;
    validate_completed(&manifest, &completed)?;
    completed
        .head
        .as_ref()
        .ok_or(SegmentMaterializerError::Corrupt(
            "active source control has no Core head",
        ))?
        .validate_source_snapshot(sources.snapshot())
        .map_err(|_| SegmentMaterializerError::Corrupt("active source metadata is incomplete"))?;
    Ok(Some((
        manifest,
        completed,
        sources,
        roles,
        requires_clean_rebuild,
    )))
}

fn event_proofs(active: &ActiveGeneration) -> Result<&EventProofs, SegmentMaterializerError> {
    active
        .event_proofs
        .as_ref()
        .map(|(_, proofs)| proofs)
        .ok_or(SegmentMaterializerError::Conflict)
}

pub(crate) fn prepare_event_proofs(
    active: &mut ActiveGeneration,
    root: &Path,
    owner: &str,
    include: &dyn Fn(
        &SourceInventory,
        &ctx_attribution_index::EventIndexSource,
    ) -> Result<
        bool,
        ctx_attribution_index::materialization::MaterializationIndexError,
    >,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<(), SegmentMaterializerError> {
    if active
        .event_proofs
        .as_ref()
        .is_none_or(|(prior, _)| prior != owner)
    {
        active.event_proofs = None;
        let scratch_root = std::env::temp_dir();
        let mut proofs = EventProofs::new(&scratch_root)?;
        let mut runs = Vec::new();
        for index in &active.event_indexes {
            let mut reader = open_event_index(root, &index.reference)?;
            #[cfg(test)]
            {
                active.event_index_reader_open_count += 1;
            }
            runs.push((
                index.publication_generation,
                proofs.append_index(
                    &mut reader,
                    &|source| include(&active.sources, source),
                    cancelled,
                )?,
            ));
        }
        active.event_proofs = Some((
            owner.to_owned(),
            proofs.merge(&scratch_root, &runs, cancelled)?,
        ));
    }
    Ok(())
}

pub(crate) fn active_event_page(
    active: &mut ActiveGeneration,
    _root: &Path,
    requested_source: &SourceKey,
    after: Option<StableEntityId>,
    maximum: usize,
    force_replacement: bool,
) -> Result<(Vec<CoreEventState>, Vec<ObservedEvent>, bool), SegmentMaterializerError> {
    let source_id = super::super::model::source_storage_id(requested_source);
    let active_source = active
        .sources
        .get(&source_id)?
        .ok_or(SegmentMaterializerError::Conflict)?;
    if active_source.state.source.identity() != requested_source.identity() {
        return Err(SegmentMaterializerError::Conflict);
    }
    if after.is_some_and(|event| event.source_digest() != requested_source.identity().digest()) {
        return Err(SegmentMaterializerError::Conflict);
    }
    let (entries, terminal) =
        event_proofs(active)?.page(requested_source.identity().digest(), after, maximum)?;
    let mut states = Vec::with_capacity(entries.len());
    let mut observed = Vec::with_capacity(entries.len());
    for state in entries {
        let descriptor_changed =
            state.event_id.source_descriptor_digest() != requested_source.exact_descriptor_digest();
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
    }
    Ok((states, observed, terminal))
}

pub(crate) fn lookup_event_metadata(
    active: &mut ActiveGeneration,
    _root: &Path,
    requested_source: &SourceKey,
    event_id: StableEntityId,
) -> Result<Option<ObservedEvent>, SegmentMaterializerError> {
    if event_id.source_digest() != requested_source.identity().digest() {
        return Err(SegmentMaterializerError::Conflict);
    }
    let state = event_proofs(active)?.lookup(requested_source.identity().digest(), event_id)?;
    Ok(state.map(|state| ObservedEvent {
        source_id: super::super::model::source_storage_id(requested_source),
        event_id,
        direct_session_id: state.lineage.session_id,
        root_session_id: state.lineage.root_session_id,
        event_sequence: state.event_sequence,
        core_record_sha256: state.core_record_sha256,
        event_output_root: state.event_output_root,
        coverage: state.coverage,
    }))
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
