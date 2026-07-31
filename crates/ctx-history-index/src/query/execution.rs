use super::*;

impl VerifiedIndex {
    /// Enumerates one exact source in strict full `StableEntityId` order.
    ///
    /// The cursor is exclusive and bound to both this immutable generation
    /// and the source's exact descriptor. Candidate selection seeks directly
    /// into each segment's exact-source order range before complete records
    /// are decoded under the default item and byte bounds.
    pub fn source_event_page(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
    ) -> Result<SourceEventPage> {
        self.core_source_event_page(source, cursor, limit)
            .map(Into::into)
    }

    /// Enumerates complete Core records for one exact source in strict full
    /// `StableEntityId` order without per-record follow-up lookups.
    pub fn core_source_event_page(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
    ) -> Result<CoreSourceEventPage> {
        self.core_source_event_page_with_budget(
            source,
            cursor,
            limit,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
    }

    /// Enumerates complete Core records under item, encoded-Core, and decoded
    /// content byte bounds. A valid first record is always admitted so even a
    /// maximum-size Core record makes cursor progress as a singleton page.
    pub fn core_source_event_page_with_budget(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
        budget: CoreEventPageBudget,
    ) -> Result<CoreSourceEventPage> {
        if !(1..=MAX_SOURCE_EVENT_PAGE_ITEMS).contains(&limit) {
            return Err(IndexError::InvalidSourceEventPageSize {
                requested: limit,
                maximum: MAX_SOURCE_EVENT_PAGE_ITEMS,
            });
        }
        validate_core_event_page_budget(budget)?;
        source.validate_contract()?;
        let after = cursor
            .map(|cursor| self.validate_source_event_cursor(source, cursor))
            .transpose()?;
        self.validate_source_event_source(source)?;
        let candidates =
            self.source_event_addresses_after(source, after, limit.saturating_add(1))?;
        let candidate_count = candidates.len();
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        let mut consumed = 0_usize;
        for candidate in candidates {
            if items.len() == limit {
                break;
            }
            let order = candidate
                .source_order
                .ok_or(IndexError::InvalidStoredDocumentField(
                    SOURCE_EVENT_ORDER_FIELD,
                ))?;
            if !items.is_empty()
                && !core_event_page_budget_admits(
                    budget,
                    encoded_core_bytes,
                    content_bytes,
                    order.encoded_core_bytes(),
                    order.content_bytes(),
                )
            {
                break;
            }
            let (record, actual_encoded_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, candidate.address, fields)?;
            let actual_order = SourceEventOrderKey::for_core_record(
                &record.core_record,
                actual_encoded_core_bytes,
            )?;
            if actual_order != order
                || record.event_id.digest() != candidate.identity_digest
                || !record.core_record.source.exact_descriptor_eq(source)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SOURCE_EVENT_ORDER_FIELD,
                ));
            }
            encoded_core_bytes = encoded_core_bytes
                .checked_add(actual_order.encoded_core_bytes())
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(actual_order.content_bytes())
                .ok_or(IndexError::CountOverflow)?;
            items.push(record);
            consumed = consumed.checked_add(1).ok_or(IndexError::CountOverflow)?;
        }
        let terminal = consumed == candidate_count;
        let next_cursor = if terminal {
            None
        } else {
            items.last().map(|event| {
                SourceEventCursor::new(self.generation_id.clone(), source.clone(), event.event_id)
            })
        };
        Ok(CoreSourceEventPage {
            generation_id: self.generation_id.clone(),
            source: source.clone(),
            items,
            encoded_core_bytes,
            content_bytes,
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
        self.core_semantic_event_page(cursor, limit).map(Into::into)
    }

    /// Returns complete Core semantic candidates in strict full
    /// `StableEntityId` order without per-record follow-up lookups.
    pub fn core_semantic_event_page(
        &self,
        cursor: Option<&SemanticEventCursor>,
        limit: usize,
    ) -> Result<CoreSemanticEventPage> {
        self.core_semantic_event_page_with_budget(cursor, limit, DEFAULT_CORE_EVENT_PAGE_BUDGET)
    }

    /// Returns complete semantic candidates after address-only selection and
    /// final-order decoding under retained Core byte bounds. Unlike exact-source
    /// pages, semantic candidates do not have a source-prefix size lookup, so at
    /// most one decoded non-fitting record is transiently considered.
    pub fn core_semantic_event_page_with_budget(
        &self,
        cursor: Option<&SemanticEventCursor>,
        limit: usize,
        budget: CoreEventPageBudget,
    ) -> Result<CoreSemanticEventPage> {
        if !(1..=MAX_SEMANTIC_EVENT_PAGE_ITEMS).contains(&limit) {
            return Err(IndexError::InvalidSemanticEventPageSize {
                requested: limit,
                maximum: MAX_SEMANTIC_EVENT_PAGE_ITEMS,
            });
        }
        validate_core_event_page_budget(budget)?;
        let after = cursor
            .map(|cursor| self.validate_semantic_event_cursor(cursor))
            .transpose()?;
        let eligibility = SemanticEligibility::CURRENT;
        let eligible_total = self.semantic_eligible_event_count()?;
        let candidates =
            self.semantic_event_addresses_after(after, eligibility, limit.saturating_add(1))?;
        let candidate_count = candidates.len();
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        let mut consumed = 0_usize;
        for candidate in candidates {
            if items.len() == limit {
                break;
            }
            let (record, record_encoded_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, candidate.address, fields)?;
            let record_content_bytes = core_content_bytes(&record.core_record.content)?;
            if record.event_id.digest() != candidate.identity_digest
                || !eligibility.includes(&record.event)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    EVENT_IDENTITY_DIGEST_FIELD,
                ));
            }
            if !items.is_empty()
                && !core_event_page_budget_admits(
                    budget,
                    encoded_core_bytes,
                    content_bytes,
                    record_encoded_core_bytes,
                    record_content_bytes,
                )
            {
                break;
            }
            encoded_core_bytes = encoded_core_bytes
                .checked_add(record_encoded_core_bytes)
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(record_content_bytes)
                .ok_or(IndexError::CountOverflow)?;
            items.push(record);
            consumed = consumed.checked_add(1).ok_or(IndexError::CountOverflow)?;
        }
        let terminal = consumed == candidate_count;
        let next_cursor = if terminal {
            None
        } else {
            items
                .last()
                .map(|event| SemanticEventCursor::new(self.generation_id.clone(), event.event_id))
        };
        Ok(CoreSemanticEventPage {
            generation_id: self.generation_id.clone(),
            eligibility,
            eligible_total,
            items,
            encoded_core_bytes,
            content_bytes,
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
        if ranking_terms.len() == 1 {
            let body_query = Box::new(TermQuery::new(
                ranking_terms[0].clone(),
                IndexRecordOption::WithFreqs,
            ));
            return self.collect_event_candidates(body_query, filters, limit, fields);
        }

        // Rank by exact query-term coverage without constructing one
        // `HashMap<DocId, coverage>` entry for every matching document. That
        // approach made memory and CPU proportional to the corpus frequency of
        // common terms even when the caller requested only a handful of
        // results. Tantivy's minimum-should-match query gives us the same
        // ordering as bounded tiers: all terms first, then N-1, down to one.
        let mut candidates = Vec::with_capacity(limit);
        let mut seen = BTreeSet::new();
        for minimum_required in (1..=ranking_terms.len()).rev() {
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
            let tier_limit = limit.saturating_add(seen.len());
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

    /// Returns one verified event together with its complete stored Core data.
    pub fn core_event_by_id(&self, event_id: Uuid) -> Result<Option<CoreEventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.event_id, &event_id.to_string()),
            IndexRecordOption::Basic,
        );
        let address = self
            .searcher
            .search(&query, &DocSetCollector)?
            .into_iter()
            .next();
        address
            .map(|address| stored_core_event_record(&self.searcher, address, fields))
            .transpose()
    }

    /// Returns a complete, requested-order Core mapping when the batch is
    /// within the caller's count and stored-Core byte budgets and every event
    /// is present.
    ///
    /// Duplicate requested IDs are rejected before Tantivy is queried. Missing
    /// events or a byte-budget overrun decline the whole batch instead of
    /// exposing a partial mapping. While decoding, previously retained Core
    /// records stay within `maximum_stored_core_bytes`; at most the one record
    /// currently being considered can exceed that retained budget.
    pub fn core_events_by_ids_if_bounded(
        &self,
        event_ids: &[Uuid],
        maximum_events: usize,
        maximum_stored_core_bytes: usize,
    ) -> Result<Option<Vec<CoreEventRecord>>> {
        Ok(self
            .core_event_batch_by_ids(
                event_ids,
                maximum_events,
                maximum_stored_core_bytes,
                usize::MAX,
                false,
            )?
            .map(|batch| batch.items))
    }

    /// Returns a complete requested-order Core batch under both encoded and
    /// decoded-content byte ceilings. This composes with
    /// [`Self::session_event_coordinates`] so presentation can select a small
    /// session prefix/window before any bodies are retained.
    pub fn core_events_by_ids_with_budget(
        &self,
        event_ids: &[Uuid],
        maximum_events: usize,
        budget: CoreEventPageBudget,
    ) -> Result<Option<CoreEventBatch>> {
        validate_core_event_page_budget(budget)?;
        self.core_event_batch_by_ids(
            event_ids,
            maximum_events,
            budget.maximum_encoded_core_bytes,
            budget.maximum_content_bytes,
            true,
        )
    }

    fn core_event_batch_by_ids(
        &self,
        event_ids: &[Uuid],
        maximum_events: usize,
        maximum_stored_core_bytes: usize,
        maximum_content_bytes: usize,
        admit_oversized_singleton: bool,
    ) -> Result<Option<CoreEventBatch>> {
        if event_ids.len() > maximum_events {
            return Ok(None);
        }
        if event_ids.is_empty() {
            return Ok(Some(CoreEventBatch {
                items: Vec::new(),
                encoded_core_bytes: 0,
                content_bytes: 0,
            }));
        }

        let fields = fields_from_schema(self.searcher.schema())?;
        let mut requested = BTreeSet::new();
        for event_id in event_ids {
            if !requested.insert(*event_id) {
                return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
            }
        }
        let query = TermSetQuery::new(
            requested
                .iter()
                .map(|event_id| Term::from_field_text(fields.event_id, &event_id.to_string()))
                .collect::<Vec<_>>(),
        );
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let mut records = BTreeMap::new();
        let mut stored_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        for address in addresses {
            let (record, record_stored_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, address, fields)?;
            let record_content_bytes = core_content_bytes(&record.core_record.content)?;
            let Some(next_stored_core_bytes) =
                stored_core_bytes.checked_add(record_stored_core_bytes)
            else {
                return Ok(None);
            };
            let Some(next_content_bytes) = content_bytes.checked_add(record_content_bytes) else {
                return Ok(None);
            };
            if (next_stored_core_bytes > maximum_stored_core_bytes
                || next_content_bytes > maximum_content_bytes)
                && !(admit_oversized_singleton && event_ids.len() == 1)
            {
                return Ok(None);
            }
            stored_core_bytes = next_stored_core_bytes;
            content_bytes = next_content_bytes;
            let event_id = record.event_id.as_uuid();
            if !requested.contains(&event_id) {
                return Err(IndexError::InvalidStoredDocumentField("event_id"));
            }
            if records.insert(event_id, record).is_some() {
                return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
            }
        }
        if records.len() != requested.len() {
            return Ok(None);
        }

        let mut ordered = Vec::with_capacity(event_ids.len());
        for event_id in event_ids {
            let Some(record) = records.remove(event_id) else {
                return Ok(None);
            };
            ordered.push(record);
        }
        Ok(Some(CoreEventBatch {
            items: ordered,
            encoded_core_bytes: stored_core_bytes,
            content_bytes,
        }))
    }

    /// Returns the complete stored Core data for one compact event ID.
    pub fn core_record_by_id(&self, event_id: Uuid) -> Result<Option<CoreRecord>> {
        Ok(self
            .core_event_by_id(event_id)?
            .map(|record| record.core_record))
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
        Ok(self
            .core_events_for_session(session_id)?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }

    /// Returns every event in one session with complete stored Core data.
    pub fn core_events_for_session(&self, session_id: Uuid) -> Result<Vec<CoreEventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut records = self.core_event_records_for_query(&query, fields)?;
        sort_core_events_for_session(&mut records);
        Ok(records)
    }

    /// Returns one session's deterministic presentation coordinates without
    /// retaining complete Core bodies. Callers can select a prefix or centered
    /// window from this small metadata and pass those IDs to
    /// [`Self::core_events_by_ids_with_budget`].
    pub fn session_event_coordinates(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<SessionEventCoordinate>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let segments = self.searcher.segment_readers();
        let mut coordinates = Vec::with_capacity(addresses.len());
        for address in addresses {
            let segment = segments
                .get(address.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
            let event_id_high = segment
                .fast_fields()
                .u64(EVENT_ID_HIGH_FIELD)?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField("event_id"))?;
            let event_id_low = segment
                .fast_fields()
                .u64(EVENT_ID_LOW_FIELD)?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField("event_id"))?;
            let event_sequence = segment
                .fast_fields()
                .u64("event_sequence")?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField("event_sequence"))?;
            let occurred_at_unix_ms = segment
                .fast_fields()
                .i64("occurred_at_unix_ms")?
                .first(address.doc_id);
            let document: TantivyDocument = self.searcher.doc(address)?;
            let event_id = stored_identity(
                &document,
                fields.event_identity,
                fields.event_id,
                fields.event_identity_digest,
                StableEntityKind::Event,
                "event_identity",
            )?;
            let stored_session_id = stored_identity(
                &document,
                fields.session_identity,
                fields.session_id,
                fields.session_identity_digest,
                StableEntityKind::Session,
                "session_identity",
            )?;
            let compact_event_id =
                Uuid::from_u128((u128::from(event_id_high) << 64) | u128::from(event_id_low));
            if event_id.as_uuid() != compact_event_id
                || stored_session_id.as_uuid() != session_id
                || required_u64(&document, fields.event_sequence, "event_sequence")?
                    != event_sequence
                || optional_i64(&document, fields.occurred_at_unix_ms)? != occurred_at_unix_ms
            {
                return Err(IndexError::InvalidStoredDocumentField("session_id"));
            }
            coordinates.push(SessionEventCoordinate {
                event_id: compact_event_id,
                event_sequence,
                occurred_at_unix_ms,
                event_type: required_string(&document, fields.event_type, "event_type")?,
                role: optional_string(&document, fields.role)?,
            });
        }
        coordinates.sort_by(|left, right| {
            left.event_sequence
                .cmp(&right.event_sequence)
                .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        if let Some(pair) = coordinates
            .windows(2)
            .find(|pair| pair[0].event_id == pair[1].event_id)
        {
            return Err(IndexError::DuplicateEventIdentity(
                pair[1].event_id.to_string(),
            ));
        }
        Ok(coordinates)
    }

    /// Returns one session only when its event cardinality is within a caller
    /// budget.
    ///
    /// The count pass reads postings without constructing stored event
    /// records. This lets best-effort consumers decline pathological sessions
    /// before allocating metadata for every event.
    pub fn events_for_session_if_bounded(
        &self,
        session_id: Uuid,
        maximum_events: usize,
    ) -> Result<Option<Vec<EventRecord>>> {
        Ok(self
            .core_events_for_session_if_bounded(session_id, maximum_events)?
            .map(|records| records.into_iter().map(|record| record.event).collect()))
    }

    /// Returns complete Core events only when session cardinality is within a
    /// caller budget, without materializing documents for a declined session.
    pub fn core_events_for_session_if_bounded(
        &self,
        session_id: Uuid,
        maximum_events: usize,
    ) -> Result<Option<Vec<CoreEventRecord>>> {
        Ok(self
            .core_events_for_session_within_budget(session_id, maximum_events, usize::MAX)?
            .map(|(records, _)| records))
    }

    /// Returns one complete session and its exact stored-Core byte count only
    /// when both caller budgets admit it. A declined session never exposes a
    /// partial event list, and retained decoded records remain within the byte
    /// budget plus at most the one record currently being considered.
    pub fn core_events_for_session_within_budget(
        &self,
        session_id: Uuid,
        maximum_events: usize,
        maximum_stored_core_bytes: usize,
    ) -> Result<Option<(Vec<CoreEventRecord>, usize)>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let count = self.searcher.search(&query, &Count)?;
        if count > maximum_events {
            return Ok(None);
        }
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let mut records = Vec::with_capacity(addresses.len());
        let mut stored_core_bytes = 0_usize;
        for address in addresses {
            let (record, record_stored_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, address, fields)?;
            if record.session_id.as_uuid() != session_id {
                return Err(IndexError::InvalidStoredDocumentField("session_id"));
            }
            let Some(next_stored_core_bytes) =
                stored_core_bytes.checked_add(record_stored_core_bytes)
            else {
                return Ok(None);
            };
            if next_stored_core_bytes > maximum_stored_core_bytes {
                return Ok(None);
            }
            stored_core_bytes = next_stored_core_bytes;
            records.push(record);
        }
        sort_core_events_for_session(&mut records);
        Ok(Some((records, stored_core_bytes)))
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

    fn core_event_records_for_query(
        &self,
        query: &dyn Query,
        fields: Fields,
    ) -> Result<Vec<CoreEventRecord>> {
        let addresses = self.searcher.search(query, &DocSetCollector)?;
        let mut records = Vec::with_capacity(addresses.len());
        for address in addresses {
            records.push(self.core_event_record(address, fields)?);
        }
        Ok(records)
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

    fn source_event_addresses_after(
        &self,
        source: &SourceKey,
        after: Option<StableEntityId>,
        capacity: usize,
    ) -> Result<Vec<EventAddressCandidate>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let source_prefix = SourceEventOrderKey::source_prefix(source);
        let range_end = SourceEventOrderKey::source_range_end(source);
        let after_bound = after
            .map(|identity| SourceEventOrderKey::source_after_bound(source, identity.digest()));
        let candidate_capacity = capacity
            .checked_mul(self.searcher.segment_readers().len())
            .ok_or(IndexError::CountOverflow)?;
        let mut candidates = Vec::with_capacity(candidate_capacity);

        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.source_event_order)?;
            let terms = inverted.terms();
            let mut stream = match after_bound.as_deref() {
                Some(bound) => terms.range().gt(bound).lt(&range_end).into_stream()?,
                None => terms
                    .range()
                    .ge(source_prefix)
                    .lt(&range_end)
                    .into_stream()?,
            };
            let mut segment_candidates = 0_usize;
            while segment_candidates < capacity && stream.advance() {
                #[cfg(test)]
                SOURCE_EVENT_ORDER_TERM_VISITS
                    .set(SOURCE_EVENT_ORDER_TERM_VISITS.get().saturating_add(1));
                let source_order = SourceEventOrderKey::decode_for_source(source, stream.key())?;
                let mut postings = inverted
                    .read_postings_from_terminfo(stream.value(), IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED && segment_candidates < capacity {
                    if !segment.is_deleted(doc_id) {
                        candidates.push(EventAddressCandidate {
                            identity_digest: source_order.event_digest(),
                            address: DocAddress::new(segment_ord as u32, doc_id),
                            source_order: Some(source_order),
                        });
                        segment_candidates = segment_candidates
                            .checked_add(1)
                            .ok_or(IndexError::CountOverflow)?;
                    }
                    doc_id = postings.advance();
                }
            }
        }

        candidates.sort_by_key(|candidate| candidate.identity_digest);
        for pair in candidates.windows(2) {
            if pair[0].identity_digest == pair[1].identity_digest {
                return Err(IndexError::InvalidStoredDocumentField(
                    SOURCE_EVENT_ORDER_FIELD,
                ));
            }
        }
        Ok(candidates)
    }

    fn semantic_event_addresses_after(
        &self,
        after: Option<StableEntityId>,
        eligibility: SemanticEligibility,
        capacity: usize,
    ) -> Result<Vec<EventAddressCandidate>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let after_digest = after.map(|identity| hex(&identity.digest()));
        let candidate_capacity = capacity
            .checked_mul(self.searcher.segment_readers().len())
            .ok_or(IndexError::CountOverflow)?;
        let mut candidates = Vec::with_capacity(candidate_capacity);
        let message_term = Term::from_field_text(fields.event_type, "message");
        let user_term = Term::from_field_text(fields.role, "user");

        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_identity_digest)?;
            let Some(message_postings) = segment
                .inverted_index(fields.event_type)?
                .read_postings(&message_term, IndexRecordOption::Basic)?
            else {
                continue;
            };
            let Some(user_postings) = segment
                .inverted_index(fields.role)?
                .read_postings(&user_term, IndexRecordOption::Basic)?
            else {
                continue;
            };
            let terms = inverted.terms();
            let mut stream = match after_digest.as_deref() {
                Some(digest) => terms.range().gt(digest.as_bytes()).into_stream()?,
                None => terms.stream()?,
            };
            let mut segment_candidates = 0_usize;
            while segment_candidates < capacity && stream.advance() {
                let identity_digest = decode_identity_digest_term(stream.key())?;
                let mut postings = inverted
                    .read_postings_from_terminfo(stream.value(), IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED && segment_candidates < capacity {
                    if !segment.is_deleted(doc_id) {
                        let mut messages = message_postings.clone();
                        let mut users = user_postings.clone();
                        let message_doc = messages.doc();
                        let user_doc = users.doc();
                        let is_message = message_doc == doc_id
                            || (message_doc < doc_id && messages.seek(doc_id) == doc_id);
                        let is_user = user_doc == doc_id
                            || (user_doc < doc_id && users.seek(doc_id) == doc_id);
                        if is_message && is_user {
                            debug_assert_eq!(eligibility, SemanticEligibility::CURRENT);
                            candidates.push(EventAddressCandidate {
                                identity_digest,
                                address: DocAddress::new(segment_ord as u32, doc_id),
                                source_order: None,
                            });
                            segment_candidates = segment_candidates
                                .checked_add(1)
                                .ok_or(IndexError::CountOverflow)?;
                        }
                    }
                    doc_id = postings.advance();
                }
            }
        }

        candidates.sort_by_key(|candidate| candidate.identity_digest);
        if candidates
            .windows(2)
            .any(|pair| pair[0].identity_digest == pair[1].identity_digest)
        {
            return Err(IndexError::InvalidStoredDocumentField(
                EVENT_IDENTITY_DIGEST_FIELD,
            ));
        }
        Ok(candidates)
    }

    fn count_semantic_eligible_events(
        &self,
        fields: Fields,
        eligibility: SemanticEligibility,
    ) -> Result<u64> {
        let message_term = Term::from_field_text(fields.event_type, "message");
        let user_term = Term::from_field_text(fields.role, "user");
        let mut count = 0_u64;

        for segment in self.searcher.segment_readers() {
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
                debug_assert_eq!(eligibility, SemanticEligibility::CURRENT);
                count = count.checked_add(1).ok_or(IndexError::CountOverflow)?;
            }
        }
        Ok(count)
    }

    fn event_record(&self, address: DocAddress, fields: Fields) -> Result<EventRecord> {
        stored_event_record(&self.searcher, address, fields)
    }

    fn core_event_record(&self, address: DocAddress, fields: Fields) -> Result<CoreEventRecord> {
        stored_core_event_record(&self.searcher, address, fields)
    }
}

