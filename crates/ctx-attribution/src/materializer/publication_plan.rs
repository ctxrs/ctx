//! Bounded in-process manifest reference planning.

use crate::graph::segment::{
    EVENT_STATE_INDEX_ROLE, EventIndexSource, EventIndexWriter, FLAT_SERVING_ROLE,
    FlatSegmentWriter, SegmentRef,
};
use crate::graph::segment_state::{SegmentCandidateControl, SegmentPublicationReferencePlan};

use super::SegmentMaterializerError;
use super::model::{
    MATERIALIZER_SOURCE_ROLE, MAX_MANIFEST_SEGMENTS, MAX_METADATA_SEGMENT_ENTRIES,
    MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES, MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS,
    MAX_PUBLICATION_FLAT_RETAINED_BYTES, MAX_PUBLICATION_RECORDS, MAX_PUBLICATION_TOMBSTONES,
    is_obsolete_derived_role,
};
use super::publication::ActiveGeneration;
use super::staging::StagedPage;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PublicationRoleCounts {
    pub(super) flat: usize,
    pub(super) event_index: usize,
    pub(super) source: usize,
}

impl PublicationRoleCounts {
    pub(super) fn total(self) -> Result<usize, SegmentMaterializerError> {
        self.flat
            .checked_add(self.event_index)
            .and_then(|count| count.checked_add(self.source))
            .ok_or(SegmentMaterializerError::Bounds)
    }

