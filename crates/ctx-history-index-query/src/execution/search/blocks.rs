use super::*;
use tantivy::postings::BlockSegmentPostings;

/// A cursor over Tantivy's existing blocks, with one query-local rank bound.
/// No second posting list, persistent index or segment-local scorer is needed.
pub(super) struct BodyPostings {
    blocks: BlockSegmentPostings,
    offset: usize,
    bound: Option<[(Score, u64); 4]>,
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
    /// when its score is below the maximum. Deleted rows are unverified and
    /// excluded before reading any score or identity fields. Filtered rows
    /// can only loosen the bound. Class cursors may have passed earlier rows
    /// in this block, so inspect only the remaining suffix. Cached bounds
    /// still bound every later suffix of this block.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rank_bound(
        &mut self,
        weight: &Bm25Weight,
        norms: &FieldNormReader,
        fields: &ManualCandidateFastFields,
        context: &LexicalSegmentContext,
        classes: &SegmentClasses,
        reader: &SegmentReader,
        meter: &mut LexicalWorkMeter,
    ) -> Result<Option<[(Score, u64); 4]>> {
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
        let docs = &self.blocks.docs()[self.offset..];
        let Some(classes) = classes.block_classes(docs, reader, meter, context) else {
            return Ok(None);
        };
        let mut bounds = [(0.0_f32, u64::MAX); 4];
        for ((&doc, &frequency), class) in docs
            .iter()
            .zip(&self.blocks.freqs()[self.offset..])
            .zip(classes)
        {
            if reader.is_deleted(doc) {
                continue;
            }
            let (score, prefix) = &mut bounds[class];
            *score = score.max(weight.score(norms.fieldnorm_id(doc), frequency));
            *prefix = (*prefix).min(fields.event_id_high(doc)? >> 16);
        }
        self.bound = Some(bounds);
        Ok(self.bound)
    }
}

/// Bounds the intersection of the current native block ranges, separately
/// for each effective class. Nonnegative maxima summed in query order bound
/// the exact f32 sum; multiplying AFTER summing preserves that bound under
/// the scorer's rounding. Taking the best class rank bounds their union.
/// Missing terms cannot add coverage. Identity uses a conservative minimum
/// across the class; equal prefixes still reach the exact digest comparison.
pub(super) fn block_rank_bound(
    prepared: &mut PreparedSegment,
    weights: &[Bm25Weight],
    reader: &SegmentReader,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<(DocId, u8, Score, u64)>> {
    let first = next_body_doc(&prepared.body_postings).unwrap_or(TERMINATED);
    if prepared
        .classes
        .prepare_bounds(first, meter, &prepared.context)
        .is_none()
    {
        return Ok(None);
    }
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
    let mut bounds = [(0_u8, 0.0_f32, u64::MAX); 4];
    for (posting, weight) in prepared.body_postings.iter_mut().zip(weights) {
        let Some(posting) = posting else {
            continue;
        };
        if posting.doc() == TERMINATED || posting.doc() > end {
            continue;
        }
        let Some(term_bounds) = posting.rank_bound(
            weight,
            norms,
            &prepared.candidate_fields,
            &prepared.context,
            &prepared.classes,
            reader,
            meter,
        )?
        else {
            return Ok(None);
        };
        for ((coverage, score, prefix), (maximum, minimum_prefix)) in
            bounds.iter_mut().zip(term_bounds)
        {
            *coverage += u8::from(minimum_prefix != u64::MAX);
            *score += maximum;
            *prefix = (*prefix).min(minimum_prefix);
        }
    }
    let mut best = (0, 0.0_f32, u64::MAX);
    for ((coverage, sum, prefix), weight) in bounds.into_iter().zip(super::classes::WEIGHTS) {
        let score = sum * weight;
        if coverage
            .cmp(&best.0)
            .then_with(|| score.total_cmp(&best.1))
            .then_with(|| best.2.cmp(&prefix))
            == Ordering::Greater
        {
            best = (coverage, score, prefix);
        }
    }
    Ok(Some((end, best.0, best.1, best.2)))
}
