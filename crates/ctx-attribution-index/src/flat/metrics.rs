use super::FlatSegmentReader;

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlatReadObservability {
    pub bytes_read: u64,
    pub range_reads: u64,
    pub chunk_reads: u64,
    pub flat_cache_bytes: usize,
    pub flat_cache_entries: usize,
    pub flat_cache_high_water_bytes: usize,
    pub flat_cache_high_water_entries: usize,
    pub block_cache_bytes: usize,
    pub block_cache_entries: usize,
}

impl FlatSegmentReader {
    pub fn observe_cache_high_water(&mut self) {
        let (entries, bytes) = self.flat_cache_state();
        self.cache_high_water_entries = self.cache_high_water_entries.max(entries);
        self.cache_high_water_bytes = self.cache_high_water_bytes.max(bytes);
    }

    fn flat_cache_state(&self) -> (usize, usize) {
        let fst_bytes = self
            .fst_cache
            .as_ref()
            .map_or(0, |(_, index)| index.as_fst().as_bytes().len());
        let tombstone_bytes = self
            .tombstone_cache
            .as_ref()
            .map_or(0, |(_, _, bytes)| *bytes);
        (
            usize::from(self.fst_cache.is_some()) + usize::from(self.tombstone_cache.is_some()),
            fst_bytes.saturating_add(tombstone_bytes),
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn observability(&self) -> FlatReadObservability {
        let (flat_cache_entries, flat_cache_bytes) = self.flat_cache_state();
        FlatReadObservability {
            bytes_read: self.read_bytes,
            range_reads: self.range_reads,
            chunk_reads: self.file.chunk_reads(),
            flat_cache_bytes,
            flat_cache_entries,
            flat_cache_high_water_bytes: self.cache_high_water_bytes,
            flat_cache_high_water_entries: self.cache_high_water_entries,
            block_cache_bytes: self.file.cached_plaintext_bytes(),
            block_cache_entries: self.file.cached_chunk_count(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_query_caches(&mut self) {
        self.fst_cache = None;
        self.tombstone_cache = None;
        self.file.clear_chunk_cache();
    }
}
