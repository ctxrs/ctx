use super::*;

/// Fixed admission ceilings for one lexical search request.
///
/// Raw admission happens before analyzer lookup or query construction. The
/// analyzed-token ceiling bounds coverage ranking to at most 32 Tantivy search
/// tiers and 1,024 term-query nodes. Empty alternatives still count because
/// callers must not turn repeated empty inputs into unbounded pre-search work.
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

/// Maximum number of complete semantic event records retained in one page.
pub const MAX_SEMANTIC_EVENT_PAGE_ITEMS: usize = 64;

/// Maximum metadata records retained by one forward semantic pairing page.
pub const MAX_SEMANTIC_PAIRING_PAGE_ITEMS: usize = 64;

/// Maximum number of complete records retained for one exact source page.
pub const MAX_SOURCE_EVENT_PAGE_ITEMS: usize = 4_096;

/// Maximum retained coordinate prefix, including one truncation lookahead.
pub const MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS: usize = 4_097;

/// Maximum retained centered event-window coordinates.
pub const MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS: usize = 101;

/// Default retained-byte ceiling for complete Core pages.
///
/// One individually valid Core record always makes progress even when it is
/// larger than a caller's chosen page budget. These defaults therefore also
/// define the absolute maximum resident singleton page.
pub const DEFAULT_CORE_EVENT_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget {
    maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
    maximum_content_bytes: MAX_CORE_CONTENT_BYTES,
};

/// Retained complete-Core byte ceilings for one source or semantic page.
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
    UserMessageCandidateV2,
}

impl SemanticEligibility {
    pub const CURRENT: Self = Self::UserMessageCandidateV2;

    pub fn includes(self, event: &EventRecord) -> bool {
        match self {
            Self::UserMessageCandidateV2 => {
                event.event_type == "message" && event.role.as_deref() == Some("user")
            }
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
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
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

impl EventSearchFilters {
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
    pub root_session_id: StableEntityId,
    pub source: SourceKey,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub native_event_id: Option<TypedKey>,
    pub branch: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

/// One verified event plus its complete generation-owned Core data.
///
/// The wrapper preserves the compact query metadata alongside the complete
/// self-contained record.
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
    pub root_session_id: StableEntityId,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub first_event_sequence: u64,
    pub first_occurred_at_unix_ms: Option<i64>,
}

/// Small body-free session coordinate used to select bounded Core batches.
#[derive(Debug, Clone, PartialEq, Eq)]
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
