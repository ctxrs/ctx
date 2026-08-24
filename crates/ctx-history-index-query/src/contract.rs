use super::*;

/// Fixed admission ceilings for one lexical search request.
///
/// Raw admission happens before analyzer lookup or query construction. The
/// analyzed-token ceiling bounds coverage ranking to at most 32 Tantivy search
/// tiers and 1,024 body term-query nodes plus bounded event-class postings. Empty alternatives
/// still count because callers must not turn repeated empty inputs into
/// unbounded pre-search work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexicalQueryLimits {
    /// Maximum aggregate UTF-8 bytes across all supplied alternatives.
    pub maximum_aggregate_bytes: usize,
    /// Maximum number of supplied positional or repeated-term alternatives.
    pub maximum_alternatives: usize,
    /// Maximum distinct terms retained after lexical analysis.
    pub maximum_unique_tokens: usize,
}

impl LexicalQueryLimits {
    /// Validates raw alternatives without allocating a normalized copy.
    pub fn validate_texts<'a, I>(self, texts: I) -> Result<()>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut alternatives = 0_usize;
        let mut aggregate_bytes = 0_usize;
        for text in texts {
            alternatives = alternatives.saturating_add(1);
            if alternatives > self.maximum_alternatives {
                return Err(IndexError::LexicalQueryAlternativesTooMany {
                    observed: alternatives,
                    maximum: self.maximum_alternatives,
                });
            }
            aggregate_bytes = aggregate_bytes.saturating_add(text.len());
            if aggregate_bytes > self.maximum_aggregate_bytes {
                return Err(IndexError::LexicalQueryBytesTooLarge {
                    actual: aggregate_bytes,
                    maximum: self.maximum_aggregate_bytes,
                });
            }
        }
        Ok(())
    }
}

/// Generous fixed limits for public and programmatic lexical queries.
///
/// The 64 KiB byte ceiling bounds tokenizer input and normalization copies;
/// the two 32-item ceilings bound repeated-query fanout and the quadratic
/// coverage-ranking plan while leaving ample room for ordinary user queries.
pub const LEXICAL_QUERY_LIMITS: LexicalQueryLimits = LexicalQueryLimits {
    maximum_aggregate_bytes: 64 * 1024,
    maximum_alternatives: 32,
    maximum_unique_tokens: 32,
};

/// Maximum number of metadata candidates retained by one lexical search.
///
/// Coverage ranking can temporarily request one additional bounded result set
/// for already-seen higher-coverage hits, so the internal collector ceiling is
/// at most twice this public limit.
pub const MAX_LEXICAL_QUERY_RESULTS: usize = 4_096;

pub(crate) fn validate_lexical_result_limit(limit: usize) -> Result<()> {
    if limit > MAX_LEXICAL_QUERY_RESULTS {
        return Err(IndexError::InvalidLexicalResultLimit {
            requested: limit,
            maximum: MAX_LEXICAL_QUERY_RESULTS,
        });
    }
    Ok(())
}

/// Maximum number of complete semantic event records retained in one page.
pub const MAX_SEMANTIC_EVENT_PAGE_ITEMS: usize = 64;

/// Maximum metadata records retained by one forward semantic pairing page.
pub const MAX_SEMANTIC_PAIRING_PAGE_ITEMS: usize = 64;

/// Maximum number of complete records retained for one exact source page.
pub const MAX_SOURCE_EVENT_PAGE_ITEMS: usize = 4_096;

/// Maximum number of complete records retained for one exact session page.
pub const MAX_SESSION_EVENT_PAGE_ITEMS: usize = 4_096;

/// Maximum retained coordinate prefix, including one truncation lookahead.
pub const MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS: usize = 4_097;

/// Maximum retained centered event-window coordinates.
pub const MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS: usize = 101;

/// Maximum copied-event occurrences retained by one bounded lineage query.
pub const MAX_COPIED_EVENT_LINEAGE_OCCURRENCES: usize = 20;

/// Maximum inverse copied-event origin UUID postings visited by one query.
pub const MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS: usize = 4_096;

