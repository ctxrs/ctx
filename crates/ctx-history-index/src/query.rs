use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BinaryHeap},
    ops::Bound,
};

use ctx_history_core::{SourceKey, SourceRecordLocator, StableEntityId, StableEntityKind};
use serde::{Deserialize, Serialize};
use tantivy::{
    collector::{DocSetCollector, TopDocs},
    query::{BooleanQuery, ConstScoreQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery},
    schema::{IndexRecordOption, Value as TantivyValue},
    tokenizer::TokenStream,
    DocAddress, DocSet, Score, TantivyDocument, Term, TERMINATED,
};
use uuid::Uuid;

use super::{
    fields_from_schema, hex, source_token, Fields, IndexError, Result, VerifiedIndex,
    MAX_BODY_PREVIEW_CHARS,
};

const ID_PREFIX_MATCH_LIMIT: usize = 2;
const BODY_ANALYZER: &str = "default";
const EVENT_ID_HIGH_FIELD: &str = "event_id_high";
const EVENT_ID_LOW_FIELD: &str = "event_id_low";
const EVENT_IDENTITY_DIGEST_FIELD: &str = "event_identity_digest";

/// Maximum number of semantic event records materialized in one page.
pub const MAX_SEMANTIC_EVENT_PAGE_ITEMS: usize = 64;

/// Maximum number of records materialized for one exact provider source page.
pub const MAX_SOURCE_EVENT_PAGE_ITEMS: usize = 4_096;

/// Exclusive full-identity keyset cursor for one source in one generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEventCursor {
    generation_id: String,
    source: SourceKey,
    after: StableEntityId,
}

