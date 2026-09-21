use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;

use crate::graph::segment::{
    CompactIndexedCoreEventState, EventIndexSource, EventLineageAccumulator, FlatSegmentWriter,
    IndexedCoreEventTombstone, SegmentRef, SegmentStore,
};
use crate::protocol::CoreSourceState;

use super::super::super::SegmentMaterializerError;
use super::super::super::model::{
    MATERIALIZER_SOURCE_ROLE, MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS,
    MAX_PUBLICATION_FLAT_RETAINED_BYTES, MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES,
    MAX_PUBLICATION_RECORDS, MAX_PUBLICATION_TOMBSTONES, MaterializerMetrics, SourceStateSegment,
};
use super::super::io::{
    CompletedSegmentPublication, PlannedSegmentWrite, plan_flat_segment, plan_json_segment,
    remove_publication_attempt_files, write_planned_json_segment,
};
use super::planning::{PublicationJob, PublicationJobKind, PublicationPipeline};
#[cfg(test)]
use super::planning::{PublicationTransactionTestFault, publication_test_hook};

pub(super) struct PublicationSink {
    pub(super) root: PathBuf,
    pub(super) materialization_id: String,
    pub(super) publication_generation: u64,
    pub(super) planned_candidate_references: usize,
    pub(super) references: Vec<SegmentRef>,
    pub(super) attempt_files: BTreeSet<String>,
    pub(super) flat_records: Vec<crate::graph::segment::ServingRecord>,
    pub(super) flat_tombstones: Vec<crate::graph::segment::EventTombstone>,
    pub(super) flat_retained_bytes: usize,
    pub(super) flat_index_associations: usize,
    pub(super) index_sources: BTreeMap<String, EventIndexSource>,
    pub(super) index_records: Vec<CompactIndexedCoreEventState>,
    pub(super) index_lineage: EventLineageAccumulator,
    pub(super) index_tombstones: Vec<IndexedCoreEventTombstone>,
    pub(super) index_accounted_bytes: usize,
    pub(super) index_open_source_id: Option<String>,
    pub(super) pipeline: PublicationPipeline,
}

impl PublicationSink {
    pub(super) fn new(
        root: &Path,

        materialization_id: &str,
        publication_generation: u64,
        worker_limit: usize,
        planned_candidate_references: usize,
    ) -> Result<Self, SegmentMaterializerError> {
        Ok(Self {
            root: root.to_owned(),
            materialization_id: materialization_id.to_owned(),
            publication_generation,
            planned_candidate_references,
            references: Vec::new(),
            attempt_files: BTreeSet::new(),
            flat_records: Vec::new(),
            flat_tombstones: Vec::new(),
            flat_retained_bytes: 0,
            flat_index_associations: 0,
            index_sources: BTreeMap::new(),
            index_records: Vec::new(),
            index_lineage: EventLineageAccumulator::new(),
            index_tombstones: Vec::new(),
            index_accounted_bytes: 0,
            index_open_source_id: None,
            pipeline: PublicationPipeline::new(root, worker_limit)?,
        })
    }

    pub(super) fn push_staged(
        &mut self,
        sources: &BTreeMap<String, CoreSourceState>,
        page: super::super::super::staging::StagedPage,
    ) -> Result<(), SegmentMaterializerError> {
        let source = page.event_source;
        if !page.index_records.is_empty()
            && sources
                .get(&source.storage_key)
                .is_none_or(|state| !state.source.exact_descriptor_eq(&source.source))
        {
            return Err(SegmentMaterializerError::Corrupt(
                "staged publication source is not current",
            ));
        }
        for record in page.serving_records {
            let work = FlatSegmentWriter::record_work(&record)?;
            if self.flat_batch_requires_flush(work.frame_bytes, work.index_associations, true)? {
                self.flush_flat()?;
            }
            self.charge_flat_work(work.frame_bytes, work.index_associations)?;
            self.flat_records.push(record);
        }
        for tombstone in page.flat_tombstones {
            let retained_bytes = FlatSegmentWriter::tombstone_work_bytes(&tombstone)?;
            if self.flat_batch_requires_flush(retained_bytes, 0, false)? {
                self.flush_flat()?;
            }
            self.charge_flat_work(retained_bytes, 0)?;
            self.flat_tombstones.push(tombstone);
        }
        if !page.index_records.is_empty() {
            self.push_index_page(&source, page.index_records, page.index_lineage)?;
        }
        for tombstone in page.index_tombstones {
            self.push_index_tombstone(&source, tombstone, false)?;
        }
        Ok(())
    }

