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
/// by `(event_id, chunk_ordinal)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::semantic) struct ActiveChunk {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) chunk_ordinal: u32,
}

impl ActiveChunk {
    pub(in crate::semantic) const fn new(event_id: Uuid, chunk_ordinal: u32) -> Self {
        Self {
            event_id,
            chunk_ordinal,
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
}

impl PartialEq for FlatScanHit {
    fn eq(&self, other: &Self) -> bool {
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
        let similarity = exact_dot_product_f32(&self.query, vector);
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
        let similarity = exact_dot_product_f32(&self.query, vector);
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

fn validate_config(config: FlatScanConfig) -> Result<usize, FlatScanError> {
    if config.dimensions == 0 {
        return Err(FlatScanError::ZeroDimensions);
    }
    if !config.normalization_tolerance.is_finite()
        || !(0.0..1.0).contains(&config.normalization_tolerance)
    {
        return Err(FlatScanError::InvalidNormalizationTolerance {
            tolerance: config.normalization_tolerance,
        });
    }
    config
        .dimensions
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or(FlatScanError::DimensionByteSizeOverflow {
            dimensions: config.dimensions,
        })
}

fn validate_normalized_f32(
    values: &[f32],
    input: FlatScanInput,
    chunk_ordinal: Option<u32>,
    tolerance: f64,
) -> Result<(), FlatScanError> {
    let mut norm_squared = 0.0_f64;
    for (dimension, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(FlatScanError::NonFinite {
                input,
                dimension,
                chunk_ordinal,
            });
        }
        norm_squared += f64::from(value) * f64::from(value);
    }
    validate_norm_squared(input, chunk_ordinal, norm_squared, tolerance)
}

fn validate_norm_squared(
    input: FlatScanInput,
    chunk_ordinal: Option<u32>,
    norm_squared: f64,
    tolerance: f64,
) -> Result<(), FlatScanError> {
    if norm_squared == 0.0 {
        return Err(FlatScanError::ZeroNorm {
            input,
            chunk_ordinal,
        });
    }
    if !norm_squared.is_finite() || (norm_squared - 1.0).abs() > tolerance {
        return Err(FlatScanError::NotNormalized {
            input,
            norm_squared,
            tolerance,
            chunk_ordinal,
        });
    }
    Ok(())
}

/// Eight independent F32 accumulators match the measured flat-F32 prototype,
/// avoid a loop-carried dependency in the hot path, and remain portable.
/// Reduction order is fixed so every backend can use this as its exact oracle.
#[inline(always)]
fn exact_dot_product_f32(query: &[f32], vector: &[f32]) -> f32 {
    let mut sums = [0.0_f32; 8];
    let mut dimension = 0_usize;
    while dimension + sums.len() <= query.len() {
        sums[0] += query[dimension] * vector[dimension];
        sums[1] += query[dimension + 1] * vector[dimension + 1];
        sums[2] += query[dimension + 2] * vector[dimension + 2];
        sums[3] += query[dimension + 3] * vector[dimension + 3];
        sums[4] += query[dimension + 4] * vector[dimension + 4];
        sums[5] += query[dimension + 5] * vector[dimension + 5];
        sums[6] += query[dimension + 6] * vector[dimension + 6];
        sums[7] += query[dimension + 7] * vector[dimension + 7];
        dimension += sums.len();
    }
    let mut similarity = sums.into_iter().sum::<f32>();
    while dimension < query.len() {
        similarity += query[dimension] * vector[dimension];
        dimension += 1;
    }
    similarity
}

fn validate_and_dot_le_bytes(
    query: &[f32],
    vector: &[u8],
    chunk_ordinal: Option<u32>,
    tolerance: f64,
) -> Result<f32, FlatScanError> {
    let mut norm_squared = 0.0_f64;
    for (dimension, bytes) in vector.chunks_exact(std::mem::size_of::<f32>()).enumerate() {
        let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if !value.is_finite() {
            return Err(FlatScanError::NonFinite {
                input: FlatScanInput::Vector,
                dimension,
                chunk_ordinal,
            });
        }
        norm_squared += f64::from(value) * f64::from(value);
    }
    validate_norm_squared(
        FlatScanInput::Vector,
        chunk_ordinal,
        norm_squared,
        tolerance,
    )?;
    let similarity = exact_dot_product_le_bytes(query, vector);
    if !similarity.is_finite() {
        return Err(FlatScanError::NonFiniteDotProduct { chunk_ordinal });
    }
    Ok(similarity)
}

#[inline(always)]
fn exact_dot_product_le_bytes(query: &[f32], vector: &[u8]) -> f32 {
    let value_at = |dimension: usize| {
        let offset = dimension * std::mem::size_of::<f32>();
        f32::from_le_bytes([
            vector[offset],
            vector[offset + 1],
            vector[offset + 2],
            vector[offset + 3],
        ])
    };
    let mut sums = [0.0_f32; 8];
    let mut dimension = 0_usize;
    while dimension + sums.len() <= query.len() {
        sums[0] += query[dimension] * value_at(dimension);
        sums[1] += query[dimension + 1] * value_at(dimension + 1);
        sums[2] += query[dimension + 2] * value_at(dimension + 2);
        sums[3] += query[dimension + 3] * value_at(dimension + 3);
        sums[4] += query[dimension + 4] * value_at(dimension + 4);
        sums[5] += query[dimension + 5] * value_at(dimension + 5);
        sums[6] += query[dimension + 6] * value_at(dimension + 6);
        sums[7] += query[dimension + 7] * value_at(dimension + 7);
        dimension += sums.len();
    }
    let mut similarity = sums.into_iter().sum::<f32>();
    while dimension < query.len() {
        similarity += query[dimension] * value_at(dimension);
        dimension += 1;
    }
    similarity
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn normalized(mut values: Vec<f32>) -> Vec<f32> {
        let norm = values
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        for value in &mut values {
            *value = (f64::from(*value) / norm) as f32;
        }
        values
    }

    fn le_bytes(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn oracle_dot(query: &[f32], vector: &[f32]) -> f32 {
        let mut lanes = [0.0_f32; 8];
        let mut index = 0_usize;
        while index + lanes.len() <= query.len() {
            for lane in 0..lanes.len() {
                lanes[lane] += query[index + lane] * vector[index + lane];
            }
            index += lanes.len();
        }
        let mut score = lanes.into_iter().sum::<f32>();
        while index < query.len() {
            score += query[index] * vector[index];
            index += 1;
        }
        score
    }

    #[test]
    fn exact_slice_and_byte_scans_match_the_f32_oracle() {
        const DIMENSIONS: usize = 13;
        const EVENTS: usize = 64;
        const CHUNKS: usize = 3;
        const TOP_K: usize = 17;

        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        let mut next_vector = || {
            normalized(
                (0..DIMENSIONS)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        let unit = ((state >> 40) as u32) as f32 / ((1_u32 << 24) - 1) as f32;
                        (unit * 2.0) - 1.0
                    })
                    .collect(),
            )
        };
        let query = next_vector();
        let vectors = (0..EVENTS)
            .map(|_| (0..CHUNKS).map(|_| next_vector()).collect::<Vec<_>>())
            .collect::<Vec<_>>();

        let mut expected = vectors
            .iter()
            .enumerate()
            .map(|(event, chunks)| {
                let mut best = FlatScanHit {
                    event_id: event_id(event as u128 + 1),
                    chunk_ordinal: 0,
                    similarity: oracle_dot(&query, &chunks[0]),
                };
                for (chunk, vector) in chunks.iter().enumerate().skip(1) {
                    let candidate = FlatScanHit {
                        event_id: best.event_id,
                        chunk_ordinal: chunk as u32,
                        similarity: oracle_dot(&query, vector),
                    };
                    if candidate.similarity.total_cmp(&best.similarity) == Ordering::Greater {
                        best = candidate;
                    }
                }
                best
            })
            .collect::<Vec<_>>();
        expected.sort_unstable_by(|left, right| right.cmp(left));
        expected.truncate(TOP_K);

        let config = FlatScanConfig::new(DIMENSIONS, TOP_K);
        let mut slice_scan = ExactFlatF32Scan::new(&query, config).unwrap();
        slice_scan
            .scan_f32(vectors.iter().enumerate().flat_map(|(event, chunks)| {
                chunks.iter().enumerate().map(move |(chunk, vector)| {
                    (
                        ActiveChunk::new(event_id(event as u128 + 1), chunk as u32),
                        vector.as_slice(),
                    )
                })
            }))
            .unwrap();
        let slice_result = slice_scan.finish().unwrap();

        let encoded = vectors
            .iter()
            .map(|chunks| {
                chunks
                    .iter()
                    .map(|vector| le_bytes(vector))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut byte_scan =
            ExactFlatF32Scan::from_query_le_bytes(&le_bytes(&query), config).unwrap();
        byte_scan
            .scan_le_bytes(encoded.iter().enumerate().flat_map(|(event, chunks)| {
                chunks.iter().enumerate().map(move |(chunk, vector)| {
                    (
                        ActiveChunk::new(event_id(event as u128 + 1), chunk as u32),
                        vector.as_slice(),
                    )
                })
            }))
            .unwrap();
        let byte_result = byte_scan.finish().unwrap();

        assert_eq!(slice_result.hits.len(), expected.len());
        assert_eq!(byte_result.hits.len(), expected.len());
        for ((slice_hit, byte_hit), expected_hit) in slice_result
            .hits
            .iter()
            .zip(&byte_result.hits)
            .zip(&expected)
        {
            assert_eq!(slice_hit.event_id, expected_hit.event_id);
            assert_eq!(slice_hit.chunk_ordinal, expected_hit.chunk_ordinal);
            assert_eq!(
                slice_hit.similarity.to_bits(),
                expected_hit.similarity.to_bits()
            );
            assert_eq!(byte_hit, slice_hit);
        }
        assert_eq!(slice_result.counters.dot_products, EVENTS * CHUNKS);
        assert_eq!(byte_result.counters.dot_products, EVENTS * CHUNKS);
    }

    #[test]
    fn prevalidated_mmap_path_matches_the_checked_path() {
        let query = normalized((1..=16).map(|value| value as f32).collect());
        let vectors = [
            normalized((1..=16).rev().map(|value| value as f32).collect()),
            normalized((1..=16).map(|value| (value * value) as f32).collect()),
        ];
        let records = || {
            vectors.iter().enumerate().map(|(index, vector)| {
                (
                    ActiveChunk::new(event_id(index as u128 + 1), index as u32),
                    vector.as_slice(),
                )
            })
        };
        let config = FlatScanConfig::new(query.len(), 2);
        let mut checked = ExactFlatF32Scan::new(&query, config).unwrap();
        checked.scan_f32(records()).unwrap();
        let checked = checked.finish().unwrap();

        let mut prevalidated = ExactFlatF32Scan::new(&query, config).unwrap();
        prevalidated.scan_prevalidated_f32(records()).unwrap();
        let prevalidated = prevalidated.finish().unwrap();

        assert_eq!(prevalidated.hits, checked.hits);
        assert_eq!(prevalidated.counters, checked.counters);
    }

    #[test]
    fn ties_use_uuid_then_lower_chunk_ordinal() {
        let query = [1.0, 0.0];
        let same = [1.0, 0.0];
        let records = [
            (ActiveChunk::new(event_id(2), 9), same.as_slice()),
            (ActiveChunk::new(event_id(2), 4), same.as_slice()),
            (ActiveChunk::new(event_id(1), 7), same.as_slice()),
        ];
        let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 2)).unwrap();
        scan.scan_f32(records).unwrap();
        let result = scan.finish().unwrap();

        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].event_id, event_id(1));
        assert_eq!(result.hits[0].chunk_ordinal, 7);
        assert_eq!(result.hits[1].event_id, event_id(2));
        assert_eq!(result.hits[1].chunk_ordinal, 4);
    }

