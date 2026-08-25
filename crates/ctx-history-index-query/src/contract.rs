use super::*;

/// Fixed admission ceilings for one lexical search request.
///
/// Raw admission happens before analyzer lookup or query construction. The
/// analyzed-token ceiling bounds manual posting fanout to 32 terms. Empty
/// alternatives still count because callers must not turn repeated empty
/// inputs into unbounded pre-search work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexicalQueryLimits {
    /// Maximum aggregate UTF-8 bytes across all supplied alternatives.
    pub maximum_aggregate_bytes: usize,
    /// Maximum number of supplied positional or repeated-term alternatives.
    pub maximum_alternatives: usize,
    /// Maximum tokens admitted from lexical analysis before deduplication.
    pub maximum_unique_tokens: usize,
}

impl LexicalQueryLimits {
    /// Validates raw alternatives without allocating a normalized copy.
    pub fn validate_texts<'a, I>(self, texts: I) -> Result<()>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut alternatives = 0_usize;
        let mut aggregate_bytes = 0_usize;
        for text in texts {
            alternatives = alternatives.saturating_add(1);
            if alternatives > self.maximum_alternatives {
                return Err(IndexError::LexicalQueryAlternativesTooMany {
                    observed: alternatives,
                    maximum: self.maximum_alternatives,
                });
            }
            aggregate_bytes = aggregate_bytes.saturating_add(text.len());
            if aggregate_bytes > self.maximum_aggregate_bytes {
                return Err(IndexError::LexicalQueryBytesTooLarge {
                    actual: aggregate_bytes,
                    maximum: self.maximum_aggregate_bytes,
                });
            }
        }
        Ok(())
    }
}

/// Generous fixed limits for public and programmatic lexical queries.
///
/// The 64 KiB byte ceiling bounds tokenizer input and normalization copies;
/// the two 32-item ceilings bound repeated-query and posting fanout while
/// leaving ample room for ordinary user queries.
pub const LEXICAL_QUERY_LIMITS: LexicalQueryLimits = LexicalQueryLimits {
    maximum_aggregate_bytes: 64 * 1024,
    maximum_alternatives: 32,
    maximum_unique_tokens: 32,
};

/// Maximum number of metadata candidates retained by one lexical search.
///
/// The manual executor uses this as both its public result ceiling and fixed
/// retained-heap ceiling; it never overcollects a second candidate set.
pub const MAX_LEXICAL_QUERY_RESULTS: usize = 4_096;

/// V1 ceilings for one manually executed lexical or filtered-list pass.
///
/// Every work counter is charged before its corresponding operation.
/// Dictionary work is charged before acquiring a field's inverted-index
/// reader, and every posting-list read is separately precharged. Substring
/// filters may expand matching literal-fact terms into one segment-local
/// bitmap; dictionary traversal, compared bytes, matching terms, posting
/// documents, and bitmap scratch are independently bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexicalWorkBudget {
    pub maximum_segments: u64,
    pub maximum_candidate_docs: u64,
    pub maximum_body_posting_advances: u64,
    pub maximum_exact_filter_terms: u64,
    pub maximum_filter_input_bytes: u64,
    /// Exact term-info lookups. The first charge precedes inverted-index
    /// acquisition, so a zero budget performs no dictionary/read acquisition.
    pub maximum_dictionary_lookups: u64,
    /// Exact posting-list reads. Body statistics retain only small `TermInfo`
    /// values; postings are opened and dropped one stable-sorted segment at a
    /// time.
    pub maximum_posting_opens: u64,
    pub maximum_filter_probes: u64,
    pub maximum_filter_seeks: u64,
    pub maximum_substring_dictionary_steps: u64,
    pub maximum_substring_dictionary_bytes: u64,
    pub maximum_substring_posting_docs: u64,
    pub maximum_substring_bitmap_bytes: u64,
    pub maximum_retained_candidates: u64,
    pub maximum_final_materializations: u64,
    pub maximum_final_materialization_bytes: u64,
    pub maximum_term_expansions: u64,
}

pub const LEXICAL_WORK_BUDGET_V1: LexicalWorkBudget = LexicalWorkBudget {
    maximum_segments: 512,
    maximum_candidate_docs: 65_536,
    maximum_body_posting_advances: 2_097_152,
    maximum_exact_filter_terms: 16_384,
    maximum_filter_input_bytes: 1024 * 1024,
    maximum_dictionary_lookups: 1_048_576,
    maximum_posting_opens: 1_048_576,
    maximum_filter_probes: 8_388_608,
    maximum_filter_seeks: 4_194_304,
    maximum_substring_dictionary_steps: 1_048_576,
    maximum_substring_dictionary_bytes: 64 * 1024 * 1024,
    maximum_substring_posting_docs: 2_097_152,
    maximum_substring_bitmap_bytes: 16 * 1024 * 1024,
    maximum_retained_candidates: 4_096,
    maximum_final_materializations: 4_096,
    maximum_final_materialization_bytes: 256 * 1024 * 1024,
    maximum_term_expansions: 262_144,
};

