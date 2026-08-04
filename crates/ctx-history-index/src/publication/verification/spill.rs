use std::{
    fs::File,
    io::{BufWriter, Write},
};

use tantivy::DocAddress;

use crate::{query::CompactIdentity, IndexError, Result};

pub(super) const VERIFICATION_SPILL_BUFFER_BYTES: usize = 8 * 1024;
const COMPACT_IDENTITY_BYTES: usize = 32;
const SOURCE_ORDINAL_BYTES: usize = std::mem::size_of::<u32>();
const IDENTITY_SPILL_RECORD_BYTES: usize = COMPACT_IDENTITY_BYTES * 3 + SOURCE_ORDINAL_BYTES + 1;
const QUERY_PROJECTION_ACCUMULATOR_BYTES: usize = 32;
pub(super) const VERIFICATION_SPILL_RECORD_BYTES: usize =
    IDENTITY_SPILL_RECORD_BYTES + QUERY_PROJECTION_ACCUMULATOR_BYTES;
const MAX_VERIFICATION_LAYOUT_HEAP_BYTES: usize = 16 * 1024 * 1024;

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
    pub(super) session: CompactIdentity,
    pub(super) parent_session: Option<CompactIdentity>,
    pub(super) root_session: CompactIdentity,
    pub(super) session_source_ordinal: u32,
}

/// Anonymous fixed-size identity and query-projection state for one audit.
#[derive(Debug)]
pub(super) struct VerificationSpill {
    file: File,
    segment_offsets: Vec<u64>,
    segment_max_docs: Vec<u32>,
    logical_bytes: u64,
    cleanup_witness: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub(super) struct SegmentVerificationWriter<'a> {
    identity_writer: BufWriter<SpillAtWriter<'a>>,
    projection_writer: BufWriter<SpillAtWriter<'a>>,
    next_doc_id: u32,
    end_doc_id: u32,
}