/// Absolute maximum exact event-and-session identity postings visited by one
/// lineage query.
///
/// This independent ceiling covers both live and deleted postings while the
/// selected event and its optional direct copied-event target are resolved.
pub const MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS: usize = 2_048;

/// Bounded post-ranking lineage policy for one selected search result.
pub const SEARCH_COPIED_EVENT_LINEAGE_POLICY: CopiedEventLineagePolicy =
    CopiedEventLineagePolicy::new(3, 64);

/// Bounded lineage-detail policy for one selected show-event response.
pub const SHOW_COPIED_EVENT_LINEAGE_POLICY: CopiedEventLineagePolicy =
    CopiedEventLineagePolicy::new(20, 4_096);

/// Caller-selected work and preview-retention ceilings for copied-event lineage.
///
/// The direct-edge query always remains generation-pinned and posting-bounded.
/// Search and show callers should pass their named policies above so
/// presentation cannot accidentally widen either product surface.
/// `maximum_occurrences` never stops counting direct claims; it only caps
/// retained preview rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedEventLineagePolicy {
    pub maximum_occurrences: usize,
    pub maximum_posting_visits: usize,
}

impl CopiedEventLineagePolicy {
    pub const fn new(maximum_occurrences: usize, maximum_posting_visits: usize) -> Self {
        Self {
            maximum_occurrences,
            maximum_posting_visits,
        }
    }

    pub(super) fn validate(self) -> Result<()> {
        if !(1..=MAX_COPIED_EVENT_LINEAGE_OCCURRENCES).contains(&self.maximum_occurrences) {
            return Err(IndexError::InvalidCopiedEventLineageOccurrenceLimit {
                requested: self.maximum_occurrences,
                maximum: MAX_COPIED_EVENT_LINEAGE_OCCURRENCES,
            });
        }
        if !(1..=MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS).contains(&self.maximum_posting_visits) {
            return Err(IndexError::InvalidCopiedEventLineagePostingVisitLimit {
                requested: self.maximum_posting_visits,
                maximum: MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS,
            });
        }
        Ok(())
    }
}

/// Default retained-byte ceiling for complete Core pages.
///
/// One individually valid Core record always makes progress even when it is
/// larger than a caller's chosen page budget. These defaults therefore also
/// define the absolute maximum resident singleton page.
pub const DEFAULT_CORE_EVENT_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget {
    maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
    maximum_content_bytes: MAX_CORE_CONTENT_BYTES,
};

/// Paired encoded and decoded-content ceilings for complete Core records.
///
/// Each query API defines whether these ceilings apply to an aggregate page,
/// a strict batch, or every individual record in a strict batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreEventPageBudget {
    pub maximum_encoded_core_bytes: usize,
    pub maximum_content_bytes: usize,
}

impl CoreEventPageBudget {
    pub const fn new(maximum_encoded_core_bytes: usize, maximum_content_bytes: usize) -> Self {
        Self {
            maximum_encoded_core_bytes,
            maximum_content_bytes,
        }
    }
}

/// Exclusive full-identity keyset cursor for one source in one generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEventCursor {
    pub(super) generation_id: String,
    pub(super) source: SourceKey,
    pub(super) after: StableEntityId,
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

/// One deterministic page of complete Core records for an exact source.
#[derive(Debug, Clone)]
pub struct CoreSourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// One decoded Core record retaining the exact stored JSON that produced it.
///
/// The backing Tantivy document remains page-bounded and owns the byte slice,
/// so derived consumers avoid a second serialization or a body-sized clone.
#[derive(Debug)]
pub struct StoredCoreEventRecord {
    pub core_record: CoreRecord,
    pub stored_json: StoredCoreRecordJson,
}

/// Owned backing storage for one exact, already-validated Core JSON value.
#[derive(Debug)]
pub struct StoredCoreRecordJson {
    pub content_bytes: usize,
    pub(super) accepted_document: ctx_history_index_format::AcceptedCoreDocument,
}

impl StoredCoreRecordJson {
    pub fn encoded_core_record(&self) -> Result<&[u8]> {
        Ok(self.accepted_document.encoded_core_record())
    }
}

