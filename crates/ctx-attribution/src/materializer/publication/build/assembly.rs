use std::path::Path;

use super::super::super::model::ActiveSource;
use super::super::super::source_inventory::SourceInventory;
use crate::graph::segment::{
    EventLineageTables, MANIFEST_SCHEMA_VERSION, ManifestCandidate, SegmentManifest, SegmentRef,
    SegmentStore, random_generation_id,
};
use crate::graph::segment_state::{SegmentCandidateControl, SegmentCompletedControl};
use crate::protocol::{CoreMaterializationReceipt, CoreSourceDelta};

use super::super::super::SegmentMaterializerError;
use super::super::super::model::{
    MATERIALIZER_SOURCE_ROLE, MAX_METADATA_SEGMENT_BYTES,
    MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES, MaterializerMetrics, SourceMutation,
    SourceStateSegment, is_obsolete_derived_role,
};
use super::super::super::publication_plan::PublicationReferenceShape;
use super::super::super::staging::StagedPage;
use super::super::active::ActiveGeneration;
use super::encoding::PublicationSink;
use super::planning::publication_worker_limit;

/// One bounded in-process publication attempt. Projected output may be written
/// to candidate-owned immutable segments as pages arrive, but no input body is
/// persisted and no segment becomes active before `finish`.
pub(crate) struct DirectCandidate {
    sink: PublicationSink,
    pub(crate) source_states: SourceInventory,
    next_removal_source: usize,
}

impl DirectCandidate {
    pub(crate) fn new(
        root: &Path,

        candidate: &SegmentCandidateControl,
        active: Option<&ActiveGeneration>,
    ) -> Result<Self, SegmentMaterializerError> {
        let initial_shape = super::super::super::publication_plan::direct_reference_shape(
            candidate,
            &candidate.publication_reference_plan,
            active,
        )?;
        let reference_limit = super::super::super::model::MAX_MANIFEST_SEGMENTS
            .checked_sub(initial_shape.retained_references()?)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if reference_limit < initial_shape.candidate_references()? {
            return Err(SegmentMaterializerError::Bounds);
        }
        Ok(Self {
            sink: PublicationSink::new(
                root,
                &candidate.materialization_id,
                candidate.graph_generation,
                publication_worker_limit(root)?,
                reference_limit,
            )?,
            source_states: SourceInventory::new(root)?,
            next_removal_source: 0,
        })
    }

    pub(crate) fn observe_source_page(
        &mut self,
        deltas: &[CoreSourceDelta],
        revision: &str,
    ) -> Result<(), SegmentMaterializerError> {
        for delta in deltas {
            let CoreSourceDelta::Present(state) = delta else {
                return Err(SegmentMaterializerError::Conflict);
            };
            self.source_states.push(&ActiveSource {
                state: state.clone(),
                materializer_revision: revision.to_owned(),
            })?;
        }
        Ok(())
    }

    pub(crate) fn source_count(&self) -> usize {
        self.source_states.len()
    }

    pub(crate) fn removal_page(
        &mut self,
        active: Option<&SourceInventory>,
        limit: usize,
    ) -> Result<(Vec<crate::protocol::CoreSourceRemoval>, bool), SegmentMaterializerError> {
        let Some(active) = active else {
            return Ok((Vec::new(), true));
        };
        let mut removed = Vec::new();
        while self.next_removal_source < active.len() && removed.len() < limit {
            let source = active.at(self.next_removal_source)?;
            self.next_removal_source += 1;
            if self
                .source_states
                .get_identity(source.state.source.identity().digest())?
                .is_none()
            {
                removed.push(crate::protocol::CoreSourceRemoval {
                    source: source.state.source,
                });
            }
        }
        Ok((removed, self.next_removal_source == active.len()))
    }

