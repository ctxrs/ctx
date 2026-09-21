#[cfg(test)]
use crate::graph::segment::IndexedCoreEventState;
use crate::graph::segment::{
    CompactIndexedCoreEventState, EventIndexSource, EventIndexWriter, EventLineageAccumulator,
    EventLineageTables, IndexedCoreEventTombstone,
};

use super::super::super::SegmentMaterializerError;
use super::super::super::model::{
    EVENT_INDEX_PUBLICATION_MEMORY_RESERVE, MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES,
    MAX_PUBLICATION_TOMBSTONES,
};
use super::super::io::plan_event_index_segment;
use super::encoding::PublicationSink;
use super::planning::{PublicationJob, PublicationJobKind};

impl PublicationSink {
    fn prepare_index_source(
        &mut self,
        source: &EventIndexSource,
    ) -> Result<(), SegmentMaterializerError> {
        if self
            .index_open_source_id
            .as_deref()
            .is_some_and(|prior| prior > source.storage_key.as_str())
        {
            self.flush_index()?;
        }
        self.index_open_source_id = Some(source.storage_key.clone());
        Ok(())
    }

    fn charge_index_open_bytes(
        &mut self,
        additional: usize,
    ) -> Result<(), SegmentMaterializerError> {
        let accounted = self
            .index_accounted_bytes
            .checked_add(additional)
            .filter(|bytes| *bytes <= MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES)
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.index_accounted_bytes = accounted;
        Ok(())
    }

    fn retain_index_source(
        &mut self,
        source: EventIndexSource,
    ) -> Result<(), SegmentMaterializerError> {
        if self.index_sources.contains_key(&source.storage_key) {
            return Ok(());
        }
        let accounted = EventIndexWriter::publication_source_accounted_bytes(&source)?;
        self.charge_index_open_bytes(accounted)?;
        self.index_sources
            .insert(source.storage_key.clone(), source);
        Ok(())
    }

