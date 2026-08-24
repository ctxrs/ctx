use std::{collections::BTreeMap, fmt, path::PathBuf, str::FromStr};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    AgentScope, CaptureProvider, EventType, ProviderNativeEventCopy,
    ProviderNativeSessionRelationship,
};
use ctx_history_index_query::{
    EventRecord, EventSearchCandidate, EventSearchFilters, IndexError, SearchAgentScope,
    SearchContentScope, VerifiedIndex, LEXICAL_QUERY_LIMITS, MAX_LEXICAL_QUERY_RESULTS,
};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    normalize_source_identity_filters, parse_since_filter, resolve_session_with_refs,
    CompactRefResolver, HistorySemanticBatch, HistorySemanticError, HistorySemanticPort,
    HistorySemanticQuery, SemanticAvailability, SemanticReason, SourceIdentityFilterArgs,
    SourceIdentityFilters,
};

mod active_session;
mod fusion;
mod shaping;

use active_session::excluded_active_session_tree;
use active_session::{
    normalize_manual_session_exclusions, resolved_manual_session_exclusion_ids,
    validate_manual_session_exclusions,
};
#[cfg(test)]
use active_session::{
    resolved_session_tree_ids, resolved_unique_session_tree_root_id, SessionAncestry,
    MAX_ACTIVE_SESSION_ANCESTORS, MAX_ACTIVE_SESSION_TREE_SESSIONS,
};
#[cfg(test)]
use fusion::weighted_rrf_score;
use fusion::{fuse_source_candidates, search_candidate_order};
use shaping::root_first_candidate_pool_is_decisive;
pub use shaping::shape_search_result_window;

const MAX_ROOT_DIVERSITY_CANDIDATES: usize = 64 * 1024;
const MIN_CANDIDATE_BATCH: usize = 256;
const CANDIDATE_OVERSAMPLE: usize = 8;
const SOURCE_FUSION_CANDIDATES: usize = 1_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackend {
    Hybrid,
    Lexical,
    Semantic,
}

impl SearchBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }
}