impl SourceEventCursor {
    pub fn new(generation_id: impl Into<String>, source: SourceKey, after: StableEntityId) -> Self {
        Self {
            generation_id: generation_id.into(),
            source,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One deterministic page of existing bounded records for an exact source.
#[derive(Debug, Clone)]
pub struct SourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<EventRecord>,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// Stable eligibility policy used by the source-backed semantic projection.
///
/// This contract is independent of lexical query terms, scores, and ranking.
/// Future eligibility changes must add a new enum variant instead of changing
/// the meaning of this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEligibility {
    LiteTurnUserMessageV1,
}

impl SemanticEligibility {
    pub const CURRENT: Self = Self::LiteTurnUserMessageV1;

    pub fn includes(self, event: &EventRecord) -> bool {
        match self {
            Self::LiteTurnUserMessageV1 => {
                event.event_type == "message"
                    && event.role.as_deref() == Some("user")
                    && !semantic_control_preview(&event.preview)
            }
        }
    }
}

/// Exclusive full-identity keyset cursor bound to one verified generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEventCursor {
    generation_id: String,
    eligibility: SemanticEligibility,
    after: StableEntityId,
}

impl SemanticEventCursor {
    pub fn new(generation_id: impl Into<String>, after: StableEntityId) -> Self {
        Self {
            generation_id: generation_id.into(),
            eligibility: SemanticEligibility::CURRENT,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn eligibility(&self) -> SemanticEligibility {
        self.eligibility
    }

    pub fn after(&self) -> StableEntityId {
        self.after
    }
}

/// One deterministic page of semantic-eligible events from a pinned index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    pub eligible_total: u64,
    pub items: Vec<EventRecord>,
    pub next_cursor: Option<SemanticEventCursor>,
    pub terminal: bool,
}

impl SemanticEventPage {
    pub fn eligible_count(&self) -> usize {
        self.items.len()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentScope {
    #[default]
    All,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedSessionTree {
    pub provider: String,
    pub provider_session_id: String,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSearchFilters {
    pub session_id: Option<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub provider: Option<String>,
    pub source_format: Option<String>,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub since_unix_ms: Option<i64>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_type: Option<String>,
    pub agent_scope: AgentScope,
    pub file: Option<String>,
    pub exclude_session_tree: Option<ExcludedSessionTree>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub locator: SourceRecordLocator,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub source_path: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub preview: String,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchCandidate {
    pub event: EventRecord,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub source_path: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub first_event_sequence: u64,
    pub first_occurred_at_unix_ms: Option<i64>,
}

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

    /// Returns semantic-eligible events in strict full `StableEntityId` order.
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

    /// Returns the exact total for the current semantic eligibility contract.
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

    /// Searches the bounded event previews using ordinary analyzed text.
    ///
    /// Every analyzed token is required. QueryParser operators and field
    /// syntax are intentionally not accepted.
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

    /// Searches bounded event previews with conjunctive metadata filters.
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
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let terms = self.body_query_terms(natural_text, fields)?;
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        validate_event_sort_fast_fields(&self.searcher)?;
        let body_query = BooleanQuery::intersection(
            terms
                .into_iter()
                .map(|term| {
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) as Box<dyn Query>
                })
                .collect(),
        );
        let query = filtered_event_query(Box::new(body_query), filters, fields)?;
        let collector = TopDocs::with_limit(limit).tweak_score(|segment_reader| {
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
        let hits: Vec<((Score, Reverse<(u64, u64)>), DocAddress)> =
            self.searcher.search(query.as_ref(), &collector)?;
        let mut candidates = Vec::with_capacity(hits.len());
        for ((score, _), address) in hits {
            candidates.push(EventSearchCandidate {
                event: self.event_record(address, fields)?,
                score,
            });
        }
        candidates.sort_by(|left, right| {
            right.score.total_cmp(&left.score).then_with(|| {
                left.event
                    .event_id
                    .as_uuid()
                    .cmp(&right.event.event_id.as_uuid())
            })
        });
        Ok(candidates)
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
            .map(|token| Term::from_field_text(fields.body_preview, &token))
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
        candidates.sort_by(|left, right| left.identity.cmp(&right.identity));
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
        candidates.sort_by(|left, right| left.identity.cmp(&right.identity));
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

pub(super) fn stored_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<EventRecord> {
    let document: TantivyDocument = searcher.doc(address)?;
    let event_id = stored_identity(
        &document,
        fields.event_identity,
        fields.event_id,
        fields.event_identity_digest,
        StableEntityKind::Event,
        "event_identity",
    )?;
    let session_id = stored_identity(
        &document,
        fields.session_identity,
        fields.session_id,
        fields.session_identity_digest,
        StableEntityKind::Session,
        "session_identity",
    )?;
    let locator: SourceRecordLocator = serde_json::from_slice(required_bytes(
        &document,
        fields.native_locator,
        "native_locator",
    )?)?;
    locator.validate_contract()?;
    let stored_source = required_string(&document, fields.source_key, "source_key")?;
    if stored_source != source_token(locator.source())
        || event_id.source_digest() != locator.source().identity().digest()
        || session_id.source_digest() != locator.source().identity().digest()
        || event_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
        || session_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
    {
        return Err(IndexError::InvalidStoredDocumentField("native_locator"));
    }

    let provider = required_string(&document, fields.provider, "provider")?;
    let source_format = required_string(&document, fields.source_format, "source_format")?;
    if provider != locator.source().provider() || source_format != locator.source().source_format()
    {
        return Err(IndexError::InvalidStoredDocumentField("provider"));
    }
    let preview = required_string(&document, fields.body_preview, "body_preview")?;
    if preview.is_empty() || preview.chars().count() > MAX_BODY_PREVIEW_CHARS {
        return Err(IndexError::InvalidStoredDocumentField("body_preview"));
    }
    let touched_files = document
        .get_all(fields.touched_file)
        .map(|value| {
            value
                .as_str()
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .ok_or(IndexError::InvalidStoredDocumentField("touched_file"))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(EventRecord {
        event_id,
        session_id,
        parent_session_id: optional_stored_identity(
            &document,
            fields.parent_session_identity,
            fields.parent_session_id,
            "parent_session_identity",
        )?,
        root_session_id: stored_identity_without_digest(
            &document,
            fields.root_session_identity,
            fields.root_session_id,
            "root_session_identity",
        )?,
        locator,
        provider,
        source_format,
        provider_session_id: optional_string(&document, fields.provider_session_id)?,
        branch: optional_string(&document, fields.branch)?,
        source_path: optional_string(&document, fields.source_path)?,
        agent_type: required_string(&document, fields.agent_type, "agent_type")?,
        is_primary: required_bool(&document, fields.is_primary, "is_primary")?,
        event_sequence: required_u64(&document, fields.event_sequence, "event_sequence")?,
        occurred_at_unix_ms: optional_i64(&document, fields.occurred_at_unix_ms)?,
        event_type: required_string(&document, fields.event_type, "event_type")?,
        role: optional_string(&document, fields.role)?,
        preview,
        workspace: optional_string(&document, fields.workspace)?,
        cwd: optional_string(&document, fields.cwd)?,
        touched_files,
    })
}

struct EventIdentityCandidate {
    identity: [u8; StableEntityId::CANONICAL_LEN],
    digest_term: String,
    event: EventRecord,
}

impl EventIdentityCandidate {
    fn new(event: EventRecord, digest_term: String) -> Result<Self> {
        Ok(Self {
            identity: event.event_id.encode_canonical()?,
            digest_term,
            event,
        })
    }
}

impl PartialEq for EventIdentityCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for EventIdentityCandidate {}

impl PartialOrd for EventIdentityCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EventIdentityCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.identity.cmp(&other.identity)
    }
}

fn semantic_control_preview(preview: &str) -> bool {
    let trimmed = preview.trim();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<turn_aborted>")
        || trimmed.starts_with("<subagent_notification>")
        || trimmed.starts_with("Warning: The maximum number of unified exec processes")
}

impl From<&EventRecord> for SessionRecord {
    fn from(event: &EventRecord) -> Self {
        Self {
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            provider: event.provider.clone(),
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            source_path: event.source_path.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
            first_event_sequence: event.event_sequence,
            first_occurred_at_unix_ms: event.occurred_at_unix_ms,
        }
    }
}

fn filtered_event_query(
    body_query: Box<dyn Query>,
    filters: &EventSearchFilters,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let mut clauses = vec![(Occur::Must, body_query)];
    add_optional_text_filter(
        &mut clauses,
        fields.provider,
        "provider",
        filters.provider.as_deref(),
    )?;
    add_optional_text_filter(
        &mut clauses,
        fields.source_format,
        "source_format",
        filters.source_format.as_deref(),
    )?;
    add_optional_text_filter(
        &mut clauses,
        fields.provider_session_id,
        "provider_session_id",
        filters.provider_session_id.as_deref(),
    )?;
    add_optional_uuid_filter(&mut clauses, fields.session_id, filters.session_id);
    add_optional_uuid_filter(
        &mut clauses,
        fields.parent_session_id,
        filters.parent_session_id,
    );
    add_optional_uuid_filter(
        &mut clauses,
        fields.root_session_id,
        filters.root_session_id,
    );
    add_optional_text_filter(
        &mut clauses,
        fields.branch,
        "branch",
        filters.branch.as_deref(),
    )?;
    add_optional_text_filter(
        &mut clauses,
        fields.event_type,
        "event_type",
        filters.event_type.as_deref(),
    )?;
    add_optional_text_filter(&mut clauses, fields.role, "role", filters.role.as_deref())?;
    add_optional_text_filter(
        &mut clauses,
        fields.agent_type,
        "agent_type",
        filters.agent_type.as_deref(),
    )?;
    if let Some(workspace) = filters.workspace.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(
                fields.workspace_filter,
                "workspace",
                workspace,
            )?),
        );
    }
    if let Some(file) = filters.file.as_deref() {
        add_filter_clause(
            &mut clauses,
            Box::new(metadata_contains_query(
                fields.touched_file_filter,
                "file",
                file,
            )?),
        );
    }
    if let Some(since_unix_ms) = filters.since_unix_ms {
        add_filter_clause(
            &mut clauses,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(
                    fields.occurred_at_unix_ms,
                    since_unix_ms,
                )),
                Bound::Unbounded,
            )),
        );
    }
    if filters.agent_scope == AgentScope::Primary && filters.session_id.is_none() {
        add_filter_clause(
            &mut clauses,
            Box::new(BooleanQuery::union(vec![
                Box::new(TermQuery::new(
                    Term::from_field_u64(fields.is_primary, 1),
                    IndexRecordOption::Basic,
                )),
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.agent_type, "primary"),
                    IndexRecordOption::Basic,
                )),
            ])),
        );
    }
    if let Some(excluded) = &filters.exclude_session_tree {
        clauses.push((
            Occur::MustNot,
            excluded_session_tree_query(excluded, fields)?,
        ));
    }
    Ok(Box::new(BooleanQuery::new(clauses)))
}

