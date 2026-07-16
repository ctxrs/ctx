//! Experimental `agent-history-v1` contract types shared by in-repo ctx SDKs.
//!
//! These types describe the SDK product contract. They are not SQLite schema
//! types and are not a promise to preserve current CLI JSON internals.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CONTRACT_VERSION: &str = "agent-history-v1";
pub const SCHEMA_VERSION: u16 = 1;
pub const SEARCH_QUERY_VERSION: &str = "ctx-search-v1";
pub const SEARCH_POSITIVE_TEXT_RULE_VERSION: &str = "ctx-search-positive-text-v1";
pub const SEARCH_MAX_CLAUSES: usize = 32;
pub const SEARCH_MAX_CLAUSE_BYTES: usize = 1024;
pub const SEARCH_MAX_TOTAL_CLAUSE_BYTES: usize = 8192;
pub const SEARCH_MAX_QUERY_JSON_BYTES: usize = 64 * 1024;
pub const SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE: usize = 32;
pub const SEARCH_MIN_LITERAL_BYTES: usize = 3;
pub const SEARCH_MAX_LITERAL_BYTES: usize = 256;
pub const SEARCH_MAX_IDENTITY_BYTES: usize = 128;
pub const SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED: usize = 1024;
pub const SEARCH_MAX_CANDIDATE_ROWS: usize = 16_384;
pub const SEARCH_MAX_RETAINED_CANDIDATE_IDS: usize = 8192;
pub const SEARCH_MAX_RESIDUAL_ROWS: usize = 8192;
pub const SEARCH_MAX_VERIFICATION_BYTES: usize = 16 * 1024 * 1024;
pub const SEARCH_MAX_VERIFICATION_LOOKUP_BYTES: usize = 16 * 1024;
pub const SEARCH_MAX_HYDRATED_ROWS: usize = 256;
pub const SEARCH_MAX_HYDRATION_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const SEARCH_MAX_HYDRATION_INPUT_BYTES_PER_EVENT: usize = 64 * 1024;
pub const SEARCH_MAX_RETURNED_TEXT_BYTES: usize = 512 * 1024;
pub const SEARCH_MAX_SERIALIZED_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const SEARCH_MAX_RESULTS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchQueryVersion {
    #[serde(rename = "ctx-search-v1")]
    V1,
}

impl SearchQueryVersion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => SEARCH_QUERY_VERSION,
        }
    }
}

/// One canonical `ctx-search-v1` matcher.
///
/// Variants are externally tagged on the wire, for example
/// `{"phrase":"publication fence"}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchClause {
    All(String),
    Phrase(String),
    Literal(String),
    Semantic(String),
}

impl SearchClause {
    pub fn all(value: impl Into<String>) -> Self {
        Self::All(value.into())
    }

    pub fn phrase(value: impl Into<String>) -> Self {
        Self::Phrase(value.into())
    }

    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    pub fn semantic(value: impl Into<String>) -> Self {
        Self::Semantic(value.into())
    }

    pub fn value(&self) -> &str {
        match self {
            Self::All(value)
            | Self::Phrase(value)
            | Self::Literal(value)
            | Self::Semantic(value) => value,
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::All(_) => "all",
            Self::Phrase(_) => "phrase",
            Self::Literal(_) => "literal",
            Self::Semantic(_) => "semantic",
        }
    }

    pub const fn is_lexical(&self) -> bool {
        !matches!(self, Self::Semantic(_))
    }

    fn canonicalized(self) -> Self {
        match self {
            Self::All(value) => Self::All(collapse_whitespace(&value)),
            Self::Phrase(value) => Self::Phrase(collapse_whitespace(&value)),
            Self::Literal(value) => Self::Literal(value.trim().to_owned()),
            Self::Semantic(value) => Self::Semantic(collapse_whitespace(&value)),
        }
    }
}

/// Stable, backend-neutral search input shared by CLI, MCP, and SDK callers.
///
/// `any` clauses are alternatives. Every `must` clause is required and every
/// `must_not` clause excludes globally, including semantic candidates. At most
/// one semantic clause is allowed and it may only appear in `any`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    pub version: SearchQueryVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any: Vec<SearchClause>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must: Vec<SearchClause>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_not: Vec<SearchClause>,
}

/// Compatibility name for integrations prepared against the original branch.
pub type SearchQueryV1 = SearchQuery;

