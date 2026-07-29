// Exact, allocation-bounded scanning for normalized flat F32 vectors.
//
// This module deliberately knows nothing about manifests or segment
// generations. A pinned-reader adapter must resolve generation order,
// tombstones, supersession, and query filters before yielding active chunks.
// All chunks for one logical event must be contiguous, and an event must be
// yielded at most once. That contract lets the scanner retain one in-progress
// event winner and an `O(top_k)` heap instead of an `O(events)` map.

use std::{
    borrow::Cow,
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    error::Error,
    fmt,
};

use uuid::Uuid;

mod scoring;
mod validation;

#[cfg(target_arch = "x86_64")]
#[cfg(test)]
use scoring::exact_dot_product_f32_avx;
#[cfg(test)]
use scoring::exact_dot_product_f32_scalar;
use scoring::{validate_and_dot_le_bytes, ExactDotProductKernel};
use validation::{validate_config, validate_normalized_f32};

pub(in crate::semantic) const DEFAULT_NORMALIZATION_TOLERANCE: f64 = 1.0e-3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::semantic) struct FlatScanConfig {
    pub(in crate::semantic) dimensions: usize,
    pub(in crate::semantic) top_k: usize,
    /// Maximum absolute error accepted for `sum(value²)` relative to `1.0`.
    pub(in crate::semantic) normalization_tolerance: f64,
}

impl FlatScanConfig {
    pub(in crate::semantic) const fn new(dimensions: usize, top_k: usize) -> Self {
        Self {
            dimensions,
            top_k,
            normalization_tolerance: DEFAULT_NORMALIZATION_TOLERANCE,
        }
    }

    pub(in crate::semantic) const fn with_normalization_tolerance(
        mut self,
        tolerance: f64,
    ) -> Self {
        self.normalization_tolerance = tolerance;
        self
    }
}

/// Metadata required by the exact scan.
///
/// The caller can retain richer metadata separately and resolve a returned hit
/// directly through its optional pinned-generation location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::semantic) struct FlatScanLocation {
    pub(in crate::semantic) segment_index: usize,
    pub(in crate::semantic) segment_ordinal: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::semantic) struct ActiveChunk {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) chunk_ordinal: u32,
    location: Option<FlatScanLocation>,
}

impl ActiveChunk {
    pub(in crate::semantic) const fn new(event_id: Uuid, chunk_ordinal: u32) -> Self {
        Self {
            event_id,
            chunk_ordinal,
            location: None,
        }
    }

