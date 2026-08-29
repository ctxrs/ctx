use anyhow::{anyhow, Result};
use ctx_history_core::{
    AgentScope, CaptureProvider, EventType, ProviderNativeEventCopy,
    ProviderNativeSessionRelationship,
};
#[cfg(test)]
use ctx_history_index_query::SearchContentScope;
use ctx_history_index_query::{
    CompiledSearchFilter, EventRecord, EventSearchCandidate, EventSearchFilters, IndexError,
    LexicalExecution, LexicalMode, LexicalSearchBatch, RankedEventRef, SearchAgentScope,
    SearchFamilyKey, SearchSessionCoordinate, SessionGroupingClaims, VerifiedIndex,
    MAX_LEXICAL_QUERY_RESULTS,
};
pub use ctx_history_index_query::{SearchDiversificationDecision, SearchDiversificationStatus};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    parse_since_filter, resolve_session_with_refs, CompactRefResolver, HistorySemanticError,
    HistorySemanticPort, SemanticAvailability, SemanticReason,
};

mod active_session;
mod execution_receipt;
mod fusion;
mod request;
mod semantic_batch;
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
pub use execution_receipt::{SearchFailurePhase, SearchStopReason, SearchWorkReceipt};
use fusion::fuse_source_candidates;
#[cfg(test)]
use fusion::weighted_rrf_score;
pub use request::{
    normalize_search_request, resolve_search_backend, unsupported_semantic_scope,
    validate_search_request, ActiveSessionExclusion, NormalizedSearchQuery, SearchBackend,
    SearchPolicy, SearchRequest,
};
use request::{normalized_request_source_identity_filters, unavailable_semantic_error};
use semantic_batch::{collect_prepared_semantic_query, semantic_retrieval_failure};
use shaping::{
    dense_result_window, session_champions_by, shape_family_result_window, FamilyShapingOutcome,
};

/// Evidence-tunable fixed horizon for one ordinary lexical session search.
const LEXICAL_SESSION_CANDIDATE_HORIZON: usize = 256;
const SOURCE_FUSION_CANDIDATES: usize = 1_600;