impl SearchQuery {
    pub fn new(any: Vec<SearchClause>) -> Self {
        Self {
            version: SearchQueryVersion::V1,
            any,
            must: Vec::new(),
            must_not: Vec::new(),
        }
    }

    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, SearchQueryError> {
        if bytes.len() > SEARCH_MAX_QUERY_JSON_BYTES {
            return Err(SearchQueryError::JsonTooLong {
                actual: bytes.len(),
                maximum: SEARCH_MAX_QUERY_JSON_BYTES,
            });
        }
        serde_json::from_slice::<Self>(bytes)
            .map_err(|error| SearchQueryError::InvalidJson(error.to_string()))?
            .canonicalized()
    }

    pub fn clause_count(&self) -> usize {
        self.any
            .len()
            .saturating_add(self.must.len())
            .saturating_add(self.must_not.len())
    }

    pub fn positive_clause_count(&self) -> usize {
        self.any.len().saturating_add(self.must.len())
    }

    pub fn clauses(&self) -> impl Iterator<Item = &SearchClause> {
        self.any
            .iter()
            .chain(self.must.iter())
            .chain(self.must_not.iter())
    }

    pub fn lexical_positive_clauses(&self) -> impl Iterator<Item = &SearchClause> {
        self.any
            .iter()
            .chain(self.must.iter())
            .filter(|clause| clause.is_lexical())
    }

    pub fn semantic_clause(&self) -> Option<&SearchClause> {
        self.any
            .iter()
            .find(|clause| matches!(clause, SearchClause::Semantic(_)))
    }

    pub fn explicit_semantic_text(&self) -> Option<&str> {
        match self.semantic_clause() {
            Some(SearchClause::Semantic(value)) => Some(value),
            _ => None,
        }
    }