impl fmt::Display for SearchBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SearchBackend {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "hybrid" => Ok(Self::Hybrid),
            "lexical" => Ok(Self::Lexical),
            "semantic" => Ok(Self::Semantic),
            other => Err(format!(
                "invalid search backend {other:?}; expected hybrid, lexical, or semantic"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    pub terms: Vec<String>,
    pub limit: usize,
    pub provider: Option<CaptureProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub source_roots: Vec<String>,
    pub source_groups: Vec<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub content_scope: SearchContentScope,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub exclude_sessions: Vec<String>,
    pub events: bool,
    pub include_current_session: bool,
    pub backend: Option<SearchBackend>,
    pub semantic_weight: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSessionExclusion {
    pub provider: String,
    pub provider_session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPolicy {
    pub default_backend: SearchBackend,
    pub semantic: SemanticAvailability,
}

impl SearchPolicy {
    pub const fn lexical_only(reason: SemanticReason) -> Self {
        Self {
            default_backend: SearchBackend::Lexical,
            semantic: SemanticAvailability::Unavailable(reason),
        }
    }

    pub const fn semantic_available() -> Self {
        Self {
            default_backend: SearchBackend::Hybrid,
            semantic: SemanticAvailability::Available,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSearchQuery {
    positional: Option<String>,
    terms: Vec<String>,
    alternatives: Vec<String>,
    display: String,
}

impl NormalizedSearchQuery {
    pub fn from_request(request: &SearchRequest) -> Self {
        let positional = normalized_query_alternative(&request.query);
        let terms = request
            .terms
            .iter()
            .filter_map(|term| normalized_query_alternative(term))
            .collect::<Vec<_>>();
        let alternatives = positional
            .iter()
            .chain(terms.iter())
            .cloned()
            .collect::<Vec<_>>();
        let display = alternatives.join(" OR ");
        Self {
            positional,
            terms,
            alternatives,
            display,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.alternatives.is_empty()
    }

    pub fn texts(&self) -> Vec<&str> {
        self.alternatives.iter().map(String::as_str).collect()
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn positional(&self) -> Option<&str> {
        self.positional.as_deref()
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}

fn normalized_query_alternative(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub fn validate_search_request(request: &SearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    validate_manual_session_exclusions(request)?;
    validate_provider_root_selectors(&request.source_roots, "source root")?;
    validate_provider_root_selectors(&request.source_groups, "source group")?;
    if request
        .workspace
        .as_deref()
        .is_some_and(|workspace| workspace.trim().is_empty())
    {
        return Err(anyhow!("query filter workspace is empty"));
    }
    if request
        .file
        .as_ref()
        .is_some_and(|file| file.to_str().is_some_and(|file| file.trim().is_empty()))
    {
        return Err(anyhow!("query filter file is empty"));
    }
    let source_identity = normalized_request_source_identity_filters(request)?;
    if !source_identity.is_empty()
        && request
            .provider
            .is_some_and(|provider| provider != CaptureProvider::Custom)
    {
        return Err(crate::SourceIdentityFilterError::CustomProviderRequired.into());
    }
    let has_query = !NormalizedSearchQuery::from_request(request).is_empty();
    if !has_query && request.file.is_none() {
        return Err(anyhow!("source-backed search needs a non-empty text query"));
    }
    if !has_query
        && request
            .backend
            .is_some_and(|backend| backend != SearchBackend::Lexical)
    {
        return Err(anyhow!(
            "semantic and hybrid search need a non-empty text query"
        ));
    }
    Ok(())
}

pub fn normalize_search_request(request: &mut SearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    normalize_manual_session_exclusions(request)?;
    normalize_provider_root_selectors(&mut request.source_roots, "source root")?;
    normalize_provider_root_selectors(&mut request.source_groups, "source group")?;
    if request.workspace.is_some() {
        request.workspace = normalized_optional_text(request.workspace.as_deref())
            .map(Some)
            .ok_or_else(|| anyhow!("query filter workspace is empty"))?;
    }
    if let Some(file) = request.file.as_ref().and_then(|file| file.to_str()) {
        let file = normalized_optional_text(Some(file))
            .ok_or_else(|| anyhow!("query filter file is empty"))?;
        request.file = Some(PathBuf::from(file));
    }
    Ok(())
}

fn normalize_provider_root_selectors(values: &mut Vec<String>, kind: &str) -> Result<()> {
    for value in values.iter_mut() {
        *value = value.trim().to_owned();
    }
    values.sort();
    values.dedup();
    validate_provider_root_selectors(values, kind)
}

fn validate_provider_root_selectors(values: &[String], kind: &str) -> Result<()> {
    if values.len() > 64 {
        return Err(anyhow!("{kind} selectors exceed the maximum of 64"));
    }
    if values.iter().any(|value| {
        value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }) {
        return Err(anyhow!(
            "invalid {kind} selector; expected 1..=64 ASCII letters, digits, hyphens, or underscores"
        ));
    }
    Ok(())
}

fn validate_lexical_query_limits(request: &SearchRequest) -> Result<()> {
    let positional = (!request.query.is_empty()).then_some(request.query.as_str());
    LEXICAL_QUERY_LIMITS.validate_texts(
        positional
            .into_iter()
            .chain(request.terms.iter().map(String::as_str)),
    )?;
    Ok(())
}

fn normalized_request_source_identity_filters(
    request: &SearchRequest,
) -> Result<SourceIdentityFilters> {
    normalize_source_identity_filters(SourceIdentityFilterArgs {
        history_source: request.history_source.clone(),
        provider_key: request.provider_key.clone(),
        source_id: request.source_id.clone(),
        source_format: request.source_format.clone(),
    })
}

pub fn resolve_search_backend(
    request: &SearchRequest,
    policy: SearchPolicy,
) -> std::result::Result<SearchBackend, HistorySemanticError> {
    if request.backend.is_none()
        && NormalizedSearchQuery::from_request(request).is_empty()
        && request.file.is_some()
    {
        return Ok(SearchBackend::Lexical);
    }
    if request.backend == Some(SearchBackend::Semantic) {
        if let Some(not_ready) = unsupported_semantic_scope(request) {
            return Err(not_ready);
        }
    }
    match request.backend {
        Some(SearchBackend::Semantic)
            if matches!(policy.semantic, SemanticAvailability::Unavailable(_)) =>
        {
            let SemanticAvailability::Unavailable(reason) = policy.semantic else {
                unreachable!("guard requires unavailable semantic policy")
            };
            Err(unavailable_semantic_error(reason))
        }
        Some(value) => Ok(value),
        None => Ok(policy.default_backend),
    }
}

pub fn unsupported_semantic_scope(request: &SearchRequest) -> Option<HistorySemanticError> {
    let content_scope = match request.content_scope {
        SearchContentScope::Calls => Some("calls"),
        SearchContentScope::Outputs => Some("outputs"),
        SearchContentScope::All | SearchContentScope::Transcript => None,
    };
    if let Some(content_scope) = content_scope {
        return Some(HistorySemanticError::not_ready(
            SemanticReason::ContentScopeUnsupported,
            format!("semantic retrieval does not support content scope '{content_scope}'"),
            false,
        ));
    }

    let event_type = request
        .event_type
        .as_deref()
        .and_then(|value| value.parse::<EventType>().ok())
        .filter(|event_type| *event_type != EventType::Message)?;
    Some(HistorySemanticError::not_ready(
        SemanticReason::EventTypeUnsupported,
        format!(
            "semantic retrieval does not support event type '{}'",
            event_type.as_str()
        ),
        false,
    ))
}

fn unavailable_semantic_error(reason: SemanticReason) -> HistorySemanticError {
    let detail = match reason {
        SemanticReason::PolicyDisabled => "semantic retrieval is disabled by policy",
        SemanticReason::PlatformUnsupported => {
            "semantic retrieval is unavailable for this execution capability"
        }
        SemanticReason::ExecutionUnavailable => {
            "semantic retrieval execution is unavailable by policy"
        }
        _ => "semantic retrieval is unavailable",
    };
    HistorySemanticError::not_ready(reason, detail, false)
}

pub fn search_filters(
    request: &SearchRequest,
    index: &VerifiedIndex,
    active_session: Option<&ActiveSessionExclusion>,
) -> Result<EventSearchFilters> {
    let references = CompactRefResolver::new(index, None);
    search_filters_with_refs(request, index, &references, active_session)
}

pub fn search_filters_with_refs(
    request: &SearchRequest,
    index: &VerifiedIndex,
    references: &CompactRefResolver<'_>,
    active_session: Option<&ActiveSessionExclusion>,
) -> Result<EventSearchFilters> {
    validate_manual_session_exclusions(request)?;
    let source_identity = normalized_request_source_identity_filters(request)?;
    let session_id = request
        .session
        .as_deref()
        .map(|id| {
            resolve_session_with_refs(references, id).map(|session| session.session_id.as_uuid())
        })
        .transpose()?;
    let excluded_session_ids = resolved_manual_session_exclusion_ids(request, references)?;
    let event_type = request
        .event_type
        .as_deref()
        .map(|value| {
            value
                .parse::<EventType>()
                .map(|event_type| event_type.as_str().to_owned())
                .map_err(|error| anyhow!("{error}"))
        })
        .transpose()?;
    let since_unix_ms = request
        .since
        .as_deref()
        .map(parse_since_filter)
        .transpose()?
        .map(|since| since.timestamp_millis());
    let exclude_session_tree = (!request.include_current_session && session_id.is_none())
        .then_some(active_session)
        .flatten()
        .map(|active_session| excluded_active_session_tree(index, active_session))
        .transpose()?;
    let allowed_source_keys = (!request.source_roots.is_empty()
        || !request.source_groups.is_empty())
    .then(|| {
        index
            .manifest()
            .provider_root_source_tokens(&request.source_roots, &request.source_groups)
            .map_err(anyhow::Error::from)
    })
    .transpose()?;
    Ok(EventSearchFilters {
        allowed_source_keys,
        session_id,
        provider: request
            .provider
            .or_else(|| (!source_identity.is_empty()).then_some(CaptureProvider::Custom))
            .map(|provider| provider.as_str().to_owned()),
        history_source: source_identity.history_source,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        workspace: normalized_optional_text(request.workspace.as_deref()),
        since_unix_ms,
        content_scope: request.content_scope,
        event_type,
        agent_scope: search_agent_scope(request, session_id),
        file: request
            .file
            .as_ref()
            .and_then(|path| normalized_optional_text(Some(&path.display().to_string()))),
        excluded_session_ids,
        exclude_session_tree,
        ..EventSearchFilters::default()
    })
}

fn search_agent_scope(request: &SearchRequest, _session_id: Option<Uuid>) -> SearchAgentScope {
    // Exact session selection remains authoritative under the default all-agent
    // policy. The explicit primary-only control is the sole narrower scope.
    if request.primary_only {
        SearchAgentScope::Primary
    } else {
        SearchAgentScope::All
    }
}

fn normalized_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Error)]
pub enum SearchExecutionError {
    #[error(transparent)]
    Semantic(#[from] HistorySemanticError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Application(#[from] anyhow::Error),
}

pub type SearchExecutionResult<T> = std::result::Result<T, SearchExecutionError>;

#[derive(Debug)]
pub struct SearchCollection {
    pub result_window: SearchResultWindow,
    pub candidate_pool: usize,
    pub candidate_pool_truncated: bool,
    pub requested_backend: SearchBackend,
    pub effective_backend: SearchBackend,
    pub semantic_weight: f32,
    pub semantic_status: &'static str,
    pub semantic_fallback: Option<SemanticFallbackDiagnostics>,
    pub semantic_diagnostics: Option<Value>,
}

#[derive(Debug)]
pub struct SearchResultWindow {
    pub limit: usize,
    pub hits: Vec<SearchHit>,
    pub more_available: bool,
}

#[derive(Debug, Clone)]
pub struct SemanticFallbackDiagnostics {
    pub reason: Option<SemanticReason>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub event: SearchEventMetadata,
    pub score: f32,
    pub more_matches_in_session: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEventMetadata {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    pub event_copy: Option<ProviderNativeEventCopy>,
    pub provider: String,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub agent_scope: Option<AgentScope>,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
}

impl From<&EventRecord> for SearchEventMetadata {
    fn from(event: &EventRecord) -> Self {
        let (provider_key, source_id) = event
            .custom_source_identity()
            .map_or((None, None), |(provider_key, source_id)| {
                (Some(provider_key.to_owned()), Some(source_id.to_owned()))
            });
        Self {
            event_id: event.event_id.as_uuid(),
            session_id: event.session_id.as_uuid(),
            parent_session_id: event.parent_session_id.map(|id| id.as_uuid()),
            root_session_id: event.root_session_id.map(|id| id.as_uuid()),
            session_relationship: event.session_relationship,
            event_copy: event.event_copy.clone(),
            provider: event.provider.clone(),
            provider_key,
            source_id,
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            agent_scope: event.agent_scope,
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
            event_type: event.event_type.clone(),
            role: event.role.clone(),
        }
    }
}

pub fn collect_search_hits<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    semantic: SemanticAvailability,
    semantic_port: &P,
) -> SearchExecutionResult<SearchCollection> {
    let prepared = prepare_semantic_search(request, index, filters, semantic)?;
    let (requested_backend, normalized_query) = match prepared {
        PreparedSemanticSearch::Complete(collection) => return Ok(collection),
        PreparedSemanticSearch::Query {
            requested_backend,
            normalized_query,
        } => (requested_backend, normalized_query),
    };

    match semantic_port.begin_query(index) {
        Ok(mut semantic_query) => collect_prepared_semantic_search(
            request,
            index,
            filters,
            requested_backend,
            normalized_query,
            |query, filters, candidate_limit| {
                semantic_query.candidates(query, filters, candidate_limit)
            },
        ),
        Err(error) => collect_prepared_semantic_search(
            request,
            index,
            filters,
            requested_backend,
            normalized_query,
            |_, _, _| Err(error.clone()),
        ),
    }
}

pub fn collect_search_hits_using<SemanticSearch>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    semantic: SemanticAvailability,
    semantic_search: SemanticSearch,
) -> SearchExecutionResult<SearchCollection>
where
    SemanticSearch: FnMut(
        &str,
        &EventSearchFilters,
        usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>,
{
    let prepared = prepare_semantic_search(request, index, filters, semantic)?;
    let (requested_backend, normalized_query) = match prepared {
        PreparedSemanticSearch::Complete(collection) => return Ok(collection),
        PreparedSemanticSearch::Query {
            requested_backend,
            normalized_query,
        } => (requested_backend, normalized_query),
    };
    collect_prepared_semantic_search(
        request,
        index,
        filters,
        requested_backend,
        normalized_query,
        semantic_search,
    )
}

enum PreparedSemanticSearch {
    Complete(SearchCollection),
    Query {
        requested_backend: SearchBackend,
        normalized_query: NormalizedSearchQuery,
    },
}

fn prepare_semantic_search(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    semantic: SemanticAvailability,
) -> SearchExecutionResult<PreparedSemanticSearch> {
    let requested_backend = request.backend.unwrap_or(SearchBackend::Lexical);
    let semantic_weight = request.semantic_weight;
    if !semantic_weight.is_finite() || !(0.0..=1.0).contains(&semantic_weight) {
        return Err(anyhow!("semantic weight must be finite and between 0.0 and 1.0").into());
    }
    if requested_backend == SearchBackend::Lexical
        || (requested_backend == SearchBackend::Hybrid && semantic_weight == 0.0)
    {
        let normalized_query = NormalizedSearchQuery::from_request(request);
        let queries = normalized_query.texts();
        let mut collection =
            collect_lexical_search_hits(index, &queries, request.limit, request.events, filters)?;
        collection.requested_backend = requested_backend;
        collection.semantic_weight = 0.0;
        return Ok(PreparedSemanticSearch::Complete(collection));
    }
    if let Some(not_ready) = unsupported_semantic_scope(request) {
        if requested_backend == SearchBackend::Semantic {
            return Err(not_ready.into());
        }
        return lexical_fallback(
            request,
            index,
            filters,
            requested_backend,
            not_ready,
            "unsupported",
        )
        .map(PreparedSemanticSearch::Complete);
    }
    if let SemanticAvailability::Unavailable(reason) = semantic {
        let not_ready = unavailable_semantic_error(reason);
        if requested_backend == SearchBackend::Semantic {
            return Err(not_ready.into());
        }
        let status = match reason {
            SemanticReason::PolicyDisabled => "disabled",
            SemanticReason::ContentScopeUnsupported
            | SemanticReason::EventTypeUnsupported
            | SemanticReason::PlatformUnsupported => "unsupported",
            _ => "unavailable",
        };
        return lexical_fallback(
            request,
            index,
            filters,
            requested_backend,
            not_ready,
            status,
        )
        .map(PreparedSemanticSearch::Complete);
    }

    Ok(PreparedSemanticSearch::Query {
        requested_backend,
        normalized_query: NormalizedSearchQuery::from_request(request),
    })
}

fn lexical_fallback(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    requested_backend: SearchBackend,
    not_ready: HistorySemanticError,
    status: &'static str,
) -> SearchExecutionResult<SearchCollection> {
    lexical_fallback_with_diagnostics(
        request,
        index,
        filters,
        requested_backend,
        not_ready,
        status,
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn lexical_fallback_with_diagnostics(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    requested_backend: SearchBackend,
    not_ready: HistorySemanticError,
    status: &'static str,
    semantic_query_diagnostics: Vec<Value>,
) -> SearchExecutionResult<SearchCollection> {
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let queries = normalized_query.texts();
    let mut collection =
        collect_lexical_search_hits(index, &queries, request.limit, request.events, filters)?;
    let fallback = semantic_fallback_diagnostics(&not_ready);
    collection.requested_backend = requested_backend;
    collection.effective_backend = SearchBackend::Lexical;
    collection.semantic_weight = if status == "unsupported" {
        0.0
    } else {
        request.semantic_weight
    };
    collection.semantic_status = status;
    collection.semantic_fallback = Some(fallback.clone());
    collection.semantic_diagnostics = Some(json!({
        "query_count": queries.len(),
        "queries": semantic_query_diagnostics,
        "fallback": {
            "reason": format!("{:?}", fallback.reason),
            "detail": fallback.detail,
        },
    }));
    Ok(collection)
}

#[allow(clippy::too_many_arguments)]
fn collect_prepared_semantic_search<SemanticSearch>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    requested_backend: SearchBackend,
    normalized_query: NormalizedSearchQuery,
    mut semantic_search: SemanticSearch,
) -> SearchExecutionResult<SearchCollection>
where
    SemanticSearch: FnMut(
        &str,
        &EventSearchFilters,
        usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>,
{
    let queries = normalized_query.texts();
    let mut semantic_by_event = BTreeMap::<Uuid, EventSearchCandidate>::new();
    let mut semantic_query_diagnostics = Vec::with_capacity(queries.len());
    for query in &queries {
        let HistorySemanticBatch {
            candidates,
            diagnostics,
        } = match semantic_search(query, filters, SOURCE_FUSION_CANDIDATES) {
            Ok(value) => value,
            Err(error) if requested_backend == SearchBackend::Hybrid => {
                return lexical_fallback_with_diagnostics(
                    request,
                    index,
                    filters,
                    requested_backend,
                    error,
                    "unavailable",
                    semantic_query_diagnostics,
                )
            }
            Err(error) => return Err(error.into()),
        };
        semantic_query_diagnostics.push(json!({
            "query": query,
            "diagnostics": diagnostics,
        }));
        for candidate in candidates {
            semantic_by_event
                .entry(candidate.event.event_id.as_uuid())
                .and_modify(|existing| {
                    if candidate.score > existing.score {
                        *existing = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }
    }
    let mut semantic_candidates = semantic_by_event.into_values().collect::<Vec<_>>();
    semantic_candidates.sort_by(|left, right| {
        right.score.total_cmp(&left.score).then_with(|| {
            left.event
                .event_id
                .as_uuid()
                .cmp(&right.event.event_id.as_uuid())
        })
    });
    semantic_candidates.truncate(SOURCE_FUSION_CANDIDATES);
    let semantic_diagnostics = json!({
        "query_count": queries.len(),
        "queries": semantic_query_diagnostics,
    });

    let candidates = if requested_backend == SearchBackend::Semantic {
        semantic_candidates
    } else {
        let lexical_candidates = index.search_event_candidates_any_with_filters(
            &queries,
            filters,
            SOURCE_FUSION_CANDIDATES,
        )?;
        fuse_source_candidates(
            lexical_candidates,
            semantic_candidates,
            request.semantic_weight,
        )
    };
    let candidate_pool = candidates.len();
    let result_window =
        shape_search_result_window(candidates.iter(), request.limit, request.events);
    Ok(SearchCollection {
        result_window,
        candidate_pool,
        candidate_pool_truncated: candidate_pool >= SOURCE_FUSION_CANDIDATES,
        requested_backend,
        effective_backend: requested_backend,
        semantic_weight: if requested_backend == SearchBackend::Semantic {
            1.0
        } else {
            request.semantic_weight
        },
        semantic_status: "ready",
        semantic_fallback: None,
        semantic_diagnostics: Some(semantic_diagnostics),
    })
}

fn semantic_fallback_diagnostics(error: &HistorySemanticError) -> SemanticFallbackDiagnostics {
    SemanticFallbackDiagnostics {
        reason: error.reason(),
        detail: error.detail().to_owned(),
    }
}

fn collect_lexical_search_hits(
    index: &VerifiedIndex,
    queries: &[&str],
    limit: usize,
    event_results: bool,
    filters: &EventSearchFilters,
) -> Result<SearchCollection> {
    let document_count = usize::try_from(index.document_count()).unwrap_or(usize::MAX);
    let maximum = document_count
        .min(MAX_ROOT_DIVERSITY_CANDIDATES)
        .min(MAX_LEXICAL_QUERY_RESULTS);
    let mut candidate_limit = limit
        .saturating_mul(CANDIDATE_OVERSAMPLE)
        .max(MIN_CANDIDATE_BATCH)
        .min(maximum.max(1));
    loop {
        let candidates = if queries.is_empty() {
            index.list_event_candidates_with_filters(filters, candidate_limit)?
        } else {
            index.search_event_candidates_any_with_filters(queries, filters, candidate_limit)?
        };
        let source_candidate_pool = candidates.len();
        let exhausted =
            source_candidate_pool < candidate_limit || candidate_limit >= document_count;
        let source_tail_score = candidates
            .iter()
            .map(|candidate| candidate.score)
            .min_by(f32::total_cmp);
        let result_window = shape_search_result_window(candidates.iter(), limit, event_results);
        let decisive_window = result_window.more_available
            && (event_results
                || source_tail_score.is_some_and(|tail_score| {
                    root_first_candidate_pool_is_decisive(&candidates, limit, tail_score)
                }));
        if decisive_window || exhausted {
            return Ok(SearchCollection {
                result_window,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: false,
                requested_backend: SearchBackend::Lexical,
                effective_backend: SearchBackend::Lexical,
                semantic_weight: 0.0,
                semantic_status: "skipped",
                semantic_fallback: None,
                semantic_diagnostics: None,
            });
        }
        if candidate_limit >= maximum {
            return Ok(SearchCollection {
                result_window,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: true,
                requested_backend: SearchBackend::Lexical,
                effective_backend: SearchBackend::Lexical,
                semantic_weight: 0.0,
                semantic_status: "skipped",
                semantic_fallback: None,
                semantic_diagnostics: None,
            });
        }
        candidate_limit = candidate_limit
            .saturating_mul(2)
            .min(maximum)
            .max(candidate_limit.saturating_add(1));
    }
}

#[cfg(test)]
mod tests;