/// Exact V1 work counters. `analyzed_tokens` is independently admission-bound
/// by [`LEXICAL_QUERY_LIMITS`] and therefore needs no second work ceiling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LexicalWorkCounters {
    pub segments: u64,
    pub candidate_docs: u64,
    pub body_posting_advances: u64,
    pub analyzed_tokens: u64,
    pub exact_filter_terms: u64,
    pub filter_input_bytes: u64,
    pub dictionary_lookups: u64,
    pub posting_opens: u64,
    pub filter_probes: u64,
    pub filter_seeks: u64,
    pub substring_dictionary_steps: u64,
    pub substring_dictionary_bytes: u64,
    pub substring_posting_docs: u64,
    pub substring_bitmap_bytes: u64,
    /// Maximum simultaneously retained candidates, not heap replacements.
    pub retained_candidates: u64,
    pub final_materializations: u64,
    pub final_materialization_bytes: u64,
    pub term_expansions: u64,
}

/// Materially distinct manually budgeted operation classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalWorkCounter {
    Segments,
    CandidateDocs,
    BodyPostingAdvances,
    ExactFilterTerms,
    FilterInputBytes,
    DictionaryLookups,
    PostingOpens,
    FilterProbes,
    FilterSeeks,
    SubstringDictionarySteps,
    SubstringDictionaryBytes,
    SubstringPostingDocs,
    SubstringBitmapBytes,
    RetainedCandidates,
    FinalMaterializations,
    FinalMaterializationBytes,
    TermExpansions,
}

impl LexicalWorkCounter {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Segments => "segments",
            Self::CandidateDocs => "candidate_docs",
            Self::BodyPostingAdvances => "body_posting_advances",
            Self::ExactFilterTerms => "exact_filter_terms",
            Self::FilterInputBytes => "filter_input_bytes",
            Self::DictionaryLookups => "dictionary_lookups",
            Self::PostingOpens => "posting_opens",
            Self::FilterProbes => "filter_probes",
            Self::FilterSeeks => "filter_seeks",
            Self::SubstringDictionarySteps => "substring_dictionary_steps",
            Self::SubstringDictionaryBytes => "substring_dictionary_bytes",
            Self::SubstringPostingDocs => "substring_posting_docs",
            Self::SubstringBitmapBytes => "substring_bitmap_bytes",
            Self::RetainedCandidates => "retained_candidates",
            Self::FinalMaterializations => "final_materializations",
            Self::FinalMaterializationBytes => "final_materialization_bytes",
            Self::TermExpansions => "term_expansions",
        }
    }
}

/// Stable location of the operation that could not be admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalSegmentContext {
    /// Position in lexicographically sorted Tantivy segment-ID order.
    pub stable_segment_index: usize,
    /// Tantivy's immutable segment ID.
    pub segment_id: String,
    /// Address ordinal required to materialize a document from this searcher.
    pub segment_ord: u32,
}

/// Exact failed pre-operation charge. `used` excludes the rejected operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalWorkExhaustion {
    pub counter: LexicalWorkCounter,
    pub used: u64,
    pub limit: u64,
    pub segment: Option<LexicalSegmentContext>,
    pub next_doc: Option<u32>,
}

impl std::fmt::Display for LexicalWorkExhaustion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} exhausted at {}/{}",
            self.counter.as_str(),
            self.used,
            self.limit
        )?;
        if let Some(segment) = &self.segment {
            write!(
                formatter,
                " in stable segment {} ({})",
                segment.stable_segment_index, segment.segment_id
            )?;
        }
        if let Some(next_doc) = self.next_doc {
            write!(formatter, " before doc {next_doc}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LexicalWorkExhaustion {}

/// Errors from compatibility lexical APIs that cannot represent partial work.
#[derive(Debug, thiserror::Error)]
pub enum LexicalSearchError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("lexical search work exhausted: {0}")]
    WorkExhausted(#[from] LexicalWorkExhaustion),
}

pub type LexicalSearchResult<T> = std::result::Result<T, LexicalSearchError>;

/// Explicit term coverage retained with every manual lexical candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexicalTermCoverage {
    pub matched_terms: u8,
    pub query_terms: u8,
}

/// Candidate shape returned by completeness-aware batch APIs.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalSearchCandidate {
    pub event: EventRecord,
    /// Class-weighted BM25 score. Coverage is deliberately separate and ranks
    /// before this value.
    pub score: f32,
    pub coverage: LexicalTermCoverage,
}

impl From<LexicalSearchCandidate> for EventSearchCandidate {
    fn from(candidate: LexicalSearchCandidate) -> Self {
        Self {
            event: candidate.event,
            score: candidate.score,
        }
    }
}