    fn replace_index_source(
        &mut self,
        source: EventIndexSource,
    ) -> Result<(), SegmentMaterializerError> {
        let previous = self
            .index_sources
            .get(&source.storage_key)
            .map(EventIndexWriter::publication_source_accounted_bytes)
            .transpose()?
            .unwrap_or(0);
        let replacement = EventIndexWriter::publication_source_accounted_bytes(&source)?;
        let accounted = self
            .index_accounted_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(replacement))
            .filter(|bytes| *bytes <= MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES)
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.index_sources
            .insert(source.storage_key.clone(), source);
        self.index_accounted_bytes = accounted;
        Ok(())
    }

    #[cfg(test)]
    fn retain_index_record(
        &mut self,
        record: IndexedCoreEventState,
    ) -> Result<(), SegmentMaterializerError> {
        let (session_ref, inserted_session, inserted_copy) =
            self.index_lineage.retain(record.key(), record.lineage)?;
        let record = CompactIndexedCoreEventState {
            source_storage_key: record.source_storage_key,
            event_id: record.event_id,
            session_ref,
            event_sequence: record.event_sequence,
            core_record_sha256: record.core_record_sha256,
            core_record_leaf_sha256: record.core_record_leaf_sha256,
            flat_record_count: record.flat_record_count,
            event_output_root: record.event_output_root,
            coverage: record.coverage,
        };
        let accounted =
            EventIndexWriter::publication_record_accounted_bytes(&record)?
                .checked_add(
                    usize::from(inserted_session)
                        .saturating_mul(EventIndexWriter::publication_session_accounted_bytes()),
                )
                .and_then(|bytes| {
                    bytes.checked_add(usize::from(inserted_copy).saturating_mul(
                        EventIndexWriter::publication_copied_origin_accounted_bytes(),
                    ))
                })
                .ok_or(SegmentMaterializerError::Bounds)?;
        self.charge_index_open_bytes(accounted)?;
        self.index_records.push(record);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn push_index_record(
        &mut self,
        source: &EventIndexSource,
        record: IndexedCoreEventState,
    ) -> Result<(), SegmentMaterializerError> {
        if self.index_records.len() + self.index_tombstones.len() == MAX_PUBLICATION_TOMBSTONES {
            self.flush_index()?;
        }
        self.retain_index_source(source.clone())?;
        self.retain_index_record(record)
    }

    pub(super) fn push_index_page(
        &mut self,
        source: &EventIndexSource,
        mut records: Vec<CompactIndexedCoreEventState>,
        lineage: EventLineageTables,
    ) -> Result<(), SegmentMaterializerError> {
        if records.len() > MAX_PUBLICATION_TOMBSTONES {
            return Err(SegmentMaterializerError::Bounds);
        }
        let open_items = self
            .index_records
            .len()
            .saturating_add(self.index_tombstones.len());
        if open_items != 0 && open_items.saturating_add(records.len()) > MAX_PUBLICATION_TOMBSTONES
        {
            self.flush_index()?;
        }
        self.prepare_index_source(source)?;
        self.retain_index_source(source.clone())?;
        let (inserted_sessions, inserted_copies) =
            self.index_lineage.absorb_page(&mut records, lineage)?;
        let records_bytes = records.iter().try_fold(0_usize, |total, record| {
            total
                .checked_add(EventIndexWriter::publication_record_accounted_bytes(
                    record,
                )?)
                .ok_or(SegmentMaterializerError::Bounds)
        })?;
        let lineage_bytes = inserted_sessions
            .checked_mul(EventIndexWriter::publication_session_accounted_bytes())
            .and_then(|bytes| {
                inserted_copies
                    .checked_mul(EventIndexWriter::publication_copied_origin_accounted_bytes())
                    .and_then(|copied| bytes.checked_add(copied))
            })
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.charge_index_open_bytes(
            records_bytes
                .checked_add(lineage_bytes)
                .ok_or(SegmentMaterializerError::Bounds)?,
        )?;
        self.index_records.append(&mut records);
        Ok(())
    }

    fn retain_index_tombstone(
        &mut self,
        tombstone: IndexedCoreEventTombstone,
    ) -> Result<(), SegmentMaterializerError> {
        let accounted = EventIndexWriter::publication_tombstone_accounted_bytes(&tombstone)?;
        self.charge_index_open_bytes(accounted)?;
        self.index_tombstones.push(tombstone);
        Ok(())
    }

    pub(super) fn push_index_tombstone(
        &mut self,
        source: &EventIndexSource,
        tombstone: IndexedCoreEventTombstone,
        replace_source: bool,
    ) -> Result<(), SegmentMaterializerError> {
        if self.index_records.len() + self.index_tombstones.len() == MAX_PUBLICATION_TOMBSTONES {
            self.flush_index()?;
        }
        self.prepare_index_source(source)?;
        if replace_source {
            self.replace_index_source(source.clone())?;
        } else {
            self.retain_index_source(source.clone())?;
        }
        self.retain_index_tombstone(tombstone)
    }

    pub(super) fn flush_index(&mut self) -> Result<(), SegmentMaterializerError> {
        if self.index_records.is_empty() && self.index_tombstones.is_empty() {
            return Ok(());
        }
        let sources: Vec<EventIndexSource> = std::mem::take(&mut self.index_sources)
            .into_values()
            .collect();
        let mut records = std::mem::take(&mut self.index_records);
        let lineage = std::mem::replace(&mut self.index_lineage, EventLineageAccumulator::new())
            .finish(&mut records)?;
        let tombstones = std::mem::take(&mut self.index_tombstones);
        let accounted_bytes = EventIndexWriter::publication_accounted_bytes(
            &sources,
            &records,
            &tombstones,
            &lineage,
        )?;
        if accounted_bytes != self.index_accounted_bytes {
            return Err(SegmentMaterializerError::Corrupt(
                "event-index publication accounting changed while sealing",
            ));
        }
        self.index_accounted_bytes = 0;
        self.index_open_source_id = None;
        self.enqueue_event_index(sources, records, tombstones, lineage)
    }

    pub(super) fn enqueue_event_index(
        &mut self,
        sources: Vec<EventIndexSource>,
        records: Vec<CompactIndexedCoreEventState>,
        tombstones: Vec<IndexedCoreEventTombstone>,
        lineage: EventLineageTables,
    ) -> Result<(), SegmentMaterializerError> {
        let accounted_bytes = EventIndexWriter::publication_accounted_bytes(
            &sources,
            &records,
            &tombstones,
            &lineage,
        )?
        .checked_add(EVENT_INDEX_PUBLICATION_MEMORY_RESERVE)
        .ok_or(SegmentMaterializerError::Bounds)?;
        let ordinal =
            u32::try_from(self.references.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
        let plan = plan_event_index_segment(
            &self.materialization_id,
            self.publication_generation,
            ordinal,
        )?;
        let plan = self.register_plan(plan)?;
        self.pipeline.dispatch(PublicationJob {
            plan,
            accounted_bytes,
            kind: PublicationJobKind::EventIndex {
                sources,
                records,
                tombstones,
                lineage,
            },
        })?;
        Ok(())
    }
}
