use std::{cmp::Ordering, collections::BTreeMap};

use super::*;

struct SourceFusionEvidence {
    event: EventRecord,
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
}

pub(super) fn fuse_source_candidates(
    lexical: Vec<EventSearchCandidate>,
    semantic: Vec<EventSearchCandidate>,
    semantic_weight: f32,
) -> Vec<EventSearchCandidate> {
    let mut evidence = BTreeMap::<Uuid, SourceFusionEvidence>::new();
    for (rank, candidate) in lexical.into_iter().enumerate() {
        evidence.insert(
            candidate.event.event_id.as_uuid(),
            SourceFusionEvidence {
                event: candidate.event,
                lexical_rank: Some(rank.saturating_add(1)),
                semantic_rank: None,
            },
        );
    }
    for (rank, candidate) in semantic.into_iter().enumerate() {
        let semantic_rank = rank.saturating_add(1);
        evidence
            .entry(candidate.event.event_id.as_uuid())
            .and_modify(|entry| entry.semantic_rank = Some(semantic_rank))
            .or_insert(SourceFusionEvidence {
                event: candidate.event,
                lexical_rank: None,
                semantic_rank: Some(semantic_rank),
            });
    }
    let mut candidates = evidence
        .into_values()
        .map(|evidence| EventSearchCandidate {
            score: weighted_rrf_score(
                evidence.lexical_rank,
                evidence.semantic_rank,
                semantic_weight,
            ),
            event: evidence.event,
        })
        .collect::<Vec<_>>();
    candidates.sort_by(search_candidate_order);
    candidates
}

pub(super) fn search_candidate_order(
    left: &EventSearchCandidate,
    right: &EventSearchCandidate,
) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| {
            right
                .event
                .occurred_at_unix_ms
                .cmp(&left.event.occurred_at_unix_ms)
        })
        .then_with(|| right.event.event_sequence.cmp(&left.event.event_sequence))
        .then_with(|| {
            left.event
                .event_id
                .as_uuid()
                .cmp(&right.event.event_id.as_uuid())
        })
}

pub(super) fn weighted_rrf_score(
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
    semantic_weight: f32,
) -> f32 {
    let reciprocal_rank = |rank: usize| 1.0 / (60.0 + rank.max(1) as f32);
    let lexical = lexical_rank.map(reciprocal_rank).unwrap_or(0.0);
    let semantic = semantic_rank.map(reciprocal_rank).unwrap_or(0.0);
    ((1.0 - semantic_weight) * lexical) + (semantic_weight * semantic)
}
