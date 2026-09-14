#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceBackedSemanticOutcome {
    /// Exact number of complete Core records decoded from changed-source pages.
    pub(crate) records_decoded: usize,
    /// Exact stored Core JSON bytes decoded from changed-source pages.
    pub(crate) record_bytes_decoded: u64,
    pub(crate) records_embedded: usize,
    pub(crate) records_reused: usize,
    pub(crate) records_filtered: usize,
    pub(crate) invalidated_chunks: usize,
    pub(crate) deleted_chunks: usize,
    pub(crate) vectors_touched: u64,
    pub(crate) vector_bytes_touched: u64,
    pub(crate) metadata_records_touched: u64,
    pub(crate) ready: bool,
    pub(crate) work_remaining: bool,
    pub(crate) semantic_progress_sequence: Option<u64>,
    pub(crate) full_rebuild_boundary: bool,
}

impl SourceBackedSemanticOutcome {
    pub fn records_decoded(&self) -> usize {
        self.records_decoded
    }

    pub fn record_bytes_decoded(&self) -> u64 {
        self.record_bytes_decoded
    }

    pub fn records_embedded(&self) -> usize {
        self.records_embedded
    }

    pub fn records_reused(&self) -> usize {
        self.records_reused
    }

    pub fn records_filtered(&self) -> usize {
        self.records_filtered
    }

    pub fn invalidated_chunks(&self) -> usize {
        self.invalidated_chunks
    }

    pub fn deleted_chunks(&self) -> usize {
        self.deleted_chunks
    }

    pub fn vectors_touched(&self) -> u64 {
        self.vectors_touched
    }

    pub fn vector_bytes_touched(&self) -> u64 {
        self.vector_bytes_touched
    }

    pub fn metadata_records_touched(&self) -> u64 {
        self.metadata_records_touched
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn work_remaining(&self) -> bool {
        self.work_remaining
    }

    /// Opaque durable progress marker for the exact source-backed semantic
    /// target reconciled by this outcome.
    pub fn semantic_progress_sequence(&self) -> Option<u64> {
        self.semantic_progress_sequence
    }
}

pub(super) fn merge_outcome(
    total: &mut SourceBackedSemanticOutcome,
    next: SourceBackedSemanticOutcome,
) {
    total.records_decoded = total.records_decoded.saturating_add(next.records_decoded);
    total.record_bytes_decoded = total
        .record_bytes_decoded
        .saturating_add(next.record_bytes_decoded);
    total.records_embedded = total.records_embedded.saturating_add(next.records_embedded);
    total.records_reused = total.records_reused.saturating_add(next.records_reused);
    total.records_filtered = total.records_filtered.saturating_add(next.records_filtered);
    total.invalidated_chunks = total
        .invalidated_chunks
        .saturating_add(next.invalidated_chunks);
    total.deleted_chunks = total.deleted_chunks.saturating_add(next.deleted_chunks);
    total.vectors_touched = total.vectors_touched.saturating_add(next.vectors_touched);
    total.vector_bytes_touched = total
        .vector_bytes_touched
        .saturating_add(next.vector_bytes_touched);
    total.metadata_records_touched = total
        .metadata_records_touched
        .saturating_add(next.metadata_records_touched);
    total.ready |= next.ready;
    total.work_remaining |= next.work_remaining;
    total.semantic_progress_sequence = total
        .semantic_progress_sequence
        .max(next.semantic_progress_sequence);
    total.full_rebuild_boundary |= next.full_rebuild_boundary;
}
