use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    fs::File,
    io::{BufWriter, Write},
    sync::Arc,
};

use memmap2::{MmapMut, MmapOptions};
use tantivy::DocAddress;

use super::scratch::{active_scratch_budget, ScratchReservation};
#[cfg(test)]
use super::scratch::{VerificationScratchBudget, MAX_VERIFICATION_SCRATCH_HEAP_BYTES};
use crate::{CompactIdentity, IndexError, Result};

pub(super) const VERIFICATION_SPILL_BUFFER_BYTES: usize = 8 * 1024;
const COMPACT_IDENTITY_BYTES: usize = 32;
const SOURCE_ORDINAL_BYTES: usize = std::mem::size_of::<u32>();
const IDENTITY_SPILL_RECORD_BYTES: usize = COMPACT_IDENTITY_BYTES * 4 + SOURCE_ORDINAL_BYTES + 4;
const QUERY_PROJECTION_ACCUMULATOR_BYTES: usize = 32;
pub(super) const VERIFICATION_SPILL_RECORD_BYTES: usize =
    IDENTITY_SPILL_RECORD_BYTES + QUERY_PROJECTION_ACCUMULATOR_BYTES;
const MAX_VERIFICATION_LAYOUT_HEAP_BYTES: usize = 16 * 1024 * 1024;
const IDENTITY_SORT_RUN_RECORDS: usize = 4_096;

type IdentitySortRun = (u64, usize);
type IdentitySortHeapEntry = Reverse<([u8; COMPACT_IDENTITY_BYTES], usize)>;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ProjectionAccumulator([u8; QUERY_PROJECTION_ACCUMULATOR_BYTES]);

impl ProjectionAccumulator {
    pub(super) fn subtract(&mut self, digest: &[u8; QUERY_PROJECTION_ACCUMULATOR_BYTES]) {
        let mut borrow = 0_u16;
        for (target, value) in self.0.iter_mut().zip(digest).rev() {
            let subtrahend = u16::from(*value) + borrow;
            let current = u16::from(*target);
            *target = current.wrapping_sub(subtrahend) as u8;
            borrow = u16::from(current < subtrahend);
        }
    }

    pub(super) fn add(&mut self, digest: &[u8; QUERY_PROJECTION_ACCUMULATOR_BYTES]) {
        let mut carry = 0_u16;
        for (target, value) in self.0.iter_mut().zip(digest).rev() {
            let sum = u16::from(*target) + u16::from(*value) + carry;
            *target = sum as u8;
            carry = sum >> 8;
        }
    }

