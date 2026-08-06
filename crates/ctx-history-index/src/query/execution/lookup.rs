use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CoreEventBatchBudgetMode {
    LegacyBounded,
    Paged,
    Strict {
        per_record_budget: Option<CoreEventPageBudget>,
    },
}

impl CoreEventBatchBudgetMode {
    fn admits_oversized_singleton(self) -> bool {
        matches!(self, Self::Paged)
    }

    fn preflights_before_decode(self) -> bool {
        matches!(self, Self::Strict { .. })
    }

    fn per_record_budget(self) -> Option<CoreEventPageBudget> {
        match self {
            Self::Strict { per_record_budget } => per_record_budget,
            Self::LegacyBounded | Self::Paged => None,
        }
    }
}

impl VerifiedIndex {
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
            || !anchor.core_record.content.is_discovery_eligible()
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
                if !assistant.core_record.content.is_discovery_eligible()
                    || matches!(
                        assistant.core_record.event_origin,
                        ctx_history_core::EventOrigin::CopiedFromAncestor { .. }
                    )
                {
                    continue;
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
                CoreEventBatchBudgetMode::LegacyBounded,
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
            CoreEventBatchBudgetMode::Paged,
        )
    }

    /// Returns a complete requested-order Core batch only when every record
    /// fits the aggregate byte ceilings. Unlike paged presentation reads, an
    /// oversized singleton is declined instead of being admitted for progress.
    /// Strict reads validate both size dimensions and order the complete
    /// FAST-only candidate set before loading any stored document.
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
            CoreEventBatchBudgetMode::Strict {
                per_record_budget: None,
            },
        )
    }

    /// Returns a complete requested-order Core batch only when it fits the
    /// aggregate ceilings and every individual record fits the independent
    /// per-record ceilings.
    ///
    /// Both size dimensions are checked from FAST metadata for the complete
    /// candidate set before any stored read, so a record cannot borrow unused
    /// aggregate capacity from another requested ID.
    pub fn core_events_by_ids_with_strict_per_record_budget(
        &self,
        event_ids: &[Uuid],
        maximum_events: usize,
        aggregate_budget: CoreEventPageBudget,
        per_record_budget: CoreEventPageBudget,
    ) -> Result<Option<CoreEventBatch>> {
        validate_core_event_page_budget(aggregate_budget)?;
        validate_core_event_page_budget(per_record_budget)?;
        self.core_event_batch_by_ids(
            event_ids,
            maximum_events,
            aggregate_budget.maximum_encoded_core_bytes,
            aggregate_budget.maximum_content_bytes,
            CoreEventBatchBudgetMode::Strict {
                per_record_budget: Some(per_record_budget),
            },
        )
    }

    /// Selects a complete requested-order Core set with one Tantivy query,
    /// then decodes records lazily under independent per-record ceilings.
    ///
    /// The complete FAST-only candidate set is validated before this returns.
    /// The iterator retains only addresses and size metadata; each call to
    /// `next` materializes one complete Core record and validates it against
    /// the preflight metadata before yielding it.
    pub fn stream_core_events_by_ids_with_strict_per_record_budget<'index>(
        &'index self,
        event_ids: &[Uuid],
        maximum_events: usize,
        per_record_budget: CoreEventPageBudget,
    ) -> Result<Option<impl Iterator<Item = Result<CoreEventRecord>> + 'index>> {
        validate_core_event_page_budget(per_record_budget)?;
        if event_ids.len() > maximum_events {
            return Ok(None);
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
        #[cfg(test)]
        CORE_EVENT_ID_SELECTION_QUERIES
            .set(CORE_EVENT_ID_SELECTION_QUERIES.get().saturating_add(1));
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let mut by_event_id = BTreeMap::new();
        for address in addresses {
            let (event_id, encoded_core_bytes, content_bytes) =
                core_event_fast_preflight(&self.searcher, address)?;
            if !requested.contains(&event_id) {
                return Err(IndexError::InvalidStoredDocumentField("event_id"));
            }
            if encoded_core_bytes > per_record_budget.maximum_encoded_core_bytes
                || content_bytes > per_record_budget.maximum_content_bytes
            {
                return Ok(None);
            }
            if by_event_id
                .insert(event_id, (address, encoded_core_bytes, content_bytes))
                .is_some()
            {
                return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
            }
        }
        if by_event_id.len() != requested.len() {
            return Ok(None);
        }

        let mut ordered = Vec::with_capacity(event_ids.len());
        for event_id in event_ids {
            let Some((address, encoded_core_bytes, content_bytes)) = by_event_id.remove(event_id)
            else {
                return Ok(None);
            };
            ordered.push((address, *event_id, encoded_core_bytes, content_bytes));
        }
        let searcher = &self.searcher;
        Ok(Some(ordered.into_iter().map(
            move |(
                address,
                expected_event_id,
                expected_encoded_core_bytes,
                expected_content_bytes,
            )| {
                let (record, encoded_core_bytes) =
                    stored_core_event_record_with_size(searcher, address, fields)?;
                let content_bytes = core_content_bytes(&record.core_record.content)?;
                if record.event_id.as_uuid() != expected_event_id {
                    return Err(IndexError::InvalidStoredDocumentField("event_id"));
                }
                if encoded_core_bytes != expected_encoded_core_bytes {
                    return Err(IndexError::InvalidStoredDocumentField(
                        CORE_RECORD_ENCODED_BYTES_FIELD,
                    ));
                }
                if content_bytes != expected_content_bytes {
                    return Err(IndexError::InvalidStoredDocumentField(
                        CORE_CONTENT_BYTES_FIELD,
                    ));
                }
                Ok(record)
            },
        )))
    }

    fn core_event_batch_by_ids(
        &self,
        event_ids: &[Uuid],
        maximum_events: usize,
        maximum_stored_core_bytes: usize,
        maximum_content_bytes: usize,
        budget_mode: CoreEventBatchBudgetMode,
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
        #[cfg(test)]
        CORE_EVENT_ID_SELECTION_QUERIES
            .set(CORE_EVENT_ID_SELECTION_QUERIES.get().saturating_add(1));
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let per_record_budget = budget_mode.per_record_budget();
        let candidates = if budget_mode.preflights_before_decode() {
            let mut by_event_id = BTreeMap::new();
            for address in addresses {
                let (event_id, record_encoded_core_bytes, record_content_bytes) =
                    core_event_fast_preflight(&self.searcher, address)?;
                if !requested.contains(&event_id) {
                    return Err(IndexError::InvalidStoredDocumentField("event_id"));
                }
                if by_event_id
                    .insert(
                        event_id,
                        (address, record_encoded_core_bytes, record_content_bytes),
                    )
                    .is_some()
                {
                    return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
                }
            }
            if by_event_id.len() != requested.len() {
                return Ok(None);
            }
            if per_record_budget.is_some_and(|budget| {
                by_event_id
                    .values()
                    .any(|(_, encoded_core_bytes, content_bytes)| {
                        *encoded_core_bytes > budget.maximum_encoded_core_bytes
                            || *content_bytes > budget.maximum_content_bytes
                    })
            }) {
                return Ok(None);
            }
            let mut aggregate_encoded_core_bytes = 0_usize;
            let mut aggregate_content_bytes = 0_usize;
            for (_, encoded_core_bytes, record_content_bytes) in by_event_id.values() {
                aggregate_encoded_core_bytes =
                    match aggregate_encoded_core_bytes.checked_add(*encoded_core_bytes) {
                        Some(total) if total <= maximum_stored_core_bytes => total,
                        _ => return Ok(None),
                    };
                aggregate_content_bytes =
                    match aggregate_content_bytes.checked_add(*record_content_bytes) {
                        Some(total) if total <= maximum_content_bytes => total,
                        _ => return Ok(None),
                    };
            }
            let mut ordered = Vec::with_capacity(event_ids.len());
            for event_id in event_ids {
                let Some((address, encoded_core_bytes, content_bytes)) =
                    by_event_id.remove(event_id)
                else {
                    return Ok(None);
                };
                ordered.push((
                    address,
                    Some((*event_id, encoded_core_bytes, content_bytes)),
                ));
            }
            ordered
        } else {
            addresses
                .into_iter()
                .map(|address| (address, None))
                .collect()
        };
        let mut records = BTreeMap::new();
        let mut stored_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        for (address, fast_preflight) in candidates {
            let (record, record_stored_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, address, fields)?;
            let record_content_bytes = core_content_bytes(&record.core_record.content)?;
            if let Some((
                preflight_event_id,
                preflight_encoded_core_bytes,
                preflight_content_bytes,
            )) = fast_preflight
            {
                if record.event_id.as_uuid() != preflight_event_id {
                    return Err(IndexError::InvalidStoredDocumentField("event_id"));
                }
                if record_stored_core_bytes != preflight_encoded_core_bytes {
                    return Err(IndexError::InvalidStoredDocumentField(
                        CORE_RECORD_ENCODED_BYTES_FIELD,
                    ));
                }
                if record_content_bytes != preflight_content_bytes {
                    return Err(IndexError::InvalidStoredDocumentField(
                        CORE_CONTENT_BYTES_FIELD,
                    ));
                }
            }
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
                && !(budget_mode.admits_oversized_singleton() && event_ids.len() == 1)
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
}
