use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use tantivy::{
    index::SegmentMeta,
    indexer::{LogMergePolicy, MergeCandidate, MergePolicy},
};

use crate::LEXICAL_SEGMENT_MERGE_FAN_IN;
use ctx_history_index_format::{
    LEXICAL_DELETED_DOCUMENT_RECLAIM_DENOMINATOR, LEXICAL_DELETED_DOCUMENT_RECLAIM_NUMERATOR,
};

const LEXICAL_MIN_LAYER_SIZE: u32 = 64;

#[derive(Debug)]
pub(crate) struct LexicalMergePolicy {
    append_policy: LogMergePolicy,
}

impl Default for LexicalMergePolicy {
    fn default() -> Self {
        let mut append_policy = LogMergePolicy::default();
        append_policy.set_min_num_segments(LEXICAL_SEGMENT_MERGE_FAN_IN);
        append_policy.set_min_layer_size(LEXICAL_MIN_LAYER_SIZE);
        append_policy.set_del_docs_ratio_before_merge(1.0);
        Self { append_policy }
    }
}

impl MergePolicy for LexicalMergePolicy {
    fn compute_merge_candidates(&self, segments: &[SegmentMeta]) -> Vec<MergeCandidate> {
        let mut candidates = self.append_policy.compute_merge_candidates(segments);
        let scheduled = candidates
            .iter()
            .flat_map(|candidate| candidate.0.iter().copied())
            .collect::<HashSet<_>>();

        candidates.extend(
            segments
                .iter()
                .filter(|segment| !scheduled.contains(&segment.id()))
                .filter(|segment| deletion_density_exceeds_limit(segment))
                .map(|segment| MergeCandidate(vec![segment.id()])),
        );
        candidates
    }
}

#[derive(Debug)]
struct DeferredLexicalMergePolicy {
    enabled: Arc<AtomicBool>,
    terminal_policy: LexicalMergePolicy,
}

impl MergePolicy for DeferredLexicalMergePolicy {
    fn compute_merge_candidates(&self, segments: &[SegmentMeta]) -> Vec<MergeCandidate> {
        if self.enabled.load(Ordering::Acquire) {
            self.terminal_policy.compute_merge_candidates(segments)
        } else {
            Vec::new()
        }
    }
}

pub(super) fn staging_merge_policy() -> (Box<dyn MergePolicy>, Arc<AtomicBool>) {
    let enabled = Arc::new(AtomicBool::new(false));
    let policy = DeferredLexicalMergePolicy {
        enabled: Arc::clone(&enabled),
        terminal_policy: LexicalMergePolicy::default(),
    };
    (Box::new(policy), enabled)
}

pub(crate) fn deletion_density_exceeds_limit(segment: &SegmentMeta) -> bool {
    u64::from(segment.num_deleted_docs()) * LEXICAL_DELETED_DOCUMENT_RECLAIM_DENOMINATOR
        > u64::from(segment.max_doc()) * LEXICAL_DELETED_DOCUMENT_RECLAIM_NUMERATOR
}

#[cfg(test)]
mod tests {
    use tantivy::{
        index::{SegmentId, SegmentMeta},
        indexer::MergePolicy,
        schema::Schema,
        Index,
    };

    use super::*;

    fn segment(index: &Index, max_doc: u32, deleted: u32) -> SegmentMeta {
        index
            .new_segment_meta(SegmentId::generate_random(), max_doc)
            .with_delete_meta(deleted, 1)
    }

    #[test]
    fn terminal_activation_cannot_enable_an_abandoned_updater_epoch() {
        let index = Index::create_in_ram(Schema::builder().build());
        let segments = (0..LEXICAL_SEGMENT_MERGE_FAN_IN)
            .map(|_| segment(&index, 1, 0))
            .collect::<Vec<_>>();
        let (abandoned_policy, _abandoned_activation) = staging_merge_policy();
        let (terminal_policy, terminal_activation) = staging_merge_policy();
        assert!(abandoned_policy
            .compute_merge_candidates(&segments)
            .is_empty());
        assert!(terminal_policy
            .compute_merge_candidates(&segments)
            .is_empty());
        terminal_activation.store(true, Ordering::Release);
        assert!(!terminal_policy
            .compute_merge_candidates(&segments)
            .is_empty());
        // A queued task on the killed updater must remain unable to start a merge.
        assert!(abandoned_policy
            .compute_merge_candidates(&segments)
            .is_empty());
    }