    pub(super) fn flush_flat(&mut self) -> Result<(), SegmentMaterializerError> {
        if self.flat_records.is_empty() && self.flat_tombstones.is_empty() {
            return Ok(());
        }
        let accounted_bytes = self
            .flat_retained_bytes
            .checked_add(MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES)
            .ok_or(SegmentMaterializerError::Bounds)?;
        let records = std::mem::take(&mut self.flat_records);
        let tombstones = std::mem::take(&mut self.flat_tombstones);
        self.flat_retained_bytes = 0;
        self.flat_index_associations = 0;
        self.enqueue_flat(records, tombstones, accounted_bytes)
    }

    pub(super) fn flat_batch_requires_flush(
        &self,
        retained_bytes: usize,
        index_associations: usize,
        record: bool,
    ) -> Result<bool, SegmentMaterializerError> {
        if retained_bytes > MAX_PUBLICATION_FLAT_RETAINED_BYTES
            || index_associations > MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS
        {
            return Err(SegmentMaterializerError::Bounds);
        }
        let count_full = if record {
            self.flat_records.len() >= MAX_PUBLICATION_RECORDS
        } else {
            self.flat_tombstones.len() >= MAX_PUBLICATION_TOMBSTONES
        };
        let bytes_full = self
            .flat_retained_bytes
            .checked_add(retained_bytes)
            .ok_or(SegmentMaterializerError::Bounds)?
            > MAX_PUBLICATION_FLAT_RETAINED_BYTES;
        let associations_full = self
            .flat_index_associations
            .checked_add(index_associations)
            .ok_or(SegmentMaterializerError::Bounds)?
            > MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS;
        Ok(
            (!self.flat_records.is_empty() || !self.flat_tombstones.is_empty())
                && (count_full || bytes_full || associations_full),
        )
    }

    pub(super) fn charge_flat_work(
        &mut self,
        retained_bytes: usize,
        index_associations: usize,
    ) -> Result<(), SegmentMaterializerError> {
        self.flat_retained_bytes = self
            .flat_retained_bytes
            .checked_add(retained_bytes)
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.flat_index_associations = self
            .flat_index_associations
            .checked_add(index_associations)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if self.flat_retained_bytes > MAX_PUBLICATION_FLAT_RETAINED_BYTES
            || self.flat_index_associations > MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS
        {
            return Err(SegmentMaterializerError::Bounds);
        }
        Ok(())
    }

    pub(super) fn seal(&mut self) -> Result<(), SegmentMaterializerError> {
        self.flush_flat()?;
        self.flush_index()
    }