    /// Canonical text for automatic hybrid reranking, when v1 permits it.
    /// Eligibility requires exactly one lexical `all` or `phrase` alternative
    /// and only lexical `all` requirements. Exclusions remain hard lexical
    /// constraints but do not enter semantic input.
    pub fn automatic_rerank_text(&self) -> Option<String> {
        let [any] = self.any.as_slice() else {
            return None;
        };
        if !matches!(any, SearchClause::All(_) | SearchClause::Phrase(_))
            || !self
                .must
                .iter()
                .all(|clause| matches!(clause, SearchClause::All(_)))
        {
            return None;
        }
        Some(
            std::iter::once(any)
                .chain(self.must.iter())
                .map(SearchClause::value)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub fn single_all_text(&self) -> Option<&str> {
        match self.any.as_slice() {
            [SearchClause::All(value)] if self.must.is_empty() && self.must_not.is_empty() => {
                Some(value)
            }
            _ => None,
        }
    }

    pub fn canonical_text(&self) -> String {
        if let Some(value) = self.single_all_text() {
            return value.to_owned();
        }
        let mut groups = Vec::new();
        if !self.any.is_empty() {
            groups.push(format!("any({})", render_clauses(&self.any, " | ")));
        }
        if !self.must.is_empty() {
            groups.push(format!("must({})", render_clauses(&self.must, " & ")));
        }
        if !self.must_not.is_empty() {
            groups.push(format!(
                "must_not({})",
                render_clauses(&self.must_not, " | ")
            ));
        }
        groups.join(" ")
    }

    /// Canonical semantic input. Explicit semantic text wins; automatic hybrid
    /// reranking uses canonical positive lexical values in placement order,
    /// joined by a single line feed. Exclusions never enter semantic input.
    pub fn canonical_positive_text(&self) -> String {
        if let Some(value) = self.explicit_semantic_text() {
            return value.to_owned();
        }
        self.automatic_rerank_text().unwrap_or_default()
    }

    pub fn canonicalized(mut self) -> Result<Self, SearchQueryError> {
        self.any = canonicalize_clauses(self.any);
        self.must = canonicalize_clauses(self.must);
        self.must_not = canonicalize_clauses(self.must_not);
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), SearchQueryError> {
        if self.positive_clause_count() == 0 {
            return Err(if self.must_not.is_empty() {
                SearchQueryError::Empty
            } else {
                SearchQueryError::NegativeOnly
            });
        }
        if self.clause_count() > SEARCH_MAX_CLAUSES {
            return Err(SearchQueryError::TooManyClauses {
                actual: self.clause_count(),
                maximum: SEARCH_MAX_CLAUSES,
            });
        }
        if self.must.iter().chain(&self.must_not).any(|clause| !clause.is_lexical()) {
            return Err(SearchQueryError::SemanticMustBeInAny);
        }
        let semantic_count = self
            .any
            .iter()
            .filter(|clause| matches!(clause, SearchClause::Semantic(_)))
            .count();
        if semantic_count > 1 {
            return Err(SearchQueryError::TooManySemanticClauses {
                actual: semantic_count,
                maximum: 1,
            });
        }

        let mut total_bytes = 0usize;
        for clause in self.clauses() {
            let bytes = clause.value().len();
            if bytes == 0 {
                return Err(SearchQueryError::EmptyClause {
                    kind: clause.kind(),
                });
            }
            if bytes > SEARCH_MAX_CLAUSE_BYTES {
                return Err(SearchQueryError::ClauseTooLong {
                    kind: clause.kind(),
                    actual: bytes,
                    maximum: SEARCH_MAX_CLAUSE_BYTES,
                });
            }
            if matches!(clause, SearchClause::Literal(_))
                && !(SEARCH_MIN_LITERAL_BYTES..=SEARCH_MAX_LITERAL_BYTES).contains(&bytes)
            {
                return Err(SearchQueryError::LiteralLength {
                    actual: bytes,
                    minimum: SEARCH_MIN_LITERAL_BYTES,
                    maximum: SEARCH_MAX_LITERAL_BYTES,
                });
            }
            let analyzed_tokens = search_analyzed_token_count(clause.value());
            if analyzed_tokens == 0 {
                return Err(SearchQueryError::NoSearchableTokens {
                    kind: clause.kind(),
                });
            }
            if analyzed_tokens > SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE {
                return Err(SearchQueryError::TooManyAnalyzedTokens {
                    kind: clause.kind(),
                    actual: analyzed_tokens,
                    maximum: SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE,
                });
            }
            total_bytes = total_bytes.saturating_add(bytes);
        }
        if total_bytes > SEARCH_MAX_TOTAL_CLAUSE_BYTES {
            return Err(SearchQueryError::QueryTooLong {
                actual: total_bytes,
                maximum: SEARCH_MAX_TOTAL_CLAUSE_BYTES,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchQueryError {
    Empty,
    NegativeOnly,
    EmptyClause {
        kind: &'static str,
    },
    TooManyClauses {
        actual: usize,
        maximum: usize,
    },
    ClauseTooLong {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    QueryTooLong {
        actual: usize,
        maximum: usize,
    },
    LiteralLength {
        actual: usize,
        minimum: usize,
        maximum: usize,
    },
    NoSearchableTokens {
        kind: &'static str,
    },
    TooManyAnalyzedTokens {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    SemanticMustBeInAny,
    TooManySemanticClauses {
        actual: usize,
        maximum: usize,
    },
    JsonTooLong {
        actual: usize,
        maximum: usize,
    },
    InvalidJson(String),
}

impl std::fmt::Display for SearchQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "search query needs a positive clause"),
            Self::NegativeOnly => write!(formatter, "search query cannot contain only must_not clauses"),
            Self::EmptyClause { kind } => write!(formatter, "{kind} clause cannot be empty"),
            Self::TooManyClauses { actual, maximum } => write!(formatter, "search query has {actual} clauses; maximum is {maximum}"),
            Self::ClauseTooLong { kind, actual, maximum } => write!(formatter, "{kind} clause is {actual} bytes; maximum is {maximum}"),
            Self::QueryTooLong { actual, maximum } => write!(formatter, "search query has {actual} clause bytes; maximum is {maximum}"),
            Self::LiteralLength { actual, minimum, maximum } => write!(formatter, "literal clause is {actual} bytes; expected {minimum}..={maximum}"),
            Self::NoSearchableTokens { kind } => write!(formatter, "{kind} clause has no searchable tokens"),
            Self::TooManyAnalyzedTokens { kind, actual, maximum } => write!(formatter, "{kind} clause has {actual} analyzed tokens; maximum is {maximum}"),
            Self::SemanticMustBeInAny => write!(formatter, "semantic clauses are allowed only in any; must and must_not require lexical matchers"),
            Self::TooManySemanticClauses { actual, maximum } => write!(formatter, "search query has {actual} semantic clauses; maximum is {maximum}"),
            Self::JsonTooLong { actual, maximum } => write!(formatter, "search query JSON is {actual} bytes; maximum is {maximum}"),
            Self::InvalidJson(message) => write!(formatter, "invalid ctx-search-v1 query JSON: {message}"),
        }
    }
}

impl std::error::Error for SearchQueryError {}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tokenize lexical clauses at Unicode alphanumeric boundaries.
pub fn search_analyzed_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_alphanumeric() || (!current.is_empty() && is_unicode_mark(ch)) {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub fn search_analyzed_token_count(value: &str) -> usize {
    search_analyzed_tokens(value).len()
}

fn is_unicode_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{0300}'..='\u{036f}'
            | '\u{1ab0}'..='\u{1aff}'
            | '\u{1dc0}'..='\u{1dff}'
            | '\u{20d0}'..='\u{20ff}'
            | '\u{fe20}'..='\u{fe2f}'
            | '\u{200c}'
            | '\u{200d}'
    )
}

fn canonicalize_clauses(clauses: Vec<SearchClause>) -> Vec<SearchClause> {
    let mut canonical = Vec::with_capacity(clauses.len());
    for clause in clauses.into_iter().map(SearchClause::canonicalized) {
        if !canonical.contains(&clause) {
            canonical.push(clause);
        }
    }
    canonical
}

fn render_clauses(clauses: &[SearchClause], separator: &str) -> String {
    clauses
        .iter()
        .map(|clause| format!("{}:{:?}", clause.kind(), clause.value()))
        .collect::<Vec<_>>()
        .join(separator)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSemanticPolicy {
    Disabled,
    AutomaticRerank,
}

impl Default for SearchSemanticPolicy {
    fn default() -> Self {
        Self::AutomaticRerank
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSemanticReadiness {
    Ready,
    NotReady,
    Unsupported,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SearchSemanticCandidate {
    pub ctx_event_id: String,
}

/// Pre-ranked semantic identities supplied by a read-only semantic adapter.
/// Vector order is rank order. This type intentionally has no indexing,
/// download, model-startup, or provider-history fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SearchSemanticInput {
    #[serde(default)]
    pub readiness: SearchSemanticReadiness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<SearchSemanticCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_documents: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searchable_documents: Option<u64>,
    #[serde(default)]
    pub coverage_complete: bool,
}

/// Shared core request envelope. Positional and flag parsing remains a CLI
/// concern; all callers enter the executor through this validated DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SearchRequestEnvelope {
    pub query: SearchQuery,
    #[serde(default)]
    pub semantic_policy: SearchSemanticPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SearchSemanticInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_limits: Option<SearchExecutionLimits>,
}

impl SearchRequestEnvelope {
    pub fn new(query: SearchQuery) -> Self {
        Self {
            query,
            semantic_policy: SearchSemanticPolicy::AutomaticRerank,
            semantic: None,
            requested_limits: None,
        }
    }

    pub fn canonicalized(mut self) -> Result<Self, SearchEnvelopeError> {
        self.query = self.query.canonicalized()?;
        if let Some(semantic) = &mut self.semantic {
            if semantic.candidates.len() > SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED {
                return Err(SearchEnvelopeError::TooManySemanticCandidates {
                    actual: semantic.candidates.len(),
                    maximum: SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED,
                });
            }
            if semantic.readiness != SearchSemanticReadiness::Ready
                && !semantic.candidates.is_empty()
            {
                return Err(SearchEnvelopeError::CandidatesWithoutReadiness);
            }
            if self.semantic_policy == SearchSemanticPolicy::Disabled
                && !semantic.candidates.is_empty()
                && self.query.semantic_clause().is_none()
            {
                return Err(SearchEnvelopeError::CandidatesWhileDisabled);
            }
            let mut seen = std::collections::BTreeSet::new();
            semantic.candidates.retain(|candidate| {
                seen.insert(candidate.ctx_event_id.trim().to_owned())
            });
            for candidate in &mut semantic.candidates {
                candidate.ctx_event_id = candidate.ctx_event_id.trim().to_owned();
                let bytes = candidate.ctx_event_id.len();
                if bytes == 0 || bytes > SEARCH_MAX_IDENTITY_BYTES {
                    return Err(SearchEnvelopeError::InvalidCandidateIdentity {
                        actual: bytes,
                        maximum: SEARCH_MAX_IDENTITY_BYTES,
                    });
                }
            }
            semantic.backend = semantic
                .backend
                .take()
                .map(|backend| backend.trim().to_owned())
                .filter(|backend| !backend.is_empty());
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchEnvelopeError {
    Query(SearchQueryError),
    TooManySemanticCandidates { actual: usize, maximum: usize },
    CandidatesWithoutReadiness,
    CandidatesWhileDisabled,
    InvalidCandidateIdentity { actual: usize, maximum: usize },
}

impl From<SearchQueryError> for SearchEnvelopeError {
    fn from(value: SearchQueryError) -> Self {
        Self::Query(value)
    }
}

impl std::fmt::Display for SearchEnvelopeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Query(error) => error.fmt(formatter),
            Self::TooManySemanticCandidates { actual, maximum } => write!(formatter, "semantic input has {actual} candidates; maximum is {maximum}"),
            Self::CandidatesWithoutReadiness => write!(formatter, "semantic candidates require ready semantic input"),
            Self::CandidatesWhileDisabled => write!(formatter, "automatic semantic candidates were supplied while semantic policy is disabled"),
            Self::InvalidCandidateIdentity { actual, maximum } => write!(formatter, "semantic candidate identity is {actual} bytes; expected 1..={maximum}"),
        }
    }
}

impl std::error::Error for SearchEnvelopeError {}

/// Hard or caller-lowered limits resolved for one shared search envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SearchExecutionLimits {
    pub query_bytes: usize,
    pub clauses: usize,
    pub analyzed_tokens_per_clause: usize,
    pub candidates_per_positive_seed: usize,
    pub candidate_rows: usize,
    pub retained_candidate_ids: usize,
    pub residual_rows: usize,
    pub verification_bytes: usize,
    pub verification_lookup_bytes: usize,
    pub hydrated_rows: usize,
    pub hydration_input_bytes: usize,
    pub hydration_input_bytes_per_event: usize,
    pub returned_text_bytes: usize,
    pub serialized_response_bytes: usize,
    pub results: usize,
    /// Resolved by executor policy. `ctx-search-v1` does not prescribe a
    /// normative wall-clock duration.
    pub elapsed_ms: u64,
}

impl SearchExecutionLimits {
    pub const fn hard_maxima() -> Self {
        Self {
            query_bytes: SEARCH_MAX_TOTAL_CLAUSE_BYTES,
            clauses: SEARCH_MAX_CLAUSES,
            analyzed_tokens_per_clause: SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE,
            candidates_per_positive_seed: SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED,
            candidate_rows: SEARCH_MAX_CANDIDATE_ROWS,
            retained_candidate_ids: SEARCH_MAX_RETAINED_CANDIDATE_IDS,
            residual_rows: SEARCH_MAX_RESIDUAL_ROWS,
            verification_bytes: SEARCH_MAX_VERIFICATION_BYTES,
            verification_lookup_bytes: SEARCH_MAX_VERIFICATION_LOOKUP_BYTES,
            hydrated_rows: SEARCH_MAX_HYDRATED_ROWS,
            hydration_input_bytes: SEARCH_MAX_HYDRATION_INPUT_BYTES,
            hydration_input_bytes_per_event: SEARCH_MAX_HYDRATION_INPUT_BYTES_PER_EVENT,
            returned_text_bytes: SEARCH_MAX_RETURNED_TEXT_BYTES,
            serialized_response_bytes: SEARCH_MAX_SERIALIZED_RESPONSE_BYTES,
            results: SEARCH_MAX_RESULTS,
            elapsed_ms: 0,
        }
    }

    pub fn resolved(
        requested: Option<&Self>,
        result_limit: usize,
        policy_elapsed_ms: u64,
    ) -> Self {
        let hard = Self::hard_maxima();
        let requested = requested.unwrap_or(&hard);
        Self {
            query_bytes: requested.query_bytes.clamp(1, hard.query_bytes),
            clauses: requested.clauses.clamp(1, hard.clauses),
            analyzed_tokens_per_clause: requested
                .analyzed_tokens_per_clause
                .clamp(1, hard.analyzed_tokens_per_clause),
            candidates_per_positive_seed: requested
                .candidates_per_positive_seed
                .clamp(1, hard.candidates_per_positive_seed),
            candidate_rows: requested.candidate_rows.clamp(1, hard.candidate_rows),
            retained_candidate_ids: requested
                .retained_candidate_ids
                .clamp(1, hard.retained_candidate_ids),
            residual_rows: requested.residual_rows.clamp(1, hard.residual_rows),
            verification_bytes: requested
                .verification_bytes
                .clamp(1, hard.verification_bytes),
            verification_lookup_bytes: requested
                .verification_lookup_bytes
                .clamp(1, hard.verification_lookup_bytes),
            hydrated_rows: requested.hydrated_rows.clamp(1, hard.hydrated_rows),
            hydration_input_bytes: requested
                .hydration_input_bytes
                .clamp(1, hard.hydration_input_bytes),
            hydration_input_bytes_per_event: requested
                .hydration_input_bytes_per_event
                .clamp(1, hard.hydration_input_bytes_per_event),
            returned_text_bytes: requested
                .returned_text_bytes
                .clamp(1, hard.returned_text_bytes),
            serialized_response_bytes: requested
                .serialized_response_bytes
                .clamp(1, hard.serialized_response_bytes),
            results: requested.results.min(result_limit).clamp(1, hard.results),
            elapsed_ms: match requested.elapsed_ms {
                0 => policy_elapsed_ms.max(1),
                requested => requested.min(policy_elapsed_ms.max(1)),
            },
        }
    }
}

impl Default for SearchExecutionLimits {
    fn default() -> Self {
        Self::hard_maxima()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchExecutionConsumption {
    pub query_bytes: usize,
    pub clauses: usize,
    pub analyzed_tokens: usize,
    pub candidate_rows: usize,
    pub retained_candidate_ids: usize,
    pub residual_rows: usize,
    pub hydrated_rows: usize,
    pub legacy_fallback_rows: usize,
    pub verification_bytes: usize,
    pub largest_verification_lookup_bytes: usize,
    pub hydration_input_bytes: usize,
    pub largest_hydration_input_bytes: usize,
    pub returned_results: usize,
    pub returned_text_bytes: usize,
    pub serialized_response_bytes: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSemanticCompleteness {
    #[default]
    NotAttempted,
    Complete,
    Partial,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSemanticSkipReason {
    Disabled,
    Unavailable,
    NotReady,
    Unsupported,
    NoLexicalCandidates,
    QueryShapeNotEligible,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchSemanticCoverage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_documents: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searchable_documents: Option<u64>,
    pub requested_candidates: usize,
    pub eligible_candidates: usize,
    pub used_candidates: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchEffectiveBackend {
    #[default]
    None,
    Lexical,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchSemanticDiagnostics {
    pub attempted: bool,
    pub required: bool,
    pub readiness: SearchSemanticReadiness,
    pub effective_backend: SearchEffectiveBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub coverage: SearchSemanticCoverage,
    pub completeness: SearchSemanticCompleteness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<SearchSemanticSkipReason>,
    pub positive_text_rule_version: String,
}

/// The only execution-diagnostics model used by core search and wire results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchExecutionDiagnostics {
    pub query_version: String,
    pub candidate_strategy: String,
    pub resolved: SearchExecutionLimits,
    pub consumed: SearchExecutionConsumption,
    pub semantic: SearchSemanticDiagnostics,
    pub rrf_k: u32,
    pub per_branch_candidate_rows: usize,
    pub requested_result_limit: usize,
    pub result_limit: usize,
    pub clauses_executed: usize,
    pub verification_dropped: usize,
    pub filter_verification_dropped: usize,
    pub candidate_budget_exhausted: bool,
    pub timed_out: bool,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncation_reasons: Vec<String>,
}

impl Default for SearchExecutionDiagnostics {
    fn default() -> Self {
        Self {
            query_version: SEARCH_QUERY_VERSION.to_owned(),
            candidate_strategy: String::new(),
            resolved: SearchExecutionLimits::hard_maxima(),
            consumed: SearchExecutionConsumption::default(),
            semantic: SearchSemanticDiagnostics {
                positive_text_rule_version: SEARCH_POSITIVE_TEXT_RULE_VERSION.to_owned(),
                ..SearchSemanticDiagnostics::default()
            },
            rrf_k: 60,
            per_branch_candidate_rows: 0,
            requested_result_limit: 0,
            result_limit: 0,
            clauses_executed: 0,
            verification_dropped: 0,
            filter_verification_dropped: 0,
            candidate_budget_exhausted: false,
            timed_out: false,
            truncated: false,
            truncation_reasons: Vec::new(),
        }
    }
}

/// Extensible JSON object used where `agent-history-v1` intentionally leaves room for
/// backend-specific additive fields.
pub type JsonObject = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    Local,
    Hosted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    pub kind: BackendKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

impl BackendInfo {
    pub fn local(data_root: Option<String>) -> Self {
        Self {
            kind: BackendKind::Local,
            data_root,
            base_url: None,
            extra: JsonObject::new(),
        }
    }

    pub fn hosted(base_url: Option<String>) -> Self {
        Self {
            kind: BackendKind::Hosted,
            data_root: None,
            base_url,
            extra: JsonObject::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentHistoryOperation {
    Status,
    Init,
    Sources,
    Import,
    Sync,
    Search,
    ShowEvent,
    ShowSession,
    LocateEvent,
    LocateSession,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHistoryErrorCode {
    InvalidRequest,
    NotFound,
    NotInitialized,
    BackendUnavailable,
    Timeout,
    Cancelled,
    NotSupported,
    AdapterError,
    DecodeError,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryErrorBody {
    pub code: AgentHistoryErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

impl AgentHistoryErrorBody {
    pub fn new(code: AgentHistoryErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            details: None,
            cause: None,
            extra: JsonObject::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_events: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_edges: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Freshness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<Totals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryStatus {
    pub initialized: bool,
    pub local_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cataloged_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSource {
    pub provider: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_support: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_import: Option<bool>,
    pub importable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_mode: Option<String>,
    pub totals: Totals,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<JsonObject>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(default)]
    pub query: Option<SearchQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<SearchRetrieval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<JsonObject>,
    #[serde(rename = "query_execution", default)]
    pub query_execution: SearchExecutionDiagnostics,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRetrieval {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_fallback_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<SearchRetrievalCoverage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<JsonObject>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRetrievalCoverage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_chunks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searchable_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_now: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_items: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<String>,
    pub result_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub why_matched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationResult {
    pub ctx_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    pub source: SourceLocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<JsonObject>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryEnvelope {
    pub contract_version: String,
    pub schema_version: u16,
    pub operation: AgentHistoryOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentHistoryStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<ProviderSource>>,
    #[serde(rename = "import", default, skip_serializing_if = "Option::is_none")]
    pub import_result: Option<ImportResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<LocationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AgentHistoryErrorBody>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

impl AgentHistoryEnvelope {
    pub fn new(operation: AgentHistoryOperation, backend: Option<BackendInfo>) -> Self {
        Self {
            contract_version: CONTRACT_VERSION.to_owned(),
            schema_version: SCHEMA_VERSION,
            operation,
            backend,
            status: None,
            sources: None,
            import_result: None,
            search: None,
            event: None,
            session: None,
            location: None,
            error: None,
            extra: JsonObject::new(),
        }
    }

    pub fn error(backend: Option<BackendInfo>, error: AgentHistoryErrorBody) -> Self {
        let mut envelope = Self::new(AgentHistoryOperation::Error, backend);
        envelope.error = Some(error);
        envelope
    }
}

pub fn camel_alias_object(value: &Value, aliases: &[(&str, &str)]) -> Value {
    let mut out = value.clone();
    if let Some(object) = out.as_object_mut() {
        for (from, to) in aliases {
            if let Some(item) = object.remove(*from) {
                object.insert((*to).to_owned(), item);
            }
        }
    }
    out
}

/// Recursively converts snake_case object keys from private CLI JSON into the
/// camelCase keys used by the public `agent-history-v1` contract.
pub fn camelize_object_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(camelize_object_keys).collect()),
        Value::Object(object) => {
            let mut out = Map::new();
            for (key, item) in object {
                let camel_key = snake_to_camel(key);
                if omitted_public_key(&camel_key) {
                    continue;
                }
                out.insert(camel_key, camelize_object_keys(item));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn omitted_public_key(key: &str) -> bool {
    matches!(
        key,
        "itemType" | "payloadType" | "recordType" | "databasePath" | "configPath"
    )
}

fn snake_to_camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut uppercase_next = false;
    for ch in key.chars() {
        if ch == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            out.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/agent-history-v1/fixtures")
    }

    #[test]
    fn parses_all_shared_fixtures_into_typed_envelopes() {
        let mut seen = 0;
        for entry in fs::read_dir(fixture_root()).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let fixture = fs::read_to_string(entry.path()).unwrap();
            let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
            assert_eq!(envelope.contract_version, CONTRACT_VERSION);
            assert_eq!(envelope.schema_version, SCHEMA_VERSION);
            match envelope.operation {
                AgentHistoryOperation::Status | AgentHistoryOperation::Init => {
                    assert!(envelope.status.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::Sources => {
                    assert!(envelope.sources.is_some(), "{:?}", entry.path())
                }
                AgentHistoryOperation::Import | AgentHistoryOperation::Sync => {
                    assert!(envelope.import_result.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::Search => {
                    assert!(envelope.search.is_some(), "{:?}", entry.path())
                }
                AgentHistoryOperation::ShowEvent => {
                    assert!(envelope.event.is_some(), "{:?}", entry.path())
                }
                AgentHistoryOperation::ShowSession => {
                    assert!(envelope.session.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::LocateEvent | AgentHistoryOperation::LocateSession => {
                    assert!(envelope.location.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::Error => {
                    assert!(envelope.error.is_some(), "{:?}", entry.path())
                }
            }
            seen += 1;
        }
        assert!(seen > 0, "expected shared agent-history-v1 fixtures");
    }

    #[test]
    fn preserves_additive_fields() {
        let fixture = r#"{
            "contractVersion": "agent-history-v1",
            "schemaVersion": 1,
            "operation": "status",
            "status": {
                "initialized": true,
                "localOnly": true,
                "futureField": {"enabled": true}
            },
            "futureEnvelopeField": "kept"
        }"#;
        let envelope: AgentHistoryEnvelope = serde_json::from_str(fixture).unwrap();
        let status = envelope.status.unwrap();
        assert_eq!(status.extra["futureField"]["enabled"], true);
        assert_eq!(envelope.extra["futureEnvelopeField"], "kept");
    }

    #[test]
    fn camelizes_private_cli_keys_recursively() {
        let raw = serde_json::json!({
            "payload_type": "search_results",
            "generated_at": "now",
            "results": [{
                "record_type": "event",
                "item_type": "event",
                "ctx_event_id": "event",
                "result_type": "event",
                "result_scope": "event",
                "citations": [{"target_type": "event", "source_path": "/tmp/session.jsonl"}]
            }]
        });
        let camel = camelize_object_keys(&raw);
        assert!(camel.get("payloadType").is_none());
        assert_eq!(camel["generatedAt"], "now");
        assert!(camel["results"][0].get("recordType").is_none());
        assert!(camel["results"][0].get("itemType").is_none());
        assert_eq!(camel["results"][0]["ctxEventId"], "event");
        assert_eq!(camel["results"][0]["resultType"], "event");
        assert_eq!(camel["results"][0]["citations"][0]["targetType"], "event");
        assert_eq!(
            camel["results"][0]["citations"][0]["sourcePath"],
            "/tmp/session.jsonl"
        );
    }

    #[test]
    fn structured_query_canonicalizes_semantic_and_lexical_placements() {
        let query = SearchQuery {
            version: SearchQueryVersion::V1,
            any: vec![
                SearchClause::all("  publication   fence "),
                SearchClause::semantic(" legacy   lookup "),
            ],
            must: vec![SearchClause::phrase("provider visibility")],
            must_not: vec![SearchClause::literal("withheld-row")],
        }
        .canonicalized()
        .unwrap();
        assert_eq!(query.any[0], SearchClause::all("publication fence"));
        assert_eq!(query.any[1], SearchClause::semantic("legacy lookup"));
        assert_eq!(query.canonical_positive_text(), "legacy lookup");
        assert_eq!(SEARCH_POSITIVE_TEXT_RULE_VERSION, "ctx-search-positive-text-v1");

        let mut automatic = SearchQuery::new(vec![SearchClause::phrase("bounded lookup")]);
        automatic.must = vec![SearchClause::all("publication visible")];
        assert_eq!(
            automatic.automatic_rerank_text().as_deref(),
            Some("bounded lookup\npublication visible")
        );
        automatic.must.push(SearchClause::phrase("not eligible"));
        assert_eq!(automatic.automatic_rerank_text(), None);
    }

    #[test]
    fn structured_query_rejects_semantic_hard_filters_and_multiple_semantic_seeds() {
        let mut hard_semantic = SearchQuery::new(vec![SearchClause::all("ctx")]);
        hard_semantic.must = vec![SearchClause::semantic("visibility")];
        assert_eq!(
            hard_semantic.validate(),
            Err(SearchQueryError::SemanticMustBeInAny)
        );

        let multiple = SearchQuery::new(vec![
            SearchClause::semantic("one"),
            SearchClause::semantic("two"),
        ]);
        assert!(matches!(
            multiple.validate(),
            Err(SearchQueryError::TooManySemanticClauses { .. })
        ));
    }

    #[test]
    fn shared_envelope_has_the_approved_hard_maxima() {
        let limits = SearchExecutionLimits::hard_maxima();
        assert_eq!(limits.candidates_per_positive_seed, 1_024);
        assert_eq!(limits.candidate_rows, 16_384);
        assert_eq!(limits.retained_candidate_ids, 8_192);
        assert_eq!(limits.residual_rows, 8_192);
        assert_eq!(limits.verification_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.verification_lookup_bytes, 16 * 1024);
        assert_eq!(limits.hydrated_rows, 256);
        assert_eq!(limits.hydration_input_bytes, 8 * 1024 * 1024);
        assert_eq!(limits.hydration_input_bytes_per_event, 64 * 1024);
        assert_eq!(limits.returned_text_bytes, 512 * 1024);
        assert_eq!(limits.serialized_response_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.results, 200);
        assert_eq!(limits.elapsed_ms, 0, "elapsed time is executor policy");
    }

    #[test]
    fn semantic_input_is_a_bounded_preranked_identity_list() {
        let query = SearchQuery::new(vec![SearchClause::semantic("bounded lookup")]);
        let mut envelope = SearchRequestEnvelope::new(query);
        envelope.semantic = Some(SearchSemanticInput {
            readiness: SearchSemanticReadiness::Ready,
            backend: Some("local-sidecar".to_owned()),
            candidates: (0..=SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED)
                .map(|index| SearchSemanticCandidate {
                    ctx_event_id: format!("event-{index}"),
                })
                .collect(),
            indexed_documents: Some(1_024),
            searchable_documents: Some(1_024),
            coverage_complete: true,
        });
        assert!(matches!(
            envelope.canonicalized(),
            Err(SearchEnvelopeError::TooManySemanticCandidates { .. })
        ));
    }
}
