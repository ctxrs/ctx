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

#[cfg(target_arch = "x86_64")]
#[test]
fn avx_dot_product_preserves_scalar_score_bits() {
    if !std::arch::is_x86_feature_detected!("avx") {
        return;
    }
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    let mut value = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = ((state >> 40) as u32) as f32 / ((1_u32 << 24) - 1) as f32;
        (unit * 2.0) - 1.0
    };
    for dimensions in [1, 7, 8, 9, 15, 16, 31, 32, 383, 384, 385] {
        for _ in 0..256 {
            let query = (0..dimensions).map(|_| value()).collect::<Vec<_>>();
            let vector = (0..dimensions).map(|_| value()).collect::<Vec<_>>();
            let scalar = exact_dot_product_f32_scalar(&query, &vector);
            // The host feature check above satisfies the target-feature
            // precondition.
            let avx = unsafe { exact_dot_product_f32_avx(&query, &vector) };
            assert_eq!(
                avx.to_bits(),
                scalar.to_bits(),
                "AVX score changed at {dimensions} dimensions"
            );
        }
    }
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
                location: None,
            };
            for (chunk, vector) in chunks.iter().enumerate().skip(1) {
                let candidate = FlatScanHit {
                    event_id: best.event_id,
                    chunk_ordinal: chunk as u32,
                    similarity: oracle_dot(&query, vector),
                    location: None,
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
    let mut byte_scan = ExactFlatF32Scan::from_query_le_bytes(&le_bytes(&query), config).unwrap();
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
        (
            ActiveChunk::at_location(
                event_id(20),
                0,
                FlatScanLocation {
                    segment_index: 2,
                    segment_ordinal: 40,
                },
            ),
            weak.as_slice(),
        ),
        (
            ActiveChunk::at_location(
                event_id(20),
                1,
                FlatScanLocation {
                    segment_index: 2,
                    segment_ordinal: 41,
                },
            ),
            best.as_slice(),
        ),
        (
            ActiveChunk::at_location(
                event_id(10),
                0,
                FlatScanLocation {
                    segment_index: 1,
                    segment_ordinal: 7,
                },
            ),
            other.as_slice(),
        ),
    ];
    let mut scan = ExactFlatF32Scan::new(&query, FlatScanConfig::new(2, 1)).unwrap();
    scan.scan_f32(records).unwrap();
    let result = scan.finish().unwrap();

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].event_id, event_id(20));
    assert_eq!(result.hits[0].chunk_ordinal, 1);
    assert_eq!(
        result.hits[0].location,
        Some(FlatScanLocation {
            segment_index: 2,
            segment_ordinal: 41,
        })
    );
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