fn validate_core_event_page_budget(budget: CoreEventPageBudget) -> Result<()> {
    for (field, requested, maximum) in [
        (
            "encoded Core",
            budget.maximum_encoded_core_bytes,
            MAX_ENCODED_CORE_RECORD_BYTES,
        ),
        (
            "content",
            budget.maximum_content_bytes,
            MAX_CORE_CONTENT_BYTES,
        ),
    ] {
        if requested == 0 || requested > maximum {
            return Err(IndexError::InvalidCoreEventPageByteLimit {
                field,
                requested,
                maximum,
            });
        }
    }
    Ok(())
}

fn core_event_page_budget_admits(
    budget: CoreEventPageBudget,
    retained_encoded_core_bytes: usize,
    retained_content_bytes: usize,
    candidate_encoded_core_bytes: usize,
    candidate_content_bytes: usize,
) -> bool {
    retained_encoded_core_bytes
        .checked_add(candidate_encoded_core_bytes)
        .is_some_and(|total| total <= budget.maximum_encoded_core_bytes)
        && retained_content_bytes
            .checked_add(candidate_content_bytes)
            .is_some_and(|total| total <= budget.maximum_content_bytes)
}

fn decode_identity_digest_term(term: &[u8]) -> Result<[u8; 32]> {
    if term.len() != 64 {
        return Err(IndexError::InvalidStoredDocumentField(
            EVENT_IDENTITY_DIGEST_FIELD,
        ));
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in term.chunks_exact(2).enumerate() {
        let Some(high) = decode_hex_digit(pair[0]) else {
            return Err(IndexError::InvalidStoredDocumentField(
                EVENT_IDENTITY_DIGEST_FIELD,
            ));
        };
        let Some(low) = decode_hex_digit(pair[1]) else {
            return Err(IndexError::InvalidStoredDocumentField(
                EVENT_IDENTITY_DIGEST_FIELD,
            ));
        };
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn decode_hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}