    #[test]
    fn deletion_reclamation_is_strictly_more_than_one_quarter() {
        let index = Index::create_in_ram(Schema::builder().build());
        let at_limit = segment(&index, 400, 100);
        let above_limit = segment(&index, 400, 101);
        let policy = LexicalMergePolicy::default();

        assert!(!deletion_density_exceeds_limit(&at_limit));
        assert!(
            policy.compute_merge_candidates(&[at_limit]).is_empty(),
            "exactly 25% deleted documents must remain below the strict threshold"
        );

        assert!(deletion_density_exceeds_limit(&above_limit));
        let candidates = policy.compute_merge_candidates(&[above_limit]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0.len(), 1);
    }

    #[test]
    fn deletion_threshold_integer_arithmetic_has_no_large_segment_hole() {
        let index = Index::create_in_ram(Schema::builder().build());
        let at_limit = segment(&index, 40_000_004, 10_000_001);
        let above_limit = segment(&index, 40_000_004, 10_000_002);
        assert!(!deletion_density_exceeds_limit(&at_limit));
        assert!(deletion_density_exceeds_limit(&above_limit));

        let candidates = LexicalMergePolicy::default().compute_merge_candidates(&[above_limit]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0.len(), 1);
    }

    #[test]
    fn delete_expunge_does_not_rewrite_a_cold_peer_or_duplicate_stock_work() {
        let index = Index::create_in_ram(Schema::builder().build());
        let delete_heavy = segment(&index, 40_000, 10_001);
        let cold_peer = segment(&index, 40_000, 0);
        let candidates = LexicalMergePolicy::default()
            .compute_merge_candidates(&[delete_heavy.clone(), cold_peer]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, vec![delete_heavy.id()]);

        let fan_in = (0..LEXICAL_SEGMENT_MERGE_FAN_IN)
            .map(|index_in_level| {
                segment(&index, 40_000, if index_in_level == 0 { 10_001 } else { 0 })
            })
            .collect::<Vec<_>>();
        let candidates = LexicalMergePolicy::default().compute_merge_candidates(&fan_in);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0.len(), LEXICAL_SEGMENT_MERGE_FAN_IN);
    }

    #[test]
    fn append_tiering_keeps_a_packed_base_out_of_one_doc_delta_merges() {
        let index = Index::create_in_ram(Schema::builder().build());
        let base = segment(&index, 1_024, 0);
        let deltas = (0..LEXICAL_SEGMENT_MERGE_FAN_IN)
            .map(|_| segment(&index, 1, 0))
            .collect::<Vec<_>>();

        let candidates = LexicalMergePolicy::default()
            .compute_merge_candidates(&[vec![base.clone()], deltas.clone()].concat());
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].0,
            deltas.iter().map(SegmentMeta::id).collect::<Vec<_>>()
        );
        assert!(!candidates[0].0.contains(&base.id()));
    }

    #[test]
    fn append_tiering_waits_for_fan_in_one_doc_deltas_before_merging() {
        let index = Index::create_in_ram(Schema::builder().build());
        let base = segment(&index, 1_024, 0);
        let one_doc_deltas = (0..(LEXICAL_SEGMENT_MERGE_FAN_IN - 1))
            .map(|_| segment(&index, 1, 0))
            .collect::<Vec<_>>();

        let candidates = LexicalMergePolicy::default()
            .compute_merge_candidates(&[vec![base], one_doc_deltas].concat());
        assert!(
            candidates.is_empty(),
            "fan-in minus one one-doc deltas must stay below the merge threshold even when a larger packed base exists"
        );
    }
}