/// One truthful result of a bounded manual lexical or filtered-list pass.
#[derive(Debug, Clone, PartialEq)]
pub struct LexicalSearchBatch {
    pub candidates: Vec<LexicalSearchCandidate>,
    /// True when every configured work operation completed. This says nothing
    /// by itself about candidates discarded by the caller's retained limit.
    pub complete: bool,
    /// True only when `candidates` contains every admissible match: work
    /// completed, the retained heap discarded no match, and all retained
    /// finals were materialized. Therefore `complete == true` with this field
    /// false is relevance/result-limit truncation, while `complete == false`
    /// is work-indeterminate. A zero result limit is conservatively
    /// non-exhaustive because no candidate pass is performed.
    pub candidate_set_exhaustive: bool,
    pub exhaustion: Option<LexicalWorkExhaustion>,
    pub counters: LexicalWorkCounters,
}

#[derive(Debug)]
pub(crate) struct LexicalWorkMeter {
    budget: LexicalWorkBudget,
    counters: LexicalWorkCounters,
    exhaustion: Option<LexicalWorkExhaustion>,
}

impl LexicalWorkMeter {
    pub(crate) fn new(budget: LexicalWorkBudget) -> Self {
        Self {
            budget,
            counters: LexicalWorkCounters::default(),
            exhaustion: None,
        }
    }

    pub(crate) fn record_analyzed_tokens(&mut self, analyzed_tokens: usize) -> Result<()> {
        self.counters.analyzed_tokens =
            u64::try_from(analyzed_tokens).map_err(|_| IndexError::CountOverflow)?;
        Ok(())
    }

    pub(crate) fn charge(
        &mut self,
        counter: LexicalWorkCounter,
        amount: u64,
        segment: Option<&LexicalSegmentContext>,
        next_doc: Option<u32>,
    ) -> bool {
        let used = self.used(counter);
        let limit = self.limit(counter);
        if used.checked_add(amount).is_none_or(|next| next > limit) {
            self.note_exhaustion(counter, used, limit, segment, next_doc);
            return false;
        }
        *self.used_mut(counter) = used + amount;
        true
    }

    pub(crate) fn charge_pair(
        &mut self,
        first: (LexicalWorkCounter, u64),
        second: (LexicalWorkCounter, u64),
        segment: Option<&LexicalSegmentContext>,
        next_doc: Option<u32>,
    ) -> bool {
        for (counter, amount) in [first, second] {
            let used = self.used(counter);
            let limit = self.limit(counter);
            if used.checked_add(amount).is_none_or(|next| next > limit) {
                self.note_exhaustion(counter, used, limit, segment, next_doc);
                return false;
            }
        }
        *self.used_mut(first.0) += first.1;
        *self.used_mut(second.0) += second.1;
        true
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhaustion.is_some()
    }

    pub(crate) fn into_parts(self) -> (LexicalWorkCounters, Option<LexicalWorkExhaustion>) {
        (self.counters, self.exhaustion)
    }

    fn note_exhaustion(
        &mut self,
        counter: LexicalWorkCounter,
        used: u64,
        limit: u64,
        segment: Option<&LexicalSegmentContext>,
        next_doc: Option<u32>,
    ) {
        if self.exhaustion.is_none() {
            self.exhaustion = Some(LexicalWorkExhaustion {
                counter,
                used,
                limit,
                segment: segment.cloned(),
                next_doc,
            });
        }
    }

    fn limit(&self, counter: LexicalWorkCounter) -> u64 {
        match counter {
            LexicalWorkCounter::Segments => self.budget.maximum_segments,
            LexicalWorkCounter::CandidateDocs => self.budget.maximum_candidate_docs,
            LexicalWorkCounter::BodyPostingAdvances => self.budget.maximum_body_posting_advances,
            LexicalWorkCounter::ExactFilterTerms => self.budget.maximum_exact_filter_terms,
            LexicalWorkCounter::FilterInputBytes => self.budget.maximum_filter_input_bytes,
            LexicalWorkCounter::DictionaryLookups => self.budget.maximum_dictionary_lookups,
            LexicalWorkCounter::PostingOpens => self.budget.maximum_posting_opens,
            LexicalWorkCounter::FilterProbes => self.budget.maximum_filter_probes,
            LexicalWorkCounter::FilterSeeks => self.budget.maximum_filter_seeks,
            LexicalWorkCounter::SubstringDictionarySteps => {
                self.budget.maximum_substring_dictionary_steps
            }
            LexicalWorkCounter::SubstringDictionaryBytes => {
                self.budget.maximum_substring_dictionary_bytes
            }
            LexicalWorkCounter::SubstringPostingDocs => self.budget.maximum_substring_posting_docs,
            LexicalWorkCounter::SubstringBitmapBytes => self.budget.maximum_substring_bitmap_bytes,
            LexicalWorkCounter::RetainedCandidates => self.budget.maximum_retained_candidates,
            LexicalWorkCounter::FinalMaterializations => self.budget.maximum_final_materializations,
            LexicalWorkCounter::FinalMaterializationBytes => {
                self.budget.maximum_final_materialization_bytes
            }
            LexicalWorkCounter::TermExpansions => self.budget.maximum_term_expansions,
        }
    }

