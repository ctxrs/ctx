use super::*;

#[derive(Debug, Clone)]
struct RankedAddressCandidate {
    coverage: u8,
    query_terms: u8,
    score: Score,
    compact_identity: CompactEventIdentity,
    occurred_at_unix_ms: Option<i64>,
    address: DocAddress,
    segment: LexicalSegmentContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompactEventIdentity {
    pub(super) high: u64,
    pub(super) low: u64,
}

impl CompactEventIdentity {
    fn from_digest(digest: [u8; 32]) -> Self {
        let compact = CompactIdentity { digest }.as_uuid().as_u128();
        Self {
            high: (compact >> 64) as u64,
            low: compact as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_identity_tiebreak_uses_bytes_beyond_the_uuid_prefix() {
        let mut lower_full_identity = [7_u8; 32];
        let mut higher_full_identity = lower_full_identity;
        lower_full_identity[31] = 1;
        higher_full_identity[31] = 2;

        assert_eq!(&lower_full_identity[..16], &higher_full_identity[..16]);
        assert_eq!(
            CompactEventIdentity::from_digest(lower_full_identity),
            CompactEventIdentity::from_digest(higher_full_identity),
            "the cheap heap deliberately cannot distinguish this collision"
        );
        assert_eq!(
            compare_identity_ascending(lower_full_identity, higher_full_identity),
            Ordering::Greater,
            "finalists must return to full stable-identity order"
        );
        assert_eq!(
            compare_identity_ascending(higher_full_identity, lower_full_identity),
            Ordering::Less
        );
    }

    #[test]
    fn compact_identity_tiebreak_is_ascending_and_uses_both_uuid_halves() {
        let lower_high = CompactEventIdentity { high: 1, low: 9 };
        let higher_high = CompactEventIdentity { high: 2, low: 0 };
        let lower_low = CompactEventIdentity { high: 7, low: 1 };
        let higher_low = CompactEventIdentity { high: 7, low: 2 };

        assert_eq!(
            compare_compact_identity_ascending(lower_high, higher_high),
            Ordering::Greater
        );
        assert_eq!(
            compare_compact_identity_ascending(lower_low, higher_low),
            Ordering::Greater
        );
    }

    #[test]
    fn compact_identity_tie_uses_the_stable_address_in_the_heap() {
        let candidate = |stable_segment_index, segment_ord, doc_id| RankedAddressCandidate {
            coverage: 1,
            query_terms: 1,
            score: 1.0,
            compact_identity: CompactEventIdentity { high: 7, low: 9 },
            occurred_at_unix_ms: None,
            address: DocAddress::new(segment_ord, doc_id),
            segment: LexicalSegmentContext {
                stable_segment_index,
                segment_id: format!("segment-{stable_segment_index}"),
                segment_ord,
            },
        };
        let earlier = candidate(2, 9, 4);
        let later_segment = candidate(3, 1, 0);
        let later_doc = candidate(2, 9, 5);

        assert_eq!(earlier.cmp(&later_segment), Ordering::Greater);
        assert_eq!(earlier.cmp(&later_doc), Ordering::Greater);
        assert_eq!(later_segment.cmp(&earlier), Ordering::Less);
    }
}

#[derive(Debug, Clone)]
struct FinalAddressCandidate {
    ranked: RankedAddressCandidate,
    order: ctx_history_index_format::EventRangeOrderKey,
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
            // The scalar UUID fields are the existing compact stable identity.
            // This avoids resolving a byte fast field in the loop. UUID
            // normalization fixes six digest bits, so this is an approximation
            // of full-digest order only for heap-boundary ties.
            .then_with(|| {
                compare_compact_identity_ascending(self.compact_identity, other.compact_identity)
            })
            // A compact-UUID tie still has an import-stable heap winner.
            // Finalists are restored to full-digest order after their
            // EventRangeOrderKeys are loaded.
            .then_with(|| compare_stable_address_ascending(self, other))
    }
}

fn compare_final_candidates(
    left: &FinalAddressCandidate,
    right: &FinalAddressCandidate,
) -> Ordering {
    left.ranked
        .coverage
        .cmp(&right.ranked.coverage)
        .then_with(|| left.ranked.score.total_cmp(&right.ranked.score))
        .then_with(|| {
            compare_identity_ascending(
                left.order.event_identity_digest(),
                right.order.event_identity_digest(),
            )
        })
        .then_with(|| compare_stable_address_ascending(&left.ranked, &right.ranked))
}

fn compare_compact_identity_ascending(
    left: CompactEventIdentity,
    right: CompactEventIdentity,
) -> Ordering {
    // `Ord` reports the better candidate as greater, so reverse the ascending
    // stable identity comparison.
    (right.high, right.low).cmp(&(left.high, left.low))
}

fn compare_stable_address_ascending(
    left: &RankedAddressCandidate,
    right: &RankedAddressCandidate,
) -> Ordering {
    (right.segment.stable_segment_index, right.address.doc_id)
        .cmp(&(left.segment.stable_segment_index, left.address.doc_id))
}

fn compare_identity_ascending(left: [u8; 32], right: [u8; 32]) -> Ordering {
    // `Ord` reports the better candidate as greater, so reverse this final
    // comparison while retaining every byte of the stable identity digest.
    right.cmp(&left)
}

impl VerifiedIndex {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn reset_manual_lexical_io_observability_for_test(&self) {
        MANUAL_INVERTED_INDEX_ACQUISITIONS.set(0);
        MANUAL_POSTING_READS.set(0);
        MANUAL_LIVE_POSTINGS.set(0);
        MANUAL_MAXIMUM_LIVE_POSTINGS.set(0);
        MANUAL_EVENT_RANGE_ORDER_DECODES.set(0);
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn manual_event_range_order_decodes_for_test(&self) -> usize {
        MANUAL_EVENT_RANGE_ORDER_DECODES.get()
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

        let ranked = retained
            .into_iter()
            .map(|Reverse(candidate)| candidate)
            .collect::<Vec<_>>();
        let mut finalists = Vec::with_capacity(ranked.len());
        for candidate in ranked {
            let reader = self
                .searcher
                .segment_readers()
                .get(candidate.address.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ))?;
            let order = event_range_order(reader, candidate.address.doc_id)?;
            if candidate.compact_identity
                != CompactEventIdentity::from_digest(order.event_identity_digest())
                || candidate.occurred_at_unix_ms != order.occurred_at_unix_ms()
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ));
            }
            finalists.push(FinalAddressCandidate {
                ranked: candidate,
                order,
            });
        }
        finalists.sort_by(|left, right| compare_final_candidates(right, left));
        receipt.record_collector_hits(finalists.len())?;
        let mut candidates = Vec::with_capacity(finalists.len());
        for candidate in finalists {
            let encoded_bytes = u64::try_from(candidate.order.encoded_core_bytes())
                .map_err(|_| IndexError::CountOverflow)?;
            if !meter.charge_pair(
                (LexicalWorkCounter::FinalMaterializations, 1),
                (LexicalWorkCounter::FinalMaterializationBytes, encoded_bytes),
                Some(&candidate.ranked.segment),
                Some(candidate.ranked.address.doc_id),
            ) {
                break;
            }
            let fast = core_event_fast_preflight(&self.searcher, candidate.ranked.address)?;
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
                stored_event_record_with_size(&self.searcher, candidate.ranked.address, fields)?;
            receipt.record_decoded(encoded_core_bytes)?;
            validate_materialized_event(candidate.order, &event, fast.0)?;
            candidates.push(LexicalSearchCandidate {
                event,
                score: candidate.ranked.score,
                coverage: LexicalTermCoverage {
                    matched_terms: candidate.ranked.coverage,
                    query_terms: candidate.ranked.query_terms,
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

        let occurred_at_unix_ms = prepared.candidate_fields.occurred_at_unix_ms(doc)?;
        if filter_plan
            .since_unix_ms
            .is_some_and(|since| occurred_at_unix_ms.is_none_or(|occurred_at| occurred_at < since))
        {
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
        let compact_identity = prepared.candidate_fields.compact_identity(doc)?;
        let candidate = RankedAddressCandidate {
            coverage,
            query_terms: query_term_count,
            score,
            compact_identity,
            occurred_at_unix_ms,
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