    pub(in crate::semantic) const fn at_location(
        event_id: Uuid,
        chunk_ordinal: u32,
        location: FlatScanLocation,
    ) -> Self {
        Self {
            event_id,
            chunk_ordinal,
            location: Some(location),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::semantic) enum FlatScanSkipReason {
    Filtered,
    Tombstoned,
    Superseded,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::semantic) struct FlatScanCounters {
    /// Active and skipped logical events observed at the adapter boundary.
    pub(in crate::semantic) events_seen: usize,
    /// Active logical events for which at least one vector was scored.
    pub(in crate::semantic) events_scored: usize,
    /// Active and skipped chunks observed at the adapter boundary.
    pub(in crate::semantic) chunks_seen: usize,
    /// Chunks whose vectors were validated and scored.
    pub(in crate::semantic) chunks_scanned: usize,
    /// Chunks omitted before touching vector bytes.
    pub(in crate::semantic) chunks_skipped: usize,
    /// F32 vector bytes read; checked scan paths also validate these bytes.
    pub(in crate::semantic) vector_bytes_read: usize,
    pub(in crate::semantic) dot_products: usize,
    pub(in crate::semantic) filtered_events: usize,
    pub(in crate::semantic) tombstoned_events: usize,
    pub(in crate::semantic) superseded_events: usize,
    pub(in crate::semantic) heap_pushes: usize,
    pub(in crate::semantic) heap_replacements: usize,
    pub(in crate::semantic) heap_rejections: usize,
    pub(in crate::semantic) peak_heap_len: usize,
}

#[derive(Debug, Clone)]
pub(in crate::semantic) struct FlatScanResult {
    /// Similarity descending, then event UUID ascending.
    pub(in crate::semantic) hits: Vec<FlatScanHit>,
    pub(in crate::semantic) counters: FlatScanCounters,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::semantic) struct FlatScanHit {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) chunk_ordinal: u32,
    pub(in crate::semantic) similarity: f32,
    pub(in crate::semantic) location: Option<FlatScanLocation>,
}

impl PartialEq for FlatScanHit {
    fn eq(&self, other: &Self) -> bool {
        // Location is provenance, not rank identity. Active-generation
        // resolution guarantees one source record for an event chunk.
        self.event_id == other.event_id
            && self.chunk_ordinal == other.chunk_ordinal
            && self.similarity.total_cmp(&other.similarity) == Ordering::Equal
    }
}

impl Eq for FlatScanHit {}

impl PartialOrd for FlatScanHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FlatScanHit {
    fn cmp(&self, other: &Self) -> Ordering {
        // Greater means a better result. Reverse<FlatScanHit> therefore keeps
        // the worst retained result at the BinaryHeap root.
        self.similarity
            .total_cmp(&other.similarity)
            .then_with(|| other.event_id.cmp(&self.event_id))
            .then_with(|| other.chunk_ordinal.cmp(&self.chunk_ordinal))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::semantic) enum FlatScanInput {
    Query,
    Vector,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::semantic) enum FlatScanError {
    ZeroDimensions,
    DimensionByteSizeOverflow {
        dimensions: usize,
    },
    InvalidNormalizationTolerance {
        tolerance: f64,
    },
    DimensionMismatch {
        input: FlatScanInput,
        expected: usize,
        actual: usize,
        chunk_ordinal: Option<u32>,
    },
    ByteLengthMismatch {
        input: FlatScanInput,
        expected: usize,
        actual: usize,
        chunk_ordinal: Option<u32>,
    },
    NonFinite {
        input: FlatScanInput,
        dimension: usize,
        chunk_ordinal: Option<u32>,
    },
    ZeroNorm {
        input: FlatScanInput,
        chunk_ordinal: Option<u32>,
    },
    NotNormalized {
        input: FlatScanInput,
        norm_squared: f64,
        tolerance: f64,
        chunk_ordinal: Option<u32>,
    },
    NonFiniteDotProduct {
        chunk_ordinal: Option<u32>,
    },
    ScanAlreadyFailed,
}

impl fmt::Display for FlatScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimensions => formatter.write_str("flat F32 dimensions must be non-zero"),
            Self::DimensionByteSizeOverflow { dimensions } => write!(
                formatter,
                "flat F32 byte size overflows for {dimensions} dimensions"
            ),
            Self::InvalidNormalizationTolerance { tolerance } => write!(
                formatter,
                "flat F32 normalization tolerance must be finite and in [0, 1), got {tolerance}"
            ),
            Self::DimensionMismatch {
                input,
                expected,
                actual,
                chunk_ordinal,
            } => write!(
                formatter,
                "{} has {actual} dimensions, expected {expected}{}",
                input.label(),
                chunk_suffix(*chunk_ordinal)
            ),
            Self::ByteLengthMismatch {
                input,
                expected,
                actual,
                chunk_ordinal,
            } => write!(
                formatter,
                "{} has {actual} F32 bytes, expected {expected}{}",
                input.label(),
                chunk_suffix(*chunk_ordinal)
            ),
            Self::NonFinite {
                input,
                dimension,
                chunk_ordinal,
            } => write!(
                formatter,
                "{} contains a non-finite value at dimension {dimension}{}",
                input.label(),
                chunk_suffix(*chunk_ordinal)
            ),
            Self::ZeroNorm {
                input,
                chunk_ordinal,
            } => write!(
                formatter,
                "{} has zero L2 norm{}",
                input.label(),
                chunk_suffix(*chunk_ordinal)
            ),
            Self::NotNormalized {
                input,
                norm_squared,
                tolerance,
                chunk_ordinal,
            } => write!(
                formatter,
                "{} is not L2-normalized (norm squared {norm_squared}, tolerance {tolerance}){}",
                input.label(),
                chunk_suffix(*chunk_ordinal)
            ),
            Self::NonFiniteDotProduct { chunk_ordinal } => write!(
                formatter,
                "flat F32 dot product is non-finite{}",
                chunk_suffix(*chunk_ordinal)
            ),
            Self::ScanAlreadyFailed => {
                formatter.write_str("flat F32 scan cannot continue after a validation failure")
            }
        }
    }
}