    fn used(&self, counter: LexicalWorkCounter) -> u64 {
        match counter {
            LexicalWorkCounter::Segments => self.counters.segments,
            LexicalWorkCounter::CandidateDocs => self.counters.candidate_docs,
            LexicalWorkCounter::BodyPostingAdvances => self.counters.body_posting_advances,
            LexicalWorkCounter::ExactFilterTerms => self.counters.exact_filter_terms,
            LexicalWorkCounter::FilterInputBytes => self.counters.filter_input_bytes,
            LexicalWorkCounter::DictionaryLookups => self.counters.dictionary_lookups,
            LexicalWorkCounter::PostingOpens => self.counters.posting_opens,
            LexicalWorkCounter::FilterProbes => self.counters.filter_probes,
            LexicalWorkCounter::FilterSeeks => self.counters.filter_seeks,
            LexicalWorkCounter::SubstringDictionarySteps => {
                self.counters.substring_dictionary_steps
            }
            LexicalWorkCounter::SubstringDictionaryBytes => {
                self.counters.substring_dictionary_bytes
            }
            LexicalWorkCounter::SubstringPostingDocs => self.counters.substring_posting_docs,
            LexicalWorkCounter::SubstringBitmapBytes => self.counters.substring_bitmap_bytes,
            LexicalWorkCounter::RetainedCandidates => self.counters.retained_candidates,
            LexicalWorkCounter::FinalMaterializations => self.counters.final_materializations,
            LexicalWorkCounter::FinalMaterializationBytes => {
                self.counters.final_materialization_bytes
            }
            LexicalWorkCounter::TermExpansions => self.counters.term_expansions,
        }
    }

    fn used_mut(&mut self, counter: LexicalWorkCounter) -> &mut u64 {
        match counter {
            LexicalWorkCounter::Segments => &mut self.counters.segments,
            LexicalWorkCounter::CandidateDocs => &mut self.counters.candidate_docs,
            LexicalWorkCounter::BodyPostingAdvances => &mut self.counters.body_posting_advances,
            LexicalWorkCounter::ExactFilterTerms => &mut self.counters.exact_filter_terms,
            LexicalWorkCounter::FilterInputBytes => &mut self.counters.filter_input_bytes,
            LexicalWorkCounter::DictionaryLookups => &mut self.counters.dictionary_lookups,
            LexicalWorkCounter::PostingOpens => &mut self.counters.posting_opens,
            LexicalWorkCounter::FilterProbes => &mut self.counters.filter_probes,
            LexicalWorkCounter::FilterSeeks => &mut self.counters.filter_seeks,
            LexicalWorkCounter::SubstringDictionarySteps => {
                &mut self.counters.substring_dictionary_steps
            }
            LexicalWorkCounter::SubstringDictionaryBytes => {
                &mut self.counters.substring_dictionary_bytes
            }
            LexicalWorkCounter::SubstringPostingDocs => &mut self.counters.substring_posting_docs,
            LexicalWorkCounter::SubstringBitmapBytes => &mut self.counters.substring_bitmap_bytes,
            LexicalWorkCounter::RetainedCandidates => &mut self.counters.retained_candidates,
            LexicalWorkCounter::FinalMaterializations => &mut self.counters.final_materializations,
            LexicalWorkCounter::FinalMaterializationBytes => {
                &mut self.counters.final_materialization_bytes
            }
            LexicalWorkCounter::TermExpansions => &mut self.counters.term_expansions,
        }
    }
}

pub(crate) fn validate_lexical_result_limit(limit: usize) -> Result<()> {
    if limit > MAX_LEXICAL_QUERY_RESULTS {
        return Err(IndexError::InvalidLexicalResultLimit {
            requested: limit,
            maximum: MAX_LEXICAL_QUERY_RESULTS,
        });
    }
    Ok(())
}

/// Maximum number of complete semantic event records retained in one page.
pub const MAX_SEMANTIC_EVENT_PAGE_ITEMS: usize = 64;

/// Maximum metadata records retained by one forward semantic pairing page.
pub const MAX_SEMANTIC_PAIRING_PAGE_ITEMS: usize = 64;

/// Maximum number of complete records retained for one exact source page.
pub const MAX_SOURCE_EVENT_PAGE_ITEMS: usize = 4_096;

/// Maximum number of complete records retained for one exact session page.
pub const MAX_SESSION_EVENT_PAGE_ITEMS: usize = 4_096;

/// Maximum retained coordinate prefix, including one truncation lookahead.
pub const MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS: usize = 4_097;

/// Maximum retained centered event-window coordinates.
pub const MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS: usize = 101;

/// Maximum copied-event occurrences retained by one bounded lineage query.
pub const MAX_COPIED_EVENT_LINEAGE_OCCURRENCES: usize = 20;

/// Maximum inverse copied-event origin UUID postings visited by one query.
pub const MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS: usize = 4_096;

/// Absolute maximum exact event-and-session identity postings visited by one
/// lineage query.
///
/// This independent ceiling covers both live and deleted postings while the
/// selected event and its optional direct copied-event target are resolved.
pub const MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS: usize = 2_048;

/// Bounded lineage-detail policy for one selected show-event response.
pub const SHOW_COPIED_EVENT_LINEAGE_POLICY: CopiedEventLineagePolicy =
    CopiedEventLineagePolicy::new(20, 4_096);