    fn add_role(&mut self, role: u32) -> Result<(), SegmentMaterializerError> {
        let count = match role {
            FLAT_SERVING_ROLE => &mut self.flat,
            EVENT_STATE_INDEX_ROLE => &mut self.event_index,
            MATERIALIZER_SOURCE_ROLE => &mut self.source,
            _ => {
                return Err(SegmentMaterializerError::Corrupt(
                    "publication planner observed an unknown segment role",
                ));
            }
        };
        *count = count
            .checked_add(1)
            .ok_or(SegmentMaterializerError::Bounds)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PublicationReferenceShape {
    pub(super) candidate: PublicationRoleCounts,
    pub(super) retained: PublicationRoleCounts,
}

impl PublicationReferenceShape {
    pub(super) fn candidate_references(self) -> Result<usize, SegmentMaterializerError> {
        self.candidate.total()
    }

    pub(super) fn retained_references(self) -> Result<usize, SegmentMaterializerError> {
        self.retained.total()
    }

    pub(super) fn total_references(self) -> Result<usize, SegmentMaterializerError> {
        self.candidate_references()?
            .checked_add(self.retained_references()?)
            .ok_or(SegmentMaterializerError::Bounds)
    }

    pub(super) fn fits_manifest(self) -> Result<bool, SegmentMaterializerError> {
        Ok(self.total_references()? <= MAX_MANIFEST_SEGMENTS)
    }
}

trait SegmentPublicationReferencePlanExt {
    fn plan_page(&mut self, page: &StagedPage) -> Result<(), SegmentMaterializerError>;
    fn prepare_index_page(&mut self, page: &StagedPage) -> Result<(), SegmentMaterializerError>;
    fn flat_requires_flush(
        &self,
        retained_bytes: usize,
        index_associations: usize,
        record: bool,
    ) -> Result<bool, SegmentMaterializerError>;
    fn charge_flat(
        &mut self,
        retained_bytes: usize,
        index_associations: usize,
    ) -> Result<(), SegmentMaterializerError>;
    fn flush_flat(&mut self) -> Result<(), SegmentMaterializerError>;
    fn prepare_index_item(
        &mut self,
        source: &EventIndexSource,
    ) -> Result<(), SegmentMaterializerError>;
    fn charge_index(&mut self, bytes: usize) -> Result<(), SegmentMaterializerError>;
    fn flush_index(&mut self) -> Result<(), SegmentMaterializerError>;
    fn sealed_roles(
        &self,
        source_count: usize,
    ) -> Result<PublicationRoleCounts, SegmentMaterializerError>;
}

impl SegmentPublicationReferencePlanExt for SegmentPublicationReferencePlan {
    fn plan_page(&mut self, page: &StagedPage) -> Result<(), SegmentMaterializerError> {
        if page.serving_records.is_empty()
            && page.flat_tombstones.is_empty()
            && page.index_records.is_empty()
            && page.index_tombstones.is_empty()
        {
            return Ok(());
        }
        let source = &page.event_source;
        for record in &page.serving_records {
            let work = FlatSegmentWriter::record_work(record)?;
            if self.flat_requires_flush(work.frame_bytes, work.index_associations, true)? {
                self.flush_flat()?;
            }
            self.flat_open_records = self
                .flat_open_records
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
            self.charge_flat(work.frame_bytes, work.index_associations)?;
        }
        for tombstone in &page.flat_tombstones {
            let retained = FlatSegmentWriter::tombstone_work_bytes(tombstone)?;
            if self.flat_requires_flush(retained, 0, false)? {
                self.flush_flat()?;
            }
            self.flat_open_tombstones = self
                .flat_open_tombstones
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
            self.charge_flat(retained, 0)?;
        }
        self.prepare_index_page(page)?;
        self.charge_index(
            page.index_lineage
                .sessions
                .len()
                .checked_mul(EventIndexWriter::publication_session_accounted_bytes())
                .and_then(|bytes| {
                    page.index_lineage
                        .copied_origins
                        .len()
                        .checked_mul(EventIndexWriter::publication_copied_origin_accounted_bytes())
                        .and_then(|copied| bytes.checked_add(copied))
                })
                .ok_or(SegmentMaterializerError::Bounds)?,
        )?;
        for record in &page.index_records {
            self.prepare_index_item(source)?;
            self.charge_index(EventIndexWriter::publication_record_accounted_bytes(
                record,
            )?)?;
            self.event_index_open_records = self
                .event_index_open_records
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
        for tombstone in &page.index_tombstones {
            self.prepare_index_item(source)?;
            self.charge_index(EventIndexWriter::publication_tombstone_accounted_bytes(
                tombstone,
            )?)?;
            self.event_index_open_tombstones = self
                .event_index_open_tombstones
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
        Ok(())
    }

    fn prepare_index_page(&mut self, page: &StagedPage) -> Result<(), SegmentMaterializerError> {
        let page_items = page
            .index_records
            .len()
            .checked_add(page.index_tombstones.len())
            .ok_or(SegmentMaterializerError::Bounds)?;
        if page_items == 0 {
            return Ok(());
        }
        if page_items > MAX_PUBLICATION_TOMBSTONES {
            return Err(SegmentMaterializerError::Bounds);
        }
        let source = &page.event_source;
        let source_bytes = EventIndexWriter::publication_source_accounted_bytes(source)?;
        let mut page_bytes = page
            .index_lineage
            .sessions
            .len()
            .checked_mul(EventIndexWriter::publication_session_accounted_bytes())
            .and_then(|bytes| {
                page.index_lineage
                    .copied_origins
                    .len()
                    .checked_mul(EventIndexWriter::publication_copied_origin_accounted_bytes())
                    .and_then(|copied| bytes.checked_add(copied))
            })
            .ok_or(SegmentMaterializerError::Bounds)?;
        for record in &page.index_records {
            page_bytes = page_bytes
                .checked_add(EventIndexWriter::publication_record_accounted_bytes(
                    record,
                )?)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
        for tombstone in &page.index_tombstones {
            page_bytes = page_bytes
                .checked_add(EventIndexWriter::publication_tombstone_accounted_bytes(
                    tombstone,
                )?)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
        let standalone_bytes = page_bytes
            .checked_add(source_bytes)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if standalone_bytes > MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES {
            return Err(SegmentMaterializerError::BoundDetail(format!(
                "event index page bytes for source {} exceed {}",
                source.storage_key, MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES
            )));
        }
        let open_items = self
            .event_index_open_records
            .checked_add(self.event_index_open_tombstones)
            .ok_or(SegmentMaterializerError::Bounds)?;
        let page_items = u32::try_from(page_items).map_err(|_| SegmentMaterializerError::Bounds)?;
        let source_changed =
            self.event_index_open_source_id.as_deref() != Some(source.storage_key.as_str());
        let incoming_bytes = u64::try_from(page_bytes).ok().and_then(|bytes| {
            if source_changed {
                u64::try_from(source_bytes)
                    .ok()
                    .and_then(|source| bytes.checked_add(source))
            } else {
                Some(bytes)
            }
        });
        let source_out_of_order = self
            .event_index_open_source_id
            .as_deref()
            .is_some_and(|prior| prior > source.storage_key.as_str());
        if index_page_requires_flush(
            open_items,
            page_items,
            self.event_index_open_accounted_bytes,
            incoming_bytes,
            source_out_of_order,
        ) {
            self.flush_index()?;
        }
        Ok(())
    }

    fn flat_requires_flush(
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
            usize::try_from(self.flat_open_records).map_err(|_| SegmentMaterializerError::Bounds)?
                >= MAX_PUBLICATION_RECORDS
        } else {
            usize::try_from(self.flat_open_tombstones)
                .map_err(|_| SegmentMaterializerError::Bounds)?
                >= MAX_PUBLICATION_TOMBSTONES
        };
        let bytes_full = self
            .flat_open_retained_bytes
            .checked_add(
                u64::try_from(retained_bytes).map_err(|_| SegmentMaterializerError::Bounds)?,
            )
            .is_none_or(|bytes| bytes > MAX_PUBLICATION_FLAT_RETAINED_BYTES as u64);
        let associations_full = self
            .flat_open_index_associations
            .checked_add(
                u64::try_from(index_associations).map_err(|_| SegmentMaterializerError::Bounds)?,
            )
            .is_none_or(|count| count > MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS as u64);
        Ok(
            (self.flat_open_records != 0 || self.flat_open_tombstones != 0)
                && (count_full || bytes_full || associations_full),
        )
    }

    fn charge_flat(
        &mut self,
        retained_bytes: usize,
        index_associations: usize,
    ) -> Result<(), SegmentMaterializerError> {
        self.flat_open_retained_bytes = self
            .flat_open_retained_bytes
            .checked_add(
                u64::try_from(retained_bytes).map_err(|_| SegmentMaterializerError::Bounds)?,
            )
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.flat_open_index_associations = self
            .flat_open_index_associations
            .checked_add(
                u64::try_from(index_associations).map_err(|_| SegmentMaterializerError::Bounds)?,
            )
            .ok_or(SegmentMaterializerError::Bounds)?;
        Ok(())
    }

    fn flush_flat(&mut self) -> Result<(), SegmentMaterializerError> {
        if self.flat_open_records == 0 && self.flat_open_tombstones == 0 {
            return Ok(());
        }
        self.flat_segments = self
            .flat_segments
            .checked_add(1)
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.flat_open_records = 0;
        self.flat_open_tombstones = 0;
        self.flat_open_retained_bytes = 0;
        self.flat_open_index_associations = 0;
        Ok(())
    }

    fn prepare_index_item(
        &mut self,
        source: &EventIndexSource,
    ) -> Result<(), SegmentMaterializerError> {
        let open_items = self
            .event_index_open_records
            .checked_add(self.event_index_open_tombstones)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if usize::try_from(open_items).map_err(|_| SegmentMaterializerError::Bounds)?
            == MAX_PUBLICATION_TOMBSTONES
        {
            self.flush_index()?;
        }
        if self
            .event_index_open_source_id
            .as_deref()
            .is_some_and(|prior| prior > source.storage_key.as_str())
        {
            self.flush_index()?;
        }
        if self.event_index_open_source_id.as_deref() != Some(source.storage_key.as_str()) {
            self.charge_index(EventIndexWriter::publication_source_accounted_bytes(
                source,
            )?)?;
            self.event_index_open_source_id = Some(source.storage_key.clone());
        }
        Ok(())
    }

    fn charge_index(&mut self, bytes: usize) -> Result<(), SegmentMaterializerError> {
        self.event_index_open_accounted_bytes = self
            .event_index_open_accounted_bytes
            .checked_add(u64::try_from(bytes).map_err(|_| SegmentMaterializerError::Bounds)?)
            .filter(|bytes| *bytes <= MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES as u64)
            .ok_or(SegmentMaterializerError::Bounds)?;
        Ok(())
    }

    fn flush_index(&mut self) -> Result<(), SegmentMaterializerError> {
        if self.event_index_open_records == 0 && self.event_index_open_tombstones == 0 {
            return Ok(());
        }
        self.event_index_segments = self
            .event_index_segments
            .checked_add(1)
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.event_index_open_records = 0;
        self.event_index_open_tombstones = 0;
        self.event_index_open_accounted_bytes = 0;
        self.event_index_open_source_id = None;
        Ok(())
    }

    fn sealed_roles(
        &self,
        source_count: usize,
    ) -> Result<PublicationRoleCounts, SegmentMaterializerError> {
        let source = source_count
            .checked_add(MAX_METADATA_SEGMENT_ENTRIES.saturating_sub(1))
            .ok_or(SegmentMaterializerError::Bounds)?
            / MAX_METADATA_SEGMENT_ENTRIES;
        Ok(PublicationRoleCounts {
            flat: usize::try_from(self.flat_segments)
                .map_err(|_| SegmentMaterializerError::Bounds)?
                .checked_add(usize::from(
                    self.flat_open_records != 0 || self.flat_open_tombstones != 0,
                ))
                .ok_or(SegmentMaterializerError::Bounds)?,
            event_index: usize::try_from(self.event_index_segments)
                .map_err(|_| SegmentMaterializerError::Bounds)?
                .checked_add(usize::from(
                    self.event_index_open_records != 0 || self.event_index_open_tombstones != 0,
                ))
                .ok_or(SegmentMaterializerError::Bounds)?,
            source: source.max(1),
        })
    }
}

fn index_page_requires_flush(
    open_items: u32,
    page_items: u32,
    open_bytes: u64,
    incoming_bytes: Option<u64>,
    source_out_of_order: bool,
) -> bool {
    open_items != 0
        && (open_items
            .checked_add(page_items)
            .is_none_or(|items| items > MAX_PUBLICATION_TOMBSTONES as u32)
            || incoming_bytes
                .and_then(|incoming| open_bytes.checked_add(incoming))
                .is_none_or(|bytes| bytes > MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES as u64)
            || source_out_of_order)
}

pub(super) fn plan_direct_pages(
    candidate: &SegmentCandidateControl,
    pages: &[StagedPage],
) -> Result<SegmentPublicationReferencePlan, SegmentMaterializerError> {
    let mut plan = candidate.publication_reference_plan.clone();
    for page in pages {
        plan.plan_page(page)?;
    }
    plan.validate().map_err(|error| match error {
        crate::core_materialization::CoreStoreError::Bounds => SegmentMaterializerError::Bounds,
        _ => SegmentMaterializerError::Corrupt("direct publication reference plan is invalid"),
    })?;
    Ok(plan)
}

pub(super) fn direct_reference_shape(
    candidate: &SegmentCandidateControl,
    plan: &SegmentPublicationReferencePlan,
    active: Option<&ActiveGeneration>,
) -> Result<PublicationReferenceShape, SegmentMaterializerError> {
    let source_count = usize::try_from(candidate.head.source_count)
        .map_err(|_| SegmentMaterializerError::Bounds)?;
    let mut candidate_roles = plan.sealed_roles(source_count)?;
    let mut retained = PublicationRoleCounts::default();
    if !candidate.force_projection_rebuild {
        for reference in active
            .into_iter()
            .flat_map(|generation| &generation.manifest.segments)
            .filter(|reference| {
                reference.role != MATERIALIZER_SOURCE_ROLE
                    && !is_obsolete_derived_role(reference.role)
            })
        {
            retained.add_role(reference.role)?;
        }
    }
    if candidate_roles.flat == 0 && retained.flat == 0 {
        candidate_roles.flat = 1;
    }
    if candidate_roles.event_index == 0 && retained.event_index == 0 {
        candidate_roles.event_index = 1;
    }
    Ok(PublicationReferenceShape {
        candidate: candidate_roles,
        retained,
    })
}

pub(super) fn classify_references(
    references: &[SegmentRef],
) -> Result<PublicationRoleCounts, SegmentMaterializerError> {
    let mut roles = PublicationRoleCounts::default();
    for reference in references {
        roles.add_role(reference.role)?;
    }
    Ok(roles)
}

#[cfg(test)]
#[path = "publication_plan_tests.rs"]
mod tests;
