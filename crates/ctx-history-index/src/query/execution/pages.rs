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
}