pub(crate) fn search_filters_with_refs(
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
    Application(#[from] anyhow::Error),
}

pub type SearchExecutionResult<T> = std::result::Result<T, SearchExecutionError>;

#[derive(Debug)]
pub struct SearchCollection<Event = SearchEventMetadata> {
    pub result_window: SearchResultWindow<Event>,
    pub candidate_pool: usize,
    pub candidate_pool_truncated: bool,
    pub lexical_diagnostics: Option<SearchLexicalDiagnostics>,
    pub diversification: SearchDiversificationDecision,
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
pub struct SearchResultWindow<Event = SearchEventMetadata> {
    pub limit: usize,
    pub hits: Vec<SearchHit<Event>>,
    pub more_available: bool,
}

#[derive(Debug, Clone)]
pub struct SemanticFallbackDiagnostics {
    pub code: Option<&'static str>,
    pub reason: Option<SemanticReason>,
    pub detail: String,
    pub retryable: bool,
}

#[derive(Debug, Clone)]
pub struct SearchHit<Event = SearchEventMetadata> {
    pub event: Event,
    pub score: f32,
    pub more_matches_in_session: usize,
}

pub(crate) type RankedSearchCollection = SearchCollection<RankedEventRef>;
pub(crate) type RankedSearchResultWindow = SearchResultWindow<RankedEventRef>;

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

fn collect_search_hits_with_receipt<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
    semantic: SemanticAvailability,
    semantic_port: &P,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection> {
    let prepared = prepare_semantic_search(request, index, filter, semantic, tracker)?;
    let (requested_backend, normalized_query) = match prepared {
        PreparedSemanticSearch::Complete(collection) => return Ok(collection),
        PreparedSemanticSearch::Query {
            requested_backend,
            normalized_query,
        } => (requested_backend, normalized_query),
    };

    match semantic_port.begin_query(index) {
        Ok(mut semantic_query) => collect_prepared_semantic_query(
            request,
            index,
            filter,
            requested_backend,
            normalized_query,
            &mut semantic_query,
            tracker,
        ),
        Err(error) => semantic_retrieval_failure(
            request,
            index,
            filter,
            requested_backend,
            error,
            Vec::new(),
            tracker,
        ),
    }
}

// Keeping the already-complete bounded result inline avoids a heap allocation
// on ordinary lexical searches; this local enum is not retained across calls.
#[allow(clippy::large_enum_variant)]
enum PreparedSemanticSearch {
    Complete(RankedSearchCollection),
    Query {
        requested_backend: SearchBackend,
        normalized_query: NormalizedSearchQuery,
    },
}

fn prepare_semantic_search(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
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
            filter,
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
            filter,
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
            filter,
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
    filter: &CompiledSearchFilter,
    requested_backend: SearchBackend,
    not_ready: HistorySemanticError,
    status: &'static str,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection> {
    lexical_fallback_with_diagnostics(
        request,
        index,
        filter,
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
    filter: &CompiledSearchFilter,
    requested_backend: SearchBackend,
    not_ready: HistorySemanticError,
    status: &'static str,
    semantic_query_diagnostics: Vec<Value>,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection> {
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let queries = normalized_query.texts();
    let mut collection = collect_lexical_search_hits(
        index,
        &queries,
        request.limit,
        request.events,
        filter,
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
            "code": fallback.code,
            "reason": format!("{:?}", fallback.reason),
            "detail": fallback.detail,
            "retryable": fallback.retryable,
        },
    }));
    Ok(collection)
}

fn semantic_fallback_diagnostics(error: &HistorySemanticError) -> SemanticFallbackDiagnostics {
    let reason = error.reason();
    SemanticFallbackDiagnostics {
        code: reason.and_then(SemanticReason::adapter_code),
        reason,
        detail: error.detail().to_owned(),
        retryable: error.retryable(),
    }
}

fn collect_lexical_search_hits(
    index: &VerifiedIndex,
    queries: &[&str],
    limit: usize,
    event_results: bool,
    filter: &CompiledSearchFilter,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection> {
    let dense = event_results || filter.filters().session_id.is_some();
    if limit == 0 {
        return Ok(empty_lexical_collection(limit, tracker.work));
    }
    let candidate_limit = lexical_candidate_horizon(limit, dense);
    tracker.set_phase(SearchFailurePhase::IndexQueryDecode);
    let mode = if queries.is_empty() {
        LexicalMode::List
    } else {
        LexicalMode::Search(queries)
    };
    let batch = record_lexical_batch(
        tracker,
        index.execute_lexical(LexicalExecution::new(mode, filter, candidate_limit)),
    )?;
    tracker.set_phase(SearchFailurePhase::ResultProjection);
    shape_lexical_batch_using(
        batch,
        limit,
        dense,
        |coordinates| index.session_grouping_claims_for_search(coordinates),
        tracker.work,
    )
}

#[cfg(test)]
fn collect_lexical_search_hits_using<LexicalSearch, GroupingClaims>(
    limit: usize,
    dense: bool,
    lexical_search: LexicalSearch,
    grouping_claims: GroupingClaims,
) -> SearchExecutionResult<RankedSearchCollection>
where
    LexicalSearch: FnOnce(usize) -> ctx_history_index_query::Result<LexicalSearchBatch>,
    GroupingClaims: FnOnce(
        &[SearchSessionCoordinate],
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
) -> SearchExecutionResult<RankedSearchCollection>
where
    GroupingClaims: FnOnce(
        &[SearchSessionCoordinate],
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
    let (mut result_window, diversification) =
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

fn empty_lexical_collection(limit: usize, work: SearchWorkReceipt) -> RankedSearchCollection {
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
) -> SearchExecutionResult<(RankedSearchResultWindow, SearchDiversificationDecision)>
where
    GroupingClaims: FnOnce(
        &[SearchSessionCoordinate],
    ) -> ctx_history_index_query::Result<Vec<SessionGroupingClaims>>,
{
    if dense || limit == 0 {
        return Ok((
            dense_result_window(candidates, limit),
            SearchDiversificationDecision {
                status: SearchDiversificationStatus::NotApplicable,
                top_n: limit,
                changed_final_top_n: None,
            },
        ));
    }

    let mut coordinate_positions = std::collections::HashMap::new();
    let mut coordinates = Vec::new();
    for candidate in candidates {
        let coordinate = candidate.event.session_coordinate();
        if let std::collections::hash_map::Entry::Vacant(entry) =
            coordinate_positions.entry(coordinate)
        {
            entry.insert(coordinates.len());
            coordinates.push(coordinate);
        }
    }
    let claims = grouping_claims(&coordinates)?;
    validate_grouping_claims(&coordinates, &claims)?;
    let champions = session_champions_by(candidates, |candidate| {
        claims[coordinate_positions[&candidate.event.session_coordinate()]].session_id
    });
    let families = champions
        .iter()
        .map(|champion| {
            SearchFamilyKey::from(
                &claims[coordinate_positions[&champion.candidate.event.session_coordinate()]],
            )
        })
        .collect::<Vec<_>>();
    let FamilyShapingOutcome {
        result_window,
        distinct_families,
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
    ))
}

fn validate_grouping_claims(
    coordinates: &[SearchSessionCoordinate],
    claims: &[SessionGroupingClaims],
) -> ctx_history_index_query::Result<()> {
    if coordinates.len() != claims.len()
        || coordinates.iter().zip(claims).any(|(coordinate, claims)| {
            claims.session_id.as_uuid() != coordinate.session_id
                || claims.source_owner.digest() != coordinate.source_owner_digest
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