    #[test]
    fn best_chunk_is_retained_before_top_k_admission() {
        let query = [1.0, 0.0];
        let weak = normalized(vec![1.0, 3.0]);
        let best = [1.0, 0.0];
        let other = normalized(vec![4.0, 3.0]);
        let records = [
            (ActiveChunk::new(event_id(20), 0), weak.as_slice()),
            (ActiveChunk::new(event_id(20), 1), best.as_slice()),
            (ActiveChunk::new(event_id(10), 0), other.as_slice()),
        ];
        let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
        scan.scan_f32(records).unwrap();
        let result = scan.finish().unwrap();

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].event_id, event_id(20));
        assert_eq!(result.hits[0].chunk_ordinal, 1);
        assert_eq!(result.counters.events_scored, 2);
        assert_eq!(result.counters.chunks_scanned, 3);
    }

    #[test]
    fn heap_and_skip_counters_stay_bounded_and_attributable() {
        let query = [1.0, 0.0];
        let orthogonal = [0.0, 1.0];
        let best = [1.0, 0.0];
        let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
        scan.scan_f32([
            (ActiveChunk::new(event_id(1), 0), orthogonal.as_slice()),
            (ActiveChunk::new(event_id(2), 0), best.as_slice()),
            (ActiveChunk::new(event_id(2), 1), best.as_slice()),
        ])
        .unwrap();
        scan.skip_event(2, FlatScanSkipReason::Filtered).unwrap();
        scan.skip_event(1, FlatScanSkipReason::Tombstoned).unwrap();
        scan.skip_event(4, FlatScanSkipReason::Superseded).unwrap();
        assert_eq!(scan.counters().peak_heap_len, 1);
        let result = scan.finish().unwrap();

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].event_id, event_id(2));
        assert_eq!(result.hits[0].chunk_ordinal, 0);
        assert_eq!(
            result.counters,
            FlatScanCounters {
                events_seen: 5,
                events_scored: 2,
                chunks_seen: 10,
                chunks_scanned: 3,
                chunks_skipped: 7,
                vector_bytes_read: 3 * 2 * std::mem::size_of::<f32>(),
                dot_products: 3,
                filtered_events: 1,
                tombstoned_events: 1,
                superseded_events: 1,
                heap_pushes: 1,
                heap_replacements: 1,
                heap_rejections: 0,
                peak_heap_len: 1,
            }
        );
    }

    #[test]
    fn heap_never_retains_more_than_top_k() {
        let query = [1.0, 0.0];
        let mut vectors = Vec::new();
        for index in 0..100 {
            vectors.push(normalized(vec![index as f32 + 1.0, 100.0]));
        }
        let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 7)).unwrap();
        scan.scan_f32(vectors.iter().enumerate().map(|(index, vector)| {
            (
                ActiveChunk::new(event_id(index as u128 + 1), 0),
                vector.as_slice(),
            )
        }))
        .unwrap();
        let result = scan.finish().unwrap();

        assert_eq!(result.hits.len(), 7);
        assert_eq!(result.counters.events_scored, 100);
        assert_eq!(result.counters.peak_heap_len, 7);
    }

    #[test]
    fn query_validation_rejects_bad_contracts() {
        assert!(matches!(
            ExactFlatF32Scan::new(&[1.0], FlatScanConfig::new(0, 1)),
            Err(FlatScanError::ZeroDimensions)
        ));
        assert!(matches!(
            ExactFlatF32Scan::new(&[1.0], FlatScanConfig::new(2, 1)),
            Err(FlatScanError::DimensionMismatch {
                input: FlatScanInput::Query,
                ..
            })
        ));
        assert!(matches!(
            ExactFlatF32Scan::new(&[f32::NAN, 0.0], FlatScanConfig::new(2, 1)),
            Err(FlatScanError::NonFinite {
                input: FlatScanInput::Query,
                ..
            })
        ));
        assert!(matches!(
            ExactFlatF32Scan::new(&[0.0, 0.0], FlatScanConfig::new(2, 1)),
            Err(FlatScanError::ZeroNorm {
                input: FlatScanInput::Query,
                ..
            })
        ));
        assert!(matches!(
            ExactFlatF32Scan::new(&[0.5, 0.0], FlatScanConfig::new(2, 1)),
            Err(FlatScanError::NotNormalized {
                input: FlatScanInput::Query,
                ..
            })
        ));
        assert!(matches!(
            ExactFlatF32Scan::new(
                &[1.0, 0.0],
                FlatScanConfig::new(2, 1).with_normalization_tolerance(f64::NAN),
            ),
            Err(FlatScanError::InvalidNormalizationTolerance { .. })
        ));
        assert!(matches!(
            ExactFlatF32Scan::from_query_le_bytes(&[0; 7], FlatScanConfig::new(2, 1)),
            Err(FlatScanError::ByteLengthMismatch {
                input: FlatScanInput::Query,
                ..
            })
        ));
    }

    #[test]
    fn vector_validation_rejects_slices_and_bytes_and_poisoned_scan() {
        let query = [1.0, 0.0];
        let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
        let short = [1.0];
        assert!(matches!(
            scan.scan_f32([(ActiveChunk::new(event_id(1), 0), short.as_slice())]),
            Err(FlatScanError::DimensionMismatch {
                input: FlatScanInput::Vector,
                ..
            })
        ));
        assert!(matches!(
            scan.scan_f32(std::iter::empty()),
            Err(FlatScanError::ScanAlreadyFailed)
        ));
        assert!(matches!(
            scan.finish(),
            Err(FlatScanError::ScanAlreadyFailed)
        ));

        let mut non_finite = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
        let bad = [f32::INFINITY, 0.0];
        assert!(matches!(
            non_finite.scan_f32([(ActiveChunk::new(event_id(1), 4), bad.as_slice())]),
            Err(FlatScanError::NonFinite {
                input: FlatScanInput::Vector,
                chunk_ordinal: Some(4),
                ..
            })
        ));

        let mut not_normalized = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
        let bad = [0.5, 0.0];
        assert!(matches!(
            not_normalized.scan_f32([(ActiveChunk::new(event_id(1), 5), bad.as_slice())]),
            Err(FlatScanError::NotNormalized {
                input: FlatScanInput::Vector,
                chunk_ordinal: Some(5),
                ..
            })
        ));

        let mut bytes = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
        assert!(matches!(
            bytes.scan_le_bytes([(ActiveChunk::new(event_id(1), 6), [0_u8; 7].as_slice())]),
            Err(FlatScanError::ByteLengthMismatch {
                input: FlatScanInput::Vector,
                chunk_ordinal: Some(6),
                ..
            })
        ));
    }

    #[test]
    fn zero_top_k_scores_without_retaining_hits() {
        let query = [1.0, 0.0];
        let vector = [1.0, 0.0];
        let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 0)).unwrap();
        scan.scan_f32([(ActiveChunk::new(event_id(1), 0), vector.as_slice())])
            .unwrap();
        let result = scan.finish().unwrap();

        assert!(result.hits.is_empty());
        assert_eq!(result.counters.events_scored, 1);
        assert_eq!(result.counters.heap_rejections, 1);
        assert_eq!(result.counters.peak_heap_len, 0);
    }
}