impl Error for FlatScanError {}

impl FlatScanInput {
    const fn label(self) -> &'static str {
        match self {
            Self::Query => "semantic query",
            Self::Vector => "semantic vector",
        }
    }
}

fn chunk_suffix(chunk_ordinal: Option<u32>) -> String {
    chunk_ordinal.map_or_else(String::new, |ordinal| format!(" in chunk {ordinal}"))
}

/// Streaming exact scanner over active, event-grouped chunks.
///
/// The borrowed F32 query constructor performs no allocation. The little-endian
/// byte constructor owns exactly one `dimensions * 4` decoded query buffer.
/// During scanning, retained state is one event candidate plus at most `top_k`
/// heap entries.
pub(in crate::semantic) struct ExactFlatF32Scan<'query> {
    query: Cow<'query, [f32]>,
    config: FlatScanConfig,
    vector_bytes: usize,
    dot_product_kernel: ExactDotProductKernel,
    pending_event: Option<FlatScanHit>,
    heap: BinaryHeap<Reverse<FlatScanHit>>,
    counters: FlatScanCounters,
    failed: bool,
}

impl<'query> ExactFlatF32Scan<'query> {
    pub(in crate::semantic) fn new(
        query: &'query [f32],
        config: FlatScanConfig,
    ) -> Result<Self, FlatScanError> {
        Self::from_query(Cow::Borrowed(query), config)
    }

    fn from_query(
        query: Cow<'query, [f32]>,
        config: FlatScanConfig,
    ) -> Result<Self, FlatScanError> {
        let vector_bytes = validate_config(config)?;
        if query.len() != config.dimensions {
            return Err(FlatScanError::DimensionMismatch {
                input: FlatScanInput::Query,
                expected: config.dimensions,
                actual: query.len(),
                chunk_ordinal: None,
            });
        }
        validate_normalized_f32(
            &query,
            FlatScanInput::Query,
            None,
            config.normalization_tolerance,
        )?;
        Ok(Self {
            query,
            config,
            vector_bytes,
            dot_product_kernel: ExactDotProductKernel::detect(),
            pending_event: None,
            heap: BinaryHeap::new(),
            counters: FlatScanCounters::default(),
            failed: false,
        })
    }

