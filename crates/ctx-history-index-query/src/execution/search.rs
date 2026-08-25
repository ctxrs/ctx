use super::*;
use crate::records::stored_event_record_with_size;

impl EventCandidateQueryReceipt {
    fn record_query_execution(&mut self) -> Result<()> {
        self.query_executions = self
            .query_executions
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn record_collector_hits(&mut self, hits: usize) -> Result<()> {
        let hits = u64::try_from(hits).map_err(|_| IndexError::CountOverflow)?;
        self.collector_hits = self
            .collector_hits
            .checked_add(hits)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn record_decoded(&mut self, encoded_core_bytes: usize) -> Result<()> {
        let encoded_core_bytes =
            u64::try_from(encoded_core_bytes).map_err(|_| IndexError::CountOverflow)?;
        let records_decoded = self
            .records_decoded
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        let encoded_core_bytes_decoded = self
            .encoded_core_bytes_decoded
            .checked_add(encoded_core_bytes)
            .ok_or(IndexError::CountOverflow)?;
        self.records_decoded = records_decoded;
        self.encoded_core_bytes_decoded = encoded_core_bytes_decoded;
        Ok(())
    }
}

use ctx_history_index_format::BODY_ANALYZER;
use std::sync::Arc;
use tantivy::{
    fieldnorm::FieldNormReader,
    postings::{Postings as _, TermInfo},
    query::Bm25Weight,
};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static MANUAL_INVERTED_INDEX_ACQUISITIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_POSTING_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_LIVE_POSTINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_MAXIMUM_LIVE_POSTINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const EVENT_RANGE_ORDER_FAST_FIELD: &str = "event_range_order";

impl VerifiedIndex {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn reset_manual_lexical_io_observability_for_test(&self) {
        MANUAL_INVERTED_INDEX_ACQUISITIONS.set(0);
        MANUAL_POSTING_READS.set(0);
        MANUAL_LIVE_POSTINGS.set(0);
        MANUAL_MAXIMUM_LIVE_POSTINGS.set(0);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn manual_inverted_index_acquisitions_for_test(&self) -> usize {
        MANUAL_INVERTED_INDEX_ACQUISITIONS.get()
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn manual_posting_reads_for_test(&self) -> usize {
        MANUAL_POSTING_READS.get()
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn maximum_simultaneous_manual_postings_for_test(&self) -> usize {
        MANUAL_MAXIMUM_LIVE_POSTINGS.get()
    }

    /// Compatibility wrapper for a complete manual lexical result.
    pub fn search_event_candidates(
        &self,
        natural_text: &str,
        limit: usize,
    ) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
        complete_compatibility_candidates(self.search_event_candidates_batch(natural_text, limit)?)
    }

    /// Completeness-aware single-text lexical search.
    pub fn search_event_candidates_batch(
        &self,
        natural_text: &str,
        limit: usize,
    ) -> LexicalSearchResult<LexicalSearchBatch> {
        self.search_event_candidates_with_filters_batch(
            natural_text,
            &EventSearchFilters::default(),
            limit,
        )
    }

    /// Compatibility wrapper for complete filtered lexical results.
    pub fn search_event_candidates_with_filters(
        &self,
        natural_text: &str,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
        complete_compatibility_candidates(self.search_event_candidates_with_filters_batch(
            natural_text,
            filters,
            limit,
        )?)
    }

    /// Completeness-aware filtered lexical search.
    pub fn search_event_candidates_with_filters_batch(
        &self,
        natural_text: &str,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> LexicalSearchResult<LexicalSearchBatch> {
        self.search_event_candidates_any_with_filters_batch(&[natural_text], filters, limit)
    }

    /// Compatibility wrapper for complete OR-composed lexical results.
    pub fn search_event_candidates_any_with_filters(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
        complete_compatibility_candidates(self.search_event_candidates_any_with_filters_batch(
            natural_texts,
            filters,
            limit,
        )?)
    }

    pub fn search_event_candidates_any_with_filters_diagnosed(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> DiagnosedEventCandidateQueryResult {
        self.search_event_candidates_any_with_filters_batch_diagnosed(natural_texts, filters, limit)
            .map(|observed| ObservedEventSearchCandidates {
                candidates: observed
                    .batch
                    .candidates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                receipt: observed.receipt,
            })
    }

    /// Executes one deterministic manual union of analyzed body postings.
    ///
    /// Segments are visited by ascending immutable Tantivy segment ID. Within
    /// each segment, body postings are merged by ascending document ID, so an
    /// incomplete result describes one deterministic fully examined prefix.
    pub fn search_event_candidates_any_with_filters_batch(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> LexicalSearchResult<LexicalSearchBatch> {
        self.search_event_candidates_any_with_filters_batch_diagnosed(natural_texts, filters, limit)
            .map(|observed| observed.batch)
            .map_err(|failure| LexicalSearchError::Index(failure.error))
    }

    pub fn search_event_candidates_any_with_filters_batch_diagnosed(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> DiagnosedLexicalSearchBatchResult {
        self.execute_manual_lexical_diagnosed(
            ManualLexicalMode::Body(natural_texts),
            filters,
            limit,
            LEXICAL_WORK_BUDGET_V1,
        )
    }

    /// Compatibility wrapper for a complete filtered list result.
    pub fn list_event_candidates_with_filters(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
        complete_compatibility_candidates(
            self.list_event_candidates_with_filters_batch(filters, limit)?,
        )
    }

    pub fn list_event_candidates_with_filters_diagnosed(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> DiagnosedEventCandidateQueryResult {
        self.list_event_candidates_with_filters_batch_diagnosed(filters, limit)
            .map(|observed| ObservedEventSearchCandidates {
                candidates: observed
                    .batch
                    .candidates
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                receipt: observed.receipt,
            })
    }

    /// Completeness-aware list mode using the same exact manual filters, heap,
    /// ordering, Core validation, and work accounting as lexical search.
    pub fn list_event_candidates_with_filters_batch(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> LexicalSearchResult<LexicalSearchBatch> {
        self.list_event_candidates_with_filters_batch_diagnosed(filters, limit)
            .map(|observed| observed.batch)
            .map_err(|failure| LexicalSearchError::Index(failure.error))
    }

    pub fn list_event_candidates_with_filters_batch_diagnosed(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> DiagnosedLexicalSearchBatchResult {
        self.execute_manual_lexical_diagnosed(
            ManualLexicalMode::List,
            filters,
            limit,
            LEXICAL_WORK_BUDGET_V1,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn search_event_candidates_any_with_filters_batch_with_budget_for_test(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
        budget: LexicalWorkBudget,
    ) -> LexicalSearchResult<LexicalSearchBatch> {
        self.execute_manual_lexical_diagnosed(
            ManualLexicalMode::Body(natural_texts),
            filters,
            limit,
            budget,
        )
        .map(|observed| observed.batch)
        .map_err(|failure| LexicalSearchError::Index(failure.error))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn search_event_candidates_any_with_filters_with_budget_for_test(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
        budget: LexicalWorkBudget,
    ) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
        complete_compatibility_candidates(
            self.search_event_candidates_any_with_filters_batch_with_budget_for_test(
                natural_texts,
                filters,
                limit,
                budget,
            )?,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn list_event_candidates_with_filters_batch_with_budget_for_test(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
        budget: LexicalWorkBudget,
    ) -> LexicalSearchResult<LexicalSearchBatch> {
        self.execute_manual_lexical_diagnosed(ManualLexicalMode::List, filters, limit, budget)
            .map(|observed| observed.batch)
            .map_err(|failure| LexicalSearchError::Index(failure.error))
    }

    fn execute_manual_lexical_diagnosed(
        &self,
        mode: ManualLexicalMode<'_>,
        filters: &EventSearchFilters,
        limit: usize,
        budget: LexicalWorkBudget,
    ) -> DiagnosedLexicalSearchBatchResult {
        #[cfg(any(test, feature = "test-support"))]
        let _failure_injection_reset = lexical_candidate_materialization_failure_reset();
        let mut receipt = EventCandidateQueryReceipt::default();
        let result = self.execute_manual_lexical_inner(mode, filters, limit, budget, &mut receipt);
        match result {
            Ok(batch) => Ok(ObservedLexicalSearchBatch { batch, receipt }),
            Err(error) => Err(Box::new(EventCandidateQueryFailure { error, receipt })),
        }
    }

    fn execute_manual_lexical_inner(
        &self,
        mode: ManualLexicalMode<'_>,
        filters: &EventSearchFilters,
        limit: usize,
        budget: LexicalWorkBudget,
        receipt: &mut EventCandidateQueryReceipt,
    ) -> Result<LexicalSearchBatch> {
        validate_lexical_result_limit(limit)?;
        if let ManualLexicalMode::Body(natural_texts) = mode {
            LEXICAL_QUERY_LIMITS.validate_texts(natural_texts.iter().copied())?;
        }
        validate_manual_filter_inputs(filters)?;
        if limit == 0 {
            return Ok(empty_lexical_batch(false));
        }

        let fields = fields_from_schema(self.searcher.schema())?;
        let (body_terms, analyzed_tokens) = match mode {
            ManualLexicalMode::Body(natural_texts) => {
                let analyzed = self.analyze_manual_body_terms(natural_texts, fields)?;
                if analyzed.0.is_empty() {
                    return Ok(empty_lexical_batch(true));
                }
                analyzed
            }
            ManualLexicalMode::List => (Vec::new(), 0),
        };

        receipt.record_query_execution()?;
        #[cfg(any(test, feature = "test-support"))]
        record_lexical_query_execution();
        let mut meter = LexicalWorkMeter::new(budget);
        meter.record_analyzed_tokens(analyzed_tokens)?;
        let Some(filter_plan) = compile_manual_filter_plan(filters, fields, &mut meter)? else {
            return Ok(finish_lexical_batch(Vec::new(), meter, false));
        };
        if filter_plan.match_none {
            return Ok(finish_lexical_batch(Vec::new(), meter, false));
        }

        let Some((prepared_segments, body_weights)) =
            self.prepare_manual_segments(&body_terms, fields, &mut meter)?
        else {
            return Ok(finish_lexical_batch(Vec::new(), meter, false));
        };

        let mut retained = BinaryHeap::new();
        let mut retained_truncated = false;
        let query_term_count =
            u8::try_from(body_terms.len()).map_err(|_| IndexError::CountOverflow)?;
        'segments: for body_segment in prepared_segments {
            let reader = self
                .searcher
                .segment_readers()
                .get(body_segment.context.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ))?;
            let Some(mut prepared) = open_manual_segment(
                reader,
                body_segment,
                &filter_plan,
                filters.content_scope,
                fields,
                &mut meter,
            )?
            else {
                break 'segments;
            };
            if prepared.filters.matches_none() {
                continue;
            }
            match mode {
                ManualLexicalMode::Body(_) => {
                    while let Some(doc) = next_body_doc(&prepared.body_postings) {
                        if !meter.charge(
                            LexicalWorkCounter::CandidateDocs,
                            1,
                            Some(&prepared.context),
                            Some(doc),
                        ) {
                            break 'segments;
                        }
                        let mut term_frequencies = [0_u32; 32];
                        for (index, postings) in prepared.body_postings.iter().enumerate() {
                            if postings
                                .as_ref()
                                .is_some_and(|postings| postings.doc() == doc)
                            {
                                term_frequencies[index] = postings
                                    .as_ref()
                                    .expect("matching posting exists")
                                    .term_freq();
                            }
                        }
                        if !reader.is_deleted(doc) {
                            let outcome = self.examine_manual_candidate(
                                reader,
                                &mut prepared,
                                doc,
                                &term_frequencies[..body_terms.len()],
                                &body_weights,
                                query_term_count,
                                &filter_plan,
                                limit,
                                &mut retained,
                                &mut retained_truncated,
                                &mut meter,
                            )?;
                            if outcome == CandidateExamination::Exhausted {
                                break 'segments;
                            }
                        }
                        for postings in &mut prepared.body_postings {
                            if postings
                                .as_ref()
                                .is_some_and(|postings| postings.doc() == doc)
                            {
                                if !meter.charge(
                                    LexicalWorkCounter::BodyPostingAdvances,
                                    1,
                                    Some(&prepared.context),
                                    Some(doc),
                                ) {
                                    break 'segments;
                                }
                                postings
                                    .as_mut()
                                    .expect("matching posting exists")
                                    .advance();
                            }
                        }
                    }
                }
                ManualLexicalMode::List => {
                    for doc in 0..reader.max_doc() {
                        if !meter.charge(
                            LexicalWorkCounter::CandidateDocs,
                            1,
                            Some(&prepared.context),
                            Some(doc),
                        ) {
                            break 'segments;
                        }
                        if reader.is_deleted(doc) {
                            continue;
                        }
                        let outcome = self.examine_manual_candidate(
                            reader,
                            &mut prepared,
                            doc,
                            &[],
                            &body_weights,
                            0,
                            &filter_plan,
                            limit,
                            &mut retained,
                            &mut retained_truncated,
                            &mut meter,
                        )?;
                        if outcome == CandidateExamination::Exhausted {
                            break 'segments;
                        }
                    }
                }
            }
        }

        let mut ranked = retained
            .into_iter()
            .map(|Reverse(candidate)| candidate)
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.cmp(left));
        receipt.record_collector_hits(ranked.len())?;
        let mut candidates = Vec::with_capacity(ranked.len());
        for candidate in ranked {
            let encoded_bytes = u64::try_from(candidate.order.encoded_core_bytes())
                .map_err(|_| IndexError::CountOverflow)?;
            if !meter.charge_pair(
                (LexicalWorkCounter::FinalMaterializations, 1),
                (LexicalWorkCounter::FinalMaterializationBytes, encoded_bytes),
                Some(&candidate.segment),
                Some(candidate.address.doc_id),
            ) {
                break;
            }
            let fast = core_event_fast_preflight(&self.searcher, candidate.address)?;
            if fast.1 != candidate.order.encoded_core_bytes()
                || fast.2 != candidate.order.content_bytes()
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ));
            }
            #[cfg(any(test, feature = "test-support"))]
            if lexical_candidate_materialization_should_fail() {
                return Err(IndexError::InvalidStoredDocumentField(
                    "test_lexical_candidate_materialization_failure",
                ));
            }
            let (event, encoded_core_bytes) =
                stored_event_record_with_size(&self.searcher, candidate.address, fields)?;
            receipt.record_decoded(encoded_core_bytes)?;
            validate_materialized_event(candidate.order, &event, fast.0)?;
            candidates.push(LexicalSearchCandidate {
                event,
                score: candidate.score,
                coverage: LexicalTermCoverage {
                    matched_terms: candidate.coverage,
                    query_terms: candidate.query_terms,
                },
            });
        }
        Ok(finish_lexical_batch(candidates, meter, retained_truncated))
    }

    fn analyze_manual_body_terms(
        &self,
        natural_texts: &[&str],
        fields: Fields,
    ) -> Result<(Vec<Term>, usize)> {
        let mut analyzer = self
            .searcher
            .index()
            .tokenizers()
            .get(BODY_ANALYZER)
            .ok_or(IndexError::MissingAnalyzer(BODY_ANALYZER))?;
        let mut terms = BTreeSet::new();
        let mut analyzed_tokens = 0_usize;
        for natural_text in natural_texts {
            let mut stream = analyzer.token_stream(natural_text);
            while stream.advance() {
                analyzed_tokens = analyzed_tokens
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                if analyzed_tokens > LEXICAL_QUERY_LIMITS.maximum_unique_tokens {
                    return Err(IndexError::LexicalQueryTokensTooMany {
                        observed: analyzed_tokens,
                        maximum: LEXICAL_QUERY_LIMITS.maximum_unique_tokens,
                    });
                }
                terms.insert(Term::from_field_text(
                    fields.body_search,
                    &stream.token().text,
                ));
            }
        }
        Ok((terms.into_iter().collect(), analyzed_tokens))
    }

    fn prepare_manual_segments(
        &self,
        body_terms: &[Term],
        fields: Fields,
        meter: &mut LexicalWorkMeter,
    ) -> Result<Option<(Vec<PreparedBodySegment>, Vec<Bm25Weight>)>> {
        let segment_count = u64::try_from(self.searcher.segment_readers().len())
            .map_err(|_| IndexError::CountOverflow)?;
        if !meter.charge(LexicalWorkCounter::Segments, segment_count, None, None) {
            return Ok(None);
        }
        let mut stable_segments = self
            .searcher
            .segment_readers()
            .iter()
            .enumerate()
            .collect::<Vec<_>>();
        stable_segments.sort_by_key(|(_, reader)| reader.segment_id());

        let mut prepared_segments = Vec::with_capacity(stable_segments.len());
        let mut body_doc_frequencies = vec![0_u64; body_terms.len()];
        let mut total_num_docs = 0_u64;
        let mut total_num_tokens = 0_u64;
        for (stable_segment_index, (segment_ord, reader)) in stable_segments.into_iter().enumerate()
        {
            let context = LexicalSegmentContext {
                stable_segment_index,
                segment_id: reader.segment_id().to_string(),
                segment_ord: u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?,
            };

            let mut body_term_infos = Vec::with_capacity(body_terms.len());
            if !body_terms.is_empty() {
                // The first term charge admits both acquisition of this
                // segment's body dictionary and its first exact lookup. This
                // guarantees a zero dictionary budget performs neither.
                if !meter.charge(
                    LexicalWorkCounter::DictionaryLookups,
                    1,
                    Some(&context),
                    None,
                ) {
                    return Ok(None);
                }
                let body_inverted = manual_inverted_index(reader, fields.body_search)?;
                total_num_docs = total_num_docs
                    .checked_add(u64::from(reader.max_doc()))
                    .ok_or(IndexError::CountOverflow)?;
                total_num_tokens = total_num_tokens
                    .checked_add(body_inverted.total_num_tokens())
                    .ok_or(IndexError::CountOverflow)?;
                for (index, term) in body_terms.iter().enumerate() {
                    if index > 0
                        && !meter.charge(
                            LexicalWorkCounter::DictionaryLookups,
                            1,
                            Some(&context),
                            None,
                        )
                    {
                        return Ok(None);
                    }
                    let term_info = body_inverted.get_term_info(term)?;
                    if let Some(term_info) = &term_info {
                        body_doc_frequencies[index] = body_doc_frequencies[index]
                            .checked_add(u64::from(term_info.doc_freq))
                            .ok_or(IndexError::CountOverflow)?;
                    }
                    body_term_infos.push(term_info);
                }
            }

            prepared_segments.push(PreparedBodySegment {
                context,
                body_term_infos,
            });
        }

        let body_weights = if body_terms.is_empty() || total_num_docs == 0 {
            Vec::new()
        } else {
            let average_fieldnorm = total_num_tokens as Score / total_num_docs as Score;
            body_doc_frequencies
                .into_iter()
                .map(|doc_frequency| {
                    explicit_bm25_weight(doc_frequency, total_num_docs, average_fieldnorm)
                })
                .collect()
        };
        Ok(Some((prepared_segments, body_weights)))
    }

    #[allow(clippy::too_many_arguments)]
    fn examine_manual_candidate(
        &self,
        reader: &SegmentReader,
        prepared: &mut PreparedSegment,
        doc: DocId,
        term_frequencies: &[u32],
        body_weights: &[Bm25Weight],
        query_term_count: u8,
        filter_plan: &ManualFilterPlan,
        limit: usize,
        retained: &mut BinaryHeap<Reverse<RankedAddressCandidate>>,
        retained_truncated: &mut bool,
        meter: &mut LexicalWorkMeter,
    ) -> Result<CandidateExamination> {
        let Some(matches_exact_filters) =
            prepared.filters.accepts(doc, meter, &prepared.context)?
        else {
            return Ok(CandidateExamination::Exhausted);
        };
        if !matches_exact_filters {
            return Ok(CandidateExamination::Rejected);
        }

        let order = event_range_order(reader, doc)?;
        if filter_plan.since_unix_ms.is_some_and(|since| {
            order
                .occurred_at_unix_ms()
                .is_none_or(|occurred_at| occurred_at < since)
        }) {
            return Ok(CandidateExamination::Rejected);
        }

        let coverage = term_frequencies
            .iter()
            .filter(|frequency| **frequency > 0)
            .count();
        let coverage = u8::try_from(coverage).map_err(|_| IndexError::CountOverflow)?;
        let mut score = if term_frequencies.is_empty() {
            0.0
        } else {
            let fieldnorm_id = prepared
                .fieldnorms
                .as_ref()
                .ok_or(IndexError::InvalidStoredDocumentField("body_search"))?
                .fieldnorm_id(doc);
            body_weights
                .iter()
                .zip(term_frequencies)
                .map(|(weight, frequency)| weight.score(fieldnorm_id, *frequency))
                .sum()
        };
        let Some(class_weight) = prepared.classes.weight(doc, meter, &prepared.context) else {
            return Ok(CandidateExamination::Exhausted);
        };
        score *= class_weight;

        let address = DocAddress::new(prepared.context.segment_ord, doc);
        let candidate = RankedAddressCandidate {
            coverage,
            query_terms: query_term_count,
            score,
            order,
            address,
            segment: prepared.context.clone(),
        };
        if retained.len() < limit {
            if !meter.charge(
                LexicalWorkCounter::RetainedCandidates,
                1,
                Some(&prepared.context),
                Some(doc),
            ) {
                return Ok(CandidateExamination::Exhausted);
            }
            retained.push(Reverse(candidate));
        } else {
            // Whether the new candidate wins or loses, one admissible match is
            // discarded once the fixed retained heap is full.
            *retained_truncated = true;
            if retained
                .peek()
                .is_some_and(|Reverse(worst)| candidate > *worst)
            {
                retained.pop();
                retained.push(Reverse(candidate));
            }
        }
        Ok(CandidateExamination::Accepted)
    }

    /// Selects semantic-eligible event IDs with the existing generic metadata
    /// predicate. Semantic consumers intentionally retain their independent
    /// non-lexical query implementation.
    pub fn semantic_filter_projection(
        &self,
        filters: &EventSearchFilters,
    ) -> Result<SemanticFilterProjection> {
        filters.validate_content_scope()?;
        validate_event_sort_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let semantic_eligibility = Box::new(BooleanQuery::intersection(vec![
            Box::new(TermQuery::new(
                Term::from_field_text(fields.event_type, "message"),
                IndexRecordOption::Basic,
            )),
            Box::new(TermQuery::new(
                Term::from_field_text(fields.role, "user"),
                IndexRecordOption::Basic,
            )),
        ]));
        let source_identity_query = self.source_identity_query(filters, fields)?;
        let query =
            filtered_event_query(semantic_eligibility, source_identity_query, filters, fields)?;
        let addresses = self
            .searcher
            .search(query.as_ref(), &DocSetCollector)
            .map_err(IndexError::from)?;
        let mut event_ids = HashSet::with_capacity(addresses.len());
        for address in addresses {
            let (event_id, _, _) = core_event_fast_preflight(&self.searcher, address)?;
            if !event_ids.insert(event_id) {
                return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
            }
        }
        Ok(SemanticFilterProjection {
            generation_id: self.generation_id.clone(),
            event_ids,
        })
    }

    fn source_identity_query(
        &self,
        filters: &EventSearchFilters,
        fields: Fields,
    ) -> Result<Option<Box<dyn Query>>> {
        if !filters.has_source_identity_filter() {
            return Ok(None);
        }
        filters.validate_source_identity_filters()?;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.provider, "custom"),
                IndexRecordOption::Basic,
            )),
        )];
        if let Some(history_source) = filters.history_source.as_deref() {
            let Some((history_provider_key, history_source_id)) =
                history_source.trim().split_once('/')
            else {
                return Ok(Some(Box::new(EmptyQuery)));
            };
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_provider_key, history_provider_key),
                    IndexRecordOption::Basic,
                )),
            ));
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_source_id, history_source_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(provider_key) = filters.provider_key.as_deref().map(str::trim) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_provider_key, provider_key),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(source_id) = filters.source_id.as_deref().map(str::trim) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_source_id, source_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        Ok(Some(Box::new(BooleanQuery::new(clauses))))
    }
}