/// Caller-selected work and preview-retention ceilings for copied-event lineage.
///
/// The direct-edge query always remains generation-pinned and posting-bounded.
/// Show callers use the named policy above so presentation cannot accidentally
/// widen that product surface. Lower-level callers must still select explicit
/// bounded values.
/// `maximum_occurrences` never stops counting direct claims; it only caps
/// retained preview rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedEventLineagePolicy {
    pub maximum_occurrences: usize,
    pub maximum_posting_visits: usize,
}

impl CopiedEventLineagePolicy {
    pub const fn new(maximum_occurrences: usize, maximum_posting_visits: usize) -> Self {
        Self {
            maximum_occurrences,
            maximum_posting_visits,
        }
    }

    pub(super) fn validate(self) -> Result<()> {
        if !(1..=MAX_COPIED_EVENT_LINEAGE_OCCURRENCES).contains(&self.maximum_occurrences) {
            return Err(IndexError::InvalidCopiedEventLineageOccurrenceLimit {
                requested: self.maximum_occurrences,
                maximum: MAX_COPIED_EVENT_LINEAGE_OCCURRENCES,
            });
        }
        if !(1..=MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS).contains(&self.maximum_posting_visits) {
            return Err(IndexError::InvalidCopiedEventLineagePostingVisitLimit {
                requested: self.maximum_posting_visits,
                maximum: MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS,
            });
        }
        Ok(())
    }
}

/// Default retained-byte ceiling for complete Core pages.
///
/// One individually valid Core record always makes progress even when it is
/// larger than a caller's chosen page budget. These defaults therefore also
/// define the absolute maximum resident singleton page.
pub const DEFAULT_CORE_EVENT_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget {
    maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
    maximum_content_bytes: MAX_CORE_CONTENT_BYTES,
};

/// Paired encoded and decoded-content ceilings for complete Core records.
///
/// Each query API defines whether these ceilings apply to an aggregate page,
/// a strict batch, or every individual record in a strict batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreEventPageBudget {
    pub maximum_encoded_core_bytes: usize,
    pub maximum_content_bytes: usize,
}

impl CoreEventPageBudget {
    pub const fn new(maximum_encoded_core_bytes: usize, maximum_content_bytes: usize) -> Self {
        Self {
            maximum_encoded_core_bytes,
            maximum_content_bytes,
        }
    }
}

/// Exclusive full-identity keyset cursor for one source in one generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEventCursor {
    pub(super) generation_id: String,
    pub(super) source: SourceKey,
    pub(super) after: StableEntityId,
}

impl SourceEventCursor {
    pub fn new(generation_id: impl Into<String>, source: SourceKey, after: StableEntityId) -> Self {
        Self {
            generation_id: generation_id.into(),
            source,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One deterministic page of existing bounded records for an exact source.
#[derive(Debug, Clone)]
pub struct SourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<EventRecord>,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// One deterministic page of complete Core records for an exact source.
#[derive(Debug, Clone)]
pub struct CoreSourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// One decoded Core record retaining the exact stored JSON that produced it.
///
/// The backing Tantivy document remains page-bounded and owns the byte slice,
/// so derived consumers avoid a second serialization or a body-sized clone.
#[derive(Debug)]
pub struct StoredCoreEventRecord {
    pub core_record: CoreRecord,
    pub stored_json: StoredCoreRecordJson,
}

/// Owned backing storage for one exact, already-validated Core JSON value.
#[derive(Debug)]
pub struct StoredCoreRecordJson {
    pub content_bytes: usize,
    pub(super) accepted_document: ctx_history_index_format::AcceptedCoreDocument,
}

impl StoredCoreRecordJson {
    pub fn encoded_core_record(&self) -> Result<&[u8]> {
        Ok(self.accepted_document.encoded_core_record())
    }
}

/// One deterministic source page retaining each record's exact stored JSON.
#[derive(Debug)]
pub struct StoredCoreSourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<StoredCoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// Opaque metadata selection for one complete-Core source page.
///
/// The plan retains only document addresses and authenticated order-key size
/// suffixes so callers can reserve exact bytes before records are decoded.
#[derive(Debug)]
pub struct CoreSourceEventPagePlan {
    pub(super) generation_id: String,
    pub(super) source: SourceKey,
    pub(super) items: Vec<EventAddressCandidate>,
    pub(super) encoded_core_bytes: usize,
    pub(super) content_bytes: usize,
    pub(super) terminal: bool,
}

impl CoreSourceEventPagePlan {
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    pub fn encoded_core_bytes(&self) -> usize {
        self.encoded_core_bytes
    }

    pub fn content_bytes(&self) -> usize {
        self.content_bytes
    }

    pub fn terminal(&self) -> bool {
        self.terminal
    }
}

/// Complete requested-order Core records plus exact retained byte totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventBatch {
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
}

impl From<CoreSourceEventPage> for SourceEventPage {
    fn from(page: CoreSourceEventPage) -> Self {
        Self {
            generation_id: page.generation_id,
            source: page.source,
            items: page.items.into_iter().map(|record| record.event).collect(),
            next_cursor: page.next_cursor,
            terminal: page.terminal,
        }
    }
}

/// Exclusive deterministic position for one exact session in one generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventCursor {
    pub(super) generation_id: String,
    pub(super) session_id: StableEntityId,
    pub(super) after: SessionEventCoordinate,
}

impl SessionEventCursor {
    pub fn new(
        generation_id: impl Into<String>,
        session_id: StableEntityId,
        after: SessionEventCoordinate,
    ) -> Self {
        Self {
            generation_id: generation_id.into(),
            session_id,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub fn after(&self) -> SessionEventCoordinate {
        self.after
    }
}

/// One deterministic bounded page of complete Core records for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSessionEventPage {
    pub generation_id: String,
    pub session_id: StableEntityId,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SessionEventCursor>,
    pub terminal: bool,
}

/// Stable metadata-only candidate policy for semantic projection from Core.
///
/// Candidate enumeration remains metadata-only. Downstream semantic projection
/// reads complete stored Core content and applies the generation policy's Core
/// content filter before chunking or embedding. This contract is independent
/// of lexical query terms, scores, and ranking. Future candidate changes must
/// add a new enum variant instead of changing the meaning of this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEligibility {
    UserMessageCandidateV4,
}

impl SemanticEligibility {
    pub const CURRENT: Self = Self::UserMessageCandidateV4;