    #[cfg(test)]
    pub(super) fn is_zero(self) -> bool {
        self.0 == [0; QUERY_PROJECTION_ACCUMULATOR_BYTES]
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SpillVerificationIdentities {
    pub(super) event: CompactIdentity,
    pub(super) session: CompactIdentity,
    pub(super) parent_session: Option<CompactIdentity>,
    pub(super) root_session: Option<CompactIdentity>,
    pub(super) session_source_ordinal: u32,
    pub(super) session_relationship_kind: Option<u8>,
    pub(super) session_witness_present: bool,
}

/// Sequential identity-only scratch for records changed relative to an audited base.
/// Its physical and logical size is proportional only to the candidate delta.
pub(super) struct IdentityDeltaSpill {
    file: File,
    records: u64,
    reservation: ScratchReservation,
}

pub(super) struct IdentityKeySpill {
    file: File,
    records: u64,
    reservation: ScratchReservation,
}

impl IdentityDeltaSpill {
    pub(super) fn create() -> Result<Self> {
        let reservation = active_scratch_budget().reserve(0, 0)?;
        Ok(Self {
            file: tempfile::tempfile()?,
            records: 0,
            reservation,
        })
    }

    pub(super) fn push(&mut self, identities: SpillVerificationIdentities) -> Result<()> {
        let growth = self
            .reservation
            .budget
            .reserve(IDENTITY_SPILL_RECORD_BYTES as u64, 0)?;
        let offset = self
            .records
            .checked_mul(IDENTITY_SPILL_RECORD_BYTES as u64)
            .ok_or(IndexError::CountOverflow)?;
        write_spill_all_at(&self.file, &encode_record(identities), offset)?;
        self.reservation.absorb(growth)?;
        self.records = self
            .records
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    pub(super) fn for_each(
        &self,
        mut visit: impl FnMut(SpillVerificationIdentities) -> Result<()>,
    ) -> Result<()> {
        let mut encoded = [0_u8; IDENTITY_SPILL_RECORD_BYTES];
        for ordinal in 0..self.records {
            let offset = ordinal
                .checked_mul(IDENTITY_SPILL_RECORD_BYTES as u64)
                .ok_or(IndexError::CountOverflow)?;
            read_spill_exact_at(&self.file, &mut encoded, offset)?;
            visit(decode_record(&encoded, "core_record")?)?;
        }
        Ok(())
    }
}

impl IdentityKeySpill {
    pub(super) fn create() -> Result<Self> {
        let reservation = active_scratch_budget().reserve(0, 0)?;
        Ok(Self {
            file: tempfile::tempfile()?,
            records: 0,
            reservation,
        })
    }

    pub(super) fn push(&mut self, identity: CompactIdentity) -> Result<()> {
        let growth = self
            .reservation
            .budget
            .reserve(COMPACT_IDENTITY_BYTES as u64, 0)?;
        let offset = self
            .records
            .checked_mul(COMPACT_IDENTITY_BYTES as u64)
            .ok_or(IndexError::CountOverflow)?;
        write_spill_all_at(&self.file, &identity.digest, offset)?;
        self.reservation.absorb(growth)?;
        self.records = self
            .records
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    pub(super) fn for_each_unique(
        &self,
        mut visit: impl FnMut(CompactIdentity) -> Result<()>,
    ) -> Result<()> {
        let run_count = identity_sort_run_count(self.records)?;
        let sort_heap_bytes = identity_sort_scratch_heap_bytes(self.records, run_count)?;
        let runs_disk_bytes = self
            .records
            .checked_mul(COMPACT_IDENTITY_BYTES as u64)
            .ok_or(IndexError::CountOverflow)?;
        let _sort_reservation = self
            .reservation
            .budget
            .reserve(runs_disk_bytes, sort_heap_bytes)?;
        let runs_file = tempfile::tempfile()?;
        let mut runs = Vec::<IdentitySortRun>::with_capacity(run_count);
        let mut input_ordinal = 0_u64;
        let mut run_offset = 0_u64;
        while input_ordinal < self.records {
            let remaining = usize::try_from((self.records - input_ordinal).min(
                u64::try_from(IDENTITY_SORT_RUN_RECORDS).map_err(|_| IndexError::CountOverflow)?,
            ))
            .map_err(|_| IndexError::CountOverflow)?;
            let mut identities = Vec::with_capacity(remaining);
            for _ in 0..remaining {
                let mut digest = [0_u8; COMPACT_IDENTITY_BYTES];
                read_spill_exact_at(
                    &self.file,
                    &mut digest,
                    input_ordinal
                        .checked_mul(COMPACT_IDENTITY_BYTES as u64)
                        .ok_or(IndexError::CountOverflow)?,
                )?;
                identities.push(digest);
                input_ordinal += 1;
            }
            identities.sort_unstable();
            identities.dedup();
            let run_bytes = identities
                .len()
                .checked_mul(COMPACT_IDENTITY_BYTES)
                .ok_or(IndexError::CountOverflow)?;
            for digest in &identities {
                write_spill_all_at(&runs_file, digest, run_offset)?;
                run_offset = run_offset
                    .checked_add(COMPACT_IDENTITY_BYTES as u64)
                    .ok_or(IndexError::CountOverflow)?;
            }
            runs.push((
                run_offset
                    .checked_sub(run_bytes as u64)
                    .ok_or(IndexError::CountOverflow)?,
                identities.len(),
            ));
        }
        if runs.len() != run_count {
            return Err(IndexError::CountOverflow);
        }

        let mut positions = vec![0_usize; runs.len()];
        let mut heap = BinaryHeap::<IdentitySortHeapEntry>::with_capacity(run_count);
        for (run, (offset, count)) in runs.iter().copied().enumerate() {
            if count != 0 {
                heap.push(Reverse((read_identity_at(&runs_file, offset)?, run)));
            }
        }
        let mut previous = None;
        while let Some(Reverse((digest, run))) = heap.pop() {
            if previous != Some(digest) {
                visit(CompactIdentity { digest })?;
                previous = Some(digest);
            }
            positions[run] += 1;
            let (offset, count) = runs[run];
            if positions[run] < count {
                let next_offset = offset
                    .checked_add(
                        u64::try_from(positions[run])
                            .map_err(|_| IndexError::CountOverflow)?
                            .checked_mul(COMPACT_IDENTITY_BYTES as u64)
                            .ok_or(IndexError::CountOverflow)?,
                    )
                    .ok_or(IndexError::CountOverflow)?;
                heap.push(Reverse((read_identity_at(&runs_file, next_offset)?, run)));
            }
        }
        Ok(())
    }
}

fn identity_sort_run_count(records: u64) -> Result<usize> {
    let records_per_run =
        u64::try_from(IDENTITY_SORT_RUN_RECORDS).map_err(|_| IndexError::CountOverflow)?;
    let runs = records
        .checked_add(records_per_run - 1)
        .ok_or(IndexError::CountOverflow)?
        / records_per_run;
    usize::try_from(runs).map_err(|_| IndexError::CountOverflow)
}

fn identity_sort_layout_heap_bytes(run_count: usize) -> Result<usize> {
    let retained_bytes_per_run = std::mem::size_of::<IdentitySortRun>()
        .checked_add(std::mem::size_of::<usize>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<IdentitySortHeapEntry>()))
        .ok_or(IndexError::CountOverflow)?;
    run_count
        .checked_mul(retained_bytes_per_run)
        .ok_or(IndexError::CountOverflow)
}

fn identity_sort_scratch_heap_bytes(records: u64, run_count: usize) -> Result<u64> {
    let merge_bytes = identity_sort_layout_heap_bytes(run_count)?;
    let run_metadata_bytes = run_count
        .checked_mul(std::mem::size_of::<IdentitySortRun>())
        .ok_or(IndexError::CountOverflow)?;
    let run_records = usize::try_from(records.min(IDENTITY_SORT_RUN_RECORDS as u64))
        .map_err(|_| IndexError::CountOverflow)?;
    let generation_bytes = run_metadata_bytes
        .checked_add(
            run_records
                .checked_mul(COMPACT_IDENTITY_BYTES)
                .ok_or(IndexError::CountOverflow)?,
        )
        .ok_or(IndexError::CountOverflow)?;
    u64::try_from(merge_bytes.max(generation_bytes)).map_err(|_| IndexError::CountOverflow)
}

fn read_identity_at(file: &File, offset: u64) -> Result<[u8; COMPACT_IDENTITY_BYTES]> {
    let mut digest = [0_u8; COMPACT_IDENTITY_BYTES];
    read_spill_exact_at(file, &mut digest, offset)?;
    Ok(digest)
}

/// Anonymous fixed-size identity and query-projection state for one audit.
#[derive(Debug)]
pub(super) struct VerificationSpill {
    file: Arc<File>,
    segment_offsets: Arc<Vec<u64>>,
    segment_max_docs: Arc<Vec<u32>>,
    logical_bytes: u64,
    _reservation: ScratchReservation,
    cleanup_witness: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub(super) struct SegmentVerificationWriter<'a> {
    identity_writer: BufWriter<SpillAtWriter<'a>>,
    projection_writer: BufWriter<SpillAtWriter<'a>>,
    next_doc_id: u32,
    end_doc_id: u32,
}

pub(super) struct ProjectionDeltas {
    _file: Arc<File>,
    mapping: Option<MmapMut>,
    segment_offsets: Arc<Vec<u64>>,
    segment_max_docs: Arc<Vec<u32>>,
}

struct SpillAtWriter<'a> {
    file: &'a File,
    offset: u64,
}

impl VerificationSpill {
    pub(super) fn create<I>(segment_max_docs: I) -> Result<Self>
    where
        I: Iterator<Item = u32> + Clone,
    {
        Self::create_with_witness(segment_max_docs, None)
    }

    fn create_with_witness<I>(
        max_docs: I,
        cleanup_witness: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<Self>
    where
        I: Iterator<Item = u32> + Clone,
    {
        let (segment_count, expected_logical_bytes) =
            preflight_spill_layout(max_docs.clone(), MAX_VERIFICATION_LAYOUT_HEAP_BYTES)?;
        let layout_heap_bytes = segment_count
            .checked_mul(std::mem::size_of::<u64>() + std::mem::size_of::<u32>())
            .ok_or(IndexError::CountOverflow)?;
        let reservation = active_scratch_budget().reserve(
            expected_logical_bytes,
            u64::try_from(layout_heap_bytes).map_err(|_| IndexError::CountOverflow)?,
        )?;
        let mut segment_offsets = Vec::with_capacity(segment_count);
        let mut segment_max_docs = Vec::with_capacity(segment_count);
        let mut logical_bytes = 0_u64;
        for max_doc in max_docs {
            segment_offsets.push(logical_bytes);
            segment_max_docs.push(max_doc);
            logical_bytes = logical_bytes
                .checked_add(segment_spill_bytes(max_doc)?)
                .ok_or(IndexError::CountOverflow)?;
        }
        debug_assert_eq!(logical_bytes, expected_logical_bytes);
        let file = tempfile::tempfile()?;
        file.set_len(logical_bytes)?;
        Ok(Self {
            file: Arc::new(file),
            segment_offsets: Arc::new(segment_offsets),
            segment_max_docs: Arc::new(segment_max_docs),
            logical_bytes,
            _reservation: reservation,
            cleanup_witness,
        })
    }

    pub(super) fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub(super) fn segment_offsets_heap_bytes(&self) -> Result<usize> {
        self.segment_offsets
            .len()
            .checked_mul(std::mem::size_of::<u64>())
            .and_then(|bytes| {
                self.segment_max_docs
                    .len()
                    .checked_mul(std::mem::size_of::<u32>())
                    .and_then(|max_docs| bytes.checked_add(max_docs))
            })
            .ok_or(IndexError::CountOverflow)
    }

    #[cfg(test)]
    pub(super) fn segment_writer(
        &self,
        segment_ord: usize,
        max_doc: u32,
    ) -> Result<SegmentVerificationWriter<'_>> {
        self.segment_range_writer(segment_ord, 0, max_doc, max_doc)
    }

    pub(super) fn segment_range_writer(
        &self,
        segment_ord: usize,
        start_doc_id: u32,
        end_doc_id: u32,
        max_doc: u32,
    ) -> Result<SegmentVerificationWriter<'_>> {
        let offset = *self
            .segment_offsets
            .get(segment_ord)
            .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
        if self.segment_max_docs.get(segment_ord).copied() != Some(max_doc)
            || start_doc_id > end_doc_id
            || end_doc_id > max_doc
        {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
        let end = offset
            .checked_add(segment_spill_bytes(max_doc)?)
            .ok_or(IndexError::CountOverflow)?;
        if end > self.logical_bytes {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
        let projection_offset = offset
            .checked_add(identity_segment_bytes(max_doc)?)
            .and_then(|offset| {
                u64::from(start_doc_id)
                    .checked_mul(QUERY_PROJECTION_ACCUMULATOR_BYTES as u64)
                    .and_then(|start| offset.checked_add(start))
            })
            .ok_or(IndexError::CountOverflow)?;
        let identity_offset = u64::from(start_doc_id)
            .checked_mul(IDENTITY_SPILL_RECORD_BYTES as u64)
            .and_then(|start| offset.checked_add(start))
            .ok_or(IndexError::CountOverflow)?;
        Ok(SegmentVerificationWriter {
            identity_writer: BufWriter::with_capacity(
                VERIFICATION_SPILL_BUFFER_BYTES,
                SpillAtWriter {
                    file: &self.file,
                    offset: identity_offset,
                },
            ),
            projection_writer: BufWriter::with_capacity(
                VERIFICATION_SPILL_BUFFER_BYTES,
                SpillAtWriter {
                    file: &self.file,
                    offset: projection_offset,
                },
            ),
            next_doc_id: start_doc_id,
            end_doc_id,
        })
    }

    pub(super) fn record(
        &self,
        address: DocAddress,
        field: &'static str,
    ) -> Result<SpillVerificationIdentities> {
        let segment_offset = self
            .segment_offsets
            .get(address.segment_ord as usize)
            .ok_or(IndexError::InvalidStoredDocumentField(field))?;
        let offset = u64::from(address.doc_id)
            .checked_mul(IDENTITY_SPILL_RECORD_BYTES as u64)
            .and_then(|offset| segment_offset.checked_add(offset))
            .ok_or(IndexError::CountOverflow)?;
        let mut encoded = [0_u8; IDENTITY_SPILL_RECORD_BYTES];
        read_spill_exact_at(&self.file, &mut encoded, offset)?;
        decode_record(&encoded, field)
    }

    pub(super) fn load_projection_deltas(&self) -> Result<ProjectionDeltas> {
        let mapping = if self.logical_bytes == 0 {
            None
        } else {
            let length =
                usize::try_from(self.logical_bytes).map_err(|_| IndexError::CountOverflow)?;
            // SAFETY: the spill owns this fixed-length temporary file for at
            // least as long as the returned projection mapping is in use.
            Some(unsafe { MmapOptions::new().len(length).map_mut(&*self.file)? })
        };
        Ok(ProjectionDeltas {
            _file: Arc::clone(&self.file),
            mapping,
            segment_offsets: Arc::clone(&self.segment_offsets),
            segment_max_docs: Arc::clone(&self.segment_max_docs),
        })
    }
}

fn preflight_spill_layout(
    max_docs: impl Iterator<Item = u32>,
    maximum_layout_heap_bytes: usize,
) -> Result<(usize, u64)> {
    let mut segment_count = 0_usize;
    let mut logical_bytes = 0_u64;
    for max_doc in max_docs {
        segment_count = segment_count
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        logical_bytes = logical_bytes
            .checked_add(segment_spill_bytes(max_doc)?)
            .ok_or(IndexError::CountOverflow)?;
        let layout_heap_bytes = segment_count
            .checked_mul(std::mem::size_of::<u64>() + std::mem::size_of::<u32>())
            .ok_or(IndexError::CountOverflow)?;
        if layout_heap_bytes > maximum_layout_heap_bytes {
            return Err(IndexError::VerificationScratchLimitExceeded {
                required_bytes: u64::try_from(layout_heap_bytes)
                    .map_err(|_| IndexError::CountOverflow)?,
                maximum_bytes: u64::try_from(maximum_layout_heap_bytes)
                    .map_err(|_| IndexError::CountOverflow)?,
            });
        }
    }
    Ok((segment_count, logical_bytes))
}

impl ProjectionDeltas {
    pub(super) fn heap_bytes(&self) -> usize {
        0
    }

    pub(super) fn set_expected(
        &mut self,
        address: DocAddress,
        expected: ProjectionAccumulator,
    ) -> Result<()> {
        *self.accumulator_mut(address)? = expected.0;
        Ok(())
    }

    pub(super) fn accumulate(
        &mut self,
        address: DocAddress,
        digest: &[u8; QUERY_PROJECTION_ACCUMULATOR_BYTES],
    ) -> Result<()> {
        let accumulator = self.accumulator_mut(address)?;
        let mut value = ProjectionAccumulator(*accumulator);
        value.add(digest);
        *accumulator = value.0;
        Ok(())
    }

    pub(super) fn is_complete(&self, address: DocAddress) -> Result<bool> {
        let range = self.accumulator_range(address)?;
        Ok(self
            .mapping
            .as_ref()
            .and_then(|mapping| mapping.get(range))
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?
            .iter()
            .all(|byte| *byte == 0))
    }

    fn accumulator_mut(&mut self, address: DocAddress) -> Result<&mut [u8; 32]> {
        let range = self.accumulator_range(address)?;
        self.mapping
            .as_mut()
            .and_then(|mapping| mapping.get_mut(range))
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))
    }

    fn accumulator_range(&self, address: DocAddress) -> Result<std::ops::Range<usize>> {
        let segment_ord = address.segment_ord as usize;
        let segment_offset = *self
            .segment_offsets
            .get(segment_ord)
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?;
        let max_doc = *self
            .segment_max_docs
            .get(segment_ord)
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?;
        if address.doc_id >= max_doc {
            return Err(IndexError::InvalidStoredDocumentField("query_projection"));
        }
        let start = segment_offset
            .checked_add(identity_segment_bytes(max_doc)?)
            .and_then(|offset| {
                u64::from(address.doc_id)
                    .checked_mul(QUERY_PROJECTION_ACCUMULATOR_BYTES as u64)
                    .and_then(|doc_offset| offset.checked_add(doc_offset))
            })
            .ok_or(IndexError::CountOverflow)?;
        let end = start
            .checked_add(QUERY_PROJECTION_ACCUMULATOR_BYTES as u64)
            .ok_or(IndexError::CountOverflow)?;
        Ok(
            usize::try_from(start).map_err(|_| IndexError::CountOverflow)?
                ..usize::try_from(end).map_err(|_| IndexError::CountOverflow)?,
        )
    }
}

impl SegmentVerificationWriter<'_> {
    pub(super) fn write_deleted(&mut self, doc_id: u32) -> Result<()> {
        self.check_doc_id(doc_id)?;
        self.identity_writer
            .write_all(&[0; IDENTITY_SPILL_RECORD_BYTES])?;
        self.projection_writer
            .write_all(&[0; QUERY_PROJECTION_ACCUMULATOR_BYTES])?;
        self.next_doc_id += 1;
        Ok(())
    }

