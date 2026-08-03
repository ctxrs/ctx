use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReverseTermHeapEntry {
    key: [u8; crate::index_document::EVENT_RANGE_ORDER_KEY_LEN],
    segment: usize,
}

pub(super) enum OrderedTermMerger<'a> {
    Ascending(TermMerger<'a>),
    Descending(ReverseTermMerger<'a>),
}

impl OrderedTermMerger<'_> {
    pub(super) fn advance(&mut self) -> Result<bool> {
        match self {
            Self::Ascending(merger) => Ok(merger.advance()),
            Self::Descending(merger) => merger.advance(),
        }
    }

    pub(super) fn key(&self) -> &[u8] {
        match self {
            Self::Ascending(merger) => merger.key(),
            Self::Descending(merger) => merger.key(),
        }
    }

    pub(super) fn current_segment_ords_and_term_infos(
        &self,
    ) -> Vec<(usize, tantivy::postings::TermInfo)> {
        match self {
            Self::Ascending(merger) => merger.current_segment_ords_and_term_infos().collect(),
            Self::Descending(merger) => merger.current_segment_ords_and_term_infos(),
        }
    }
}

/// Tantivy exposes reverse term streams but its public multi-segment merger is
/// forward-only. This bounded merger retains one cursor per immutable segment.
pub(super) struct ReverseTermMerger<'a> {
    streams: Vec<TermStreamer<'a>>,
    heap: BinaryHeap<ReverseTermHeapEntry>,
    current_segments: Vec<usize>,
    current_key: [u8; crate::index_document::EVENT_RANGE_ORDER_KEY_LEN],
    initialized: bool,
}

impl<'a> ReverseTermMerger<'a> {
    pub(super) fn new(streams: Vec<TermStreamer<'a>>) -> Self {
        Self {
            streams,
            heap: BinaryHeap::new(),
            current_segments: Vec::new(),
            current_key: [0; crate::index_document::EVENT_RANGE_ORDER_KEY_LEN],
            initialized: false,
        }
    }

    fn advance_segment(&mut self, segment: usize) -> Result<()> {
        if self.streams[segment].advance() {
            let key = self.streams[segment]
                .key()
                .try_into()
                .map_err(|_| IndexError::InvalidStoredDocumentField(EVENT_RANGE_ORDER_FIELD))?;
            self.heap.push(ReverseTermHeapEntry { key, segment });
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<bool> {
        if self.initialized {
            let current_segments = std::mem::take(&mut self.current_segments);
            for segment in current_segments {
                self.advance_segment(segment)?;
            }
        } else {
            for segment in 0..self.streams.len() {
                self.advance_segment(segment)?;
            }
            self.initialized = true;
        }

        let Some(entry) = self.heap.pop() else {
            return Ok(false);
        };
        self.current_key = entry.key;
        self.current_segments.push(entry.segment);
        while self
            .heap
            .peek()
            .is_some_and(|candidate| candidate.key == self.current_key)
        {
            self.current_segments.push(self.heap.pop().unwrap().segment);
        }
        Ok(true)
    }

    fn key(&self) -> &[u8] {
        &self.current_key
    }

    fn current_segment_ords_and_term_infos(&self) -> Vec<(usize, tantivy::postings::TermInfo)> {
        self.current_segments
            .iter()
            .map(|segment| (*segment, self.streams[*segment].value().clone()))
            .collect()
    }
}
