use super::*;

mod lineage;
mod lookup;
mod pages;
mod search;
mod sessions;

#[cfg(test)]
std::thread_local! {
    static MANIFEST_SOURCE_IDENTITY_COMPARISONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

fn retained_manifest_source_by_identity<'a>(
    sources: &'a [ctx_history_core::CertifiedSource],
    source: &SourceKey,
) -> Option<&'a ctx_history_core::CertifiedSource> {
    let identity_digest = source.identity().digest();
    let index = sources
        .binary_search_by(|candidate| {
            #[cfg(test)]
            MANIFEST_SOURCE_IDENTITY_COMPARISONS.with(|comparisons| {
                comparisons.set(comparisons.get().saturating_add(1));
            });
            candidate
                .observation()
                .source()
                .identity()
                .digest()
                .cmp(&identity_digest)
        })
        .ok()?;
    sources.get(index)
}

impl VerifiedIndex {
    /// Counts distinct live session identities from the merged Tantivy term
    /// dictionaries without reading stored event bodies.
    pub fn session_count(&self) -> Result<u64> {
        let session_id = fields_from_schema(self.searcher.schema())?.session_id;
        let segments = self.searcher.segment_readers();
        let inverted_indexes = segments
            .iter()
            .map(|segment| segment.inverted_index(session_id))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let streams = inverted_indexes
            .iter()
            .map(|inverted| inverted.terms().stream())
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut merged = TermMerger::new(streams);
        let mut count = 0_u64;
        while merged.advance() {
            let mut has_live_posting = false;
            for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
                let inverted = inverted_indexes.get(segment_ord).ok_or(
                    IndexError::InvalidStoredDocumentField(SESSION_ID_HIGH_FIELD),
                )?;
                let segment =
                    segments
                        .get(segment_ord)
                        .ok_or(IndexError::InvalidStoredDocumentField(
                            SESSION_ID_HIGH_FIELD,
                        ))?;
                let mut postings =
                    inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        has_live_posting = true;
                        break;
                    }
                    doc_id = postings.advance();
                }
                if has_live_posting {
                    break;
                }
            }
            if has_live_posting {
                count = count.checked_add(1).ok_or(IndexError::CountOverflow)?;
            }
        }
        Ok(count)
    }

    fn body_query_terms(&self, natural_texts: &[&str], fields: Fields) -> Result<Vec<Term>> {
        let mut analyzer = self
            .searcher
            .index()
            .tokenizers()
            .get(BODY_ANALYZER)
            .ok_or(IndexError::MissingAnalyzer(BODY_ANALYZER))?;
        let mut terms = BTreeSet::new();
        for natural_text in natural_texts {
            let mut stream = analyzer.token_stream(natural_text);
            while stream.advance() {
                terms.insert(Term::from_field_text(
                    fields.body_search,
                    &stream.token().text,
                ));
                if terms.len() > LEXICAL_QUERY_LIMITS.maximum_unique_tokens {
                    return Err(IndexError::LexicalQueryTokensTooMany {
                        observed: terms.len(),
                        maximum: LEXICAL_QUERY_LIMITS.maximum_unique_tokens,
                    });
                }
            }
        }
        Ok(terms.into_iter().collect())
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
        let retained = retained_manifest_source_by_identity(&self.manifest.sources, source)
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
        capacity: usize,
    ) -> Result<Vec<EventAddressCandidate>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let eligibility = self.semantic_eligibility_postings()?;
        let after_order = after.map(SemanticEventOrderKey::for_event).transpose()?;
        let segments = self.searcher.segment_readers();
        let inverted_indexes = segments
            .iter()
            .map(|segment| segment.inverted_index(fields.semantic_event_order))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let streams = inverted_indexes
            .iter()
            .map(|inverted| match after_order.as_ref() {
                Some(after) => inverted.terms().range().gt(after.as_bytes()).into_stream(),
                None => inverted.terms().stream(),
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut merged = TermMerger::new(streams);
        let mut candidates = Vec::with_capacity(capacity);
        while candidates.len() < capacity && merged.advance() {
            #[cfg(test)]
            SEMANTIC_EVENT_ORDER_TERM_VISITS
                .set(SEMANTIC_EVENT_ORDER_TERM_VISITS.get().saturating_add(1));
            let order = SemanticEventOrderKey::decode(merged.key())?;
            let mut address = None;
            for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
                let inverted = inverted_indexes.get(segment_ord).ok_or(
                    IndexError::InvalidStoredDocumentField(SEMANTIC_EVENT_ORDER_FIELD),
                )?;
                let segment =
                    segments
                        .get(segment_ord)
                        .ok_or(IndexError::InvalidStoredDocumentField(
                            SEMANTIC_EVENT_ORDER_FIELD,
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
                                SEMANTIC_EVENT_ORDER_FIELD,
                            ));
                        }
                    }
                    doc_id = postings.advance();
                }
            }
            if let Some(address) = address {
                if eligibility.includes(address)? {
                    candidates.push(EventAddressCandidate {
                        identity_digest: order.event_digest(),
                        address,
                        source_order: None,
                    });
                }
            }
        }
        Ok(candidates)
    }

    fn semantic_eligibility_postings(&self) -> Result<&SemanticEligibilityPostings> {
        if let Some(postings) = self.semantic_eligibility_postings.get() {
            return Ok(postings);
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut total = 0_u64;
        let mut selected_segments = Vec::with_capacity(self.searcher.segment_readers().len());
        for segment in self.searcher.segment_readers() {
            let max_doc =
                usize::try_from(segment.max_doc()).map_err(|_| IndexError::CountOverflow)?;
            let mut message_docs = vec![false; max_doc];
            let mut copied_docs = vec![false; max_doc];
            let mut discovery_eligible_docs = vec![false; max_doc];
            let mut selected_docs = vec![false; max_doc];
            let discovery_eligible = segment.inverted_index(fields.discovery_eligible)?;
            if let Some(term_info) = discovery_eligible
                .get_term_info(&Term::from_field_u64(fields.discovery_eligible, 1))?
            {
                let mut postings = discovery_eligible
                    .read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        discovery_eligible_docs[doc_id as usize] = true;
                    }
                    doc_id = postings.advance();
                }
            }
            let event_types = segment.inverted_index(fields.event_type)?;
            if let Some(term_info) =
                event_types.get_term_info(&Term::from_field_text(fields.event_type, "message"))?
            {
                let mut postings = event_types
                    .read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        message_docs[doc_id as usize] = true;
                    }
                    doc_id = postings.advance();
                }
            }
            let origins = segment.inverted_index(fields.event_origin_kind)?;
            if let Some(term_info) = origins.get_term_info(&Term::from_field_text(
                fields.event_origin_kind,
                "copied_from_ancestor",
            ))? {
                let mut postings =
                    origins.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        copied_docs[doc_id as usize] = true;
                    }
                    doc_id = postings.advance();
                }
            }
            let roles = segment.inverted_index(fields.role)?;
            if let Some(term_info) =
                roles.get_term_info(&Term::from_field_text(fields.role, "user"))?
            {
                let mut postings =
                    roles.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id)
                        && message_docs[doc_id as usize]
                        && !copied_docs[doc_id as usize]
                        && discovery_eligible_docs[doc_id as usize]
                    {
                        selected_docs[doc_id as usize] = true;
                        total = total.checked_add(1).ok_or(IndexError::CountOverflow)?;
                    }
                    doc_id = postings.advance();
                }
            }
            selected_segments.push(selected_docs);
        }
        let computed = SemanticEligibilityPostings {
            total,
            segments: selected_segments,
        };
        let _ = self.semantic_eligibility_postings.set(computed);
        self.semantic_eligibility_postings
            .get()
            .ok_or(IndexError::WriterInvariant(
                "semantic eligibility postings were not cached",
            ))
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
                        return Err(IndexError::DuplicateEventIdentity(
                            order.event_id().to_string(),
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

pub(super) fn validate_core_event_page_budget(budget: CoreEventPageBudget) -> Result<()> {
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

pub(super) fn core_event_page_budget_admits(
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

#[cfg(test)]
mod manifest_source_lookup_tests {
    use std::{hint::black_box, time::Instant};

    use ctx_history_core::{
        CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceObservation, TypedKey,
    };

    use super::*;

    const FULL_CORPUS_SOURCE_COUNT: usize = 5_916;
    const SCALED_SOURCE_COUNT: usize = FULL_CORPUS_SOURCE_COUNT * 4;

    fn source(sequence: usize) -> SourceKey {
        SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::provider_native(
                "session-file",
                TypedKey::utf8(format!("manifest-source-{sequence:05}.jsonl")).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn certificate(source: SourceKey) -> CertifiedSource {
        let observation = SourceObservation::new(source, "regular-file-v1", vec![1]).unwrap();
        CertifiedSource::certify(
            observation.clone(),
            observation,
            "codex-parser-v1",
            [1; 32],
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 10,
                ..ScannedSourceCounts::default()
            },
        )
        .unwrap()
    }

    fn sorted_manifest_sources(cardinality: usize) -> Vec<CertifiedSource> {
        let mut sources = (0..cardinality)
            .map(|sequence| certificate(source(sequence)))
            .collect::<Vec<_>>();
        sources.sort_by_key(|candidate| candidate.observation().source().identity().digest());
        assert!(sources.windows(2).all(|pair| {
            pair[0].observation().source().identity().digest()
                < pair[1].observation().source().identity().digest()
        }));
        sources
    }

    fn reset_comparisons() {
        MANIFEST_SOURCE_IDENTITY_COMPARISONS.with(|comparisons| comparisons.set(0));
    }

    fn comparisons() -> usize {
        MANIFEST_SOURCE_IDENTITY_COMPARISONS.with(std::cell::Cell::get)
    }

    fn comparison_bound(cardinality: usize) -> usize {
        (usize::BITS - (cardinality - 1).leading_zeros()) as usize + 1
    }

    fn lookup_comparisons(sources: &[CertifiedSource], target: &SourceKey) -> usize {
        reset_comparisons();
        let retained = retained_manifest_source_by_identity(sources, target).unwrap();
        assert!(retained.observation().source().exact_descriptor_eq(target));
        comparisons()
    }

    #[test]
    fn manifest_source_identity_lookup_preserves_exact_descriptor_semantics() {
        let sources = sorted_manifest_sources(31);
        for index in [0, sources.len() / 2, sources.len() - 1] {
            let target = sources[index].observation().source();
            assert!(lookup_comparisons(&sources, target) <= comparison_bound(sources.len()));
        }

        let absent = source(usize::MAX);
        reset_comparisons();
        assert!(retained_manifest_source_by_identity(&sources, &absent).is_none());
        assert!(comparisons() <= comparison_bound(sources.len()));

        let retained = sources[sources.len() / 2].observation().source();
        let changed_descriptor = SourceKey::derive(
            retained.provider(),
            "codex_prompt_history_jsonl",
            retained.schema_variant(),
            retained.provider_identity_version(),
            retained.anchor().clone(),
        )
        .unwrap();
        assert_eq!(changed_descriptor, *retained);
        assert!(!changed_descriptor.exact_descriptor_eq(retained));
        let found = retained_manifest_source_by_identity(&sources, &changed_descriptor).unwrap();
        assert_eq!(found.observation().source(), retained);
        assert!(!found
            .observation()
            .source()
            .exact_descriptor_eq(&changed_descriptor));
    }

    #[test]
    fn manifest_source_identity_lookup_is_logarithmic_at_full_corpus_cardinality() {
        let full_corpus = sorted_manifest_sources(FULL_CORPUS_SOURCE_COUNT);
        let full_corpus_target = full_corpus.last().unwrap().observation().source();
        let full_corpus_comparisons = lookup_comparisons(&full_corpus, full_corpus_target);
        assert!(full_corpus_comparisons <= comparison_bound(FULL_CORPUS_SOURCE_COUNT));

        let scaled = sorted_manifest_sources(SCALED_SOURCE_COUNT);
        let scaled_target = scaled.last().unwrap().observation().source();
        let scaled_comparisons = lookup_comparisons(&scaled, scaled_target);
        assert!(scaled_comparisons <= comparison_bound(SCALED_SOURCE_COUNT));
        assert!(scaled_comparisons <= full_corpus_comparisons + 2);
    }

    #[test]
    #[ignore = "focused linear/binary manifest-source lookup benchmark; invoke explicitly"]
    fn source_event_page_manifest_validation_benchmark_report() {
        const LOOKUPS: usize = 4_096;

        for cardinality in [FULL_CORPUS_SOURCE_COUNT, SCALED_SOURCE_COUNT] {
            let sources = sorted_manifest_sources(cardinality);
            let target = sources.last().unwrap().observation().source().clone();

            let mut linear_comparisons = 0_usize;
            let linear_started = Instant::now();
            for _ in 0..LOOKUPS {
                let retained = sources
                    .iter()
                    .find(|candidate| {
                        linear_comparisons = linear_comparisons.saturating_add(1);
                        candidate.observation().source() == &target
                    })
                    .unwrap();
                black_box(retained);
            }
            let linear_elapsed = linear_started.elapsed();

            reset_comparisons();
            let binary_started = Instant::now();
            for _ in 0..LOOKUPS {
                let retained =
                    retained_manifest_source_by_identity(black_box(&sources), black_box(&target))
                        .unwrap();
                black_box(retained);
            }
            let binary_elapsed = binary_started.elapsed();
            let binary_comparisons = comparisons();

            assert_eq!(linear_comparisons, cardinality * LOOKUPS);
            assert!(binary_comparisons <= comparison_bound(cardinality) * LOOKUPS);
            eprintln!(
                "manifest_sources={cardinality} lookups={LOOKUPS} \
                 linear_comparisons_per_lookup={} binary_comparisons_per_lookup={:.2} \
                 linear_ns_per_lookup={} binary_ns_per_lookup={} elapsed_speedup={:.2}x",
                linear_comparisons / LOOKUPS,
                binary_comparisons as f64 / LOOKUPS as f64,
                linear_elapsed.as_nanos() / LOOKUPS as u128,
                binary_elapsed.as_nanos() / LOOKUPS as u128,
                linear_elapsed.as_secs_f64() / binary_elapsed.as_secs_f64().max(f64::EPSILON),
            );
        }
    }
}