    pub(super) fn write_record(
        &mut self,
        doc_id: u32,
        identities: SpillVerificationIdentities,
        projection_delta: ProjectionAccumulator,
    ) -> Result<()> {
        self.check_doc_id(doc_id)?;
        self.identity_writer.write_all(&encode_record(identities))?;
        self.projection_writer.write_all(&projection_delta.0)?;
        self.next_doc_id += 1;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<()> {
        if self.next_doc_id != self.end_doc_id {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
        self.identity_writer.flush()?;
        self.projection_writer.flush()?;
        Ok(())
    }

    fn check_doc_id(&self, doc_id: u32) -> Result<()> {
        if doc_id != self.next_doc_id || doc_id >= self.end_doc_id {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
        Ok(())
    }
}

impl Write for SpillAtWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = write_spill_at(self.file, bytes, self.offset)?;
        self.offset = self
            .offset
            .checked_add(written as u64)
            .ok_or_else(|| std::io::Error::other("identity spill offset overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn segment_spill_bytes(max_doc: u32) -> Result<u64> {
    u64::from(max_doc)
        .checked_mul(VERIFICATION_SPILL_RECORD_BYTES as u64)
        .ok_or(IndexError::CountOverflow)
}

fn identity_segment_bytes(max_doc: u32) -> Result<u64> {
    u64::from(max_doc)
        .checked_mul(IDENTITY_SPILL_RECORD_BYTES as u64)
        .ok_or(IndexError::CountOverflow)
}

fn encode_record(identities: SpillVerificationIdentities) -> [u8; IDENTITY_SPILL_RECORD_BYTES] {
    let mut encoded = [0_u8; IDENTITY_SPILL_RECORD_BYTES];
    let mut cursor = 0;
    encode_identity(&mut encoded, &mut cursor, identities.event);
    encode_identity(&mut encoded, &mut cursor, identities.session);
    encoded[cursor] = u8::from(identities.parent_session.is_some());
    cursor += 1;
    encode_identity(
        &mut encoded,
        &mut cursor,
        identities
            .parent_session
            .unwrap_or(CompactIdentity { digest: [0; 32] }),
    );
    encoded[cursor] = u8::from(identities.root_session.is_some());
    cursor += 1;
    encode_identity(
        &mut encoded,
        &mut cursor,
        identities
            .root_session
            .unwrap_or(CompactIdentity { digest: [0; 32] }),
    );
    encoded[cursor..cursor + SOURCE_ORDINAL_BYTES]
        .copy_from_slice(&identities.session_source_ordinal.to_be_bytes());
    cursor += SOURCE_ORDINAL_BYTES;
    encoded[cursor] = identities.session_relationship_kind.unwrap_or(0);
    cursor += 1;
    encoded[cursor] = u8::from(identities.session_witness_present);
    encoded
}

fn encode_identity(
    encoded: &mut [u8; IDENTITY_SPILL_RECORD_BYTES],
    cursor: &mut usize,
    identity: CompactIdentity,
) {
    encoded[*cursor..*cursor + 32].copy_from_slice(&identity.digest);
    *cursor += 32;
}

fn decode_record(
    encoded: &[u8; IDENTITY_SPILL_RECORD_BYTES],
    field: &'static str,
) -> Result<SpillVerificationIdentities> {
    let mut cursor = 0;
    let event = decode_identity(encoded, &mut cursor);
    let session = decode_identity(encoded, &mut cursor);
    let has_parent = match encoded[cursor] {
        0 => false,
        1 => true,
        _ => return Err(IndexError::InvalidStoredDocumentField(field)),
    };
    cursor += 1;
    let parent = decode_identity(encoded, &mut cursor);
    let has_root = match encoded[cursor] {
        0 => false,
        1 => true,
        _ => return Err(IndexError::InvalidStoredDocumentField(field)),
    };
    cursor += 1;
    let root = decode_identity(encoded, &mut cursor);
    let session_source_ordinal = u32::from_be_bytes(
        encoded[cursor..cursor + SOURCE_ORDINAL_BYTES]
            .try_into()
            .expect("fixed source ordinal layout"),
    );
    cursor += SOURCE_ORDINAL_BYTES;
    let session_relationship_kind = match encoded[cursor] {
        0 => None,
        tag @ 1..=5 => Some(tag),
        _ => return Err(IndexError::InvalidStoredDocumentField(field)),
    };
    cursor += 1;
    let session_witness_present = match encoded[cursor] {
        0 => false,
        1 => true,
        _ => return Err(IndexError::InvalidStoredDocumentField(field)),
    };
    if (!has_parent && parent.digest != [0; 32]) || (!has_root && root.digest != [0; 32]) {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(SpillVerificationIdentities {
        event,
        session,
        parent_session: has_parent.then_some(parent),
        root_session: has_root.then_some(root),
        session_source_ordinal,
        session_relationship_kind,
        session_witness_present,
    })
}

fn decode_identity(
    encoded: &[u8; IDENTITY_SPILL_RECORD_BYTES],
    cursor: &mut usize,
) -> CompactIdentity {
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&encoded[*cursor..*cursor + 32]);
    *cursor += 32;
    CompactIdentity { digest }
}

#[cfg(unix)]
fn read_spill_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read_at(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        filled += read;
    }
    Ok(())
}

#[cfg(windows)]
fn read_spill_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.seek_read(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        filled += read;
    }
    Ok(())
}

#[cfg(unix)]
fn write_spill_at(file: &File, buffer: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buffer, offset)
}

fn write_spill_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buffer.is_empty() {
        let written = write_spill_at(file, buffer, offset)?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buffer = &buffer[written..];
        offset = offset
            .checked_add(written as u64)
            .ok_or_else(|| std::io::Error::other("identity spill offset overflow"))?;
    }
    Ok(())
}

impl Drop for VerificationSpill {
    fn drop(&mut self) {
        if let Some(witness) = &self.cleanup_witness {
            witness.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(windows)]
fn write_spill_at(file: &File, buffer: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buffer, offset)
}

#[cfg(test)]
mod tests;