/// One deterministic source page retaining each record's exact stored JSON.
#[derive(Debug)]
pub struct StoredCoreSourceEventPage {
    pub generation_id: String,
    pub source: SourceKey,
    pub items: Vec<StoredCoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SourceEventCursor>,
    pub terminal: bool,
}

/// Opaque metadata selection for one complete-Core source page.
///
/// The plan retains only document addresses and authenticated order-key size
/// suffixes so callers can reserve exact bytes before records are decoded.
#[derive(Debug)]
pub struct CoreSourceEventPagePlan {
    pub(super) generation_id: String,
    pub(super) source: SourceKey,
    pub(super) items: Vec<EventAddressCandidate>,
    pub(super) encoded_core_bytes: usize,
    pub(super) content_bytes: usize,
    pub(super) terminal: bool,
}

impl CoreSourceEventPagePlan {
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    pub fn encoded_core_bytes(&self) -> usize {
        self.encoded_core_bytes
    }

    pub fn content_bytes(&self) -> usize {
        self.content_bytes
    }

    pub fn terminal(&self) -> bool {
        self.terminal
    }
}

/// Complete requested-order Core records plus exact retained byte totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventBatch {
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
}

impl From<CoreSourceEventPage> for SourceEventPage {
    fn from(page: CoreSourceEventPage) -> Self {
        Self {
            generation_id: page.generation_id,
            source: page.source,
            items: page.items.into_iter().map(|record| record.event).collect(),
            next_cursor: page.next_cursor,
            terminal: page.terminal,
        }
    }
}

/// Exclusive deterministic position for one exact session in one generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventCursor {
    pub(super) generation_id: String,
    pub(super) session_id: StableEntityId,
    pub(super) after: SessionEventCoordinate,
}

impl SessionEventCursor {
    pub fn new(
        generation_id: impl Into<String>,
        session_id: StableEntityId,
        after: SessionEventCoordinate,
    ) -> Self {
        Self {
            generation_id: generation_id.into(),
            session_id,
            after,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub fn after(&self) -> SessionEventCoordinate {
        self.after
    }
}

/// One deterministic bounded page of complete Core records for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSessionEventPage {
    pub generation_id: String,
    pub session_id: StableEntityId,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SessionEventCursor>,
    pub terminal: bool,
}

/// Stable metadata-only candidate policy for semantic projection from Core.
///
/// Candidate enumeration remains metadata-only. Downstream semantic projection
/// reads complete stored Core content and applies the generation policy's Core
/// content filter before chunking or embedding. This contract is independent
/// of lexical query terms, scores, and ranking. Future candidate changes must
/// add a new enum variant instead of changing the meaning of this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEligibility {
    UserMessageCandidateV4,
}

impl SemanticEligibility {
    pub const CURRENT: Self = Self::UserMessageCandidateV4;

    pub fn includes(self, event: &EventRecord) -> bool {
        match self {
            Self::UserMessageCandidateV4 => ctx_history_index_format::is_semantic_candidate(
                &event.event_type,
                event.role.as_deref(),
            ),
        }
    }
}

/// Exclusive full-identity keyset cursor bound to one verified generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEventCursor {
    pub(super) generation_id: String,
    pub(super) eligibility: SemanticEligibility,
    pub(super) after: StableEntityId,
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

/// One deterministic page of metadata-selected semantic candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    /// Exact count of metadata candidates before Core content filtering.
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

/// Body-free event eligibility selected from one immutable Core generation.
///
/// The IDs are derived from the same indexed metadata predicates as lexical
/// search. Semantic scorers can therefore reject ineligible events before
/// touching vector bytes without reopening provider sources or retaining Core
/// content for the candidate corpus.
#[derive(Debug, Clone)]
pub struct SemanticFilterProjection {
    pub(super) generation_id: String,
    pub(super) event_ids: HashSet<Uuid>,
}

impl SemanticFilterProjection {
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn len(&self) -> usize {
        self.event_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.event_ids.is_empty()
    }

