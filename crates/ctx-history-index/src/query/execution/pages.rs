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
        let plan = self.plan_core_source_event_page_with_budget(source, cursor, limit, budget)?;
        self.materialize_core_source_event_page(plan)
    }

    /// Enumerates one source page while retaining each record's exact stored
    /// Core JSON for digesting and byte-identical derived transport.
    pub fn stored_core_source_event_page(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
    ) -> Result<StoredCoreSourceEventPage> {
        self.stored_core_source_event_page_with_budget(
            source,
            cursor,
            limit,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
    }

    /// Enumerates one exact-source page under explicit byte bounds while
    /// retaining every record's canonical stored JSON.
    pub fn stored_core_source_event_page_with_budget(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
        budget: CoreEventPageBudget,
    ) -> Result<StoredCoreSourceEventPage> {
        let plan = self.plan_core_source_event_page_with_budget(source, cursor, limit, budget)?;
        self.materialize_stored_core_source_event_page(plan)
    }

    /// Selects one exact-source page and reports its exact retained byte cost
    /// without loading or decoding any stored Core record.
    pub fn plan_core_source_event_page_with_budget(
        &self,
        source: &SourceKey,
        cursor: Option<&SourceEventCursor>,
        limit: usize,
        budget: CoreEventPageBudget,
    ) -> Result<CoreSourceEventPagePlan> {
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
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
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
            encoded_core_bytes = encoded_core_bytes
                .checked_add(order.encoded_core_bytes())
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(order.content_bytes())
                .ok_or(IndexError::CountOverflow)?;
            items.push(candidate);
        }
        let terminal = items.len() == candidate_count;
        Ok(CoreSourceEventPagePlan {
            generation_id: self.generation_id.clone(),
            source: source.clone(),
            items,
            encoded_core_bytes,
            content_bytes,
            terminal,
        })
    }

    /// Loads and decodes a previously selected page, revalidating every size
    /// suffix and identity projection against the pinned generation.
    pub fn materialize_core_source_event_page(
        &self,
        plan: CoreSourceEventPagePlan,
    ) -> Result<CoreSourceEventPage> {
        if plan.generation_id != self.generation_id {
            return Err(IndexError::SourceEventCursorGenerationMismatch {
                cursor_generation: plan.generation_id,
                pinned_generation: self.generation_id.clone(),
            });
        }
        self.validate_source_event_source(&plan.source)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(plan.items.len());
        let mut actual_encoded_core_bytes = 0_usize;
        let mut actual_content_bytes = 0_usize;
        for candidate in plan.items {
            let expected_order =
                candidate
                    .source_order
                    .ok_or(IndexError::InvalidStoredDocumentField(
                        SOURCE_EVENT_ORDER_FIELD,
                    ))?;
            let (record, stored_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, candidate.address, fields)?;
            let actual_order =
                SourceEventOrderKey::for_core_record(&record.core_record, stored_core_bytes)?;
            if actual_order != expected_order
                || record.event_id.digest() != candidate.identity_digest
                || !record.core_record.source.exact_descriptor_eq(&plan.source)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SOURCE_EVENT_ORDER_FIELD,
                ));
            }
            actual_encoded_core_bytes = actual_encoded_core_bytes
                .checked_add(actual_order.encoded_core_bytes())
                .ok_or(IndexError::CountOverflow)?;
            actual_content_bytes = actual_content_bytes
                .checked_add(actual_order.content_bytes())
                .ok_or(IndexError::CountOverflow)?;
            items.push(record);
        }
        if actual_encoded_core_bytes != plan.encoded_core_bytes
            || actual_content_bytes != plan.content_bytes
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SOURCE_EVENT_ORDER_FIELD,
            ));
        }
        let next_cursor = if plan.terminal {
            None
        } else {
            items.last().map(|event| {
                SourceEventCursor::new(
                    self.generation_id.clone(),
                    plan.source.clone(),
                    event.event_id,
                )
            })
        };
        Ok(CoreSourceEventPage {
            generation_id: self.generation_id.clone(),
            source: plan.source,
            items,
            encoded_core_bytes: actual_encoded_core_bytes,
            content_bytes: actual_content_bytes,
            next_cursor,
            terminal: plan.terminal,
        })
    }

    /// Loads one planned source page while retaining the exact stored Core
    /// JSON behind every decoded record.
    pub fn materialize_stored_core_source_event_page(
        &self,
        plan: CoreSourceEventPagePlan,
    ) -> Result<StoredCoreSourceEventPage> {
        if plan.generation_id != self.generation_id {
            return Err(IndexError::SourceEventCursorGenerationMismatch {
                cursor_generation: plan.generation_id,
                pinned_generation: self.generation_id.clone(),
            });
        }
        self.validate_source_event_source(&plan.source)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(plan.items.len());
        let mut actual_encoded_core_bytes = 0_usize;
        let mut actual_content_bytes = 0_usize;
        for candidate in plan.items {
            let expected_order =
                candidate
                    .source_order
                    .ok_or(IndexError::InvalidStoredDocumentField(
                        SOURCE_EVENT_ORDER_FIELD,
                    ))?;
            let record = stored_core_event_record_with_source_json(
                &self.searcher,
                candidate.address,
                fields,
            )?;
            let stored_core_bytes = record.stored_json.encoded_core_record()?.len();
            let actual_order =
                SourceEventOrderKey::for_core_record(&record.core_record, stored_core_bytes)?;
            if actual_order != expected_order
                || record.core_record.event_id.digest() != candidate.identity_digest
                || !record.core_record.source.exact_descriptor_eq(&plan.source)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SOURCE_EVENT_ORDER_FIELD,
                ));
            }
            actual_encoded_core_bytes = actual_encoded_core_bytes
                .checked_add(actual_order.encoded_core_bytes())
                .ok_or(IndexError::CountOverflow)?;
            actual_content_bytes = actual_content_bytes
                .checked_add(actual_order.content_bytes())
                .ok_or(IndexError::CountOverflow)?;
            items.push(record);
        }
        if actual_encoded_core_bytes != plan.encoded_core_bytes
            || actual_content_bytes != plan.content_bytes
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SOURCE_EVENT_ORDER_FIELD,
            ));
        }
        let next_cursor = if plan.terminal {
            None
        } else {
            items.last().map(|event| {
                SourceEventCursor::new(
                    self.generation_id.clone(),
                    plan.source.clone(),
                    event.core_record.event_id,
                )
            })
        };
        Ok(StoredCoreSourceEventPage {
            generation_id: self.generation_id.clone(),
            source: plan.source,
            items,
            encoded_core_bytes: actual_encoded_core_bytes,
            content_bytes: actual_content_bytes,
            next_cursor,
            terminal: plan.terminal,
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
        let fields = fields_from_schema(self.searcher.schema())?;
        let candidates = self.semantic_event_addresses_after(after, limit.saturating_add(1))?;
        let candidate_count = candidates.len();
        let mut items = Vec::with_capacity(limit.min(candidate_count));
        for candidate in candidates.into_iter().take(limit) {
            let record = stored_event_record(&self.searcher, candidate.address, fields)?;
            if record.event_id.digest() != candidate.identity_digest
                || !eligibility.includes(&record)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SEMANTIC_EVENT_ORDER_FIELD,
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
        if !terminal && next_cursor.is_none() {
            return Err(IndexError::InvalidStoredDocumentField(
                SEMANTIC_EVENT_ORDER_FIELD,
            ));
        }
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

    /// Returns complete semantic candidates after neutral Core-order selection
    /// and current semantic-policy filtering under retained Core byte bounds.
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
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut items = Vec::with_capacity(limit);
        let mut encoded_core_bytes = 0_usize;
        let mut content_bytes = 0_usize;
        let candidates = self.semantic_event_addresses_after(after, limit.saturating_add(1))?;
        let candidate_count = candidates.len();
        for candidate in candidates.into_iter().take(limit) {
            let (fast_event_id, preflight_encoded_core_bytes, record_content_bytes) =
                core_event_fast_preflight(&self.searcher, candidate.address)?;
            if fast_event_id
                != (CompactIdentity {
                    digest: candidate.identity_digest,
                })
                .as_uuid()
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SEMANTIC_EVENT_ORDER_FIELD,
                ));
            }
            if !items.is_empty()
                && !core_event_page_budget_admits(
                    budget,
                    encoded_core_bytes,
                    content_bytes,
                    preflight_encoded_core_bytes,
                    record_content_bytes,
                )
            {
                break;
            }
            let (record, record_encoded_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, candidate.address, fields)?;
            if record.event_id.digest() != candidate.identity_digest
                || !eligibility.includes(&record.event)
            {
                return Err(IndexError::InvalidStoredDocumentField(
                    SEMANTIC_EVENT_ORDER_FIELD,
                ));
            }
            if core_content_bytes(&record.core_record.content)? != record_content_bytes
                || record_encoded_core_bytes != preflight_encoded_core_bytes
            {
                return Err(IndexError::InvalidStoredDocumentField("core_record"));
            }
            encoded_core_bytes = encoded_core_bytes
                .checked_add(record_encoded_core_bytes)
                .ok_or(IndexError::CountOverflow)?;
            content_bytes = content_bytes
                .checked_add(record_content_bytes)
                .ok_or(IndexError::CountOverflow)?;
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
        if !terminal && next_cursor.is_none() {
            return Err(IndexError::InvalidStoredDocumentField(
                SEMANTIC_EVENT_ORDER_FIELD,
            ));
        }
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

    /// Returns the exact count selected by the current semantic metadata
    /// policy. Core owns only the neutral order; this count is derived and
    /// cached by the semantic query contract.
    pub fn semantic_eligible_event_count(&self) -> Result<u64> {
        Ok(self.semantic_eligibility_postings()?.total)
    }
}