    pub(crate) fn stage_pages(
        &mut self,
        candidate: &mut SegmentCandidateControl,
        pages: Vec<StagedPage>,
    ) -> Result<(), SegmentMaterializerError> {
        if pages.is_empty() || pages.len() > super::super::super::model::MAX_DIRECT_BATCH_PAGES {
            return Err(SegmentMaterializerError::Bounds);
        }
        for page in pages {
            let plan = super::super::super::publication_plan::plan_direct_pages(
                candidate,
                std::slice::from_ref(&page),
            )?;
            let index_rollovers = plan
                .event_index_segments
                .checked_sub(candidate.publication_reference_plan.event_index_segments)
                .ok_or(SegmentMaterializerError::Corrupt(
                    "event index publication plan moved backward",
                ))?;
            if index_rollovers > 1 {
                return Err(SegmentMaterializerError::Corrupt(
                    "one event page requires multiple index rollovers",
                ));
            }
            let publication_semantics_sha256 =
                page.advance_publication_semantics(&candidate.publication_semantics_sha256)?;
            self.sink
                .push_staged(&self.source_states, page, index_rollovers == 1)?;
            candidate.publication_reference_plan = plan;
            candidate.publication_semantics_sha256 = publication_semantics_sha256;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Publication consumes the candidate, active base, completion receipt, and metrics under the supplied root and cancellation owner."
    )]
    pub(crate) fn finish(
        mut self,
        root: &Path,

        candidate: &SegmentCandidateControl,
        active: Option<&ActiveGeneration>,
        completed: SegmentCompletedControl,
        receipt: CoreMaterializationReceipt,
        metrics: &mut MaterializerMetrics,
        cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<SegmentManifest, SegmentMaterializerError> {
        validate_candidate(candidate, &completed)?;
        let mut shape = super::super::super::publication_plan::direct_reference_shape(
            candidate,
            &candidate.publication_reference_plan,
            active,
        )?;
        shape.candidate.source = self
            .source_states
            .metadata_segment_count(source_payload_budget(&completed)?)?;
        if !shape.fits_manifest()? {
            return self
                .sink
                .finish_transaction(Err(SegmentMaterializerError::Bounds));
        }
        let source_states = self.source_states;
        let result = finish_candidate(
            root,
            candidate,
            active,
            completed,
            receipt,
            metrics,
            shape,
            &mut self.sink,
            source_states,
            cancelled,
        );
        self.sink.finish_transaction(result)
    }

    pub(crate) fn abort(mut self) -> Result<(), SegmentMaterializerError> {
        self.sink.abort()
    }
}

fn validate_candidate(
    candidate: &SegmentCandidateControl,
    completed: &SegmentCompletedControl,
) -> Result<(), SegmentMaterializerError> {
    if candidate.publication_semantics_sha256 != completed.publication_semantics_sha256 {
        return Err(SegmentMaterializerError::Corrupt(
            "completed publication semantics digest conflicts with candidate",
        ));
    }
    if candidate.schema_contract != crate::graph::segment_graph::SEGMENT_SCHEMA_IDENTITY
        || completed.schema_contract != candidate.schema_contract
    {
        return Err(SegmentMaterializerError::Corrupt(
            "publication candidate schema identity is incompatible",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_candidate(
    root: &Path,

    candidate: &SegmentCandidateControl,
    active: Option<&ActiveGeneration>,
    completed: SegmentCompletedControl,
    receipt: CoreMaterializationReceipt,
    metrics: &mut MaterializerMetrics,
    shape: PublicationReferenceShape,
    sink: &mut PublicationSink,
    source_states: SourceInventory,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<SegmentManifest, SegmentMaterializerError> {
    candidate
        .head
        .validate_source_snapshot(source_states.snapshot())
        .map_err(|_| SegmentMaterializerError::Conflict)?;
    sink.seal()?;
    sink.wait_for_publication_jobs()?;
    write_source_segments(&completed, source_states, sink)?;
    let retained = if candidate.force_projection_rebuild {
        Vec::new()
    } else {
        active
            .map(|generation| {
                generation
                    .manifest
                    .segments
                    .iter()
                    .filter(|reference| {
                        reference.role != MATERIALIZER_SOURCE_ROLE
                            && !is_obsolete_derived_role(reference.role)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    ensure_required_roles(sink, &retained)?;
    if super::super::super::publication_plan::classify_references(&sink.references)?
        != shape.candidate
        || super::super::super::publication_plan::classify_references(&retained)? != shape.retained
    {
        return Err(SegmentMaterializerError::Corrupt(
            "publication output conflicts with its preflight reference shape",
        ));
    }
    sink.references.extend(retained);
    if sink.references.len() != shape.total_references()? {
        return Err(SegmentMaterializerError::Corrupt(
            "publication reference count conflicts with its preflight shape",
        ));
    }
    sink.finish_publication_jobs(metrics)?;
    for (ordinal, reference) in sink.references.iter_mut().enumerate() {
        reference.ordinal = u32::try_from(ordinal).map_err(|_| SegmentMaterializerError::Bounds)?;
    }
    let identities = super::super::super::model::manifest_identities();
    let manifest = SegmentManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generation_id: random_generation_id()?,
        prior_generation_id: active.map(|generation| generation.manifest.generation_id.clone()),
        graph_generation: candidate.graph_generation,
        core_receipt: receipt.clone(),
        materializer_identity: receipt.materializer_revision.clone(),
        schema_identity: identities.schema.to_owned(),
        evidence_identity: identities.evidence.to_owned(),
        ordering_identity: identities.ordering.to_owned(),
        segments: std::mem::take(&mut sink.references),
        predecessor_segments: active
            .map(|generation| generation.manifest.segments.clone())
            .unwrap_or_default(),
    };
    let store = SegmentStore::new(root);
    let staged = store.stage_manifest(&manifest)?;
    let fence = sink.before_manifest_activation().and_then(|()| {
        if cancelled.is_some_and(|check| check()) {
            Err(SegmentMaterializerError::Cancelled)
        } else {
            Ok(())
        }
    });
    if let Err(error) = fence {
        super::super::super::locking::remove_private_file_if_exists(staged.path())?;
        return Err(error);
    }
    activate_manifest(&store, staged)
}

fn activate_manifest(
    store: &SegmentStore,

    staged: ManifestCandidate,
) -> Result<SegmentManifest, SegmentMaterializerError> {
    store.publish_candidate(staged).map_err(Into::into)
}

fn source_payload_budget(
    completed: &SegmentCompletedControl,
) -> Result<usize, SegmentMaterializerError> {
    let envelope = SourceStateSegment {
        completed: completed.clone(),
        mutations: Vec::new(),
    }
    .encode()?
    .len();
    MAX_METADATA_SEGMENT_BYTES
        .checked_sub(envelope)
        .ok_or(SegmentMaterializerError::Bounds)
}

fn write_source_segments(
    completed: &SegmentCompletedControl,
    sources: SourceInventory,
    sink: &mut PublicationSink,
) -> Result<(), SegmentMaterializerError> {
    let maximum = source_payload_budget(completed)?;
    let mut offset = 0_usize;
    loop {
        let end = sources.metadata_page_end(offset, maximum)?;
        let mut mutations = Vec::new();
        for ordinal in offset..end {
            let source = sources.at(ordinal)?;
            mutations.push(SourceMutation::Upsert {
                state: source.state,
                materializer_revision: source.materializer_revision,
            });
        }
        let segment = SourceStateSegment {
            completed: completed.clone(),
            mutations,
        };
        let _validated = segment.encode()?;
        sink.write_source_segment(&segment)?;
        offset = end;
        if offset == sources.len() {
            return Ok(());
        }
    }
}

fn ensure_required_roles(
    sink: &mut PublicationSink,
    retained: &[SegmentRef],
) -> Result<(), SegmentMaterializerError> {
    let has_flat = sink
        .references
        .iter()
        .chain(retained)
        .any(|reference| reference.role == crate::graph::segment::FLAT_SERVING_ROLE);
    if !has_flat {
        sink.enqueue_flat(
            Vec::new(),
            Vec::new(),
            MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES,
        )?;
    }
    let has_index = sink
        .references
        .iter()
        .chain(retained)
        .any(|reference| reference.role == crate::graph::segment::EVENT_STATE_INDEX_ROLE);
    if !has_index {
        sink.enqueue_event_index(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            EventLineageTables {
                sessions: Vec::new(),
                copied_origins: Vec::new(),
            },
        )?;
    }
    Ok(())
}
