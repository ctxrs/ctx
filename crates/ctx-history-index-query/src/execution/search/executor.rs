use super::*;

#[derive(Debug, Clone)]
struct RankedCandidateRef {
    coverage: u8,
    query_terms: u8,
    score: Score,
    order: ctx_history_index_format::EventRangeOrderKey,
    address: DocAddress,
    stable_segment_index: usize,
}

impl PartialEq for RankedCandidateRef {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedCandidateRef {}

impl PartialOrd for RankedCandidateRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedCandidateRef {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_primary_rank(self.coverage, self.score, other)
            .then_with(|| {
                compare_identity_ascending(
                    self.order.event_identity_digest(),
                    other.order.event_identity_digest(),
                )
            })
            // Exact event digests are unique in a verified generation. Keep
            // the stable address as a defensive total-order fallback.
            .then_with(|| compare_stable_address_ascending(self, other))
    }
}

fn compare_primary_rank(coverage: u8, score: Score, other: &RankedCandidateRef) -> Ordering {
    coverage
        .cmp(&other.coverage)
        .then_with(|| score.total_cmp(&other.score))
}

fn compare_stable_address_ascending(
    left: &RankedCandidateRef,
    right: &RankedCandidateRef,
) -> Ordering {
    compare_stable_address_parts(
        (left.stable_segment_index, left.address.doc_id),
        (right.stable_segment_index, right.address.doc_id),
    )
}

fn compare_stable_address_parts(left: (usize, DocId), right: (usize, DocId)) -> Ordering {
    right.cmp(&left)
}

fn compare_identity_ascending(left: [u8; 32], right: [u8; 32]) -> Ordering {
    // `Ord` reports the better candidate as greater, so reverse this final
    // comparison while retaining every byte of the stable identity digest.
    right.cmp(&left)
}

fn exact_digest_prefix(digest: [u8; 32]) -> u64 {
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], 0, 0,
    ]) >> 16
}