pub(super) struct ProjectionDeltas {
    segments: Vec<Vec<u8>>,
    heap_bytes: usize,
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
        Ok(Self {
            file,
            segment_offsets,
            segment_max_docs,
            logical_bytes,
            cleanup_witness,
        })
    }

    pub(super) fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub(super) fn segment_offsets_heap_bytes(&self) -> Result<usize> {
        self.segment_offsets
            .capacity()
            .checked_mul(std::mem::size_of::<u64>())
            .and_then(|bytes| {
                self.segment_max_docs
                    .capacity()
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
        let mut segments = Vec::with_capacity(self.segment_offsets.len());
        let mut heap_bytes = 0_usize;
        for (&segment_offset, &max_doc) in self.segment_offsets.iter().zip(&self.segment_max_docs) {
            let projection_bytes = projection_segment_bytes(max_doc)?;
            let projection_bytes =
                usize::try_from(projection_bytes).map_err(|_| IndexError::CountOverflow)?;
            heap_bytes = heap_bytes
                .checked_add(projection_bytes)
                .ok_or(IndexError::CountOverflow)?;
            let mut deltas = vec![0_u8; projection_bytes];
            let offset = segment_offset
                .checked_add(identity_segment_bytes(max_doc)?)
                .ok_or(IndexError::CountOverflow)?;
            read_spill_exact_at(&self.file, &mut deltas, offset)?;
            segments.push(deltas);
        }
        Ok(ProjectionDeltas {
            segments,
            heap_bytes,
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
        self.heap_bytes
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
        let segment = self
            .segments
            .get(address.segment_ord as usize)
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?;
        let start = usize::try_from(address.doc_id)
            .map_err(|_| IndexError::CountOverflow)?
            .checked_mul(QUERY_PROJECTION_ACCUMULATOR_BYTES)
            .ok_or(IndexError::CountOverflow)?;
        let end = start
            .checked_add(QUERY_PROJECTION_ACCUMULATOR_BYTES)
            .ok_or(IndexError::CountOverflow)?;
        Ok(segment
            .get(start..end)
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?
            .iter()
            .all(|byte| *byte == 0))
    }

    fn accumulator_mut(&mut self, address: DocAddress) -> Result<&mut [u8; 32]> {
        let segment = self
            .segments
            .get_mut(address.segment_ord as usize)
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))?;
        let start = usize::try_from(address.doc_id)
            .map_err(|_| IndexError::CountOverflow)?
            .checked_mul(QUERY_PROJECTION_ACCUMULATOR_BYTES)
            .ok_or(IndexError::CountOverflow)?;
        let end = start
            .checked_add(QUERY_PROJECTION_ACCUMULATOR_BYTES)
            .ok_or(IndexError::CountOverflow)?;
        segment
            .get_mut(start..end)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(IndexError::InvalidStoredDocumentField("query_projection"))
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

fn projection_segment_bytes(max_doc: u32) -> Result<u64> {
    u64::from(max_doc)
        .checked_mul(QUERY_PROJECTION_ACCUMULATOR_BYTES as u64)
        .ok_or(IndexError::CountOverflow)
}

fn encode_record(identities: SpillVerificationIdentities) -> [u8; IDENTITY_SPILL_RECORD_BYTES] {
    let mut encoded = [0_u8; IDENTITY_SPILL_RECORD_BYTES];
    let mut cursor = 0;
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
    encode_identity(&mut encoded, &mut cursor, identities.root_session);
    encoded[cursor..cursor + SOURCE_ORDINAL_BYTES]
        .copy_from_slice(&identities.session_source_ordinal.to_be_bytes());
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
    let session = decode_identity(encoded, &mut cursor);
    let has_parent = match encoded[cursor] {
        0 => false,
        1 => true,
        _ => return Err(IndexError::InvalidStoredDocumentField(field)),
    };
    cursor += 1;
    let parent = decode_identity(encoded, &mut cursor);
    let root_session = decode_identity(encoded, &mut cursor);
    let session_source_ordinal = u32::from_be_bytes(
        encoded[cursor..cursor + SOURCE_ORDINAL_BYTES]
            .try_into()
            .expect("fixed source ordinal layout"),
    );
    if !has_parent && parent.digest != [0; 32] {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(SpillVerificationIdentities {
        session,
        parent_session: has_parent.then_some(parent),
        root_session,
        session_source_ordinal,
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

impl Drop for VerificationSpill {
    fn drop(&mut self) {
        if let Some(witness) = &self.cleanup_witness {
            witness.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identities() -> SpillVerificationIdentities {
        SpillVerificationIdentities {
            session: CompactIdentity { digest: [1; 32] },
            parent_session: Some(CompactIdentity { digest: [2; 32] }),
            root_session: CompactIdentity { digest: [3; 32] },
            session_source_ordinal: 4,
        }
    }

    #[test]
    fn anonymous_file_is_cleaned_up() {
        let cleaned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spill = VerificationSpill::create_with_witness(
            [1].into_iter(),
            Some(std::sync::Arc::clone(&cleaned)),
        )
        .unwrap();
        assert!(!cleaned.load(std::sync::atomic::Ordering::SeqCst));
        drop(spill);
        assert!(cleaned.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn layout_limit_rejects_segment_metadata_before_allocation() {
        let one_segment_bytes = std::mem::size_of::<u64>() + std::mem::size_of::<u32>();
        let error = preflight_spill_layout([0, 0].into_iter(), one_segment_bytes).unwrap_err();
        assert!(matches!(
            error,
            IndexError::VerificationScratchLimitExceeded {
                required_bytes,
                maximum_bytes,
            } if required_bytes == (one_segment_bytes * 2) as u64
                && maximum_bytes == one_segment_bytes as u64
        ));
    }

    #[test]
    fn logical_layout_has_no_fixed_four_million_slot_ceiling() {
        const FIRST_SLOT_BEYOND_OLD_LIMIT: u32 = 4_036_624;

        let spill = VerificationSpill::create([FIRST_SLOT_BEYOND_OLD_LIMIT].into_iter()).unwrap();
        let final_doc_id = FIRST_SLOT_BEYOND_OLD_LIMIT - 1;
        let mut writer = spill
            .segment_range_writer(
                0,
                final_doc_id,
                FIRST_SLOT_BEYOND_OLD_LIMIT,
                FIRST_SLOT_BEYOND_OLD_LIMIT,
            )
            .unwrap();
        writer
            .write_record(final_doc_id, identities(), ProjectionAccumulator::default())
            .unwrap();
        writer.finish().unwrap();

        assert_eq!(
            spill.logical_bytes(),
            u64::from(FIRST_SLOT_BEYOND_OLD_LIMIT) * VERIFICATION_SPILL_RECORD_BYTES as u64
        );
        assert_eq!(
            spill
                .record(DocAddress::new(0, final_doc_id), "test")
                .unwrap()
                .session
                .digest,
            [1; 32]
        );
    }

    #[test]
    fn projection_accumulator_preserves_multiset_addition_modulo_256_bits() {
        let first = [0xff; QUERY_PROJECTION_ACCUMULATOR_BYTES];
        let second = [1; QUERY_PROJECTION_ACCUMULATOR_BYTES];
        let mut accumulator = ProjectionAccumulator::default();
        accumulator.subtract(&first);
        accumulator.subtract(&second);
        accumulator.add(&second);
        accumulator.add(&first);
        assert!(accumulator.is_zero());
    }

    #[test]
    fn contiguous_projection_state_roundtrips_separately_from_identities() {
        let digest = [9; QUERY_PROJECTION_ACCUMULATOR_BYTES];
        let mut expected = ProjectionAccumulator::default();
        expected.subtract(&digest);
        let spill = VerificationSpill::create([1].into_iter()).unwrap();
        let mut writer = spill.segment_writer(0, 1).unwrap();
        writer.write_record(0, identities(), expected).unwrap();
        writer.finish().unwrap();

        let stored = spill.record(DocAddress::new(0, 0), "test").unwrap();
        assert_eq!(stored.session.digest, [1; 32]);
        let mut projections = spill.load_projection_deltas().unwrap();
        assert!(!projections.is_complete(DocAddress::new(0, 0)).unwrap());
        projections
            .accumulate(DocAddress::new(0, 0), &digest)
            .unwrap();
        assert!(projections.is_complete(DocAddress::new(0, 0)).unwrap());
    }

    #[test]
    fn disjoint_segment_ranges_write_exact_positional_records() {
        let spill = VerificationSpill::create([4].into_iter()).unwrap();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                let mut writer = spill.segment_range_writer(0, 0, 2, 4).unwrap();
                writer
                    .write_record(0, identities(), ProjectionAccumulator::default())
                    .unwrap();
                writer.write_deleted(1).unwrap();
                writer.finish().unwrap();
            });
            let second = scope.spawn(|| {
                let mut writer = spill.segment_range_writer(0, 2, 4, 4).unwrap();
                writer.write_deleted(2).unwrap();
                writer
                    .write_record(3, identities(), ProjectionAccumulator::default())
                    .unwrap();
                writer.finish().unwrap();
            });
            first.join().unwrap();
            second.join().unwrap();
        });

        assert_eq!(
            spill
                .record(DocAddress::new(0, 0), "test")
                .unwrap()
                .session
                .digest,
            [1; 32]
        );
        assert_eq!(
            spill
                .record(DocAddress::new(0, 3), "test")
                .unwrap()
                .root_session
                .digest,
            [3; 32]
        );
    }
}

#[cfg(windows)]
fn write_spill_at(file: &File, buffer: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buffer, offset)
}
