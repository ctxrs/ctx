use super::{EventRecord, EventSearchCandidate};
use ctx_history_index_format::{IndexError, Result};

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
