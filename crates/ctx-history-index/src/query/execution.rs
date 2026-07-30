use super::*;

impl VerifiedIndex {
    /// Enumerates one exact source in strict full `StableEntityId` order.
    ///
    /// The cursor is exclusive and bound to both this immutable generation
    /// and the source's exact descriptor. Only `limit + 1` records are
    /// materialized; the additional record is the terminal lookahead.
    pub fn source_event_page(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
    ) -> Result<SourceEventPage> {
        if !(1..=MAX_SOURCE_EVENT_PAGE_ITEMS).contains(&limit) {
            return Err(IndexError::InvalidSourceEventPageSize {
                requested: limit,
                maximum: MAX_SOURCE_EVENT_PAGE_ITEMS,
            });
        }
        source.validate_contract()?;
        let after = cursor
            .map(|cursor| self.validate_source_event_cursor(source, cursor))
            .transpose()?;
        self.validate_source_event_source(source)?;
        let mut items = self.source_event_records_after(source, after, limit.saturating_add(1))?;
        let terminal = items.len() <= limit;
        if !terminal {
            items.truncate(limit);
        }
        let next_cursor = if terminal {
            None
        } else {
            items.last().map(|event| {
                SourceEventCursor::new(self.generation_id.clone(), source.clone(), event.event_id)
            })
        };
        Ok(SourceEventPage {
            generation_id: self.generation_id.clone(),
            source: source.clone(),
            items,
            next_cursor,
            terminal,
        })
    }

    /// Returns metadata-selected semantic candidates in strict full
    /// `StableEntityId` order.
    ///
    /// The cursor is an exclusive keyset bound to this pinned generation.
    /// At most [`MAX_SEMANTIC_EVENT_PAGE_ITEMS`] records, plus one lookahead
    /// record, are held while collecting a page.
    pub fn semantic_event_page(
        &self,
        cursor: Option<&SemanticEventCursor>,
        limit: usize,
    ) -> Result<SemanticEventPage> {
        if !(1..=MAX_SEMANTIC_EVENT_PAGE_ITEMS).contains(&limit) {
            return Err(IndexError::InvalidSemanticEventPageSize {
                requested: limit,
                maximum: MAX_SEMANTIC_EVENT_PAGE_ITEMS,
            });
        }
        let after = cursor
            .map(|cursor| self.validate_semantic_event_cursor(cursor))
            .transpose()?;
        let eligibility = SemanticEligibility::CURRENT;
        let eligible_total = self.semantic_eligible_event_count()?;
        let mut items =
            self.semantic_event_records_after(after, eligibility, limit.saturating_add(1))?;
        let terminal = items.len() <= limit;
        if !terminal {
            items.truncate(limit);
        }
        let next_cursor = if terminal {
            None
        } else {
            items
                .last()
                .map(|event| SemanticEventCursor::new(self.generation_id.clone(), event.event_id))
        };
        Ok(SemanticEventPage {
            generation_id: self.generation_id.clone(),
            eligibility,
            eligible_total,
            items,
            next_cursor,
            terminal,
        })
    }

