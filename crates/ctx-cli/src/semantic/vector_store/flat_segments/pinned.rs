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
    #[cfg(test)]
    active_events: Arc<Vec<FlatActiveEvent>>,
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

    #[cfg(test)]
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
    dimensions: usize,
    stride_bytes: usize,
    vectors: Mmap,
    scoring_chunks: Vec<FlatScoringChunk>,
}

#[derive(Debug, Clone, Copy)]
struct FlatScoringChunk {
    ordinal: usize,
    event_id: Uuid,
    seq: u64,
    source_text_hash: FlatSourceHash,
    chunk_index: u32,
    start_char: u32,
    end_char: u32,
}

impl PinnedScanSegment {
    #[cfg(test)]
    pub(in crate::semantic) fn active_chunk_count(&self) -> usize {
        self.inner.scoring_chunks.len()
    }

    #[cfg(test)]
    pub(in crate::semantic) fn chunks(&self) -> FlatScanChunkIter<'_> {
        FlatScanChunkIter {
            segment: self,
            chunks: self.inner.scoring_chunks.iter(),
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
        let index = self
            .inner
            .scoring_chunks
            .binary_search_by_key(&ordinal, |chunk| chunk.ordinal)
            .ok()?;
        Some(self.chunk_ref(&self.inner.scoring_chunks[index]))
    }

    fn vector(&self, ordinal: usize) -> &[f32] {
        let start = HEADER_BYTES + ordinal * self.inner.stride_bytes;
        let pointer = self.inner.vectors[start..].as_ptr().cast::<f32>();
        // The format fixes the payload at a page-aligned offset and every row
        // at a 64-byte stride. Opening rejects non-little-endian targets and
        // validates the complete mapped byte range before this slice is built.
        unsafe { std::slice::from_raw_parts(pointer, self.inner.dimensions) }
    }

    fn chunk_ref(&self, chunk: &FlatScoringChunk) -> FlatScanChunkRef<'_> {
        FlatScanChunkRef {
            event_id: chunk.event_id,
            seq: chunk.seq,
            source_text_hash: chunk.source_text_hash,
            chunk_index: chunk.chunk_index,
            start_char: chunk.start_char,
            end_char: chunk.end_char,
            vector: self.vector(chunk.ordinal),
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

#[cfg(test)]
pub(in crate::semantic) struct FlatScanChunkIter<'a> {
    segment: &'a PinnedScanSegment,
    chunks: std::slice::Iter<'a, FlatScoringChunk>,
}

#[cfg(test)]
impl<'a> Iterator for FlatScanChunkIter<'a> {
    type Item = FlatScanChunkRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.chunks
            .next()
            .map(|chunk| self.segment.chunk_ref(chunk))
    }
}

pub(in crate::semantic) struct FlatScanChunkRef<'a> {
    pub(in crate::semantic) event_id: Uuid,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) chunk_index: u32,
    pub(in crate::semantic) start_char: u32,
    pub(in crate::semantic) end_char: u32,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::semantic) vector: &'a [f32],
}

pub(super) fn load_pinned_generation(
    root: &Path,
    selected: &SelectedManifest,
) -> FlatResult<PinnedFlatGeneration> {
    let manifest = &selected.envelope.manifest;
    let mut loaded = Vec::with_capacity(manifest.segments.len());
    let (active_events, _) = load_active_events(root, &manifest.model, manifest, None)?;
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
        loaded.push(segment);
    }

    let deleted_events = 0;
    let authority = active_events
        .iter()
        .map(|event| (event.event_id, event))
        .collect::<HashMap<_, _>>();
    let mut scan_segments = Vec::with_capacity(loaded.len());
    let mut active_chunks = 0_usize;
    for segment in loaded {
        let vector_count = usize_from_u64(segment.descriptor.vector_count, "vector count")?;
        let mut scoring_chunks = Vec::new();
        for ordinal in 0..vector_count {
            let metadata = metadata_at(&segment.metadata, ordinal);
            let Some(event) = authority.get(&metadata.event_id).copied() else {
                continue;
            };
            let ordinal_u64 = u64::try_from(ordinal).map_err(|_| {
                FlatStoreError::Corrupt("vector ordinal does not fit u64".to_owned())
            })?;
            let end = event
                .first_vector_ordinal
                .checked_add(u64::from(event.chunk_count))
                .ok_or_else(|| FlatStoreError::Corrupt("event vector range overflow".to_owned()))?;
            if event.vector_generation != segment.descriptor.generation
                || ordinal_u64 < event.first_vector_ordinal
                || ordinal_u64 >= end
            {
                continue;
            }
            if event.source_text_hash != metadata.source_text_hash {
                return Err(FlatStoreError::Corrupt(format!(
                    "active event {} source hash disagrees with vector metadata",
                    metadata.event_id
                )));
            }
            scoring_chunks.push(FlatScoringChunk {
                ordinal,
                event_id: metadata.event_id,
                seq: event.seq,
                source_text_hash: event.source_text_hash,
                chunk_index: metadata.chunk_index,
                start_char: metadata.start_char,
                end_char: metadata.end_char,
            });
            active_chunks = active_chunks
                .checked_add(1)
                .ok_or_else(|| FlatStoreError::Corrupt("active chunk count overflow".to_owned()))?;
        }
        scan_segments.push(PinnedScanSegment {
            inner: Arc::new(PinnedScanSegmentInner {
                dimensions: usize_from_u32(manifest.model.dimensions, "dimensions")?,
                stride_bytes: segment.stride_bytes,
                vectors: segment.vectors,
                scoring_chunks,
            }),
        });
    }

    let active_vector_bytes = u64::try_from(active_chunks)
        .ok()
        .and_then(|count| count.checked_mul(u64::from(manifest.model.dimensions)))
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| FlatStoreError::Corrupt("active vector byte count overflow".to_owned()))?;
    if u64::try_from(active_events.len()).ok() != Some(manifest.active_events)
        || u64::try_from(active_chunks).ok() != Some(manifest.active_chunks)
    {
        return Err(FlatStoreError::Corrupt(
            "manifest active counts disagree with flat event authority".to_owned(),
        ));
    }
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
            #[cfg(test)]
            active_events,
            stats,
        }),
    })
}
