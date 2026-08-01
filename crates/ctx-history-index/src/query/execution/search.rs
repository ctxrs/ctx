use super::*;

impl VerifiedIndex {
    /// Searches full policy-selected event text using ordinary analyzed text.
    ///
    /// A lone full canonical Git object ID first ranks certified typed outcome
    /// producers, then falls back to ordinary lexical matches.
    ///
    /// An analyzed token admits a partial match. Full query-term coverage ranks
    /// ahead of partial coverage, followed by ordinary lexical relevance.
    /// QueryParser operators and field syntax are intentionally not accepted.
    pub fn search_event_candidates(
        &self,
        natural_text: &str,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.search_event_candidates_with_filters(
            natural_text,
            &EventSearchFilters::default(),
            limit,
        )
    }

    /// Searches policy-selected event text with conjunctive metadata filters.
    ///
    /// Exact-value fields use their canonical indexed spelling. Workspace and
    /// touched-file filters use case-insensitive substring matching over
    /// bounded indexed metadata.
    pub fn search_event_candidates_with_filters(
        &self,
        natural_text: &str,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.search_event_candidates_any_with_filters(&[natural_text], filters, limit)
    }

    /// Searches OR-composed natural-text alternatives with shared filters.
    ///
    /// Matching any unique analyzed token admits the event. Results rank by
    /// query-term coverage before ordinary lexical relevance. This is the
    /// indexed implementation of the CLI's repeated `--term` contract.
    pub fn search_event_candidates_any_with_filters(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        validate_lexical_result_limit(limit)?;
        LEXICAL_QUERY_LIMITS.validate_texts(natural_texts.iter().copied())?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let ranking_terms = self.body_query_terms(natural_texts, fields)?;
        if ranking_terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut candidates = Vec::with_capacity(limit);
        let mut seen = BTreeSet::new();
        if let Some(object_id) = canonical_git_object_id_query(natural_texts) {
            let exact_query = Box::new(TermQuery::new(
                Term::from_field_text(fields.repository_produced_object_id, object_id),
                IndexRecordOption::Basic,
            ));
            for candidate in self.collect_event_candidates(exact_query, filters, limit, fields)? {
                if seen.insert(candidate.event.event_id.as_uuid()) {
                    candidates.push(candidate);
                }
            }
            if candidates.len() == limit {
                return Ok(candidates);
            }
        }
        if ranking_terms.len() == 1 {
            #[cfg(test)]
            record_lexical_query_construction();
            let body_query = Box::new(TermQuery::new(
                ranking_terms[0].clone(),
                IndexRecordOption::WithFreqs,
            ));
            let lexical_limit = limit
                .checked_add(seen.len())
                .ok_or(IndexError::CountOverflow)?;
            for candidate in
                self.collect_event_candidates(body_query, filters, lexical_limit, fields)?
            {
                if seen.insert(candidate.event.event_id.as_uuid()) {
                    candidates.push(candidate);
                    if candidates.len() == limit {
                        break;
                    }
                }
            }
            return Ok(candidates);
        }

        // Rank by exact query-term coverage without constructing one
        // `HashMap<DocId, coverage>` entry for every matching document. That
        // approach made memory and CPU proportional to the corpus frequency of
        // common terms even when the caller requested only a handful of
        // results. Tantivy's minimum-should-match query gives us the same
        // ordering as bounded tiers: all terms first, then N-1, down to one.
        for minimum_required in (1..=ranking_terms.len()).rev() {
            #[cfg(test)]
            record_lexical_query_construction();
            let alternatives = ranking_terms
                .iter()
                .cloned()
                .map(|term| {
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) as Box<dyn Query>
                })
                .collect();
            let body_query = Box::new(BooleanQuery::union_with_minimum_required_clauses(
                alternatives,
                minimum_required,
            ));
            // Lower-coverage tiers also contain every prior higher-coverage
            // hit. Bounded over-collection by exactly the number already seen
            // guarantees enough unique lookahead without a total-count scan.
            let tier_limit = limit
                .checked_add(seen.len())
                .ok_or(IndexError::CountOverflow)?;
            for candidate in
                self.collect_event_candidates(body_query, filters, tier_limit, fields)?
            {
                if seen.insert(candidate.event.event_id.as_uuid()) {
                    candidates.push(candidate);
                    if candidates.len() == limit {
                        return Ok(candidates);
                    }
                }
            }
        }
        Ok(candidates)
    }

    /// Lists filtered metadata records without requiring a lexical term.
    pub fn list_event_candidates_with_filters(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        validate_lexical_result_limit(limit)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        self.collect_event_candidates(Box::new(AllQuery), filters, limit, fields)
    }

    fn collect_event_candidates(
        &self,
        body_query: Box<dyn Query>,
        filters: &EventSearchFilters,
        limit: usize,
        fields: Fields,
    ) -> Result<Vec<EventSearchCandidate>> {
        validate_event_sort_fast_fields(&self.searcher)?;
        let source_identity_query = self.source_identity_query(filters, fields)?;
        let query = filtered_event_query(body_query, source_identity_query, filters, fields)?;
        let collector = TopDocs::with_limit(limit).tweak_score(move |segment_reader| {
            // These readers were checked above. The fallbacks keep this
            // infallible collector closure panic-free if Tantivy ever changes
            // when it resolves a validated fast field.
            let high = segment_reader
                .fast_fields()
                .u64(EVENT_ID_HIGH_FIELD)
                .ok()
                .map(|column| column.first_or_default_col(0));
            let low = segment_reader
                .fast_fields()
                .u64(EVENT_ID_LOW_FIELD)
                .ok()
                .map(|column| column.first_or_default_col(0));
            move |doc, score| {
                let high = high.as_ref().map_or(0, |column| column.get_val(doc));
                let low = low.as_ref().map_or(0, |column| column.get_val(doc));
                (score, Reverse((high, low)))
            }
        });
        type ScoredDocAddress = ((Score, Reverse<(u64, u64)>), DocAddress);
        #[cfg(test)]
        record_lexical_query_execution();
        let hits: Vec<ScoredDocAddress> = self.searcher.search(query.as_ref(), &collector)?;
        let mut candidates = Vec::with_capacity(hits.len());
        for ((score, _), address) in hits {
            candidates.push(EventSearchCandidate {
                event: self.event_record(address, fields)?,
                score,
            });
        }
        Ok(candidates)
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

fn canonical_git_object_id_query<'a>(natural_texts: &'a [&str]) -> Option<&'a str> {
    let [natural_text] = natural_texts else {
        return None;
    };
    matches!(natural_text.len(), 40 | 64)
        .then_some(*natural_text)
        .filter(|value| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}
