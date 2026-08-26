use std::collections::HashMap;

use anyhow::{anyhow, Result};
use ctx_history_core::{
    AgentScope, CaptureProvider, EventType, ProviderNativeEventCopy,
    ProviderNativeSessionRelationship, StableEntityId,
};
use ctx_history_index_query::{
    EventRecord, EventSearchCandidate, EventSearchFilters, IndexError, LexicalSearchBatch,
    LexicalSearchError, SearchAgentScope, SearchFamilyKey, SessionGroupingClaims, VerifiedIndex,
    MAX_LEXICAL_QUERY_RESULTS,
};
#[cfg(test)]
use ctx_history_index_query::{LexicalSearchResult, SearchContentScope};
pub use ctx_history_index_query::{SearchDiversificationDecision, SearchDiversificationStatus};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    parse_since_filter, resolve_session_with_refs, CompactRefResolver, HistorySemanticBatch,
    HistorySemanticError, HistorySemanticPort, HistorySemanticQuery, SemanticAvailability,
    SemanticReason,
};

mod active_session;
mod execution_receipt;
mod fusion;
mod request;
mod shaping;

use active_session::excluded_active_session_tree;
#[cfg(test)]
use active_session::{
    proven_active_session_tree_ids, resolved_session_tree_ids,
    resolved_unique_session_tree_root_id, SessionAncestry, MAX_ACTIVE_SESSION_ANCESTORS,
    MAX_ACTIVE_SESSION_TREE_SESSIONS,
};
use active_session::{resolved_manual_session_exclusion_ids, validate_manual_session_exclusions};
pub(crate) use execution_receipt::{collect_search_hits_observed, ObservedSearchExecutionError};
use execution_receipt::{lexical_terminal_state, record_lexical_batch, SearchWorkTracker};
pub use execution_receipt::{
    SearchConcentrationReceipt, SearchCopyClusterAvailability, SearchFailurePhase,
    SearchLiteralRootConcentration, SearchStopReason, SearchWorkReceipt,
};
use fusion::fuse_source_candidates;
#[cfg(test)]
use fusion::weighted_rrf_score;
pub use request::{
    normalize_search_request, resolve_search_backend, unsupported_semantic_scope,
    validate_search_request, ActiveSessionExclusion, NormalizedSearchQuery, SearchBackend,
    SearchPolicy, SearchRequest,
};
use request::{normalized_request_source_identity_filters, unavailable_semantic_error};
pub use shaping::shape_search_result_window;
use shaping::{
    dense_result_window, session_champions, shape_family_result_window, FamilyShapingOutcome,
};

