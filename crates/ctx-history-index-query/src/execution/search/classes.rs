use super::*;
use tantivy::postings::BlockSegmentPostings;

// Shared by candidate scoring and per-class block bounds. Apply the multiplier
// AFTER summing term scores, preserving the scorer's f32 evaluation order.
pub(super) const WEIGHTS: [Score; 4] = [1.0, 0.9, 0.8, 0.6];

pub(super) struct SegmentClasses {
    default: usize,
    postings: Vec<(usize, BlockSegmentPostings)>,
}

impl SegmentClasses {
    pub(super) fn open(
        reader: &SegmentReader,
        field: Field,
        scope: SearchContentScope,
        meter: &mut LexicalWorkMeter,
        context: &LexicalSegmentContext,
    ) -> Result<Option<Self>> {
        let mut classes = Self {
            default: match scope {
                SearchContentScope::All => 2,
                SearchContentScope::Transcript => 1,
                SearchContentScope::Calls | SearchContentScope::Outputs => 0,
            },
            postings: Vec::new(),
        };
        if classes.default == 0 {
            return Ok(Some(classes));
        }
        let types = [("message", 0), ("summary", 1)].into_iter().chain(
            OUTPUT_EVENT_TYPES
                .iter()
                .filter(|_| scope == SearchContentScope::All)
                .map(|&name| (name, 3)),
        );
        for (name, class) in types {
            if !meter.charge(
                LexicalWorkCounter::DictionaryLookups,
                1,
                Some(context),
                None,
            ) {
                return Ok(None);
            }
            let inverted = manual_inverted_index(reader, field)?;
            let Some(info) = inverted.get_term_info(&Term::from_field_text(field, name))? else {
                continue;
            };
            if !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(context), None) {
                return Ok(None);
            }
            #[cfg(any(test, feature = "test-support"))]
            MANUAL_POSTING_READS.set(MANUAL_POSTING_READS.get().saturating_add(1));
            classes.postings.push((
                class,
                inverted.read_block_postings_from_terminfo(&info, IndexRecordOption::Basic)?,
            ));
        }
        Ok(Some(classes))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn posting_count(&self) -> usize {
        self.postings.len()
    }

    pub(super) fn weight(
        &mut self,
        doc: DocId,
        meter: &mut LexicalWorkMeter,
        context: &LexicalSegmentContext,
    ) -> Option<Score> {
        for (class, posting) in &mut self.postings {
            if !meter.charge(
                LexicalWorkCounter::FilterProbes,
                1,
                Some(context),
                Some(doc),
            ) {
                return None;
            }
            if contains(posting, doc, meter, context)? {
                return Some(WEIGHTS[*class]);
            }
        }
        Some(WEIGHTS[self.default])
    }

    /// Keep the shared cursors at the first unexamined body document before
    /// cloning them for lookahead. Otherwise repeated clones of an old cursor
    /// would rescan Tantivy's linear skip list from the beginning.
    pub(super) fn prepare_bounds(
        &mut self,
        doc: DocId,
        meter: &mut LexicalWorkMeter,
        context: &LexicalSegmentContext,
    ) -> Option<()> {
        for (_, posting) in &mut self.postings {
            seek_block(posting, doc, meter, context)?;
        }
        Some(())
    }

    /// At most 128 document memberships per class inspection, precharged as
    /// block-bound work. Native class-block seeks are separately charged. A
    /// temporary cursor shares the posting bytes without moving the candidate
    /// cursor beyond an unexamined document. No per-segment class bitmap is built.
    pub(super) fn block_classes(
        &self,
        docs: &[DocId],
        reader: &SegmentReader,
        meter: &mut LexicalWorkMeter,
        context: &LexicalSegmentContext,
    ) -> Option<[usize; 128]> {
        let mut classes = [self.default; 128];
        for (class, posting) in self.postings.iter().rev() {
            if !meter.charge(
                LexicalWorkCounter::BodyPostingAdvances,
                1,
                Some(context),
                docs.first().copied(),
            ) {
                return None;
            }
            let mut cursor = posting.clone();
            #[cfg(any(test, feature = "test-support"))]
            MANUAL_MAXIMUM_LIVE_POSTINGS.set(
                MANUAL_MAXIMUM_LIVE_POSTINGS
                    .get()
                    .max(MANUAL_LIVE_POSTINGS.get() + 1),
            );
            for (&doc, slot) in docs.iter().zip(&mut classes) {
                if !reader.is_deleted(doc) && contains(&mut cursor, doc, meter, context)? {
                    *slot = *class;
                }
            }
        }
        Some(classes)
    }
}

fn contains(
    posting: &mut BlockSegmentPostings,
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    context: &LexicalSegmentContext,
) -> Option<bool> {
    seek_block(posting, doc, meter, context)?;
    Some(posting.docs().binary_search(&doc).is_ok())
}

fn seek_block(
    posting: &mut BlockSegmentPostings,
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    context: &LexicalSegmentContext,
) -> Option<()> {
    if posting.docs().last().is_some_and(|&last| doc > last) {
        if !meter.charge(LexicalWorkCounter::FilterSeeks, 1, Some(context), Some(doc)) {
            return None;
        }
        posting.seek(doc);
    }
    Some(())
}