#[derive(Debug, Clone, Copy)]
enum ManualLexicalMode<'a> {
    Body(&'a [&'a str]),
    List,
}

/// Stable segment identity plus the small body metadata required for exact
/// corpus-wide BM25 statistics. No cursor or reader is retained here.
struct PreparedBodySegment {
    context: LexicalSegmentContext,
    body_term_infos: Vec<Option<TermInfo>>,
}

struct PreparedSegment {
    context: LexicalSegmentContext,
    body_postings: Vec<Option<SegmentPostings>>,
    fieldnorms: Option<FieldNormReader>,
    filters: SegmentFilters,
    classes: SegmentClasses,
}

impl PreparedSegment {
    fn new(
        context: LexicalSegmentContext,
        body_postings: Vec<Option<SegmentPostings>>,
        fieldnorms: Option<FieldNormReader>,
        filters: SegmentFilters,
        classes: SegmentClasses,
    ) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        record_live_postings_added(
            body_postings.iter().flatten().count()
                + filters.posting_count()
                + classes.posting_count(),
        );
        Self {
            context,
            body_postings,
            fieldnorms,
            filters,
            classes,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn posting_count(&self) -> usize {
        self.body_postings.iter().flatten().count()
            + self.filters.posting_count()
            + self.classes.posting_count()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for PreparedSegment {
    fn drop(&mut self) {
        let posting_count = self.posting_count();
        MANUAL_LIVE_POSTINGS.set(MANUAL_LIVE_POSTINGS.get() - posting_count);
    }
}

struct SegmentFilters {
    required: Vec<Vec<SegmentPostings>>,
    prohibited: Vec<Vec<SegmentPostings>>,
    workspace: Option<SegmentBitmap>,
    file: Option<SegmentBitmap>,
}

impl SegmentFilters {
    #[cfg(any(test, feature = "test-support"))]
    fn posting_count(&self) -> usize {
        self.required.iter().map(Vec::len).sum::<usize>()
            + self.prohibited.iter().map(Vec::len).sum::<usize>()
    }

    fn matches_none(&self) -> bool {
        self.required.iter().any(Vec::is_empty)
            || self.workspace.as_ref().is_some_and(|bitmap| !bitmap.any)
            || self.file.as_ref().is_some_and(|bitmap| !bitmap.any)
    }

    fn accepts(
        &mut self,
        doc: DocId,
        meter: &mut LexicalWorkMeter,
        segment: &LexicalSegmentContext,
    ) -> Result<Option<bool>> {
        for group in &mut self.required {
            let Some(matches) = posting_group_contains(group, doc, meter, segment) else {
                return Ok(None);
            };
            if !matches {
                return Ok(Some(false));
            }
        }
        for group in &mut self.prohibited {
            let Some(matches) = posting_group_contains(group, doc, meter, segment) else {
                return Ok(None);
            };
            if matches {
                return Ok(Some(false));
            }
        }
        for bitmap in [self.workspace.as_ref(), self.file.as_ref()] {
            let Some(bitmap) = bitmap else {
                continue;
            };
            if !meter.charge(
                LexicalWorkCounter::FilterProbes,
                1,
                Some(segment),
                Some(doc),
            ) {
                return Ok(None);
            }
            if !bitmap.contains(doc)? {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }
}

struct SegmentBitmap {
    words: Vec<u64>,
    any: bool,
}

impl SegmentBitmap {
    fn insert(&mut self, doc: DocId) -> Result<()> {
        let doc = usize::try_from(doc).map_err(|_| IndexError::CountOverflow)?;
        let word = self
            .words
            .get_mut(doc / u64::BITS as usize)
            .ok_or(IndexError::InvalidStoredDocumentField("literal_fact"))?;
        *word |= 1_u64 << (doc % u64::BITS as usize);
        self.any = true;
        Ok(())
    }

    fn contains(&self, doc: DocId) -> Result<bool> {
        let doc = usize::try_from(doc).map_err(|_| IndexError::CountOverflow)?;
        let word = self
            .words
            .get(doc / u64::BITS as usize)
            .ok_or(IndexError::InvalidStoredDocumentField("literal_fact"))?;
        Ok(word & (1_u64 << (doc % u64::BITS as usize)) != 0)
    }
}

enum SegmentClasses {
    Unweighted,
    Weighted(Box<WeightedSegmentClasses>),
}

struct WeightedSegmentClasses {
    message: Option<SegmentPostings>,
    summary: Option<SegmentPostings>,
    outputs: Vec<SegmentPostings>,
    scope: SearchContentScope,
}

impl SegmentClasses {
    #[cfg(any(test, feature = "test-support"))]
    fn posting_count(&self) -> usize {
        match self {
            Self::Unweighted => 0,
            Self::Weighted(weighted) => {
                usize::from(weighted.message.is_some())
                    + usize::from(weighted.summary.is_some())
                    + weighted.outputs.len()
            }
        }
    }

    fn weight(
        &mut self,
        doc: DocId,
        meter: &mut LexicalWorkMeter,
        segment: &LexicalSegmentContext,
    ) -> Option<Score> {
        let Self::Weighted(weighted) = self else {
            return Some(1.0);
        };
        if posting_option_contains(&mut weighted.message, doc, meter, segment)? {
            return Some(1.0);
        }
        if posting_option_contains(&mut weighted.summary, doc, meter, segment)? {
            return Some(0.9);
        }
        if posting_group_contains(&mut weighted.outputs, doc, meter, segment)? {
            return Some(0.6);
        }
        Some(match weighted.scope {
            SearchContentScope::All => 0.8,
            SearchContentScope::Transcript => 0.9,
            SearchContentScope::Calls | SearchContentScope::Outputs => unreachable!(),
        })
    }
}

enum PostingOpen {
    Opened { postings: Box<SegmentPostings> },
    Missing,
    Exhausted,
}

/// Tantivy's `Bm25Weight` is used only as a pure formula value. Both corpus
/// inputs come from the explicitly metered manual term-info prepass, and the
/// executor calls only `score(fieldnorm_id, term_frequency)` afterward. No
/// `Query`, `Weight`, `Scorer`, `TopDocs`, or `Searcher` execution is hidden in
/// this helper.
fn explicit_bm25_weight(
    doc_frequency: u64,
    total_num_docs: u64,
    average_fieldnorm: Score,
) -> Bm25Weight {
    Bm25Weight::for_one_term_without_explain(doc_frequency, total_num_docs, average_fieldnorm)
}

fn manual_inverted_index(reader: &SegmentReader, field: Field) -> Result<Arc<InvertedIndexReader>> {
    #[cfg(any(test, feature = "test-support"))]
    MANUAL_INVERTED_INDEX_ACQUISITIONS
        .set(MANUAL_INVERTED_INDEX_ACQUISITIONS.get().saturating_add(1));
    Ok(reader.inverted_index(field)?)
}

fn manual_read_postings(
    inverted: &InvertedIndexReader,
    term_info: &TermInfo,
    option: IndexRecordOption,
) -> Result<SegmentPostings> {
    #[cfg(any(test, feature = "test-support"))]
    MANUAL_POSTING_READS.set(MANUAL_POSTING_READS.get().saturating_add(1));
    Ok(inverted.read_postings_from_terminfo(term_info, option)?)
}

#[cfg(any(test, feature = "test-support"))]
fn record_live_postings_added(posting_count: usize) {
    let live = MANUAL_LIVE_POSTINGS.get().saturating_add(posting_count);
    MANUAL_LIVE_POSTINGS.set(live);
    MANUAL_MAXIMUM_LIVE_POSTINGS.set(MANUAL_MAXIMUM_LIVE_POSTINGS.get().max(live));
}

fn open_manual_segment(
    reader: &SegmentReader,
    prepared: PreparedBodySegment,
    filter_plan: &ManualFilterPlan,
    content_scope: SearchContentScope,
    fields: Fields,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<PreparedSegment>> {
    let PreparedBodySegment {
        context,
        body_term_infos,
    } = prepared;
    let Some(body_postings) = open_body_postings(
        reader,
        fields.body_search,
        &body_term_infos,
        meter,
        &context,
    )?
    else {
        return Ok(None);
    };
    let fieldnorms = if body_term_infos.is_empty() {
        None
    } else {
        Some(reader.get_fieldnorms_reader(fields.body_search)?)
    };
    let Some(filters) = open_segment_filters(reader, filter_plan, fields, meter, &context)? else {
        return Ok(None);
    };
    let Some(classes) =
        open_segment_classes(reader, fields.event_type, content_scope, meter, &context)?
    else {
        return Ok(None);
    };
    Ok(Some(PreparedSegment::new(
        context,
        body_postings,
        fieldnorms,
        filters,
        classes,
    )))
}

fn open_body_postings(
    reader: &SegmentReader,
    field: Field,
    term_infos: &[Option<TermInfo>],
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<Vec<Option<SegmentPostings>>>> {
    let mut inverted = None;
    let mut postings = Vec::with_capacity(term_infos.len());
    for term_info in term_infos {
        let Some(term_info) = term_info else {
            postings.push(None);
            continue;
        };
        // Term metadata was gathered in the corpus-statistics prepass. Charge
        // the posting read before reacquiring the segment's inverted reader.
        if !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(segment), None) {
            return Ok(None);
        }
        let inverted = match &inverted {
            Some(inverted) => inverted,
            None => inverted.insert(manual_inverted_index(reader, field)?),
        };
        postings.push(Some(manual_read_postings(
            inverted,
            term_info,
            IndexRecordOption::WithFreqs,
        )?));
    }
    Ok(Some(postings))
}

fn open_posting(
    reader: &SegmentReader,
    term: &Term,
    option: IndexRecordOption,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<PostingOpen> {
    // This charge precedes both inverted-reader acquisition and the exact
    // term-info lookup. A zero dictionary budget therefore performs neither.
    if !meter.charge(
        LexicalWorkCounter::DictionaryLookups,
        1,
        Some(segment),
        None,
    ) {
        return Ok(PostingOpen::Exhausted);
    }
    let inverted = manual_inverted_index(reader, term.field())?;
    let Some(term_info) = inverted.get_term_info(term)? else {
        return Ok(PostingOpen::Missing);
    };
    if !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(segment), None) {
        return Ok(PostingOpen::Exhausted);
    }
    Ok(PostingOpen::Opened {
        postings: Box::new(manual_read_postings(&inverted, &term_info, option)?),
    })
}

fn open_segment_filters(
    reader: &SegmentReader,
    plan: &ManualFilterPlan,
    fields: Fields,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<SegmentFilters>> {
    let Some(required) = open_posting_groups(reader, &plan.required, meter, segment)? else {
        return Ok(None);
    };
    let Some(prohibited) = open_posting_groups(reader, &plan.prohibited, meter, segment)? else {
        return Ok(None);
    };
    if required.iter().any(Vec::is_empty) {
        return Ok(Some(SegmentFilters {
            required,
            prohibited,
            workspace: None,
            file: None,
        }));
    }
    let workspace = if let Some(needle) = plan.workspace_substring.as_ref() {
        let workspace_fields = [
            fields.fact_workspace,
            fields.fact_session_cwd,
            fields.fact_tool_workdir,
            fields.fact_project,
        ];
        let Some(bitmap) =
            open_substring_bitmap(reader, &workspace_fields, needle, meter, segment)?
        else {
            return Ok(None);
        };
        Some(bitmap)
    } else {
        None
    };
    let file = if workspace.as_ref().is_some_and(|bitmap| !bitmap.any) {
        None
    } else if let Some(needle) = plan.file_substring.as_ref() {
        let Some(bitmap) =
            open_substring_bitmap(reader, &[fields.fact_file], needle, meter, segment)?
        else {
            return Ok(None);
        };
        Some(bitmap)
    } else {
        None
    };
    Ok(Some(SegmentFilters {
        required,
        prohibited,
        workspace,
        file,
    }))
}

fn open_substring_bitmap(
    reader: &SegmentReader,
    fields: &[Field],
    needle: &AsciiFoldSubstring,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<SegmentBitmap>> {
    let word_count = usize::try_from(reader.max_doc())
        .map_err(|_| IndexError::CountOverflow)?
        .div_ceil(u64::BITS as usize);
    let bitmap_bytes = word_count
        .checked_mul(size_of::<u64>())
        .ok_or(IndexError::CountOverflow)?;
    let bitmap_bytes = u64::try_from(bitmap_bytes).map_err(|_| IndexError::CountOverflow)?;
    if !meter.charge(
        LexicalWorkCounter::SubstringBitmapBytes,
        bitmap_bytes,
        Some(segment),
        None,
    ) {
        return Ok(None);
    }
    let mut bitmap = SegmentBitmap {
        words: vec![0; word_count],
        any: false,
    };

    for field in fields {
        // The step charge precedes both inverted-index acquisition and the
        // first dictionary advance. The final unsuccessful advance is also a
        // real, bounded dictionary operation and is intentionally counted.
        if !meter.charge(
            LexicalWorkCounter::SubstringDictionarySteps,
            1,
            Some(segment),
            None,
        ) {
            return Ok(None);
        }
        let inverted = manual_inverted_index(reader, *field)?;
        let mut terms = inverted.terms().stream()?;
        loop {
            if !terms.advance() {
                break;
            }
            let key = terms.key();
            let key_bytes = u64::try_from(key.len()).map_err(|_| IndexError::CountOverflow)?;
            if !meter.charge(
                LexicalWorkCounter::SubstringDictionaryBytes,
                key_bytes,
                Some(segment),
                None,
            ) {
                return Ok(None);
            }
            std::str::from_utf8(key)
                .map_err(|_| IndexError::InvalidStoredDocumentField("literal_fact"))?;
            if needle.matches(key) {
                if !meter.charge(LexicalWorkCounter::TermExpansions, 1, Some(segment), None)
                    || !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(segment), None)
                {
                    return Ok(None);
                }
                let mut postings =
                    manual_read_postings(&inverted, terms.value(), IndexRecordOption::Basic)?;
                let mut doc = postings.doc();
                while doc != TERMINATED {
                    if !meter.charge(
                        LexicalWorkCounter::SubstringPostingDocs,
                        1,
                        Some(segment),
                        Some(doc),
                    ) {
                        return Ok(None);
                    }
                    bitmap.insert(doc)?;
                    doc = postings.advance();
                }
            }
            if !meter.charge(
                LexicalWorkCounter::SubstringDictionarySteps,
                1,
                Some(segment),
                None,
            ) {
                return Ok(None);
            }
        }
    }
    Ok(Some(bitmap))
}

fn open_posting_groups(
    reader: &SegmentReader,
    groups: &[CanonicalAnyOfTerms],
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<Vec<Vec<SegmentPostings>>>> {
    let mut opened_groups = Vec::with_capacity(groups.len());
    for group in groups {
        let mut postings = Vec::with_capacity(group.terms.len());
        for term in &group.terms {
            match open_posting(reader, term, IndexRecordOption::Basic, meter, segment)? {
                PostingOpen::Opened {
                    postings: opened, ..
                } => postings.push(*opened),
                PostingOpen::Missing => {}
                PostingOpen::Exhausted => return Ok(None),
            }
        }
        opened_groups.push(postings);
    }
    Ok(Some(opened_groups))
}

fn open_segment_classes(
    reader: &SegmentReader,
    event_type_field: Field,
    scope: SearchContentScope,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<SegmentClasses>> {
    if matches!(
        scope,
        SearchContentScope::Calls | SearchContentScope::Outputs
    ) {
        return Ok(Some(SegmentClasses::Unweighted));
    }
    let open = |value: &str, meter: &mut LexicalWorkMeter| {
        open_posting(
            reader,
            &Term::from_field_text(event_type_field, value),
            IndexRecordOption::Basic,
            meter,
            segment,
        )
    };
    let message = match open("message", meter)? {
        PostingOpen::Opened { postings, .. } => Some(*postings),
        PostingOpen::Missing => None,
        PostingOpen::Exhausted => return Ok(None),
    };
    let summary = match open("summary", meter)? {
        PostingOpen::Opened { postings, .. } => Some(*postings),
        PostingOpen::Missing => None,
        PostingOpen::Exhausted => return Ok(None),
    };
    let mut outputs = Vec::new();
    if scope == SearchContentScope::All {
        for output_type in OUTPUT_EVENT_TYPES {
            match open(output_type, meter)? {
                PostingOpen::Opened { postings, .. } => outputs.push(*postings),
                PostingOpen::Missing => {}
                PostingOpen::Exhausted => return Ok(None),
            }
        }
    }
    Ok(Some(SegmentClasses::Weighted(Box::new(
        WeightedSegmentClasses {
            message,
            summary,
            outputs,
            scope,
        },
    ))))
}

fn posting_option_contains(
    postings: &mut Option<SegmentPostings>,
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Option<bool> {
    let Some(postings) = postings else {
        return Some(false);
    };
    posting_contains(postings, doc, meter, segment)
}

fn posting_group_contains(
    postings: &mut [SegmentPostings],
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Option<bool> {
    for postings in postings {
        if posting_contains(postings, doc, meter, segment)? {
            return Some(true);
        }
    }
    Some(false)
}

fn posting_contains(
    postings: &mut SegmentPostings,
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Option<bool> {
    if !meter.charge(
        LexicalWorkCounter::FilterProbes,
        1,
        Some(segment),
        Some(doc),
    ) {
        return None;
    }
    let posting_doc = postings.doc();
    if posting_doc == doc {
        return Some(true);
    }
    if posting_doc < doc {
        if !meter.charge(LexicalWorkCounter::FilterSeeks, 1, Some(segment), Some(doc)) {
            return None;
        }
        return Some(postings.seek(doc) == doc);
    }
    Some(false)
}

fn next_body_doc(postings: &[Option<SegmentPostings>]) -> Option<DocId> {
    postings
        .iter()
        .filter_map(|postings| postings.as_ref().map(|postings| postings.doc()))
        .filter(|doc| *doc != TERMINATED)
        .min()
}

fn event_range_order(
    reader: &SegmentReader,
    doc: DocId,
) -> Result<ctx_history_index_format::EventRangeOrderKey> {
    let column = reader
        .fast_fields()
        .bytes(EVENT_RANGE_ORDER_FAST_FIELD)?
        .ok_or(IndexError::InvalidStoredDocumentField(
            EVENT_RANGE_ORDER_FAST_FIELD,
        ))?;
    let mut term_ords = column.term_ords(doc);
    let term_ord = term_ords
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(
            EVENT_RANGE_ORDER_FAST_FIELD,
        ))?;
    if term_ords.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(
            EVENT_RANGE_ORDER_FAST_FIELD,
        ));
    }
    let mut encoded = Vec::with_capacity(ctx_history_index_format::EVENT_RANGE_ORDER_KEY_LEN);
    if !column.ord_to_bytes(term_ord, &mut encoded)? {
        return Err(IndexError::InvalidStoredDocumentField(
            EVENT_RANGE_ORDER_FAST_FIELD,
        ));
    }
    ctx_history_index_format::EventRangeOrderKey::decode(&encoded)
}

fn validate_materialized_event(
    order: ctx_history_index_format::EventRangeOrderKey,
    event: &EventRecord,
    fast_event_id: Uuid,
) -> Result<()> {
    if event.event_id.as_uuid() != fast_event_id
        || event.event_id.digest() != order.event_identity_digest()
        || event.event_sequence != order.event_sequence()
        || event.occurred_at_unix_ms != order.occurred_at_unix_ms()
    {
        return Err(IndexError::InvalidStoredDocumentField(
            EVENT_RANGE_ORDER_FAST_FIELD,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct RankedAddressCandidate {
    coverage: u8,
    query_terms: u8,
    score: Score,
    order: ctx_history_index_format::EventRangeOrderKey,
    address: DocAddress,
    segment: LexicalSegmentContext,
}

impl PartialEq for RankedAddressCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedAddressCandidate {}

impl PartialOrd for RankedAddressCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedAddressCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.coverage
            .cmp(&other.coverage)
            .then_with(|| self.score.total_cmp(&other.score))
            // Smaller full stable identity is better.
            .then_with(|| {
                compare_identity_ascending(
                    self.order.event_identity_digest(),
                    other.order.event_identity_digest(),
                )
            })
    }
}

fn compare_identity_ascending(left: [u8; 32], right: [u8; 32]) -> Ordering {
    // `Ord` reports the better candidate as greater, so reverse this final
    // comparison while retaining every byte of the stable identity digest.
    right.cmp(&left)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateExamination {
    Rejected,
    Accepted,
    Exhausted,
}

fn empty_lexical_batch(candidate_set_exhaustive: bool) -> LexicalSearchBatch {
    LexicalSearchBatch {
        candidates: Vec::new(),
        complete: true,
        candidate_set_exhaustive,
        exhaustion: None,
        counters: LexicalWorkCounters::default(),
    }
}

fn finish_lexical_batch(
    candidates: Vec<LexicalSearchCandidate>,
    meter: LexicalWorkMeter,
    retained_truncated: bool,
) -> LexicalSearchBatch {
    let (counters, exhaustion) = meter.into_parts();
    let complete = exhaustion.is_none();
    LexicalSearchBatch {
        candidates,
        complete,
        candidate_set_exhaustive: complete && !retained_truncated,
        exhaustion,
        counters,
    }
}

fn complete_compatibility_candidates(
    batch: LexicalSearchBatch,
) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
    let LexicalSearchBatch {
        candidates,
        complete,
        exhaustion,
        ..
    } = batch;
    if !complete {
        return Err(LexicalSearchError::WorkExhausted(exhaustion.expect(
            "an incomplete lexical batch always has an exhaustion reason",
        )));
    }
    Ok(candidates.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_bm25_helper_uses_only_explicit_pinned_statistics() {
        reset_lexical_query_work();

        let doc_frequency = 2_u64;
        let total_num_docs = 10_u64;
        let average_fieldnorm = 5.0_f32;
        let fieldnorm_id = FieldNormReader::fieldnorm_to_id(5);
        let term_frequency = 3_u32;
        let score = explicit_bm25_weight(doc_frequency, total_num_docs, average_fieldnorm)
            .score(fieldnorm_id, term_frequency);

        // Pin Tantivy BM25's k1=1.2, b=0.75 formula independently of the
        // helper result. This is formula use, not dynamic query execution.
        let idf = (1.0_f32
            + ((total_num_docs - doc_frequency) as f32 + 0.5) / (doc_frequency as f32 + 0.5))
            .ln();
        let norm = 1.2_f32
            * (1.0 - 0.75
                + 0.75 * FieldNormReader::id_to_fieldnorm(fieldnorm_id) as f32 / average_fieldnorm);
        let expected = idf * (1.0 + 1.2) * term_frequency as f32 / (term_frequency as f32 + norm);
        assert!((score - expected).abs() < f32::EPSILON * 8.0);
        assert_eq!(lexical_query_constructions(), 0);
        assert_eq!(lexical_query_executions(), 0);
    }

    #[test]
    fn ranking_identity_tiebreak_uses_bytes_beyond_the_uuid_prefix() {
        let mut lower_full_identity = [7_u8; 32];
        let mut higher_full_identity = lower_full_identity;
        lower_full_identity[31] = 1;
        higher_full_identity[31] = 2;

        assert_eq!(
            &lower_full_identity[..16],
            &higher_full_identity[..16],
            "the compact UUID material is intentionally identical"
        );
        assert_eq!(
            compare_identity_ascending(lower_full_identity, higher_full_identity),
            Ordering::Greater,
            "the lexicographically smaller full identity must rank first"
        );
        assert_eq!(
            compare_identity_ascending(higher_full_identity, lower_full_identity),
            Ordering::Less
        );
    }
}