    pub fn contains(&self, event_id: Uuid) -> bool {
        self.event_ids.contains(&event_id)
    }

    pub fn event_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.event_ids.iter().copied()
    }
}

/// One deterministic page of complete Core semantic candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreSemanticEventPage {
    pub generation_id: String,
    pub eligibility: SemanticEligibility,
    pub eligible_total: u64,
    pub items: Vec<CoreEventRecord>,
    pub encoded_core_bytes: usize,
    pub content_bytes: usize,
    pub next_cursor: Option<SemanticEventCursor>,
    pub terminal: bool,
}

impl CoreSemanticEventPage {
    pub fn eligible_count(&self) -> usize {
        self.items.len()
    }
}

impl From<CoreSemanticEventPage> for SemanticEventPage {
    fn from(page: CoreSemanticEventPage) -> Self {
        Self {
            generation_id: page.generation_id,
            eligibility: page.eligibility,
            eligible_total: page.eligible_total,
            items: page.items.into_iter().map(|record| record.event).collect(),
            next_cursor: page.next_cursor,
            terminal: page.terminal,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentScope {
    #[default]
    All,
    Primary,
    Subagent,
}

pub type SearchAgentScope = AgentScope;

/// Event-content classes eligible for one search request.
///
/// `All` retains every indexed event type, including future types unknown to
/// this query implementation. The narrower variants select only their named
/// stable event classes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchContentScope {
    #[default]
    All,
    Transcript,
    Calls,
    Outputs,
}

impl SearchContentScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Transcript => "transcript",
            Self::Calls => "calls",
            Self::Outputs => "outputs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedSessionTree {
    pub provider: String,
    pub provider_session_id: String,
    pub session_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSearchFilters {
    /// Exact source-key terms resolved from the pinned generation. `None`
    /// means unrestricted; `Some([])` deliberately matches no events.
    pub allowed_source_keys: Option<Vec<String>>,
    pub session_id: Option<Uuid>,
    pub excluded_session_ids: Vec<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub provider: Option<String>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub since_unix_ms: Option<i64>,
    pub content_scope: SearchContentScope,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_scope: SearchAgentScope,
    pub file: Option<String>,
    pub exclude_session_tree: Option<ExcludedSessionTree>,
}

impl EventSearchFilters {
    pub(super) fn validate_content_scope(&self) -> Result<()> {
        if self.content_scope != SearchContentScope::All && self.event_type.is_some() {
            return Err(IndexError::ContentScopeEventTypeConflict {
                scope: self.content_scope.as_str(),
            });
        }
        Ok(())
    }

    pub fn matches_source_identity(&self, event: &EventRecord) -> bool {
        if !self.has_source_identity_filter() {
            return true;
        }
        custom_source_identity(event).is_some_and(|(provider_key, source_id)| {
            source_identity_values_match(self, provider_key, source_id)
        })
    }

    pub(super) fn has_source_identity_filter(&self) -> bool {
        self.history_source.is_some() || self.provider_key.is_some() || self.source_id.is_some()
    }

    pub(super) fn validate_source_identity_filters(&self) -> Result<()> {
        for (field, value) in [
            ("history_source", self.history_source.as_deref()),
            ("provider_key", self.provider_key.as_deref()),
            ("source_id", self.source_id.as_deref()),
        ] {
            if let Some(value) = value {
                validated_filter_text(field, value)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: Option<StableEntityId>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub event_copy: Option<ProviderNativeEventCopy>,
    pub source: SourceKey,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub native_event_id: Option<TypedKey>,
    pub agent_scope: Option<CoreAgentScope>,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
}

impl EventRecord {
    /// Returns the exporter-declared route for a custom JSONL event.
    ///
    /// Custom source identity is retained in the native event key so query
    /// surfaces can display the same route used by exact source filters.
    pub fn custom_source_identity(&self) -> Option<(&str, &str)> {
        if self.provider != "custom" {
            return None;
        }
        let Some(TypedKey::Composite(values)) = self.native_event_id.as_ref() else {
            return None;
        };
        let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(_)] =
            values.as_slice()
        else {
            return None;
        };
        Some((provider_key, source_id))
    }
}

/// One direct provider-native event-copy claim targeting the selected event.
///
/// All identities are full stable IDs from the same stored Core record. The
/// direct copied-from pair identifies the exact event edge, while the parent,
/// claimed root, and relationship fields preserve that child session's own
/// direct durable claims. They are not publication-time graph authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedEventLineageOccurrence {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub copied_from_event_id: StableEntityId,
    pub copied_from_session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub claimed_root_session_id: Option<StableEntityId>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub copy_proof: ProviderNativeCopyProof,
    pub depth: usize,
}

/// Query-time resolution of one selected event's direct copied-event target.
///
/// A missing target is an ordinary lineage answer. Core does not infer or
/// traverse a transitive ancestry chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopiedEventLineageResolution {
    Resolved {
        event_id: StableEntityId,
        session_id: StableEntityId,
    },
    Unresolved {
        event_id: Uuid,
        session_id: Option<StableEntityId>,
    },
}

impl CopiedEventLineageResolution {
    pub const fn state_str(&self) -> &'static str {
        match self {
            Self::Resolved { .. } => "resolved",
            Self::Unresolved { .. } => "unresolved",
        }
    }
}

/// Observed direct-copy count for one optional relationship kind.
///
/// Counts are exact only when the containing lineage result is not truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedEventLineageRelationshipCount {
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub observed_count: u64,
}

