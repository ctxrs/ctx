use super::*;

#[derive(Clone)]
pub(in crate::semantic) struct PinnedFlatGeneration {
    inner: Arc<PinnedGenerationInner>,
}

struct PinnedGenerationInner {
    generation: u64,
    generation_hash: String,
    contract: FlatModelContract,
    scan_segments: Vec<PinnedScanSegment>,
    active_events: Vec<FlatActiveEvent>,
    stats: FlatActiveStats,
}

impl PinnedFlatGeneration {
    pub(in crate::semantic) fn generation(&self) -> u64 {
        self.inner.generation
    }

    pub(in crate::semantic) fn generation_hash(&self) -> &str {
        &self.inner.generation_hash
    }

    pub(in crate::semantic) fn model_contract(&self) -> &FlatModelContract {
        &self.inner.contract
    }

    pub(in crate::semantic) fn scan_segments(&self) -> &[PinnedScanSegment] {
        &self.inner.scan_segments
    }

    pub(in crate::semantic) fn active_events(&self) -> &[FlatActiveEvent] {
        &self.inner.active_events
    }

    pub(in crate::semantic) fn stats(&self) -> &FlatActiveStats {
        &self.inner.stats
    }
}

#[derive(Clone)]
pub(in crate::semantic) struct PinnedScanSegment {
    inner: Arc<PinnedScanSegmentInner>,
}

struct PinnedScanSegmentInner {
    vector_count: usize,
    dimensions: usize,
    stride_bytes: usize,
    vectors: Mmap,
    metadata: Mmap,
    active_bits: Vec<u64>,
    scoring_chunks: Vec<FlatScoringChunk>,
}

#[derive(Debug, Clone, Copy)]
struct FlatScoringChunk {
    ordinal: usize,
    event_id: Uuid,
    chunk_index: u32,
}

impl PinnedScanSegment {
    pub(in crate::semantic) fn vector_count(&self) -> usize {
        self.inner.vector_count
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_chunk_count(&self) -> usize {
        self.inner.scoring_chunks.len()
    }

    pub(in crate::semantic) fn chunks(&self) -> FlatScanChunkIter<'_> {
        FlatScanChunkIter {
            segment: self,
            ordinal: 0,
        }
    }

    /// Iterate the fixed-width active metadata needed by exact scoring.
    ///
    /// Generation pinning builds this compact projection while validating
    /// metadata, so steady queries neither decode full metadata records nor
    /// test the active bitmap for every stored ordinal.
    pub(in crate::semantic) fn scoring_chunks(&self) -> FlatScoringChunkIter<'_> {
        FlatScoringChunkIter {
            segment: self,
            chunks: self.inner.scoring_chunks.iter(),
        }
    }

    pub(in crate::semantic) fn chunk_at(&self, ordinal: usize) -> Option<FlatScanChunkRef<'_>> {
        if ordinal >= self.vector_count() || !self.is_active(ordinal) {
            return None;
        }
        Some(self.chunk_ref(ordinal))
    }

    fn is_active(&self, ordinal: usize) -> bool {
        let word = ordinal / 64;
        let bit = ordinal % 64;
        self.inner
            .active_bits
            .get(word)
            .is_some_and(|value| value & (1_u64 << bit) != 0)
    }

    fn metadata(&self, ordinal: usize) -> FlatChunkMetadata {
        let start = HEADER_BYTES + ordinal * METADATA_RECORD_BYTES;
        decode_metadata_record(&self.inner.metadata[start..start + METADATA_RECORD_BYTES])
    }

    fn vector(&self, ordinal: usize) -> &[f32] {
        let start = HEADER_BYTES + ordinal * self.inner.stride_bytes;
        let pointer = self.inner.vectors[start..].as_ptr().cast::<f32>();
        // The format fixes the payload at a page-aligned offset and every row
        // at a 64-byte stride. Opening rejects non-little-endian targets and
        // validates the complete mapped byte range before this slice is built.
        unsafe { std::slice::from_raw_parts(pointer, self.inner.dimensions) }
    }

    fn chunk_ref(&self, ordinal: usize) -> FlatScanChunkRef<'_> {
        let metadata = self.metadata(ordinal);
        FlatScanChunkRef {
            event_id: metadata.event_id,
            seq: metadata.seq,
            source_text_hash: metadata.source_text_hash,
            chunk_index: metadata.chunk_index,
            start_char: metadata.start_char,
            end_char: metadata.end_char,
            vector: self.vector(ordinal),
        }
    }
}

pub(in crate::semantic) struct FlatScoringChunkIter<'a> {
    segment: &'a PinnedScanSegment,
    chunks: std::slice::Iter<'a, FlatScoringChunk>,
}

impl<'a> Iterator for FlatScoringChunkIter<'a> {
    type Item = FlatScoringChunkRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let chunk = self.chunks.next()?;
        Some(FlatScoringChunkRef {
            ordinal: chunk.ordinal,
            event_id: chunk.event_id,
            chunk_index: chunk.chunk_index,
            vector: self.segment.vector(chunk.ordinal),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.chunks.size_hint()
    }
}

impl ExactSizeIterator for FlatScoringChunkIter<'_> {}

pub(in crate::semantic) struct FlatScoringChunkRef<'a> {
    pub(in crate::semantic) ordinal: usize,
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) chunk_index: u32,
    pub(in crate::semantic) vector: &'a [f32],
}