    /// Returns the exact total for the current metadata candidate contract.
    ///
    /// The count is computed lazily from this immutable searcher and cached for
    /// the lifetime of the pin.
    pub fn semantic_eligible_event_count(&self) -> Result<u64> {
        if let Some(count) = self.semantic_eligible_event_count.get() {
            return Ok(*count);
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let count = self.count_semantic_eligible_events(fields, SemanticEligibility::CURRENT)?;
        if self.semantic_eligible_event_count.set(count).is_err() {
            return Ok(*self.semantic_eligible_event_count.get().unwrap_or(&count));
        }
        Ok(count)
    }

    /// Searches full policy-selected event text using ordinary analyzed text.
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
    /// Exact-value fields use their canonical stored spelling. Workspace and
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
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut query_terms = BTreeSet::new();
        for natural_text in natural_texts {
            query_terms.extend(self.body_query_terms(natural_text, fields)?);
        }
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let ranking_terms = query_terms.into_iter().collect::<Vec<_>>();
        let mut alternatives = ranking_terms
            .iter()
            .cloned()
            .map(|term| {
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) as Box<dyn Query>
            })
            .collect::<Vec<_>>();
        let body_query: Box<dyn Query> = if alternatives.len() == 1 {
            alternatives.pop().expect("one query term")
        } else {
            Box::new(BooleanQuery::union(alternatives))
        };
        self.collect_event_candidates(body_query, &ranking_terms, filters, limit, fields)
    }

    /// Lists filtered metadata records without requiring a lexical term.
    pub fn list_event_candidates_with_filters(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        self.collect_event_candidates(Box::new(AllQuery), &[], filters, limit, fields)
    }

    fn collect_event_candidates(
        &self,
        body_query: Box<dyn Query>,
        ranking_terms: &[Term],
        filters: &EventSearchFilters,
        limit: usize,
        fields: Fields,
    ) -> Result<Vec<EventSearchCandidate>> {
        validate_event_sort_fast_fields(&self.searcher)?;
        let coverage_by_segment =
            self.query_term_coverage_by_segment(ranking_terms, fields.body_search)?;
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
            let coverage = coverage_by_segment
                .get(&segment_reader.segment_id())
                .cloned()
                .unwrap_or_default();
            move |doc, score| {
                let high = high.as_ref().map_or(0, |column| column.get_val(doc));
                let low = low.as_ref().map_or(0, |column| column.get_val(doc));
                (
                    coverage.get(&doc).copied().unwrap_or(0),
                    score,
                    Reverse((high, low)),
                )
            }
        });
        type ScoredDocAddress = ((u32, Score, Reverse<(u64, u64)>), DocAddress);
        let hits: Vec<ScoredDocAddress> = self.searcher.search(query.as_ref(), &collector)?;
        let mut candidates = Vec::with_capacity(hits.len());
        for ((_, score, _), address) in hits {
            candidates.push(EventSearchCandidate {
                event: self.event_record(address, fields)?,
                score,
            });
        }
        Ok(candidates)
    }

    fn query_term_coverage_by_segment(
        &self,
        terms: &[Term],
        body_field: tantivy::schema::Field,
    ) -> Result<HashMap<SegmentId, Arc<HashMap<DocId, u32>>>> {
        let mut coverage_by_segment = HashMap::new();
        for segment in self.searcher.segment_readers() {
            let mut coverage = HashMap::<DocId, u32>::new();
            if !terms.is_empty() {
                let inverted = segment.inverted_index(body_field)?;
                for term in terms {
                    let Some(mut postings) =
                        inverted.read_postings(term, IndexRecordOption::Basic)?
                    else {
                        continue;
                    };
                    let mut doc = postings.doc();
                    while doc != TERMINATED {
                        if !segment.is_deleted(doc) {
                            let count = coverage.entry(doc).or_default();
                            *count = count.saturating_add(1);
                        }
                        doc = postings.advance();
                    }
                }
            }
            coverage_by_segment.insert(segment.segment_id(), Arc::new(coverage));
        }
        Ok(coverage_by_segment)
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
        let identities = self.custom_source_identity_events(fields)?;
        let terms = identities
            .iter()
            .filter(|(_, provider_key, source_id)| {
                source_identity_values_match(filters, provider_key, source_id)
            })
            .map(|(event_id, _, _)| Term::from_field_text(fields.event_id, &event_id.to_string()))
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return Ok(Some(Box::new(EmptyQuery)));
        }
        Ok(Some(Box::new(TermSetQuery::new(terms))))
    }

    fn custom_source_identity_events(
        &self,
        fields: Fields,
    ) -> Result<&Vec<(Uuid, String, String)>> {
        if self.custom_source_identity_events.get().is_none() {
            let query = TermQuery::new(
                Term::from_field_text(fields.provider, "custom"),
                IndexRecordOption::Basic,
            );
            let identities = self
                .event_records_for_query(&query, fields)?
                .into_iter()
                .filter_map(|event| {
                    let event_id = event.event_id.as_uuid();
                    custom_source_identity(&event).map(|(provider_key, source_id)| {
                        (event_id, provider_key.to_owned(), source_id.to_owned())
                    })
                })
                .collect();
            let _ = self.custom_source_identity_events.set(identities);
        }
        Ok(self
            .custom_source_identity_events
            .get()
            .expect("custom source identity cache was initialized"))
    }

    pub fn event_by_id(&self, event_id: Uuid) -> Result<Option<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.event_id, &event_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut events = self.event_records_for_query(&query, fields)?;
        events.sort_by_key(|event| event.event_id.as_uuid());
        Ok(events.into_iter().next())
    }

    /// Returns at most two UUID-prefix matches, enough to distinguish a unique
    /// lookup from an ambiguous one.
    pub fn events_by_id_prefix(&self, prefix: &str) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = RegexQuery::from_pattern(
            &format!("{}.*", canonical_uuid_prefix(prefix)?),
            fields.event_id,
        )?;
        let mut events = self.event_records_for_query(&query, fields)?;
        events.sort_by_key(|event| event.event_id.as_uuid());
        events.truncate(ID_PREFIX_MATCH_LIMIT);
        Ok(events)
    }

    pub fn session_by_id(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        let events = self.events_for_session(session_id)?;
        Ok(events.first().map(SessionRecord::from))
    }

    /// Returns at most two UUID-prefix matches, enough to distinguish a unique
    /// lookup from an ambiguous one.
    pub fn sessions_by_id_prefix(&self, prefix: &str) -> Result<Vec<SessionRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = RegexQuery::from_pattern(
            &format!("{}.*", canonical_uuid_prefix(prefix)?),
            fields.session_id,
        )?;
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        let mut sessions = BTreeMap::new();
        for event in &events {
            sessions
                .entry(event.session_id.as_uuid())
                .or_insert_with(|| SessionRecord::from(event));
        }
        Ok(sessions.into_values().take(ID_PREFIX_MATCH_LIMIT).collect())
    }

    /// Returns at most two sessions for an exact provider-native session key.
    ///
    /// Two are sufficient for callers to distinguish a unique lookup from an
    /// ambiguous provider key without materializing the full provider history.
    pub fn sessions_by_provider_session_id(
        &self,
        provider_session_id: &str,
        provider: Option<&str>,
    ) -> Result<Vec<SessionRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let provider_session_id =
            validated_filter_text("provider_session_id", provider_session_id)?;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.provider_session_id, provider_session_id),
                IndexRecordOption::Basic,
            )),
        )];
        if let Some(provider) = provider {
            let provider = validated_filter_text("provider", provider)?;
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.provider, provider),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let query = BooleanQuery::new(clauses);
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        let mut sessions = BTreeMap::new();
        for event in &events {
            sessions
                .entry(event.session_id.as_uuid())
                .or_insert_with(|| SessionRecord::from(event));
        }
        Ok(sessions.into_values().take(ID_PREFIX_MATCH_LIMIT).collect())
    }

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        Ok(events)
    }

    fn body_query_terms(&self, natural_text: &str, fields: Fields) -> Result<Vec<Term>> {
        let mut analyzer = self
            .searcher
            .index()
            .tokenizers()
            .get(BODY_ANALYZER)
            .ok_or(IndexError::MissingAnalyzer(BODY_ANALYZER))?;
        let mut stream = analyzer.token_stream(natural_text);
        let mut tokens = BTreeMap::<String, ()>::new();
        while stream.advance() {
            tokens.insert(stream.token().text.clone(), ());
        }
        Ok(tokens
            .into_keys()
            .map(|token| Term::from_field_text(fields.body_search, &token))
            .collect())
    }

    fn event_records_for_query(
        &self,
        query: &dyn Query,
        fields: Fields,
    ) -> Result<Vec<EventRecord>> {
        let addresses = self.searcher.search(query, &DocSetCollector)?;
        let mut events = Vec::with_capacity(addresses.len());
        for address in addresses {
            events.push(self.event_record(address, fields)?);
        }
        Ok(events)
    }

    fn validate_semantic_event_cursor(
        &self,
        cursor: &SemanticEventCursor,
    ) -> Result<StableEntityId> {
        if cursor.generation_id != self.generation_id {
            return Err(IndexError::SemanticEventCursorGenerationMismatch {
                cursor_generation: cursor.generation_id.clone(),
                pinned_generation: self.generation_id.clone(),
            });
        }
        if cursor.eligibility != SemanticEligibility::CURRENT {
            return Err(IndexError::SemanticEventCursorEligibilityMismatch);
        }
        cursor.after.validate_contract()?;
        if cursor.after.entity_kind() != StableEntityKind::Event {
            return Err(IndexError::InvalidSemanticEventCursorIdentity);
        }
        Ok(cursor.after)
    }

    fn validate_source_event_source(&self, source: &SourceKey) -> Result<()> {
        let retained = self
            .manifest
            .sources
            .iter()
            .find(|candidate| candidate.observation().source() == source)
            .ok_or_else(|| {
                IndexError::SourceEventSourceNotRetained(source.identity().to_string())
            })?;
        if !retained.observation().source().exact_descriptor_eq(source) {
            return Err(IndexError::SourceEventSourceDescriptorMismatch(
                source.identity().to_string(),
            ));
        }
        Ok(())
    }

    fn validate_source_event_cursor(
        &self,
        source: &SourceKey,
        cursor: &SourceEventCursor,
    ) -> Result<StableEntityId> {
        if cursor.generation_id != self.generation_id {
            return Err(IndexError::SourceEventCursorGenerationMismatch {
                cursor_generation: cursor.generation_id.clone(),
                pinned_generation: self.generation_id.clone(),
            });
        }
        cursor.source.validate_contract()?;
        if !cursor.source.exact_descriptor_eq(source) {
            return Err(IndexError::SourceEventCursorSourceMismatch);
        }
        cursor.after.validate_contract()?;
        if cursor.after.entity_kind() != StableEntityKind::Event
            || cursor.after.source_digest() != source.identity().digest()
            || cursor.after.source_descriptor_digest() != source.exact_descriptor_digest()
        {
            return Err(IndexError::InvalidSourceEventCursorIdentity);
        }
        Ok(cursor.after)
    }

    fn source_event_records_after(
        &self,
        source: &SourceKey,
        after: Option<StableEntityId>,
        capacity: usize,
    ) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let source_term = Term::from_field_text(fields.source_key, &source_token(source));
        let after_digest = after.map(|identity| hex(&identity.digest()));
        let mut candidates = BinaryHeap::with_capacity(capacity);

        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let source_inverted = segment.inverted_index(fields.source_key)?;
            let Some(source_postings) =
                source_inverted.read_postings(&source_term, IndexRecordOption::Basic)?
            else {
                continue;
            };
            let identity_inverted = segment.inverted_index(fields.event_identity_digest)?;
            let terms = identity_inverted.terms();
            let mut stream = match after_digest.as_deref() {
                Some(digest) => terms.range().gt(digest.as_bytes()).into_stream()?,
                None => terms.stream()?,
            };
            while stream.advance() {
                if candidates.len() == capacity
                    && candidates
                        .peek()
                        .is_some_and(|largest: &EventIdentityCandidate| {
                            stream.key() > largest.digest_term.as_bytes()
                        })
                {
                    break;
                }
                let mut identity_postings = identity_inverted
                    .read_postings_from_terminfo(stream.value(), IndexRecordOption::Basic)?;
                let mut doc_id = identity_postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        let mut source_membership = source_postings.clone();
                        let source_doc = source_membership.doc();
                        let matches_source = source_doc == doc_id
                            || (source_doc < doc_id && source_membership.seek(doc_id) == doc_id);
                        if matches_source {
                            let address = DocAddress::new(segment_ord as u32, doc_id);
                            let event = self.event_record(address, fields)?;
                            let digest_term = hex(&event.event_id.digest());
                            if digest_term.as_bytes() != stream.key()
                                || !event.locator.source().exact_descriptor_eq(source)
                            {
                                return Err(IndexError::InvalidStoredDocumentField(
                                    EVENT_IDENTITY_DIGEST_FIELD,
                                ));
                            }
                            candidates.push(EventIdentityCandidate::new(event, digest_term)?);
                            if candidates.len() > capacity {
                                candidates.pop();
                            }
                        }
                    }
                    doc_id = identity_postings.advance();
                }
            }
        }

        let mut candidates = candidates.into_vec();
        candidates.sort_by_key(|candidate| candidate.identity);
        Ok(candidates
            .into_iter()
            .map(|candidate| candidate.event)
            .collect())
    }

    fn semantic_event_records_after(
        &self,
        after: Option<StableEntityId>,
        eligibility: SemanticEligibility,
        capacity: usize,
    ) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let after_digest = after.map(|identity| hex(&identity.digest()));
        let mut candidates = BinaryHeap::with_capacity(capacity);

        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_identity_digest)?;
            let terms = inverted.terms();
            let mut stream = match after_digest.as_deref() {
                Some(digest) => terms.range().gt(digest.as_bytes()).into_stream()?,
                None => terms.stream()?,
            };
            while stream.advance() {
                if candidates.len() == capacity
                    && candidates
                        .peek()
                        .is_some_and(|largest: &EventIdentityCandidate| {
                            stream.key() > largest.digest_term.as_bytes()
                        })
                {
                    break;
                }
                let mut postings = inverted
                    .read_postings_from_terminfo(stream.value(), IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        let address = DocAddress::new(segment_ord as u32, doc_id);
                        let event = self.event_record(address, fields)?;
                        let digest_term = hex(&event.event_id.digest());
                        if digest_term.as_bytes() != stream.key() {
                            return Err(IndexError::InvalidStoredDocumentField(
                                EVENT_IDENTITY_DIGEST_FIELD,
                            ));
                        }
                        if eligibility.includes(&event) {
                            candidates.push(EventIdentityCandidate::new(event, digest_term)?);
                            if candidates.len() > capacity {
                                candidates.pop();
                            }
                        }
                    }
                    doc_id = postings.advance();
                }
            }
        }

        let mut candidates = candidates.into_vec();
        candidates.sort_by_key(|candidate| candidate.identity);
        Ok(candidates
            .into_iter()
            .map(|candidate| candidate.event)
            .collect())
    }

    fn count_semantic_eligible_events(
        &self,
        fields: Fields,
        eligibility: SemanticEligibility,
    ) -> Result<u64> {
        let message_term = Term::from_field_text(fields.event_type, "message");
        let user_term = Term::from_field_text(fields.role, "user");
        let mut count = 0_u64;

        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let Some(mut messages) = segment
                .inverted_index(fields.event_type)?
                .read_postings(&message_term, IndexRecordOption::Basic)?
            else {
                continue;
            };
            let Some(mut users) = segment
                .inverted_index(fields.role)?
                .read_postings(&user_term, IndexRecordOption::Basic)?
            else {
                continue;
            };
            let mut message_doc = messages.doc();
            let mut user_doc = users.doc();
            while message_doc != TERMINATED && user_doc != TERMINATED {
                if message_doc < user_doc {
                    message_doc = messages.seek(user_doc);
                    continue;
                }
                if user_doc < message_doc {
                    user_doc = users.seek(message_doc);
                    continue;
                }
                let doc_id = message_doc;
                message_doc = messages.advance();
                user_doc = users.advance();
                if segment.is_deleted(doc_id) {
                    continue;
                }
                let event =
                    self.event_record(DocAddress::new(segment_ord as u32, doc_id), fields)?;
                if eligibility.includes(&event) {
                    count = count.checked_add(1).ok_or(IndexError::CountOverflow)?;
                }
            }
        }
        Ok(count)
    }

    fn event_record(&self, address: DocAddress, fields: Fields) -> Result<EventRecord> {
        stored_event_record(&self.searcher, address, fields)
    }
}