fn add_optional_text_filter(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    field: tantivy::schema::Field,
    field_name: &'static str,
    value: Option<&str>,
) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = validated_filter_text(field_name, value)?;
    add_filter_clause(
        clauses,
        Box::new(TermQuery::new(
            Term::from_field_text(field, value),
            IndexRecordOption::Basic,
        )),
    );
    Ok(())
}

fn add_optional_uuid_filter(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    field: tantivy::schema::Field,
    value: Option<Uuid>,
) {
    if let Some(value) = value {
        add_filter_clause(
            clauses,
            Box::new(TermQuery::new(
                Term::from_field_text(field, &value.to_string()),
                IndexRecordOption::Basic,
            )),
        );
    }
}

fn add_filter_clause(clauses: &mut Vec<(Occur, Box<dyn Query>)>, filter: Box<dyn Query>) {
    clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(filter, 0.0))));
}

fn metadata_contains_query(
    field: tantivy::schema::Field,
    field_name: &'static str,
    value: &str,
) -> Result<RegexQuery> {
    let value = validated_filter_text(field_name, value)?.to_lowercase();
    RegexQuery::from_pattern(&format!(".*{}.*", escape_regex_literal(&value)), field)
        .map_err(IndexError::from)
}

fn excluded_session_tree_query(
    excluded: &ExcludedSessionTree,
    fields: Fields,
) -> Result<Box<dyn Query>> {
    let provider = validated_filter_text("excluded_provider", &excluded.provider)?;
    let provider_session_id = validated_filter_text(
        "excluded_provider_session_id",
        &excluded.provider_session_id,
    )?;
    let provider_thread = BooleanQuery::intersection(vec![
        Box::new(TermQuery::new(
            Term::from_field_text(fields.provider, provider),
            IndexRecordOption::Basic,
        )),
        Box::new(TermQuery::new(
            Term::from_field_text(fields.provider_session_id, provider_session_id),
            IndexRecordOption::Basic,
        )),
    ]);
    let Some(session_id) = excluded.session_id else {
        return Ok(Box::new(provider_thread));
    };
    let session_id = session_id.to_string();
    let mut alternatives: Vec<Box<dyn Query>> = vec![Box::new(provider_thread)];
    for field in [
        fields.session_id,
        fields.parent_session_id,
        fields.root_session_id,
    ] {
        alternatives.push(Box::new(TermQuery::new(
            Term::from_field_text(field, &session_id),
            IndexRecordOption::Basic,
        )));
    }
    Ok(Box::new(BooleanQuery::union(alternatives)))
}