/// One bounded reverse copied-event lineage result from a pinned generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedEventLineage {
    pub generation_id: String,
    pub selected_event_id: Uuid,
    pub selected_session_id: Option<StableEntityId>,
    pub resolution: CopiedEventLineageResolution,
    pub selected_depth: usize,
    /// Exact when `truncated` is false; otherwise a lower bound.
    pub observed_count: u64,
    /// Number of retained preview rows; this may be smaller than an exact
    /// `observed_count` without making the direct-edge query truncated.
    pub returned: usize,
    pub occurrences: Vec<CopiedEventLineageOccurrence>,
    pub relationship_counts: Vec<CopiedEventLineageRelationshipCount>,
    /// True only when a posting-work ceiling prevented completion.
    /// A full preview with additional exactly counted rows remains false.
    pub truncated: bool,
}

impl CopiedEventLineage {
    /// Returns the total only when every reverse direct edge was visited within
    /// the posting-work ceiling. Preview retention alone never makes a count
    /// inexact.
    pub fn exact_observed_count(&self) -> Option<u64> {
        (!self.truncated).then_some(self.observed_count)
    }
}

/// One verified event plus its complete generation-owned Core data.
///
/// The event projection is derived from the complete self-contained record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventRecord {
    pub event: EventRecord,
    pub core_record: CoreRecord,
}

impl std::ops::Deref for CoreEventRecord {
    type Target = EventRecord;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
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
    pub root_session_id: Option<StableEntityId>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub provider: String,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub agent_scope: Option<CoreAgentScope>,
    pub first_event_sequence: u64,
    pub first_occurred_at_unix_ms: Option<i64>,
}

/// Small body-free session coordinate used to select bounded Core batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventCoordinate {
    pub event_id: Uuid,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
}

pub(super) type SessionEventCoordinateSortKey = (u64, Option<i64>, u64, u64);

impl SessionEventCoordinate {
    pub(super) fn from_sort_key(sort_key: SessionEventCoordinateSortKey) -> Self {
        let (event_sequence, occurred_at_unix_ms, event_id_high, event_id_low) = sort_key;
        Self {
            event_id: Uuid::from_u128((u128::from(event_id_high) << 64) | u128::from(event_id_low)),
            event_sequence,
            occurred_at_unix_ms,
        }
    }

    pub(super) fn sort_key(&self) -> SessionEventCoordinateSortKey {
        let event_id = self.event_id.as_u128();
        (
            self.event_sequence,
            self.occurred_at_unix_ms,
            (event_id >> 64) as u64,
            event_id as u64,
        )
    }
}
