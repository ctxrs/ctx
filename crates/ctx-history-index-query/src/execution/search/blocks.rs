use super::*;
use tantivy::postings::BlockSegmentPostings;

/// A cursor over Tantivy's existing blocks, with one query-local rank bound.
/// No second posting list, persistent index or segment-local scorer is needed.
pub(super) struct BodyPostings {
    blocks: BlockSegmentPostings,
    offset: usize,
    bound: Option<(Score, u64)>,
}

impl BodyPostings {
    pub(super) fn open(inverted: &InvertedIndexReader, info: &TermInfo) -> Result<Self> {
        #[cfg(any(test, feature = "test-support"))]
        MANUAL_POSTING_READS.set(MANUAL_POSTING_READS.get().saturating_add(1));
        Ok(Self {
            blocks: inverted
                .read_block_postings_from_terminfo(info, IndexRecordOption::WithFreqs)?,
            offset: 0,
            bound: None,
        })
    }

    pub(super) fn doc(&self) -> DocId {
        self.blocks
            .docs()
            .get(self.offset)
            .copied()
            .unwrap_or(TERMINATED)
    }

    pub(super) fn term_freq(&self) -> u32 {
        self.blocks.freq(self.offset)
    }

    pub(super) fn advance(&mut self) {
        self.offset += 1;
        if self.offset >= self.blocks.block_len() {
            self.blocks.advance();
            self.offset = 0;
            self.bound = None;
        }
    }

    pub(super) fn seek(&mut self, target: DocId) {
        if target > self.last_doc() {
            self.bound = None;
        }
        self.offset = self.blocks.seek(target);
    }

    pub(super) fn last_doc(&self) -> DocId {
        self.blocks.docs().last().copied().unwrap_or(TERMINATED)
    }

    /// Each uncached inspection is precharged and visits at most 128 postings.
    /// Tantivy's stored maximum chooses a (tf, norm) pair using the indexing
    /// segment's average length. That pair can underestimate a global scorer's
    /// maximum after segments with different length distributions are combined.
    /// Compute the exact per-term maximum with the SAME global weight and
    /// quantized norms as candidate scoring instead. This also handles tails.
    /// The minimum identity prefix bounds every document in the block, even
    /// when its score is below the maximum. Including deleted/filtered/earlier
    /// documents only loosens these bounds.
    pub(super) fn rank_bound(
        &mut self,
        weight: &Bm25Weight,
        norms: &FieldNormReader,
        fields: &ManualCandidateFastFields,
        context: &LexicalSegmentContext,
        meter: &mut LexicalWorkMeter,
    ) -> Result<Option<(Score, u64)>> {
        if let Some(bound) = self.bound {
            return Ok(Some(bound));
        }
        if !meter.charge(
            LexicalWorkCounter::BodyPostingAdvances,
            1,
            Some(context),
            Some(self.doc()),
        ) {
            return Ok(None);
        }
        let mut score = 0.0_f32;
        let mut prefix = u64::MAX;
        for (&doc, &frequency) in self.blocks.docs().iter().zip(self.blocks.freqs()) {
            score = score.max(weight.score(norms.fieldnorm_id(doc), frequency));
            prefix = prefix.min(fields.event_id_high(doc)? >> 16);
        }
        self.bound = Some((score, prefix));
        Ok(self.bound)
    }
}

/// Bounds the intersection of the current native block ranges. Summing term
/// maxima in query order bounds the exact f32 sum, including when maxima come
/// from different documents. Class multipliers are <= 1. Missing terms cannot
/// add coverage. Identity uses a conservative minimum over the whole union;
/// equal prefixes must still reach the exact digest comparison.
pub(super) fn block_rank_bound(
    prepared: &mut PreparedSegment,
    weights: &[Bm25Weight],
    meter: &mut LexicalWorkMeter,
) -> Result<Option<(DocId, u8, Score, u64)>> {
    let end = prepared
        .body_postings
        .iter()
        .flatten()
        .filter(|posting| posting.doc() != TERMINATED)
        .map(BodyPostings::last_doc)
        .min()
        .unwrap_or(TERMINATED);
    let norms = prepared
        .fieldnorms
        .as_ref()
        .ok_or(IndexError::InvalidStoredDocumentField("body_search"))?;
    let mut coverage = 0;
    let mut score = 0.0;
    let mut prefix = u64::MAX;
    for (posting, weight) in prepared.body_postings.iter_mut().zip(weights) {
        let Some(posting) = posting else {
            continue;
        };
        if posting.doc() == TERMINATED || posting.doc() > end {
            continue;
        }
        let Some((maximum, minimum_prefix)) = posting.rank_bound(
            weight,
            norms,
            &prepared.candidate_fields,
            &prepared.context,
            meter,
        )?
        else {
            return Ok(None);
        };
        coverage += 1;
        score += maximum;
        prefix = prefix.min(minimum_prefix);
    }
    Ok(Some((end, coverage, score, prefix)))
}