fn validated_filter_text<'a>(field: &'static str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(IndexError::EmptyQueryFilter { field });
    }
    if value.len() > super::MAX_DOCUMENT_METADATA_BYTES {
        return Err(IndexError::QueryFilterTooLarge {
            field,
            actual: value.len(),
            maximum: super::MAX_DOCUMENT_METADATA_BYTES,
        });
    }
    Ok(value)
}

fn escape_regex_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn stored_identity(
    document: &TantivyDocument,
    identity_field: tantivy::schema::Field,
    uuid_field: tantivy::schema::Field,
    digest_field: tantivy::schema::Field,
    expected_kind: StableEntityKind,
    field_name: &'static str,
) -> Result<StableEntityId> {
    let identity =
        StableEntityId::decode_canonical(required_bytes(document, identity_field, field_name)?)?;
    let uuid = required_string(document, uuid_field, field_name)?;
    let digest = required_string(document, digest_field, field_name)?;
    if identity.entity_kind() != expected_kind
        || uuid != identity.as_uuid().to_string()
        || digest != hex(&identity.digest())
    {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(identity)
}

fn stored_identity_without_digest(
    document: &TantivyDocument,
    identity_field: tantivy::schema::Field,
    uuid_field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<StableEntityId> {
    decode_stored_session_identity(
        required_bytes(document, identity_field, field_name)?,
        required_string(document, uuid_field, field_name)?,
        field_name,
    )
}

fn optional_stored_identity(
    document: &TantivyDocument,
    identity_field: tantivy::schema::Field,
    uuid_field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<Option<StableEntityId>> {
    let identity = document
        .get_first(identity_field)
        .and_then(|value| value.as_bytes());
    let uuid = document
        .get_first(uuid_field)
        .and_then(|value| value.as_str());
    match (identity, uuid) {
        (None, None) => Ok(None),
        (Some(identity), Some(uuid)) => {
            decode_stored_session_identity(identity, uuid.to_owned(), field_name).map(Some)
        }
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}

fn decode_stored_session_identity(
    identity: &[u8],
    uuid: String,
    field_name: &'static str,
) -> Result<StableEntityId> {
    let identity = StableEntityId::decode_canonical(identity)?;
    if identity.entity_kind() != StableEntityKind::Session || uuid != identity.as_uuid().to_string()
    {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(identity)
}

fn required_string(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<String> {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))
}

fn optional_string(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
) -> Result<Option<String>> {
    document
        .get_first(field)
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(IndexError::InvalidStoredDocumentField("optional_text"))
        })
        .transpose()
}

fn required_bytes<'a>(
    document: &'a TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<&'a [u8]> {
    document
        .get_first(field)
        .and_then(|value| value.as_bytes())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))
}