    pub fn includes(self, event: &EventRecord) -> bool {
        match self {
            Self::UserMessageCandidateV4 => ctx_history_index_format::is_semantic_candidate(
                &event.event_type,
                event.role.as_deref(),
            ),
        }
    }
}

/// Exclusive full-identity keyset cursor bound to one verified generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEventCursor {
    pub(super) generation_id: String,
    pub(super) eligibility: SemanticEligibility,
    pub(super) after: StableEntityId,
}

impl SemanticEventCursor {
    pub fn new(generation_id: impl Into<String>, after: StableEntityId) -> Self {
        Self {
            generation_id: generation_id.into(),
            eligibility: SemanticEligibility::CURRENT,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn eligibility(&self) -> SemanticEligibility {
        self.eligibility
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One deterministic page of metadata-selected semantic candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    /// Exact count of metadata candidates before Core content filtering.
    pub eligible_total: u64,
    pub items: Vec<EventRecord>,
    pub next_cursor: Option<SemanticEventCursor>,
    pub terminal: bool,
}

impl SemanticEventPage {
    pub fn eligible_count(&self) -> usize {
        self.items.len()
    }
}

/// Body-free event eligibility selected from one immutable Core generation.
///
/// The IDs are derived from the same indexed metadata predicates as lexical
/// search. Semantic scorers can therefore reject ineligible events before
/// touching vector bytes without reopening provider sources or retaining Core
/// content for the candidate corpus.
#[derive(Debug, Clone)]
pub struct SemanticFilterProjection {
    pub(super) generation_id: String,
    pub(super) event_ids: HashSet<Uuid>,
}

impl SemanticFilterProjection {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn len(&self) -> usize {
        self.event_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.event_ids.is_empty()
    }

    pub fn contains(&self, event_id: Uuid) -> bool {
        self.event_ids.contains(&event_id)
    }

    pub fn event_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.event_ids.iter().copied()
    }
}

/// One deterministic page of complete Core semantic candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    pub eligible_total: u64,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SemanticEventCursor>,
    pub terminal: bool,
}

impl CoreSemanticEventPage {
    pub fn eligible_count(&self) -> usize {
        self.items.len()
    }
}

impl From<CoreSemanticEventPage> for SemanticEventPage {
    fn from(page: CoreSemanticEventPage) -> Self {
        Self {
            generation_id: page.generation_id,
            eligibility: page.eligibility,
            eligible_total: page.eligible_total,
            items: page.items.into_iter().map(|record| record.event).collect(),
            next_cursor: page.next_cursor,
            terminal: page.terminal,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentScope {
    #[default]
    All,
    Primary,
    Subagent,
}

pub type SearchAgentScope = AgentScope;

/// Event-content classes eligible for one search request.
///
/// `All` retains every indexed event type, including future types unknown to
/// this query implementation. The narrower variants select only their named
/// stable event classes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchContentScope {
    #[default]
    All,
    Transcript,
    Calls,
    Outputs,
}

impl SearchContentScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Transcript => "transcript",
            Self::Calls => "calls",
            Self::Outputs => "outputs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedSessionTree {
    pub session_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSearchFilters {
    /// Exact source-key terms resolved from the pinned generation. `None`
    /// means unrestricted; `Some([])` deliberately matches no events.
    pub allowed_source_keys: Option<Vec<String>>,
    pub session_id: Option<Uuid>,
    pub excluded_session_ids: Vec<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub provider: Option<String>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub since_unix_ms: Option<i64>,
    pub content_scope: SearchContentScope,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_scope: SearchAgentScope,
    pub file: Option<String>,
    pub exclude_session_tree: Option<ExcludedSessionTree>,
}

impl EventSearchFilters {
    pub(super) fn validate_content_scope(&self) -> Result<()> {
        if self.content_scope != SearchContentScope::All && self.event_type.is_some() {
            return Err(IndexError::ContentScopeEventTypeConflict {
                scope: self.content_scope.as_str(),
            });
        }
        Ok(())
    }

    pub fn matches_source_identity(&self, event: &EventRecord) -> bool {
        if !self.has_source_identity_filter() {
            return true;
        }
        custom_source_identity(event).is_some_and(|(provider_key, source_id)| {
            source_identity_values_match(self, provider_key, source_id)
        })
    }

    pub(super) fn has_source_identity_filter(&self) -> bool {
        self.history_source.is_some() || self.provider_key.is_some() || self.source_id.is_some()
    }

    pub(super) fn validate_source_identity_filters(&self) -> Result<()> {
        for (field, value) in [
            ("history_source", self.history_source.as_deref()),
            ("provider_key", self.provider_key.as_deref()),
            ("source_id", self.source_id.as_deref()),
        ] {
            if let Some(value) = value {
                validated_filter_text(field, value)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: Option<StableEntityId>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub event_copy: Option<ProviderNativeEventCopy>,
    pub source: SourceKey,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub native_event_id: Option<TypedKey>,
    pub agent_scope: Option<CoreAgentScope>,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
}

impl EventRecord {
    /// Returns the exporter-declared route for a custom JSONL event.
    ///
    /// Custom source identity is retained in the native event key so query
    /// surfaces can display the same route used by exact source filters.
    pub fn custom_source_identity(&self) -> Option<(&str, &str)> {
        if self.provider != "custom" {
            return None;
        }
        let Some(TypedKey::Composite(values)) = self.native_event_id.as_ref() else {
            return None;
        };
        let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(_)] =
            values.as_slice()
        else {
            return None;
        };
        Some((provider_key, source_id))
    }
}

/// One direct provider-native event-copy claim targeting the selected event.
///
/// All identities are full stable IDs from the same stored Core record. The
/// direct copied-from pair identifies the exact event edge, while the parent,
/// claimed root, and relationship fields preserve that child session's own
/// direct durable claims. They are not publication-time graph authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedEventLineageOccurrence {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub copied_from_event_id: StableEntityId,
    pub copied_from_session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub claimed_root_session_id: Option<StableEntityId>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub copy_proof: ProviderNativeCopyProof,
    pub depth: usize,
}

/// Query-time resolution of one selected event's direct copied-event target.
///
/// A missing target is an ordinary lineage answer. Core does not infer or
/// traverse a transitive ancestry chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopiedEventLineageResolution {
    Resolved {
        event_id: StableEntityId,
        session_id: StableEntityId,
    },
    Unresolved {
        event_id: Uuid,
        session_id: Option<StableEntityId>,
    },
}

impl CopiedEventLineageResolution {
    pub const fn state_str(&self) -> &'static str {
        match self {
            Self::Resolved { .. } => "resolved",
            Self::Unresolved { .. } => "unresolved",
        }
    }
}

/// Observed direct-copy count for one optional relationship kind.
///
/// Counts are exact only when the containing lineage result is not truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedEventLineageRelationshipCount {
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub observed_count: u64,
}

/// One bounded reverse copied-event lineage result from a pinned generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedEventLineage {
    pub generation_id: String,
    pub selected_event_id: Uuid,
    pub selected_session_id: Option<StableEntityId>,
    pub resolution: CopiedEventLineageResolution,
    pub selected_depth: usize,
    /// Exact when `truncated` is false; otherwise a lower bound.
    pub observed_count: u64,
    /// Number of retained preview rows; this may be smaller than an exact
    /// `observed_count` without making the direct-edge query truncated.
    pub returned: usize,
    pub occurrences: Vec<CopiedEventLineageOccurrence>,
    pub relationship_counts: Vec<CopiedEventLineageRelationshipCount>,
    /// True only when a posting-work ceiling prevented completion.
    /// A full preview with additional exactly counted rows remains false.
    pub truncated: bool,
}