pub(in crate::semantic) struct FlatScanChunkIter<'a> {
    segment: &'a PinnedScanSegment,
    ordinal: usize,
}

impl<'a> Iterator for FlatScanChunkIter<'a> {
    type Item = FlatScanChunkRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.ordinal < self.segment.vector_count() {
            let ordinal = self.ordinal;
            self.ordinal += 1;
            if !self.segment.is_active(ordinal) {
                continue;
            }
            return Some(self.segment.chunk_ref(ordinal));
        }
        None
    }
}

pub(in crate::semantic) struct FlatScanChunkRef<'a> {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) chunk_index: u32,
    pub(in crate::semantic) start_char: u32,
    pub(in crate::semantic) end_char: u32,
    pub(in crate::semantic) vector: &'a [f32],
}

pub(super) fn load_pinned_generation(
    root: &Path,
    selected: &SelectedManifest,
) -> FlatResult<PinnedFlatGeneration> {
    let manifest = &selected.envelope.manifest;
    let mut loaded = Vec::with_capacity(manifest.segments.len());
    let mut versions = HashMap::<Uuid, EventVersion>::new();
    let mut stored_chunks = 0_u64;
    let mut stored_vector_bytes = 0_u64;

    for descriptor in &manifest.segments {
        let segment = load_and_validate_segment(root, &manifest.model, descriptor)?;
        stored_chunks = stored_chunks
            .checked_add(descriptor.vector_count)
            .ok_or_else(|| FlatStoreError::Corrupt("stored chunk count overflow".to_owned()))?;
        stored_vector_bytes = stored_vector_bytes
            .checked_add(
                descriptor
                    .vector_count
                    .checked_mul(u64::from(manifest.model.dimensions))
                    .and_then(|value| value.checked_mul(4))
                    .ok_or_else(|| {
                        FlatStoreError::Corrupt("stored vector byte count overflow".to_owned())
                    })?,
            )
            .ok_or_else(|| FlatStoreError::Corrupt("stored vector bytes overflow".to_owned()))?;
        for mutation in &segment.mutations {
            versions.insert(
                mutation.event_id,
                EventVersion {
                    generation: descriptor.generation,
                    kind: mutation.kind,
                },
            );
        }
        loaded.push(segment);
    }

    let deleted_events = versions
        .values()
        .filter(|version| version.kind == MutationKind::Delete)
        .count();
    let mut summaries = BTreeMap::<Uuid, FlatActiveEvent>::new();
    let mut scan_segments = Vec::with_capacity(loaded.len());
    let mut active_chunks = 0_usize;
    for segment in loaded {
        let vector_count = usize_from_u64(segment.descriptor.vector_count, "vector count")?;
        let mut active_bits = vec![0_u64; vector_count.div_ceil(64)];
        let mut scoring_chunks = Vec::new();
        for ordinal in 0..vector_count {
            let metadata = metadata_at(&segment.metadata, ordinal);
            let active = versions.get(&metadata.event_id).is_some_and(|version| {
                version.kind == MutationKind::Replace
                    && version.generation == segment.descriptor.generation
            });
            if !active {
                continue;
            }
            active_bits[ordinal / 64] |= 1_u64 << (ordinal % 64);
            scoring_chunks.push(FlatScoringChunk {
                ordinal,
                event_id: metadata.event_id,
                chunk_index: metadata.chunk_index,
            });
            active_chunks = active_chunks
                .checked_add(1)
                .ok_or_else(|| FlatStoreError::Corrupt("active chunk count overflow".to_owned()))?;
            let entry = summaries
                .entry(metadata.event_id)
                .or_insert(FlatActiveEvent {
                    event_id: metadata.event_id,
                    seq: metadata.seq,
                    source_text_hash: metadata.source_text_hash,
                    chunk_count: 0,
                });
            if entry.seq != metadata.seq || entry.source_text_hash != metadata.source_text_hash {
                return Err(FlatStoreError::Corrupt(format!(
                    "active event {} has inconsistent sequence or source hash",
                    metadata.event_id
                )));
            }
            entry.chunk_count = entry.chunk_count.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt(format!(
                    "active event {} has too many chunks",
                    metadata.event_id
                ))
            })?;
        }
        scan_segments.push(PinnedScanSegment {
            inner: Arc::new(PinnedScanSegmentInner {
                vector_count,
                dimensions: usize_from_u32(manifest.model.dimensions, "dimensions")?,
                stride_bytes: segment.stride_bytes,
                vectors: segment.vectors,
                metadata: segment.metadata,
                active_bits,
                scoring_chunks,
            }),
        });
    }

    let active_vector_bytes = u64::try_from(active_chunks)
        .ok()
        .and_then(|count| count.checked_mul(u64::from(manifest.model.dimensions)))
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| FlatStoreError::Corrupt("active vector byte count overflow".to_owned()))?;
    let active_events = summaries.into_values().collect::<Vec<_>>();
    let stats = FlatActiveStats {
        generation: manifest.generation,
        generation_hash: Some(selected.generation_hash.clone()),
        segment_count: manifest.segments.len(),
        active_events: active_events.len(),
        active_chunks,
        active_vector_bytes,
        stored_chunks,
        stored_vector_bytes,
        deleted_events,
    };
    Ok(PinnedFlatGeneration {
        inner: Arc::new(PinnedGenerationInner {
            generation: manifest.generation,
            generation_hash: selected.generation_hash.clone(),
            contract: manifest.model.clone(),
            scan_segments,
            active_events,
            stats,
        }),
    })
}
