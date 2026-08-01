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
        let candidates =
            self.source_event_addresses_after(source, after, limit.saturating_add(1))?;
        let candidate_count = candidates.len();
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        for candidate in candidates.into_iter().take(limit) {
            let record = stored_event_record(&self.searcher, candidate.address, fields)?;
            if record.event_id.digest() != candidate.identity_digest
                || !record.source.exact_descriptor_eq(source)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SOURCE_EVENT_ORDER_FIELD,
                ));
            }
            items.push(record);
        }
        let terminal = items.len() == candidate_count;
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
        let candidates =
            self.semantic_event_addresses_after(after, eligibility, limit.saturating_add(1))?;
        let candidate_count = candidates.len();
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        for candidate in candidates.into_iter().take(limit) {
            let record = stored_event_record(&self.searcher, candidate.address, fields)?;
            if record.event_id.digest() != candidate.identity_digest
                || !eligibility.includes(&record)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    EVENT_IDENTITY_DIGEST_FIELD,
                ));
            }
            items.push(record);
        }
        let terminal = items.len() == candidate_count;
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

    pub fn event_by_id(&self, event_id: Uuid) -> Result<Option<EventRecord>> {
        Ok(self
            .events_by_ids_if_bounded(&[event_id], 1)?
            .and_then(|mut events| events.pop()))
    }

    /// Returns one verified event together with its complete stored Core data.
    pub fn core_event_by_id(&self, event_id: Uuid) -> Result<Option<CoreEventRecord>> {
        Ok(self
            .core_events_by_ids_if_bounded(&[event_id], 1, usize::MAX)?
            .and_then(|mut events| events.pop()))
    }

    /// Streams forward from one semantic user anchor until the next user and
    /// returns the latest nonempty assistant text in that turn.
    ///
    /// Session coordinates are sought directly in fixed-size term pages. Tool
    /// records remain metadata-only, assistant Core bodies are decoded one at
    /// a time, and no session-wide collector or retained session cache is used.
    pub fn semantic_lite_turn_assistant(
        &self,
        anchor: &CoreEventRecord,
        page_items: usize,
        pairing_budget: CoreEventPageBudget,
    ) -> Result<Option<(String, i64)>> {
        if !(1..=MAX_SEMANTIC_PAIRING_PAGE_ITEMS).contains(&page_items) {
            return Err(IndexError::InvalidSessionEventCoordinateLimit {
                requested: page_items,
                maximum: MAX_SEMANTIC_PAIRING_PAGE_ITEMS,
            });
        }
        validate_core_event_page_budget(pairing_budget)?;
        if !SemanticEligibility::CURRENT.includes(&anchor.event)
            || anchor.event_id != anchor.core_record.event_id
            || anchor.session_id != anchor.core_record.session_id
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }

        let session_id = anchor.session_id;
        let mut after = SessionEventOrderKey::for_core_record(&anchor.core_record)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let anchor_query = TermQuery::new(
            Term::from_field_bytes(fields.session_event_order, after.as_bytes()),
            IndexRecordOption::Basic,
        );
        if self.searcher.search(&anchor_query, &Count)? != 1 {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }

        let segments = self.searcher.segment_readers();
        let range_end = SessionEventOrderKey::session_range_end(session_id)?;
        let inverted_indexes = segments
            .iter()
            .map(|segment| segment.inverted_index(fields.session_event_order))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let streams = inverted_indexes
            .iter()
            .map(|inverted| {
                inverted
                    .terms()
                    .range()
                    .gt(after.as_bytes())
                    .lt(&range_end)
                    .into_stream()
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut merged = TermMerger::new(streams);

        let mut latest_assistant = None;
        loop {
            let candidates = session_event_address_page(
                session_id,
                page_items,
                &mut merged,
                &inverted_indexes,
                segments,
            )?;
            if candidates.is_empty() {
                return Ok(latest_assistant);
            }

            for candidate in candidates {
                let event = stored_event_record(&self.searcher, candidate.address, fields)?;
                if candidate.order <= after
                    || event.session_id != session_id
                    || event.event_id.as_uuid() != candidate.order.event_id()
                    || event.event_sequence != candidate.order.event_sequence()
                    || event.occurred_at_unix_ms != candidate.order.occurred_at_unix_ms()
                {
                    return Err(IndexError::InvalidStoredDocumentField(
                        SESSION_EVENT_ORDER_FIELD,
                    ));
                }
                after = candidate.order;
                if event.event_type == "message" && event.role.as_deref() == Some("user") {
                    return Ok(latest_assistant);
                }
                if event.event_type != "message" || event.role.as_deref() != Some("assistant") {
                    continue;
                }

                let Some(batch) = self.core_events_by_ids_with_strict_budget(
                    &[event.event_id.as_uuid()],
                    1,
                    pairing_budget,
                )?
                else {
                    return Ok(None);
                };
                let assistant = batch.items.into_iter().next().ok_or(
                    IndexError::InvalidStoredDocumentField(SESSION_EVENT_ORDER_FIELD),
                )?;
                if assistant.session_id != session_id {
                    return Err(IndexError::InvalidStoredDocumentField(
                        SESSION_EVENT_ORDER_FIELD,
                    ));
                }
                let text = assistant.core_record.content.meaningful_text().trim();
                if !text.is_empty() {
                    latest_assistant = Some((
                        text.to_owned(),
                        assistant.occurred_at_unix_ms.unwrap_or_default(),
                    ));
                }
            }
        }
    }

    /// Returns a complete requested-order body-free metadata mapping when the
    /// caller's count bound admits it and every compact event ID is present.
    pub fn events_by_ids_if_bounded(
        &self,
        event_ids: &[Uuid],
        maximum_events: usize,
    ) -> Result<Option<Vec<EventRecord>>> {
        if event_ids.len() > maximum_events {
            return Ok(None);
        }
        if event_ids.is_empty() {
            return Ok(Some(Vec::new()));
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
        for address in addresses {
            let record = stored_event_record(&self.searcher, address, fields)?;
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
        Ok(Some(ordered))
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
    /// decoded-content byte ceilings. This composes with the bounded
    /// [`Self::session_event_coordinate_prefix`] and
    /// [`Self::session_event_coordinate_window`] selectors so presentation
    /// never retains all session coordinates before Core decode.
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

    /// Returns a complete requested-order Core batch only when every record
    /// fits the aggregate byte ceilings. Unlike paged presentation reads, an
    /// oversized singleton is declined instead of being admitted for progress.
    pub fn core_events_by_ids_with_strict_budget(
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
            false,
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
            if next_stored_core_bytes > maximum_stored_core_bytes
                || next_content_bytes > maximum_content_bytes
            {
                if !(admit_oversized_singleton && event_ids.len() == 1) {
                    return Ok(None);
                }
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
        validate_event_sort_fast_fields(&self.searcher)?;
        let collector = TopDocs::with_limit(ID_PREFIX_MATCH_LIMIT).tweak_score(|segment_reader| {
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
            move |doc, _score| {
                Reverse((
                    high.as_ref().map_or(0, |column| column.get_val(doc)),
                    low.as_ref().map_or(0, |column| column.get_val(doc)),
                ))
            }
        });
        type PrefixHit = (Reverse<(u64, u64)>, DocAddress);
        let hits: Vec<PrefixHit> = self.searcher.search(&query, &collector)?;
        let mut events = hits
            .into_iter()
            .map(|(_, address)| stored_event_record(&self.searcher, address, fields))
            .collect::<Result<Vec<_>>>()?;
        events.sort_by_key(|event| event.event_id.as_uuid());
        Ok(events)
    }

    pub fn session_by_id(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        self.session_record_by_id(session_id)
    }

    /// Returns at most two UUID-prefix matches, enough to distinguish a unique
    /// lookup from an ambiguous one.
    pub fn sessions_by_id_prefix(&self, prefix: &str) -> Result<Vec<SessionRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = RegexQuery::from_pattern(
            &format!("{}.*", canonical_uuid_prefix(prefix)?),
            fields.session_id,
        )?;
        self.session_records_for_ambiguity_query(&query)
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
        self.session_records_for_ambiguity_query(&query)
    }

    fn session_records_for_ambiguity_query(&self, query: &dyn Query) -> Result<Vec<SessionRecord>> {
        let session_ids = self
            .searcher
            .search(query, &SessionIdCollector::new(ID_PREFIX_MATCH_LIMIT))?;
        let mut sessions = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let Some(session) = self.session_record_by_id(session_id)? else {
                return Err(IndexError::InvalidStoredDocumentField("session_id"));
            };
            sessions.push(session);
        }
        Ok(sessions)
    }

    fn session_record_by_id(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        let Some(coordinate) = self
            .session_event_coordinate_prefix(session_id, 1)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let event = self
            .event_by_id(coordinate.event_id)?
            .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
        if event.session_id.as_uuid() != session_id {
            return Err(IndexError::InvalidStoredDocumentField("session_id"));
        }
        Ok(Some(SessionRecord::from(&event)))
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

    /// Returns every deterministic coordinate for one session without stored
    /// Core bodies. Presentation callers must use the bounded prefix/window
    /// selectors instead; this complete enumeration is for bounded maintenance
    /// contexts that already constrain session cardinality.
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
            let compact_event_id =
                Uuid::from_u128((u128::from(event_id_high) << 64) | u128::from(event_id_low));
            coordinates.push(SessionEventCoordinate {
                event_id: compact_event_id,
                event_sequence,
                occurred_at_unix_ms,
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

    /// Returns the first `limit` deterministic coordinates for one session
    /// without decoding stored Core records or retaining the complete session.
    pub fn session_event_coordinate_prefix(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> Result<Vec<SessionEventCoordinate>> {
        if !(1..=MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS).contains(&limit) {
            return Err(IndexError::InvalidSessionEventCoordinateLimit {
                requested: limit,
                maximum: MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS,
            });
        }
        validate_session_event_coordinate_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let collector = TopDocs::with_limit(limit).tweak_score(move |segment_reader| {
            let score = session_event_coordinate_score(segment_reader);
            move |doc, original_score| Reverse(score(doc, original_score))
        });
        type CoordinateHit = (Reverse<SessionEventCoordinateSortKey>, DocAddress);
        let hits: Vec<CoordinateHit> = self.searcher.search(&query, &collector)?;
        let coordinates = hits
            .into_iter()
            .map(|(Reverse(sort_key), _)| SessionEventCoordinate::from_sort_key(sort_key))
            .collect::<Vec<_>>();
        validate_session_event_coordinates(&coordinates)?;
        Ok(coordinates)
    }

    /// Returns a deterministic body-free window centered on one exact event.
    /// At most 101 coordinates are retained regardless of session cardinality.
    pub fn session_event_coordinate_window(
        &self,
        session_id: Uuid,
        selected_event_id: Uuid,
        before: usize,
        after: usize,
    ) -> Result<Option<Vec<SessionEventCoordinate>>> {
        let requested = before
            .checked_add(after)
            .and_then(|neighbors| neighbors.checked_add(1))
            .unwrap_or(usize::MAX);
        if !(1..=MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS).contains(&requested) {
            return Err(IndexError::InvalidSessionEventCoordinateLimit {
                requested,
                maximum: MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
            });
        }
        validate_session_event_coordinate_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let session_term = || {
            TermQuery::new(
                Term::from_field_text(fields.session_id, &session_id.to_string()),
                IndexRecordOption::Basic,
            )
        };
        let selected_query = BooleanQuery::new(vec![
            (Occur::Must, Box::new(session_term())),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.event_id, &selected_event_id.to_string()),
                    IndexRecordOption::Basic,
                )),
            ),
        ]);
        let selected_collector = TopDocs::with_limit(2).tweak_score(session_event_coordinate_score);
        type SelectedCoordinateHit = (SessionEventCoordinateSortKey, DocAddress);
        let selected_hits: Vec<SelectedCoordinateHit> =
            self.searcher.search(&selected_query, &selected_collector)?;
        let selected_sort_key = match selected_hits.as_slice() {
            [] => return Ok(None),
            [(sort_key, _)] => *sort_key,
            _ => {
                return Err(IndexError::DuplicateEventIdentity(
                    selected_event_id.to_string(),
                ));
            }
        };
        let selected = SessionEventCoordinate::from_sort_key(selected_sort_key);
        if selected.event_id != selected_event_id {
            return Err(IndexError::InvalidStoredDocumentField("event_id"));
        }

        let mut preceding = if before == 0 {
            Vec::new()
        } else {
            let collector = TopDocs::with_limit(before).tweak_score(move |segment_reader| {
                let score = session_event_coordinate_score(segment_reader);
                move |doc, original_score| {
                    let sort_key = score(doc, original_score);
                    (sort_key < selected_sort_key, sort_key)
                }
            });
            type PrecedingHit = ((bool, SessionEventCoordinateSortKey), DocAddress);
            let hits: Vec<PrecedingHit> = self.searcher.search(&session_term(), &collector)?;
            let mut coordinates = hits
                .into_iter()
                .filter_map(|((is_preceding, sort_key), _)| {
                    is_preceding.then(|| SessionEventCoordinate::from_sort_key(sort_key))
                })
                .collect::<Vec<_>>();
            coordinates.reverse();
            coordinates
        };
        let following = if after == 0 {
            Vec::new()
        } else {
            let collector = TopDocs::with_limit(after).tweak_score(move |segment_reader| {
                let score = session_event_coordinate_score(segment_reader);
                move |doc, original_score| {
                    let sort_key = score(doc, original_score);
                    (sort_key > selected_sort_key, Reverse(sort_key))
                }
            });
            type FollowingHit = ((bool, Reverse<SessionEventCoordinateSortKey>), DocAddress);
            let hits: Vec<FollowingHit> = self.searcher.search(&session_term(), &collector)?;
            hits.into_iter()
                .filter_map(|((is_following, Reverse(sort_key)), _)| {
                    is_following.then(|| SessionEventCoordinate::from_sort_key(sort_key))
                })
                .collect::<Vec<_>>()
        };
        preceding.push(selected);
        preceding.extend(following);
        validate_session_event_coordinates(&preceding)?;
        Ok(Some(preceding))
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
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        if self.searcher.search(&query, &Count)? > maximum_events {
            return Ok(None);
        }
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        Ok(Some(events))
    }

    /// Returns exact normalized Core-content bytes for a nonempty session only
    /// when its cardinality is within the caller's bound. This reads indexed
    /// size metadata and never loads or decodes stored Core records.
    pub fn core_content_bytes_for_session_if_bounded(
        &self,
        session_id: Uuid,
        maximum_events: usize,
    ) -> Result<Option<usize>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let count = self.searcher.search(&query, &Count)?;
        if count == 0 || count > maximum_events {
            return Ok(None);
        }
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let segments = self.searcher.segment_readers();
        let mut total = 0_usize;
        for address in addresses {
            let segment = segments
                .get(address.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
            let value = segment
                .fast_fields()
                .u64(CORE_CONTENT_BYTES_FIELD)?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField(
                    CORE_CONTENT_BYTES_FIELD,
                ))?;
            let value = usize::try_from(value).map_err(|_| IndexError::CountOverflow)?;
            total = total.checked_add(value).ok_or(IndexError::CountOverflow)?;
        }
        Ok(Some(total))
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

/// Pulls one globally ordered page from traversal-scoped segment streams.
///
/// `TermMerger` retains one frontier per segment between calls, so a term that
/// loses an earlier page's global cutoff is not sought and decoded again on a
/// later page. The page itself retains only `capacity` addresses.
fn session_event_address_page(
    session_id: StableEntityId,
    capacity: usize,
    merged: &mut TermMerger<'_>,
    inverted_indexes: &[std::sync::Arc<InvertedIndexReader>],
    segments: &[SegmentReader],
) -> Result<Vec<SessionEventAddressCandidate>> {
    if inverted_indexes.len() != segments.len() {
        return Err(IndexError::InvalidStoredDocumentField(
            SESSION_EVENT_ORDER_FIELD,
        ));
    }
    let mut candidates = Vec::with_capacity(capacity);
    while candidates.len() < capacity && merged.advance() {
        #[cfg(test)]
        SESSION_EVENT_ORDER_TERM_VISITS
            .set(SESSION_EVENT_ORDER_TERM_VISITS.get().saturating_add(1));
        let order = SessionEventOrderKey::decode_for_session(session_id, merged.key())?;
        #[cfg(test)]
        SESSION_EVENT_ORDER_VISITED_SEQUENCES
            .with(|sequences| sequences.borrow_mut().push(order.event_sequence()));
        let mut address = None;
        for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
            let inverted =
                inverted_indexes
                    .get(segment_ord)
                    .ok_or(IndexError::InvalidStoredDocumentField(
                        SESSION_EVENT_ORDER_FIELD,
                    ))?;
            let segment =
                segments
                    .get(segment_ord)
                    .ok_or(IndexError::InvalidStoredDocumentField(
                        SESSION_EVENT_ORDER_FIELD,
                    ))?;
            let mut postings =
                inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if !segment.is_deleted(doc_id) {
                    let segment_ord =
                        u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                    if address
                        .replace(DocAddress::new(segment_ord, doc_id))
                        .is_some()
                    {
                        return Err(IndexError::InvalidStoredDocumentField(
                            SESSION_EVENT_ORDER_FIELD,
                        ));
                    }
                }
                doc_id = postings.advance();
            }
        }
        if let Some(address) = address {
            candidates.push(SessionEventAddressCandidate { order, address });
        }
    }
    Ok(candidates)
}

struct SessionIdCollector {
    limit: usize,
}

impl SessionIdCollector {
    fn new(limit: usize) -> Self {
        Self { limit }
    }
}

struct SessionIdSegmentCollector {
    high: tantivy::columnar::Column<u64>,
    low: tantivy::columnar::Column<u64>,
    limit: usize,
    session_ids: BTreeSet<Uuid>,
    invalid: bool,
}

struct SessionIdSegmentFruit {
    session_ids: BTreeSet<Uuid>,
    invalid: bool,
}

impl Collector for SessionIdCollector {
    type Fruit = Vec<Uuid>;
    type Child = SessionIdSegmentCollector;

    fn for_segment(
        &self,
        _segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(SessionIdSegmentCollector {
            high: segment.fast_fields().u64(SESSION_ID_HIGH_FIELD)?,
            low: segment.fast_fields().u64(SESSION_ID_LOW_FIELD)?,
            limit: self.limit,
            session_ids: BTreeSet::new(),
            invalid: false,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<SessionIdSegmentFruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut session_ids = BTreeSet::new();
        for fruit in segment_fruits {
            if fruit.invalid {
                return Err(tantivy::TantivyError::InvalidArgument(
                    "session identity fast field is absent".to_owned(),
                ));
            }
            for session_id in fruit.session_ids {
                session_ids.insert(session_id);
                if session_ids.len() > self.limit {
                    session_ids.pop_last();
                }
            }
        }
        Ok(session_ids.into_iter().collect())
    }
}

impl SegmentCollector for SessionIdSegmentCollector {
    type Fruit = SessionIdSegmentFruit;

    fn collect(&mut self, doc: DocId, _score: Score) {
        let (Some(high), Some(low)) = (self.high.first(doc), self.low.first(doc)) else {
            self.invalid = true;
            return;
        };
        self.session_ids
            .insert(Uuid::from_u128((u128::from(high) << 64) | u128::from(low)));
        if self.session_ids.len() > self.limit {
            self.session_ids.pop_last();
        }
    }

    fn harvest(self) -> Self::Fruit {
        SessionIdSegmentFruit {
            session_ids: self.session_ids,
            invalid: self.invalid,
        }
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