impl CopiedEventLineage {
    /// Returns the total only when every reverse direct edge was visited within
    /// the posting-work ceiling. Preview retention alone never makes a count
    /// inexact.
    pub fn exact_observed_count(&self) -> Option<u64> {
        (!self.truncated).then_some(self.observed_count)
    }
}

/// One verified event plus its complete generation-owned Core data.
///
/// The event projection is derived from the complete self-contained record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRecord {
    pub event: EventRecord,
    pub core_record: CoreRecord,
}

impl std::ops::Deref for CoreEventRecord {
    type Target = EventRecord;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchCandidate {
    pub event: EventRecord,
    pub score: f32,
}

/// Exact, content-free work performed by one low-level candidate query.
///
/// `collector_hits` is the number of retained candidate addresses handed to
/// materialization. Decoded records and bytes are charged only after a stored
/// Core record has decoded successfully.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventCandidateQueryReceipt {
    pub query_executions: u64,
    pub collector_hits: u64,
    pub records_decoded: u64,
    pub encoded_core_bytes_decoded: u64,
}

/// Candidate rows paired with the exact low-level work that produced them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObservedEventSearchCandidates {
    pub candidates: Vec<EventSearchCandidate>,
    pub receipt: EventCandidateQueryReceipt,
}

/// A candidate-query failure retaining exact work completed before the error.
#[derive(Debug)]
pub struct EventCandidateQueryFailure {
    pub error: IndexError,
    pub receipt: EventCandidateQueryReceipt,
}