fn required_u64(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<u64> {
    document
        .get_first(field)
        .and_then(|value| value.as_u64())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))
}

fn required_bool(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<bool> {
    match required_u64(document, field, field_name)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}

fn optional_i64(document: &TantivyDocument, field: tantivy::schema::Field) -> Result<Option<i64>> {
    document
        .get_first(field)
        .map(|value| {
            value.as_i64().ok_or(IndexError::InvalidStoredDocumentField(
                "occurred_at_unix_ms",
            ))
        })
        .transpose()
}

fn canonical_uuid_prefix(prefix: &str) -> Result<String> {
    let mut digits = String::with_capacity(32);
    for character in prefix.chars() {
        if character == '-' {
            continue;
        }
        if !character.is_ascii_hexdigit() || digits.len() == 32 {
            return Err(IndexError::InvalidIdPrefix);
        }
        digits.push(character.to_ascii_lowercase());
    }
    if digits.is_empty() {
        return Err(IndexError::InvalidIdPrefix);
    }
    let mut canonical = String::with_capacity(digits.len() + 4);
    for (index, character) in digits.chars().enumerate() {
        if matches!(index, 8 | 12 | 16 | 20) {
            canonical.push('-');
        }
        canonical.push(character);
    }
    Ok(canonical)
}

fn validate_event_sort_fast_fields(searcher: &tantivy::Searcher) -> Result<()> {
    for segment in searcher.segment_readers() {
        segment.fast_fields().u64(EVENT_ID_HIGH_FIELD)?;
        segment.fast_fields().u64(EVENT_ID_LOW_FIELD)?;
    }
    Ok(())
}

fn sort_events_for_session(events: &mut [EventRecord]) {
    events.sort_by(|left, right| {
        left.event_sequence
            .cmp(&right.event_sequence)
            .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
            .then_with(|| left.event_id.as_uuid().cmp(&right.event_id.as_uuid()))
    });
}