    pub(super) fn enqueue_flat(
        &mut self,
        records: Vec<crate::graph::segment::ServingRecord>,
        tombstones: Vec<crate::graph::segment::EventTombstone>,
        accounted_bytes: usize,
    ) -> Result<(), SegmentMaterializerError> {
        let ordinal =
            u32::try_from(self.references.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
        let plan = plan_flat_segment(
            &self.materialization_id,
            self.publication_generation,
            ordinal,
        )?;
        let plan = self.register_plan(plan)?;
        self.pipeline.dispatch(PublicationJob {
            plan,
            accounted_bytes,
            kind: PublicationJobKind::Flat {
                records,
                tombstones,
            },
        })?;
        Ok(())
    }

    pub(super) fn wait_for_publication_jobs(&mut self) -> Result<(), SegmentMaterializerError> {
        self.drain_publication_jobs()
    }

    pub(super) fn finish_publication_jobs(
        &mut self,
        metrics: &mut MaterializerMetrics,
    ) -> Result<(), SegmentMaterializerError> {
        self.drain_publication_jobs()?;
        let pipeline = self.pipeline.metrics()?;
        if pipeline.peak_workers > pipeline.worker_limit
            || pipeline.peak_in_flight_bytes > pipeline.in_flight_byte_limit
            || pipeline.jobs_started != pipeline.jobs_completed
            || pipeline.jobs_completed != pipeline.checked_readbacks
        {
            return Err(SegmentMaterializerError::Corrupt(
                "publication pipeline exceeded its credits",
            ));
        }
        metrics.publication_worker_limit = metrics.publication_worker_limit.max(
            u64::try_from(pipeline.worker_limit).map_err(|_| SegmentMaterializerError::Bounds)?,
        );
        metrics.publication_jobs_started = metrics.publication_jobs_started.saturating_add(
            u64::try_from(pipeline.jobs_started).map_err(|_| SegmentMaterializerError::Bounds)?,
        );
        metrics.publication_jobs_completed = metrics.publication_jobs_completed.saturating_add(
            u64::try_from(pipeline.jobs_completed).map_err(|_| SegmentMaterializerError::Bounds)?,
        );
        metrics.publication_peak_workers = metrics.publication_peak_workers.max(
            u64::try_from(pipeline.peak_workers).map_err(|_| SegmentMaterializerError::Bounds)?,
        );
        metrics.publication_in_flight_byte_limit = metrics.publication_in_flight_byte_limit.max(
            u64::try_from(pipeline.in_flight_byte_limit)
                .map_err(|_| SegmentMaterializerError::Bounds)?,
        );
        metrics.publication_peak_in_flight_bytes = metrics.publication_peak_in_flight_bytes.max(
            u64::try_from(pipeline.peak_in_flight_bytes)
                .map_err(|_| SegmentMaterializerError::Bounds)?,
        );
        metrics.publication_checked_readbacks =
            metrics.publication_checked_readbacks.saturating_add(
                u64::try_from(pipeline.checked_readbacks)
                    .map_err(|_| SegmentMaterializerError::Bounds)?,
            );
        Ok(())
    }

    pub(super) fn register_plan(
        &mut self,
        plan: PlannedSegmentWrite,
    ) -> Result<PlannedSegmentWrite, SegmentMaterializerError> {
        if self.references.len() >= self.planned_candidate_references
            || self.references.len() >= super::super::super::model::MAX_MANIFEST_SEGMENTS
        {
            return Err(SegmentMaterializerError::Corrupt(
                "publication output exceeded its preflight reference budget",
            ));
        }
        if !self.attempt_files.insert(plan.file_name().to_owned()) {
            return Err(SegmentMaterializerError::Corrupt(
                "publication attempt allocated a duplicate file name",
            ));
        }
        #[cfg(test)]
        if let Some(hook) = publication_test_hook(&self.root) {
            hook.segment_writes_started.fetch_add(1, Ordering::SeqCst);
        }
        self.references.push(plan.placeholder());
        Ok(plan)
    }

    fn record_completed_reference(
        &mut self,
        completed: CompletedSegmentPublication,
    ) -> Result<(), SegmentMaterializerError> {
        if completed.checked_readbacks != 1
            || !self
                .attempt_files
                .contains(completed.reference.file_name.as_str())
        {
            return Err(SegmentMaterializerError::Corrupt(
                "synchronous publication returned inconsistent completion",
            ));
        }
        let index = usize::try_from(completed.reference.ordinal)
            .map_err(|_| SegmentMaterializerError::Bounds)?;
        let placeholder =
            self.references
                .get_mut(index)
                .ok_or(SegmentMaterializerError::Corrupt(
                    "synchronous publication reference ordinal is outside the manifest",
                ))?;
        if placeholder.ordinal != completed.reference.ordinal
            || placeholder.generation_id != completed.reference.generation_id
            || placeholder.role != completed.reference.role
            || placeholder.file_name != completed.reference.file_name
        {
            return Err(SegmentMaterializerError::Corrupt(
                "synchronous publication changed its predetermined reference",
            ));
        }
        *placeholder = completed.reference;
        Ok(())
    }

    pub(super) fn write_source_segment(
        &mut self,
        segment: &SourceStateSegment,
    ) -> Result<(), SegmentMaterializerError> {
        let ordinal =
            u32::try_from(self.references.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
        let plan = plan_json_segment(
            &self.materialization_id,
            self.publication_generation,
            MATERIALIZER_SOURCE_ROLE,
            ordinal,
        )?;
        let plan = self.register_plan(plan)?;
        let completed = write_planned_json_segment(&self.root, plan, segment)?;
        self.record_completed_reference(completed)
    }

    fn drain_publication_jobs(&mut self) -> Result<(), SegmentMaterializerError> {
        self.pipeline.drain_pending(&mut self.references)
    }

    pub(super) fn finish_transaction<T>(
        &mut self,
        result: Result<T, SegmentMaterializerError>,
    ) -> Result<T, SegmentMaterializerError> {
        let publication_error = match result {
            Ok(value) => {
                self.attempt_files.clear();
                return Ok(value);
            }
            Err(error) => error,
        };
        if matches!(
            publication_error,
            SegmentMaterializerError::Store(
                crate::graph::segment::SegmentStoreError::ActivationDurabilityUncertain { .. }
            )
        ) {
            // The manifest rename is the commit point. Its segments must stay
            // reachable while the caller reloads and retries the uncertain
            // directory-sync result.
            self.attempt_files.clear();
            return Err(publication_error);
        }

        match self.abort() {
            Ok(()) => Err(publication_error),
            Err(cleanup_error) => Err(cleanup_error),
        }
    }

    pub(super) fn abort(&mut self) -> Result<(), SegmentMaterializerError> {
        // This is the single failure boundary after sink creation. A sibling
        // may still be creating or authenticating its predetermined file, so
        // drain before removing manifest temporaries or segment attempt files.
        self.pipeline.drain();
        let candidate_cleanup: Result<usize, SegmentMaterializerError> =
            SegmentStore::new(&self.root)
                .cleanup_candidates()
                .map_err(Into::into);
        let segment_cleanup = remove_publication_attempt_files(&self.root, &self.attempt_files);
        #[cfg(test)]
        if segment_cleanup.is_ok()
            && let Some(hook) = publication_test_hook(&self.root)
        {
            hook.cleanup_root_syncs.fetch_add(1, Ordering::SeqCst);
        }
        match (candidate_cleanup, segment_cleanup) {
            (Ok(_), Ok(())) => {
                self.attempt_files.clear();
                Ok(())
            }
            (Err(cleanup_error), _) | (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        }
    }

    pub(super) fn before_manifest_activation(&self) -> Result<(), SegmentMaterializerError> {
        #[cfg(test)]
        if let Some(hook) = &self.pipeline.hook
            && hook.transaction_fault
                == Some(PublicationTransactionTestFault::CancelBeforeManifestActivation)
        {
            hook.pre_activation_reached
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        #[cfg(test)]
        if self.pipeline.hook.as_ref().is_some_and(|hook| {
            hook.transaction_fault
                == Some(PublicationTransactionTestFault::BeforeManifestActivation)
        }) {
            return Err(SegmentMaterializerError::Corrupt(
                "injected publication failure before manifest activation",
            ));
        }
        Ok(())
    }
}