    /// Scan active records backed by native F32 slices.
    ///
    /// The iterator may span segments, but all chunks for an event must be
    /// contiguous and already resolved against newer records and tombstones.
    pub(in crate::semantic) fn scan_f32<'vector, I>(
        &mut self,
        chunks: I,
    ) -> Result<(), FlatScanError>
    where
        I: IntoIterator<Item = (ActiveChunk, &'vector [f32])>,
    {
        self.ensure_usable()?;
        for (metadata, vector) in chunks {
            if let Err(error) = self.scan_one_f32(metadata, vector) {
                self.failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Scan F32 slices whose containing segment has already validated every
    /// finite component and L2 norm.
    ///
    /// Dimensions and the resulting dot product are still checked per query.
    /// Pinned mmap readers should use this path after their checksum and vector
    /// payload validation; callers without that proof must use `scan_f32`.
    pub(in crate::semantic) fn scan_prevalidated_f32<'vector, I>(
        &mut self,
        chunks: I,
    ) -> Result<(), FlatScanError>
    where
        I: IntoIterator<Item = (ActiveChunk, &'vector [f32])>,
    {
        self.ensure_usable()?;
        for (metadata, vector) in chunks {
            if let Err(error) = self.scan_one_prevalidated_f32(metadata, vector) {
                self.failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Scan active records backed by unaligned little-endian F32 bytes.
    ///
    /// This path decodes directly from an mmap-compatible byte slice without a
    /// per-vector allocation.
    pub(in crate::semantic) fn scan_le_bytes<'vector, I>(
        &mut self,
        chunks: I,
    ) -> Result<(), FlatScanError>
    where
        I: IntoIterator<Item = (ActiveChunk, &'vector [u8])>,
    {
        self.ensure_usable()?;
        for (metadata, vector) in chunks {
            if let Err(error) = self.scan_one_le_bytes(metadata, vector) {
                self.failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Account for an event removed by filters or active-generation resolution.
    ///
    /// No vector bytes are touched. Calling this method is also an explicit
    /// event boundary for the active chunk stream.
    pub(in crate::semantic) fn skip_event(
        &mut self,
        chunk_count: usize,
        reason: FlatScanSkipReason,
    ) -> Result<(), FlatScanError> {
        self.ensure_usable()?;
        self.flush_pending();
        self.counters.events_seen = self.counters.events_seen.saturating_add(1);
        self.counters.chunks_seen = self.counters.chunks_seen.saturating_add(chunk_count);
        self.counters.chunks_skipped = self.counters.chunks_skipped.saturating_add(chunk_count);
        match reason {
            FlatScanSkipReason::Filtered => {
                self.counters.filtered_events = self.counters.filtered_events.saturating_add(1);
            }
            FlatScanSkipReason::Tombstoned => {
                self.counters.tombstoned_events = self.counters.tombstoned_events.saturating_add(1);
            }
            FlatScanSkipReason::Superseded => {
                self.counters.superseded_events = self.counters.superseded_events.saturating_add(1);
            }
        }
        Ok(())
    }

    pub(in crate::semantic) const fn counters(&self) -> &FlatScanCounters {
        &self.counters
    }

    pub(in crate::semantic) fn finish(mut self) -> Result<FlatScanResult, FlatScanError> {
        self.ensure_usable()?;
        self.flush_pending();
        let mut hits = self
            .heap
            .into_iter()
            .map(|Reverse(hit)| hit)
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| right.cmp(left));
        Ok(FlatScanResult {
            hits,
            counters: self.counters,
        })
    }

    fn scan_one_f32(&mut self, metadata: ActiveChunk, vector: &[f32]) -> Result<(), FlatScanError> {
        self.observe_chunk(metadata);
        if vector.len() != self.config.dimensions {
            return Err(FlatScanError::DimensionMismatch {
                input: FlatScanInput::Vector,
                expected: self.config.dimensions,
                actual: vector.len(),
                chunk_ordinal: Some(metadata.chunk_ordinal),
            });
        }
        self.counters.vector_bytes_read = self
            .counters
            .vector_bytes_read
            .saturating_add(self.vector_bytes);
        validate_normalized_f32(
            vector,
            FlatScanInput::Vector,
            Some(metadata.chunk_ordinal),
            self.config.normalization_tolerance,
        )?;
        let similarity = self.dot_product_kernel.dot(&self.query, vector);
        if !similarity.is_finite() {
            return Err(FlatScanError::NonFiniteDotProduct {
                chunk_ordinal: Some(metadata.chunk_ordinal),
            });
        }
        self.record_scored_chunk(metadata, similarity);
        Ok(())
    }

    fn scan_one_prevalidated_f32(
        &mut self,
        metadata: ActiveChunk,
        vector: &[f32],
    ) -> Result<(), FlatScanError> {
        self.observe_chunk(metadata);
        if vector.len() != self.config.dimensions {
            return Err(FlatScanError::DimensionMismatch {
                input: FlatScanInput::Vector,
                expected: self.config.dimensions,
                actual: vector.len(),
                chunk_ordinal: Some(metadata.chunk_ordinal),
            });
        }
        self.counters.vector_bytes_read = self
            .counters
            .vector_bytes_read
            .saturating_add(self.vector_bytes);
        let similarity = self.dot_product_kernel.dot(&self.query, vector);
        if !similarity.is_finite() {
            return Err(FlatScanError::NonFiniteDotProduct {
                chunk_ordinal: Some(metadata.chunk_ordinal),
            });
        }
        self.record_scored_chunk(metadata, similarity);
        Ok(())
    }

    fn scan_one_le_bytes(
        &mut self,
        metadata: ActiveChunk,
        vector: &[u8],
    ) -> Result<(), FlatScanError> {
        self.observe_chunk(metadata);
        if vector.len() != self.vector_bytes {
            return Err(FlatScanError::ByteLengthMismatch {
                input: FlatScanInput::Vector,
                expected: self.vector_bytes,
                actual: vector.len(),
                chunk_ordinal: Some(metadata.chunk_ordinal),
            });
        }
        self.counters.vector_bytes_read = self
            .counters
            .vector_bytes_read
            .saturating_add(self.vector_bytes);
        let similarity = validate_and_dot_le_bytes(
            &self.query,
            vector,
            Some(metadata.chunk_ordinal),
            self.config.normalization_tolerance,
        )?;
        self.record_scored_chunk(metadata, similarity);
        Ok(())
    }

    fn ensure_usable(&self) -> Result<(), FlatScanError> {
        if self.failed {
            Err(FlatScanError::ScanAlreadyFailed)
        } else {
            Ok(())
        }
    }

    fn observe_chunk(&mut self, metadata: ActiveChunk) {
        self.counters.chunks_seen = self.counters.chunks_seen.saturating_add(1);
        if self
            .pending_event
            .is_none_or(|pending| pending.event_id != metadata.event_id)
        {
            self.flush_pending();
            self.counters.events_seen = self.counters.events_seen.saturating_add(1);
        }
    }

    fn record_scored_chunk(&mut self, metadata: ActiveChunk, similarity: f32) {
        self.counters.chunks_scanned = self.counters.chunks_scanned.saturating_add(1);
        self.counters.dot_products = self.counters.dot_products.saturating_add(1);
        let candidate = FlatScanHit {
            event_id: metadata.event_id,
            chunk_ordinal: metadata.chunk_ordinal,
            similarity,
            location: metadata.location,
        };
        match &mut self.pending_event {
            Some(pending) if pending.event_id == candidate.event_id => {
                let better_chunk = candidate.similarity.total_cmp(&pending.similarity)
                    == Ordering::Greater
                    || (candidate.similarity.total_cmp(&pending.similarity) == Ordering::Equal
                        && candidate.chunk_ordinal < pending.chunk_ordinal);
                if better_chunk {
                    *pending = candidate;
                }
            }
            None => self.pending_event = Some(candidate),
            Some(_) => {
                // observe_chunk flushes a prior event before this method runs.
                self.pending_event = Some(candidate);
            }
        }
    }

    fn flush_pending(&mut self) {
        let Some(candidate) = self.pending_event.take() else {
            return;
        };
        self.counters.events_scored = self.counters.events_scored.saturating_add(1);
        if self.config.top_k == 0 {
            self.counters.heap_rejections = self.counters.heap_rejections.saturating_add(1);
            return;
        }
        if self.heap.len() < self.config.top_k {
            self.heap.push(Reverse(candidate));
            self.counters.heap_pushes = self.counters.heap_pushes.saturating_add(1);
            self.counters.peak_heap_len = self.counters.peak_heap_len.max(self.heap.len());
            return;
        }
        let should_replace = self
            .heap
            .peek()
            .is_some_and(|Reverse(worst)| candidate > *worst);
        if should_replace {
            let _ = self.heap.pop();
            self.heap.push(Reverse(candidate));
            self.counters.heap_replacements = self.counters.heap_replacements.saturating_add(1);
        } else {
            self.counters.heap_rejections = self.counters.heap_rejections.saturating_add(1);
        }
    }
}

impl ExactFlatF32Scan<'static> {
    /// Decode a little-endian F32 query once, then scan without query-sized
    /// allocation growth.
    pub(in crate::semantic) fn from_query_le_bytes(
        query: &[u8],
        config: FlatScanConfig,
    ) -> Result<Self, FlatScanError> {
        let vector_bytes = validate_config(config)?;
        if query.len() != vector_bytes {
            return Err(FlatScanError::ByteLengthMismatch {
                input: FlatScanInput::Query,
                expected: vector_bytes,
                actual: query.len(),
                chunk_ordinal: None,
            });
        }
        let mut decoded = Vec::with_capacity(config.dimensions);
        for bytes in query.chunks_exact(std::mem::size_of::<f32>()) {
            decoded.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
        Self::from_query(Cow::Owned(decoded), config)
    }
}

#[cfg(test)]
mod tests;