fn compare_identity_prefix_ascending(candidate_prefix: u64, worst_digest: [u8; 32]) -> Ordering {
    exact_digest_prefix(worst_digest).cmp(&candidate_prefix)
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

    /// Executes one diagnosed Search or List pass against a caller-compiled
    /// filter.
    ///
    /// Segments are visited by ascending immutable Tantivy segment ID. Within
    /// each segment, body postings intersect required positive groups in
    /// ascending document ID. A full heap prunes documents whose best possible
    /// rank cannot enter it. Posting traversal remains independently bounded;
    /// an incomplete result describes a deterministic, work-limited pass.
    pub fn execute_lexical(
        &self,
        execution: LexicalExecution<'_>,
    ) -> DiagnosedLexicalSearchBatchResult {
        #[cfg(any(test, feature = "test-support"))]
        let _failure_injection_reset = lexical_candidate_materialization_failure_reset();
        let mut receipt = EventCandidateQueryReceipt::default();
        let result = self.execute_lexical_inner(execution, &mut receipt);
        match result {
            Ok(batch) => Ok(ObservedLexicalSearchBatch { batch, receipt }),
            Err(error) => Err(Box::new(EventCandidateQueryFailure { error, receipt })),
        }
    }

    fn execute_lexical_inner(
        &self,
        execution: LexicalExecution<'_>,
        receipt: &mut EventCandidateQueryReceipt,
    ) -> Result<LexicalSearchBatch> {
        let LexicalExecution {
            mode,
            filter: compiled_filter,
            limit,
            budget,
        } = execution;
        validate_lexical_result_limit(limit)?;
        if let LexicalMode::Search(natural_texts) = mode {
            LEXICAL_QUERY_LIMITS.validate_texts(natural_texts.iter().copied())?;
        }
        if limit == 0 {
            return Ok(empty_lexical_batch(false));
        }

        let fields = fields_from_schema(self.searcher.schema())?;
        let (body_terms, analyzed_tokens) = match mode {
            LexicalMode::Search(natural_texts) => {
                let analyzed = self.analyze_manual_body_terms(natural_texts, fields)?;
                if analyzed.0.is_empty() {
                    return Ok(empty_lexical_batch(true));
                }
                analyzed
            }
            LexicalMode::List => (Vec::new(), 0),
        };

        receipt.record_query_execution()?;
        #[cfg(any(test, feature = "test-support"))]
        record_lexical_query_execution();
        let mut meter = LexicalWorkMeter::new(budget);
        meter.record_analyzed_tokens(analyzed_tokens)?;
        let Some(filter_plan) =
            compile_lexical_filter_adapter(compiled_filter, fields, &mut meter)?
        else {
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

        let mut retained: BinaryHeap<Reverse<RankedCandidateRef>> = BinaryHeap::new();
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
                compiled_filter.filters().content_scope,
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
                LexicalMode::Search(_) => {
                    while let Some(doc) = next_body_doc(&prepared.body_postings) {
                        // Once an omitted match is known, skip whole posting
                        // intervals whose rank bounds cannot beat the heap.
                        // Before then, inspect pruning witnesses individually
                        // so filtered-out rows cannot imply truncation.
                        if retained_truncated {
                            if let Some(Reverse(worst)) = retained.peek() {
                                let next = coverage_pivot(&prepared.body_postings, worst.coverage);
                                if next == TERMINATED {
                                    break;
                                }
                                if next != doc {
                                    if !seek_body_postings(&mut prepared, next, &mut meter) {
                                        break 'segments;
                                    }
                                    continue;
                                }
                                let Some((end, coverage, score, prefix)) =
                                    super::blocks::block_rank_bound(
                                        &mut prepared,
                                        &body_weights,
                                        &mut meter,
                                    )?
                                else {
                                    break 'segments;
                                };
                                let rank = compare_primary_rank(coverage, score, worst);
                                if rank == Ordering::Less
                                    || (rank == Ordering::Equal
                                        && compare_identity_prefix_ascending(
                                            prefix,
                                            worst.order.event_identity_digest(),
                                        ) == Ordering::Less)
                                {
                                    if !seek_body_postings(
                                        &mut prepared,
                                        end.saturating_add(1),
                                        &mut meter,
                                    ) {
                                        break 'segments;
                                    }
                                    continue;
                                }
                            }
                        }
                        let deleted = reader.is_deleted(doc);
                        if !deleted {
                            let Some(next) = prepared.filters.next_required_doc(
                                doc,
                                &mut meter,
                                &prepared.context,
                            ) else {
                                break 'segments;
                            };
                            if next == TERMINATED {
                                break;
                            }
                            if next != doc {
                                if !seek_body_postings(&mut prepared, next, &mut meter) {
                                    break 'segments;
                                }
                                continue;
                            }
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
                        let pruned = if !deleted && retained.len() >= limit {
                            let Some(pruned) = prune_by_rank_bound(
                                &mut prepared,
                                doc,
                                &term_frequencies[..body_terms.len()],
                                &body_weights,
                                &retained,
                                &filter_plan,
                                &mut retained_truncated,
                                &mut meter,
                            )?
                            else {
                                break 'segments;
                            };
                            pruned
                        } else {
                            false
                        };
                        if !pruned
                            && !meter.charge(
                                LexicalWorkCounter::CandidateDocs,
                                1,
                                Some(&prepared.context),
                                Some(doc),
                            )
                        {
                            break 'segments;
                        }
                        if !deleted && !pruned {
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
                LexicalMode::List => {
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
                        let Some(next) =
                            prepared
                                .filters
                                .next_required_doc(doc, &mut meter, &prepared.context)
                        else {
                            break 'segments;
                        };
                        if next != doc {
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

        let mut ranked = retained
            .into_iter()
            .map(|Reverse(candidate)| candidate)
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.cmp(left));
        receipt.record_collector_hits(ranked.len())?;
        let mut candidates = Vec::with_capacity(ranked.len());
        for candidate in ranked {
            let reader = self
                .searcher
                .segment_readers()
                .get(candidate.address.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ))?;
            let segment = LexicalSegmentContext {
                stable_segment_index: candidate.stable_segment_index,
                segment_id: reader.segment_id().to_string(),
                segment_ord: candidate.address.segment_ord,
            };
            let encoded_bytes = u64::try_from(candidate.order.encoded_core_bytes())
                .map_err(|_| IndexError::CountOverflow)?;
            if !meter.charge_pair(
                (LexicalWorkCounter::FinalMaterializations, 1),
                (LexicalWorkCounter::FinalMaterializationBytes, encoded_bytes),
                Some(&segment),
                Some(candidate.address.doc_id),
            ) {
                break;
            }
            #[cfg(any(test, feature = "test-support"))]
            if lexical_candidate_materialization_should_fail() {
                return Err(IndexError::InvalidStoredDocumentField(
                    "test_lexical_candidate_materialization_failure",
                ));
            }
            let (event, _) = ranked_event_ref_at_address_with_order(
                &self.searcher,
                candidate.address,
                fields,
                candidate.order,
            )?;
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
        prepared: &mut PreparedSegment,
        doc: DocId,
        term_frequencies: &[u32],
        body_weights: &[Bm25Weight],
        query_term_count: u8,
        filter_plan: &LexicalFilterAdapter,
        limit: usize,
        retained: &mut BinaryHeap<Reverse<RankedCandidateRef>>,
        retained_truncated: &mut bool,
        meter: &mut LexicalWorkMeter,
    ) -> Result<CandidateExamination> {
        let Some(matches_exact_filters) =
            candidate_matches_filters(prepared, doc, filter_plan, meter)?
        else {
            return Ok(CandidateExamination::Exhausted);
        };
        if !matches_exact_filters {
            return Ok(CandidateExamination::Rejected);
        }

        let coverage = term_coverage(term_frequencies);
        let mut score = body_score(prepared, doc, term_frequencies, body_weights)?;
        let Some(class_weight) = prepared.classes.weight(doc, meter, &prepared.context) else {
            return Ok(CandidateExamination::Exhausted);
        };
        score *= class_weight;

        let address = DocAddress::new(prepared.context.segment_ord, doc);
        let heap_was_full = retained.len() >= limit;
        let boundary_event_id_high = if heap_was_full {
            // Whether the new candidate wins or loses, one admissible match is
            // discarded once the fixed retained heap is full.
            *retained_truncated = true;
            let worst = retained.peek().map(|Reverse(worst)| worst).ok_or(
                IndexError::InvalidStoredDocumentField(EVENT_RANGE_ORDER_FAST_FIELD),
            )?;
            match compare_primary_rank(coverage, score, worst) {
                Ordering::Less => return Ok(CandidateExamination::Accepted),
                Ordering::Greater => None,
                Ordering::Equal => {
                    let event_id_high = prepared.candidate_fields.event_id_high(doc)?;
                    let candidate_prefix = event_id_high >> 16;
                    match compare_identity_prefix_ascending(
                        candidate_prefix,
                        worst.order.event_identity_digest(),
                    ) {
                        Ordering::Less => return Ok(CandidateExamination::Accepted),
                        Ordering::Equal | Ordering::Greater => Some(event_id_high),
                    }
                }
            }
        } else {
            if !meter.charge(
                LexicalWorkCounter::RetainedCandidates,
                1,
                Some(&prepared.context),
                Some(doc),
            ) {
                return Ok(CandidateExamination::Exhausted);
            }
            None
        };

        let order = prepared.candidate_fields.event_range_order(doc)?;
        if prepared.candidate_fields.occurred_at_unix_ms(doc)? != order.occurred_at_unix_ms()
            || !prepared
                .candidate_fields
                .exact_identity_matches_compact_fields(
                    doc,
                    order.event_identity_digest(),
                    boundary_event_id_high,
                )?
        {
            return Err(IndexError::InvalidStoredDocumentField(
                EVENT_RANGE_ORDER_FAST_FIELD,
            ));
        }
        let candidate = RankedCandidateRef {
            coverage,
            query_terms: query_term_count,
            score,
            order,
            address,
            stable_segment_index: prepared.context.stable_segment_index,
        };
        if heap_was_full {
            if retained
                .peek()
                .is_some_and(|Reverse(worst)| candidate > *worst)
            {
                retained.pop();
                retained.push(Reverse(candidate));
            }
        } else {
            retained.push(Reverse(candidate));
        }
        Ok(CandidateExamination::Accepted)
    }
}

fn seek_body_postings(
    prepared: &mut PreparedSegment,
    next: DocId,
    meter: &mut LexicalWorkMeter,
) -> bool {
    for postings in prepared.body_postings.iter_mut().flatten() {
        if postings.doc() < next {
            if !meter.charge(
                LexicalWorkCounter::BodyPostingAdvances,
                1,
                Some(&prepared.context),
                Some(next),
            ) {
                return false;
            }
            postings.seek(next);
        }
    }
    true
}

/// Before the kth cursor, fewer than k terms can match any document. This
/// is a coverage bound, independent of BM25 and of segment-local statistics.
fn coverage_pivot(postings: &[Option<BodyPostings>], minimum: u8) -> DocId {
    if minimum <= 1 {
        return next_body_doc(postings).unwrap_or(TERMINATED);
    }
    if usize::from(minimum) == postings.len() {
        return postings
            .iter()
            .map(|posting| posting.as_ref().map_or(TERMINATED, |posting| posting.doc()))
            .max()
            .unwrap_or(TERMINATED);
    }
    let mut docs = [TERMINATED; 32];
    for (slot, posting) in docs.iter_mut().zip(postings) {
        if let Some(posting) = posting {
            *slot = posting.doc();
        }
    }
    let index = usize::from(minimum.max(1)) - 1;
    *docs[..postings.len()].select_nth_unstable(index).1
}

fn candidate_matches_filters(
    prepared: &mut PreparedSegment,
    doc: DocId,
    filter_plan: &LexicalFilterAdapter,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<bool>> {
    let Some(matches) = prepared
        .filters
        .accepts_remaining(doc, meter, &prepared.context)?
    else {
        return Ok(None);
    };
    if !matches {
        return Ok(Some(false));
    }
    let occurred_at = prepared.candidate_fields.occurred_at_unix_ms(doc)?;
    Ok(Some(filter_plan.since_unix_ms.is_none_or(|since| {
        occurred_at.is_some_and(|occurred_at| occurred_at >= since)
    })))
}

fn term_coverage(term_frequencies: &[u32]) -> u8 {
    // Query admission bounds this slice to 32 terms.
    term_frequencies
        .iter()
        .filter(|frequency| **frequency > 0)
        .count() as u8
}

fn body_score(
    prepared: &PreparedSegment,
    doc: DocId,
    term_frequencies: &[u32],
    body_weights: &[Bm25Weight],
) -> Result<Score> {
    if term_frequencies.is_empty() {
        return Ok(0.0);
    }
    let fieldnorm_id = prepared
        .fieldnorms
        .as_ref()
        .ok_or(IndexError::InvalidStoredDocumentField("body_search"))?
        .fieldnorm_id(doc);
    Ok(body_weights
        .iter()
        .zip(term_frequencies)
        .map(|(weight, frequency)| weight.score(fieldnorm_id, *frequency))
        .sum())
}

/// The maximum class weight is 1.0 in every scope, so the unweighted body
/// score bounds the final BM25 score. Preserve coverage-first ordering and
/// never prune equal scores without a strictly worse exact identity prefix.
/// Each probe follows a body cursor movement (or a segment's initial doc),
/// bounding probes by body_posting_advances + segments, with at most 32 terms.
#[allow(clippy::too_many_arguments)]
fn prune_by_rank_bound(
    prepared: &mut PreparedSegment,
    doc: DocId,
    term_frequencies: &[u32],
    body_weights: &[Bm25Weight],
    retained: &BinaryHeap<Reverse<RankedCandidateRef>>,
    filter_plan: &LexicalFilterAdapter,
    retained_truncated: &mut bool,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<bool>> {
    let Some(Reverse(worst)) = retained.peek() else {
        return Ok(Some(false));
    };
    let coverage = term_coverage(term_frequencies);
    let rank = coverage.cmp(&worst.coverage);
    let rank = if rank == Ordering::Equal {
        let mut score_bound = body_score(prepared, doc, term_frequencies, body_weights)?;
        if score_bound >= worst.score {
            // Tighten only when needed. Class posting work is independently
            // precharged, including for a document ultimately pruned here.
            let Some(weight) = prepared.classes.weight(doc, meter, &prepared.context) else {
                return Ok(None);
            };
            score_bound *= weight;
        }
        score_bound.total_cmp(&worst.score)
    } else {
        rank
    };
    let pruned = match rank {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => {
            let prefix = prepared.candidate_fields.event_id_high(doc)? >> 16;
            compare_identity_prefix_ascending(prefix, worst.order.event_identity_digest())
                == Ordering::Less
        }
    };
    if pruned && !*retained_truncated {
        // A rank-pruned document might fail a negative, substring or time
        // filter. Prove that at least one admissible match was discarded before
        // reporting result-limit truncation; never confuse pruning with work
        // exhaustion or report a filtered-out document as an omitted match.
        let Some(matches) = candidate_matches_filters(prepared, doc, filter_plan, meter)? else {
            return Ok(None);
        };
        *retained_truncated = matches;
    }
    Ok(Some(pruned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_identity_tiebreak_uses_bytes_after_the_uuid() {
        let mut lower = [7_u8; 32];
        let mut higher = lower;
        lower[31] = 1;
        higher[31] = 2;

        assert_eq!(&lower[..16], &higher[..16]);
        assert_eq!(compare_identity_ascending(lower, higher), Ordering::Greater);
        assert_eq!(compare_identity_ascending(higher, lower), Ordering::Less);
    }

    #[test]
    fn exact_prefix_excludes_uuid_overwrites_and_later_bytes() {
        let mut first = [7_u8; 32];
        let mut second = first;
        first[6] = 0x0f;
        second[6] = 0xf0;
        second[31] ^= 1;

        assert_eq!(exact_digest_prefix(first), exact_digest_prefix(second));
    }

    #[test]
    fn six_byte_prefix_is_rejection_only() {
        let worst = [0x40; 32];

        assert_eq!(
            compare_identity_prefix_ascending(0x5050_5050_5050, worst),
            Ordering::Less,
            "a larger prefix is exactly worse and may be rejected"
        );
        assert_eq!(
            compare_identity_prefix_ascending(0x3030_3030_3030, worst),
            Ordering::Greater,
            "a smaller prefix may win but still needs its exact order key"
        );
        assert_eq!(
            compare_identity_prefix_ascending(0x4040_4040_4040, worst),
            Ordering::Equal,
            "an equal prefix cannot decide the full identity order"
        );
    }

    #[test]
    fn exact_identity_tie_uses_the_stable_address() {
        assert_eq!(
            compare_stable_address_parts((2, 4), (3, 0)),
            Ordering::Greater
        );
        assert_eq!(
            compare_stable_address_parts((2, 4), (2, 5)),
            Ordering::Greater
        );
        assert_eq!(compare_stable_address_parts((3, 0), (2, 4)), Ordering::Less);
    }
}