pub type DiagnosedEventCandidateQueryResult =
    std::result::Result<ObservedEventSearchCandidates, Box<EventCandidateQueryFailure>>;

/// Completeness-aware lexical batch paired with the exact low-level work that
/// produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedLexicalSearchBatch {
    pub batch: LexicalSearchBatch,
    pub receipt: EventCandidateQueryReceipt,
}

pub type DiagnosedLexicalSearchBatchResult =
    std::result::Result<ObservedLexicalSearchBatch, Box<EventCandidateQueryFailure>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: Option<StableEntityId>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub provider: String,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub agent_scope: Option<CoreAgentScope>,
    pub first_event_sequence: u64,
    pub first_occurred_at_unix_ms: Option<i64>,
}

/// Maximum exact session coordinates accepted by one grouping-authority read.
pub const MAX_SESSION_GROUPING_COORDINATES: usize = 4_096;
/// Maximum sparse authority witnesses accepted for each exact coordinate.
pub const MAX_SESSION_GROUPING_WITNESSES_PER_COORDINATE: usize = 4;
/// Maximum live witnesses retained by one grouping-authority read.
pub const MAX_SESSION_GROUPING_WITNESSES: usize =
    MAX_SESSION_GROUPING_COORDINATES * MAX_SESSION_GROUPING_WITNESSES_PER_COORDINATE;

/// Coalesced exact provider claims for one source-owned session.
///
/// Every optional field remains absent unless at least one sparse authority
/// witness contains that direct literal claim. Conflicting positives fail the
/// complete lookup; this type carries no traversal or inferred topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionGroupingClaims {
    pub session_id: StableEntityId,
    pub source_owner: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: Option<StableEntityId>,
    pub relationship: Option<ProviderNativeSessionRelationship>,
}

/// Why a session received its search-family identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchFamilyBasis {
    /// An exact provider-emitted root claim is the family identity.
    LiteralProviderRoot,
    /// Ranking groups an otherwise unclaimed session with itself.
    /// This is not a provider claim.
    OwnSessionFallback,
}

/// Pure ranking key derived from [`SessionGroupingClaims`].
///
/// Equality and hashing intentionally use only `session_id`. `basis` records
/// why that identity was selected; it is not part of family identity. Thus an
/// unclaimed root session and a child with a literal claim to that root group
/// together even though their evidence bases differ.
#[derive(Debug, Clone, Copy)]
pub struct SearchFamilyKey {
    pub session_id: StableEntityId,
    pub basis: SearchFamilyBasis,
}

impl PartialEq for SearchFamilyKey {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
    }
}

impl Eq for SearchFamilyKey {}

impl std::hash::Hash for SearchFamilyKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.session_id, state);
    }
}

impl SearchFamilyKey {
    pub fn from_claims(claims: &SessionGroupingClaims) -> Self {
        match claims.root_session_id {
            Some(session_id) => Self {
                session_id,
                basis: SearchFamilyBasis::LiteralProviderRoot,
            },
            None => Self {
                session_id: claims.session_id,
                basis: SearchFamilyBasis::OwnSessionFallback,
            },
        }
    }
}

impl From<&SessionGroupingClaims> for SearchFamilyKey {
    fn from(claims: &SessionGroupingClaims) -> Self {
        Self::from_claims(claims)
    }
}

/// Whether search diversification is authoritative for the requested top N.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDiversificationStatus {
    Applied,
    NotApplicable,
    Indeterminate,
}

/// One bounded search query's diversification decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchDiversificationDecision {
    pub status: SearchDiversificationStatus,
    pub top_n: usize,
    pub changed_final_top_n: Option<bool>,
}

/// Small body-free session coordinate used to select bounded Core batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventCoordinate {
    pub event_id: Uuid,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
}

pub(super) type SessionEventCoordinateSortKey = (u64, Option<i64>, u64, u64);

impl SessionEventCoordinate {
    pub(super) fn from_sort_key(sort_key: SessionEventCoordinateSortKey) -> Self {
        let (event_sequence, occurred_at_unix_ms, event_id_high, event_id_low) = sort_key;
        Self {
            event_id: Uuid::from_u128((u128::from(event_id_high) << 64) | u128::from(event_id_low)),
            event_sequence,
            occurred_at_unix_ms,
        }
    }

    pub(super) fn sort_key(&self) -> SessionEventCoordinateSortKey {
        let event_id = self.event_id.as_u128();
        (
            self.event_sequence,
            self.occurred_at_unix_ms,
            (event_id >> 64) as u64,
            event_id as u64,
        )
    }
}