/// Evidence-tunable fixed horizon for one ordinary lexical session search.
const LEXICAL_SESSION_CANDIDATE_HORIZON: usize = 256;
const SOURCE_FUSION_CANDIDATES: usize = 1_600;

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
        .and_then(|active_session| excluded_active_session_tree(index, active_session));
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
    Lexical(#[from] LexicalSearchError),
    #[error(transparent)]
    Application(#[from] anyhow::Error),
}

pub type SearchExecutionResult<T> = std::result::Result<T, SearchExecutionError>;

#[derive(Debug)]
pub struct SearchCollection {
    pub result_window: SearchResultWindow,
    pub candidate_pool: usize,
    pub candidate_pool_truncated: bool,
    pub lexical_diagnostics: Option<SearchLexicalDiagnostics>,
    pub diversification: SearchDiversificationDecision,
    pub concentration: SearchConcentrationReceipt,
    pub requested_backend: SearchBackend,
    pub effective_backend: SearchBackend,
    pub semantic_weight: f32,
    pub semantic_status: &'static str,
    pub semantic_fallback: Option<SemanticFallbackDiagnostics>,
    pub semantic_diagnostics: Option<Value>,
    pub work: SearchWorkReceipt,
    pub stop_reason: Option<SearchStopReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchLexicalDiagnostics {
    pub work_complete: bool,
    pub candidate_set_exhaustive: bool,
    pub exhaustion: Option<SearchLexicalExhaustionDiagnostics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchLexicalExhaustionDiagnostics {
    pub counter: &'static str,
    pub used: u64,
    pub limit: u64,
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
    collect_search_hits_observed(request, index, filters, semantic, semantic_port)
        .map_err(|failure| *failure.error)
}

fn collect_search_hits_with_receipt<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    semantic: SemanticAvailability,
    semantic_port: &P,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<SearchCollection> {
    let prepared = prepare_semantic_search(request, index, filters, semantic, tracker)?;
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
            tracker,
        ),
        Err(error) => collect_prepared_semantic_search(
            request,
            index,
            filters,
            requested_backend,
            normalized_query,
            |_, _, _| Err(error.clone()),
            tracker,
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
    let mut tracker = SearchWorkTracker::new();
    let prepared = prepare_semantic_search(request, index, filters, semantic, &mut tracker)?;
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
        &mut tracker,
    )
}

// Keeping the already-complete bounded result inline avoids a heap allocation
// on ordinary lexical searches; this local enum is not retained across calls.
#[allow(clippy::large_enum_variant)]
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
    tracker: &mut SearchWorkTracker,
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
        let mut collection = collect_lexical_search_hits(
            index,
            &queries,
            request.limit,
            request.events,
            filters,
            tracker,
        )?;
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
            tracker,
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
            tracker,
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
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<SearchCollection> {
    lexical_fallback_with_diagnostics(
        request,
        index,
        filters,
        requested_backend,
        not_ready,
        status,
        Vec::new(),
        tracker,
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
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<SearchCollection> {
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let queries = normalized_query.texts();
    let mut collection = collect_lexical_search_hits(
        index,
        &queries,
        request.limit,
        request.events,
        filters,
        tracker,
    )?;
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
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<SearchCollection>
where
    SemanticSearch: FnMut(
        &str,
        &EventSearchFilters,
        usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>,
{
    tracker.set_phase(SearchFailurePhase::SemanticRetrieval);
    let queries = normalized_query.texts();
    let mut semantic_by_event = HashMap::<StableEntityId, EventSearchCandidate>::new();
    let mut semantic_query_diagnostics = Vec::with_capacity(queries.len());
    for query in &queries {
        tracker.record_retrieval_round()?;
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
                    tracker,
                )
            }
            Err(error) => return Err(error.into()),
        };
        semantic_query_diagnostics.push(json!({
            "diagnostics": diagnostics,
        }));
        for candidate in candidates {
            semantic_by_event
                .entry(candidate.event.event_id)
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
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| {
                left.event
                    .event_id
                    .as_uuid()
                    .cmp(&right.event.event_id.as_uuid())
            })
            .then_with(|| {
                left.event
                    .event_id
                    .digest()
                    .cmp(&right.event.event_id.digest())
            })
    });
    semantic_candidates.truncate(SOURCE_FUSION_CANDIDATES);
    let semantic_candidates_truncated = semantic_candidates.len() == SOURCE_FUSION_CANDIDATES;
    let semantic_diagnostics = json!({
        "query_count": queries.len(),
        "queries": semantic_query_diagnostics,
    });

    let (candidates, lexical_diagnostics, candidate_pool_truncated) =
        if requested_backend == SearchBackend::Semantic {
            (semantic_candidates, None, semantic_candidates_truncated)
        } else {
            tracker.set_phase(SearchFailurePhase::IndexQueryDecode);
            let lexical_batch = record_lexical_batch(
                tracker,
                index.search_event_candidates_any_with_filters_batch_diagnosed(
                    &queries,
                    filters,
                    SOURCE_FUSION_CANDIDATES,
                ),
            )?;
            let lexical_diagnostics = lexical_diagnostics(&lexical_batch);
            let lexical_candidates_truncated = !lexical_batch.candidate_set_exhaustive;
            let lexical_candidates = lexical_batch
                .candidates
                .into_iter()
                .map(Into::into)
                .collect();
            (
                fuse_source_candidates(
                    lexical_candidates,
                    semantic_candidates,
                    request.semantic_weight,
                ),
                Some(lexical_diagnostics),
                lexical_candidates_truncated || semantic_candidates_truncated,
            )
        };
    let candidate_pool = candidates.len();
    tracker.set_phase(SearchFailurePhase::ResultProjection);
    let (result_window, diversification, concentration) = shape_search_candidates_using(
        &candidates,
        request.limit,
        dense_search(request),
        DiversificationCompleteness::BackendUnknown,
        |coordinates| index.session_grouping_claims(coordinates),
    )?;
    Ok(SearchCollection {
        result_window,
        candidate_pool,
        candidate_pool_truncated,
        lexical_diagnostics,
        diversification,
        concentration,
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
        work: tracker.work,
        stop_reason: Some(SearchStopReason::FixedPool),
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
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<SearchCollection> {
    let dense = event_results || filters.session_id.is_some();
    if limit == 0 {
        return Ok(empty_lexical_collection(limit, tracker.work));
    }
    let candidate_limit = lexical_candidate_horizon(limit, dense);
    tracker.set_phase(SearchFailurePhase::IndexQueryDecode);
    let batch = record_lexical_batch(
        tracker,
        if queries.is_empty() {
            index.list_event_candidates_with_filters_batch_diagnosed(filters, candidate_limit)
        } else {
            index.search_event_candidates_any_with_filters_batch_diagnosed(
                queries,
                filters,
                candidate_limit,
            )
        },
    )?;
    tracker.set_phase(SearchFailurePhase::ResultProjection);
    shape_lexical_batch_using(
        batch,
        limit,
        dense,
        |coordinates| index.session_grouping_claims(coordinates),
        tracker.work,
    )
}

#[cfg(test)]
fn collect_lexical_search_hits_using<LexicalSearch, GroupingClaims>(
    limit: usize,
    dense: bool,
    lexical_search: LexicalSearch,
    grouping_claims: GroupingClaims,
) -> SearchExecutionResult<SearchCollection>
where
    LexicalSearch: FnOnce(usize) -> LexicalSearchResult<LexicalSearchBatch>,
    GroupingClaims: FnOnce(
        &[(StableEntityId, StableEntityId)],
    ) -> ctx_history_index_query::Result<Vec<SessionGroupingClaims>>,
{
    if limit == 0 {
        return Ok(empty_lexical_collection(
            limit,
            SearchWorkReceipt::default(),
        ));
    }
    let candidate_limit = lexical_candidate_horizon(limit, dense);
    let batch = lexical_search(candidate_limit)?;
    shape_lexical_batch_using(
        batch,
        limit,
        dense,
        grouping_claims,
        SearchWorkReceipt::default(),
    )
}

fn shape_lexical_batch_using<GroupingClaims>(
    batch: LexicalSearchBatch,
    limit: usize,
    dense: bool,
    grouping_claims: GroupingClaims,
    work: SearchWorkReceipt,
) -> SearchExecutionResult<SearchCollection>
where
    GroupingClaims: FnOnce(
        &[(StableEntityId, StableEntityId)],
    ) -> ctx_history_index_query::Result<Vec<SessionGroupingClaims>>,
{
    let candidate_pool = batch.candidates.len();
    let candidate_pool_truncated = !batch.candidate_set_exhaustive;
    let stop_reason = lexical_terminal_state(&batch);
    let completeness = DiversificationCompleteness::Lexical {
        work_complete: batch.complete,
        candidate_set_exhaustive: batch.candidate_set_exhaustive,
    };
    let lexical_diagnostics = lexical_diagnostics(&batch);
    let candidates = batch
        .candidates
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    let (mut result_window, diversification, concentration) =
        shape_search_candidates_using(&candidates, limit, dense, completeness, grouping_claims)?;
    if dense && batch.complete && !batch.candidate_set_exhaustive && candidate_pool == limit {
        // At the maximum retained horizon, completed heap truncation proves an
        // additional event even though no lookahead slot can be retained.
        result_window.more_available = true;
    }
    Ok(SearchCollection {
        result_window,
        candidate_pool,
        candidate_pool_truncated,
        lexical_diagnostics: Some(lexical_diagnostics),
        diversification,
        concentration,
        requested_backend: SearchBackend::Lexical,
        effective_backend: SearchBackend::Lexical,
        semantic_weight: 0.0,
        semantic_status: "skipped",
        semantic_fallback: None,
        semantic_diagnostics: None,
        work,
        stop_reason,
    })
}

fn empty_lexical_collection(limit: usize, work: SearchWorkReceipt) -> SearchCollection {
    let concentration = SearchConcentrationReceipt {
        distinct_sessions: 0,
        largest_session_candidate_count: 0,
        provider_copy_candidate_count: 0,
        literal_roots: SearchLiteralRootConcentration::Observed {
            distinct_families: 0,
            candidate_count: 0,
            largest_family_candidate_count: 0,
        },
        copy_clusters: SearchCopyClusterAvailability::NotConstructedV1,
    };
    SearchCollection {
        result_window: SearchResultWindow {
            limit,
            hits: Vec::new(),
            more_available: false,
        },
        candidate_pool: 0,
        candidate_pool_truncated: false,
        lexical_diagnostics: None,
        diversification: SearchDiversificationDecision {
            status: SearchDiversificationStatus::NotApplicable,
            top_n: limit,
            changed_final_top_n: None,
        },
        concentration,
        requested_backend: SearchBackend::Lexical,
        effective_backend: SearchBackend::Lexical,
        semantic_weight: 0.0,
        semantic_status: "skipped",
        semantic_fallback: None,
        semantic_diagnostics: None,
        work,
        stop_reason: None,
    }
}

fn lexical_candidate_horizon(limit: usize, dense: bool) -> usize {
    let lookahead = limit.saturating_add(1);
    if dense {
        lookahead.min(MAX_LEXICAL_QUERY_RESULTS)
    } else {
        lookahead.clamp(LEXICAL_SESSION_CANDIDATE_HORIZON, MAX_LEXICAL_QUERY_RESULTS)
    }
}

#[derive(Debug, Clone, Copy)]
enum DiversificationCompleteness {
    Lexical {
        work_complete: bool,
        candidate_set_exhaustive: bool,
    },
    BackendUnknown,
}

fn shape_search_candidates_using<GroupingClaims>(
    candidates: &[EventSearchCandidate],
    limit: usize,
    dense: bool,
    completeness: DiversificationCompleteness,
    grouping_claims: GroupingClaims,
) -> SearchExecutionResult<(
    SearchResultWindow,
    SearchDiversificationDecision,
    SearchConcentrationReceipt,
)>
where
    GroupingClaims: FnOnce(
        &[(StableEntityId, StableEntityId)],
    ) -> ctx_history_index_query::Result<Vec<SessionGroupingClaims>>,
{
    if dense || limit == 0 {
        let champions = session_champions(candidates);
        return Ok((
            dense_result_window(candidates, limit),
            SearchDiversificationDecision {
                status: SearchDiversificationStatus::NotApplicable,
                top_n: limit,
                changed_final_top_n: None,
            },
            SearchConcentrationReceipt {
                distinct_sessions: u32::try_from(champions.len())
                    .map_err(|_| anyhow!("search session concentration overflow"))?,
                largest_session_candidate_count: u32::try_from(
                    champions
                        .iter()
                        .map(|champion| champion.match_count)
                        .max()
                        .unwrap_or(0),
                )
                .map_err(|_| anyhow!("search session concentration overflow"))?,
                provider_copy_candidate_count: u32::try_from(
                    candidates
                        .iter()
                        .filter(|candidate| candidate.event.event_copy.is_some())
                        .count(),
                )
                .map_err(|_| anyhow!("search copy concentration overflow"))?,
                literal_roots: if dense {
                    SearchLiteralRootConcentration::NotObservedDense
                } else {
                    SearchLiteralRootConcentration::Observed {
                        distinct_families: 0,
                        candidate_count: 0,
                        largest_family_candidate_count: 0,
                    }
                },
                copy_clusters: SearchCopyClusterAvailability::NotConstructedV1,
            },
        ));
    }

    let champions = session_champions(candidates);
    let coordinates = champions
        .iter()
        .map(|champion| {
            (
                champion.candidate.event.session_id,
                champion.candidate.event.source.identity(),
            )
        })
        .collect::<Vec<_>>();
    let claims = grouping_claims(&coordinates)?;
    validate_grouping_claims(&coordinates, &claims)?;
    let families = claims.iter().map(SearchFamilyKey::from).collect::<Vec<_>>();
    let FamilyShapingOutcome {
        result_window,
        distinct_families,
        distinct_literal_root_families,
        literal_root_candidate_count,
        largest_literal_root_candidate_count,
        changed_final_top_n,
    } = shape_family_result_window(&champions, &families, limit);
    let status = match completeness {
        DiversificationCompleteness::Lexical {
            work_complete: true,
            candidate_set_exhaustive,
        } if candidate_set_exhaustive || distinct_families >= limit => {
            SearchDiversificationStatus::Applied
        }
        DiversificationCompleteness::Lexical { .. }
        | DiversificationCompleteness::BackendUnknown => SearchDiversificationStatus::Indeterminate,
    };
    Ok((
        result_window,
        SearchDiversificationDecision {
            status,
            top_n: limit,
            changed_final_top_n: (status == SearchDiversificationStatus::Applied)
                .then_some(changed_final_top_n),
        },
        SearchConcentrationReceipt {
            distinct_sessions: u32::try_from(champions.len())
                .map_err(|_| anyhow!("search session concentration overflow"))?,
            largest_session_candidate_count: u32::try_from(
                champions
                    .iter()
                    .map(|champion| champion.match_count)
                    .max()
                    .unwrap_or(0),
            )
            .map_err(|_| anyhow!("search session concentration overflow"))?,
            provider_copy_candidate_count: u32::try_from(
                candidates
                    .iter()
                    .filter(|candidate| candidate.event.event_copy.is_some())
                    .count(),
            )
            .map_err(|_| anyhow!("search copy concentration overflow"))?,
            literal_roots: SearchLiteralRootConcentration::Observed {
                distinct_families: u32::try_from(distinct_literal_root_families)
                    .map_err(|_| anyhow!("search root concentration overflow"))?,
                candidate_count: u32::try_from(literal_root_candidate_count)
                    .map_err(|_| anyhow!("search root concentration overflow"))?,
                largest_family_candidate_count: u32::try_from(largest_literal_root_candidate_count)
                    .map_err(|_| anyhow!("search root concentration overflow"))?,
            },
            copy_clusters: SearchCopyClusterAvailability::NotConstructedV1,
        },
    ))
}

fn validate_grouping_claims(
    coordinates: &[(StableEntityId, StableEntityId)],
    claims: &[SessionGroupingClaims],
) -> ctx_history_index_query::Result<()> {
    if coordinates.len() != claims.len()
        || coordinates
            .iter()
            .zip(claims)
            .any(|(&(session_id, source_owner), claims)| {
                claims.session_id != session_id || claims.source_owner != source_owner
            })
    {
        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
    }
    Ok(())
}

fn lexical_diagnostics(batch: &LexicalSearchBatch) -> SearchLexicalDiagnostics {
    SearchLexicalDiagnostics {
        work_complete: batch.complete,
        candidate_set_exhaustive: batch.candidate_set_exhaustive,
        exhaustion: batch.exhaustion.as_ref().map(|exhaustion| {
            SearchLexicalExhaustionDiagnostics {
                counter: exhaustion.counter.as_str(),
                used: exhaustion.used,
                limit: exhaustion.limit,
            }
        }),
    }
}

fn dense_search(request: &SearchRequest) -> bool {
    request.events || request.session.is_some()
}

#[cfg(test)]
mod tests;
