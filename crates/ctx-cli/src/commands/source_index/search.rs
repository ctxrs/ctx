mod hydration;
mod query;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_index::{
    EventRecord, EventSearchCandidate, EventSearchFilters, VerifiedIndex, MAX_LEXICAL_QUERY_RESULTS,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{
        count_bucket, duration_bucket, text_length_bucket, RefreshStatus, SearchTelemetry,
    },
    config,
    local_usage::{CliUsage, ResultObservationAction, SearchContextObservation},
    output::{print_json, JsonOutputFormat},
    semantic::{
        coordinate_source_backed_refresh, semantic_query_service_supported,
        PinnedSourceBackedGeneration, SourceBackedRefreshDaemonUnavailable,
        SourceBackedRefreshMode, SourceBackedRefreshObservation, SourceBackedSemanticNotReady,
    },
    ui::{
        canonical_human_output_bytes, diagnostic, Action, Diagnostic, DiagnosticLevel, Document,
        RenderContext, Ui,
    },
    RefreshArg, SearchArgs, SearchBackendArg,
};

use super::{
    render::{
        pretty_json_stdout_bytes, render_search_document, render_search_not_ready_document,
        search_json,
    },
    shared::{index_root, render_active_generation_race, ActiveGenerationRaceCommand},
};

use hydration::presentations_for_search_hits;
pub(in crate::commands::source_index) use hydration::SearchPresentation;
#[cfg(test)]
pub(super) use hydration::{
    presentations_for_search_hits_with_budget, SearchPresentationHydrationBudget,
    SearchPresentationRetentionBudgetExceeded, SEARCH_PRESENTATION_HYDRATION_BUDGET,
    SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
};
pub(crate) use query::SourceSearchRequest;
pub(super) use query::{index_search_filters, NormalizedSearchQuery};
use query::{normalize_search_request, resolve_source_search_backend, validate_search_request};

const MAX_SESSION_DIVERSITY_CANDIDATES: usize = 64 * 1024;
const MIN_CANDIDATE_BATCH: usize = 256;
const CANDIDATE_OVERSAMPLE: usize = 8;
const SOURCE_FUSION_CANDIDATES: usize = 1_600;
const MAX_USAGE_CONTEXT_EVENTS_PER_SESSION: usize = 256;
pub(super) const MISSING_INDEX_ERROR: &str =
    "the Core index does not exist; retry with daemon refresh enabled";
const QUEUED_WITHOUT_GENERATION_ERROR: &str =
    "daemon source refresh was queued but no published generation exists; retry with --refresh wait";

#[derive(Debug)]
pub(super) struct SearchCollection {
    pub(super) result_window: SearchResultWindow,
    pub(super) candidate_pool: usize,
    pub(super) candidate_pool_truncated: bool,
    pub(super) requested_backend: SearchBackendArg,
    pub(super) effective_backend: SearchBackendArg,
    pub(super) semantic_weight: f32,
    pub(super) semantic_status: &'static str,
    pub(super) semantic_fallback: Option<SemanticFallbackDiagnostics>,
    pub(super) semantic_diagnostics: Option<Value>,
}

#[derive(Debug)]
pub(super) struct SearchResultWindow {
    pub(super) limit: usize,
    pub(super) hits: Vec<SearchHit>,
    pub(super) more_available: bool,
}

#[derive(Debug, Clone)]
pub(super) struct SemanticFallbackDiagnostics {
    pub(super) code: &'static str,
    pub(super) detail: String,
}

#[derive(Debug, Clone)]
pub(super) struct SearchHit {
    pub(super) event: SearchEventMetadata,
    pub(super) score: f32,
    pub(super) more_matches_in_session: usize,
}

/// The exact immutable event fields retained after candidate shaping and used
/// by search presentation, rendering, and context accounting. Large source,
/// native-identity, and touched-file metadata stays outside the result window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::commands::source_index) struct SearchEventMetadata {
    pub(in crate::commands::source_index) event_id: Uuid,
    pub(in crate::commands::source_index) session_id: Uuid,
    pub(in crate::commands::source_index) parent_session_id: Option<Uuid>,
    pub(in crate::commands::source_index) root_session_id: Uuid,
    pub(in crate::commands::source_index) provider: String,
    pub(in crate::commands::source_index) source_format: String,
    pub(in crate::commands::source_index) provider_session_id: Option<String>,
    pub(in crate::commands::source_index) branch: Option<String>,
    pub(in crate::commands::source_index) agent_type: String,
    pub(in crate::commands::source_index) is_primary: bool,
    pub(in crate::commands::source_index) event_sequence: u64,
    pub(in crate::commands::source_index) occurred_at_unix_ms: Option<i64>,
    pub(in crate::commands::source_index) event_type: String,
    pub(in crate::commands::source_index) role: Option<String>,
    pub(in crate::commands::source_index) workspace: Option<String>,
    pub(in crate::commands::source_index) cwd: Option<String>,
}

impl From<&EventRecord> for SearchEventMetadata {
    fn from(event: &EventRecord) -> Self {
        Self {
            event_id: event.event_id.as_uuid(),
            session_id: event.session_id.as_uuid(),
            parent_session_id: event.parent_session_id.map(|id| id.as_uuid()),
            root_session_id: event.root_session_id.as_uuid(),
            provider: event.provider.clone(),
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
            event_type: event.event_type.clone(),
            role: event.role.clone(),
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
        }
    }
}

pub(super) struct RefreshOutcome {
    pub(super) pin: PinnedSourceBackedGeneration,
    pub(super) status: &'static str,
    pub(super) source_count: usize,
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let human_output = args.format != JsonOutputFormat::Json;
    let result = run_search_inner(args, data_root.clone(), telemetry, local_usage, ui);
    render_search_error(result, human_output, &data_root, ui)
}

fn run_search_inner(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let config = config::AppConfig::load(&data_root)?;
    let mut request = SourceSearchRequest::from(&args);
    normalize_search_request(&mut request)?;
    let requested_backend = resolve_source_search_backend(&request, &config)?;
    request.backend = Some(requested_backend);
    request.semantic_enabled = config.semantic_search_enabled();
    let semantic_weight = request.semantic_weight;
    let json_output = args.format == JsonOutputFormat::Json;
    if request.refresh == RefreshArg::Background
        && request.semantic_enabled
        && semantic_query_service_supported()
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
        && !(requested_backend == SearchBackendArg::Hybrid && semantic_weight == 0.0)
    {
        crate::semantic::wait_for_daemon_query_service(&data_root, Duration::from_secs(3));
    }
    let refresh_started = Instant::now();
    let refresh = refresh_for_search(&request, &data_root)?;
    let initial_refresh_duration = refresh_started.elapsed();
    telemetry.refresh_mode = Some(request.refresh);

    let query_started = Instant::now();
    let (value, collection, index, refresh_status, refresh_source_count) =
        search_pinned_generation(&request, &data_root, semantic_weight, refresh)?;
    if !json_output {
        if let Some(fallback) = collection.semantic_fallback.as_ref() {
            let warning = render_semantic_fallback_warning(ui.stderr_context(), fallback);
            ui.write_stderr(&warning)?;
        }
    }
    let query_duration = query_started.elapsed();
    telemetry.refresh_duration = Some(duration_bucket(initial_refresh_duration));
    telemetry.refresh_status = Some(RefreshStatus::from_safe_summary(refresh_status));
    telemetry.refresh_source_count = Some(count_bucket(refresh_source_count as u64));
    telemetry.query_duration = Some(duration_bucket(query_duration));
    telemetry.query_length = Some(text_length_bucket(request.query.chars().count()));
    telemetry.query_term_count = Some(count_bucket(
        request
            .query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .count() as u64,
    ));
    telemetry.backend_requested = Some(collection.requested_backend);
    telemetry.backend_effective = Some(collection.effective_backend);
    telemetry.has_indexed_content_after = Some(index.document_count() > 0);
    telemetry.result_count = Some(count_bucket(collection.result_window.hits.len() as u64));
    telemetry.citation_count = Some(count_bucket(collection.result_window.hits.len() as u64));
    telemetry.zero_result = Some(collection.result_window.hits.is_empty());

    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let result_count = results.len();
    let search_context = if config.local_usage.enabled {
        search_context_observation(&value, &collection, &index)
    } else {
        SearchContextObservation::unavailable()
    };
    let render_started = Instant::now();
    let output_bytes = if args.format == JsonOutputFormat::Json {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let document = render_search_document(&value, args.verbose, ui.stdout_context());
        let output_bytes = canonical_human_output_bytes(|context| {
            render_search_document(&value, args.verbose, context)
        });
        ui.write_stdout(&document)?;
        output_bytes
    };
    telemetry.render_duration = Some(duration_bucket(render_started.elapsed()));
    local_usage.set_result_observation(ResultObservationAction::Search, result_count, 0, 0);
    local_usage.set_search_context_observation(search_context);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

pub(super) fn render_search_error<T>(
    result: Result<T>,
    human_output: bool,
    data_root: &Path,
    ui: &mut Ui,
) -> Result<T> {
    let result = render_active_generation_race(
        result,
        !human_output,
        ActiveGenerationRaceCommand::Search,
        ui,
    );
    match result {
        Ok(value) => Ok(value),
        Err(error) if human_output && search_index_is_not_ready(data_root, &error) => {
            let document = render_search_not_ready_document(ui.stderr_context());
            ui.write_stderr(&document)?;
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

fn search_index_is_not_ready(data_root: &Path, error: &anyhow::Error) -> bool {
    let missing_generation = error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            MISSING_INDEX_ERROR | QUEUED_WITHOUT_GENERATION_ERROR
        )
    });
    let root = index_root(data_root);
    let active_generation_missing = VerifiedIndex::active_generation_id(&root)
        .ok()
        .flatten()
        .is_none();
    missing_generation
        || (active_generation_missing
            && error
                .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
                .is_some())
}

pub(super) fn render_semantic_fallback_warning(
    context: &RenderContext,
    fallback: &SemanticFallbackDiagnostics,
) -> Document {
    let (detail, action) = match fallback.code {
        "semantic_disabled" => (
            "Keyword search was used because semantic search is disabled.",
            "ctx setup --semantic",
        ),
        _ => (
            "Keyword search was used because semantic retrieval did not complete.",
            "ctx doctor",
        ),
    };
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary: "Semantic search is unavailable",
            detail: Some(detail),
            fields: &[],
            action: Some(Action { command: action }),
        },
    )
}

pub(crate) fn mcp_search(
    mut request: SourceSearchRequest,
    data_root: &Path,
) -> Result<(Value, SearchContextObservation)> {
    normalize_search_request(&mut request)?;
    let config = config::AppConfig::load(data_root)?;
    request.backend = Some(resolve_source_search_backend(&request, &config)?);
    request.semantic_enabled = config.semantic_search_enabled();
    let semantic_weight = request.semantic_weight;
    let refresh = refresh_for_search(&request, data_root)?;
    let (value, collection, index, _, _) =
        search_pinned_generation(&request, data_root, semantic_weight, refresh)?;
    let observation = if config.local_usage.enabled {
        search_context_observation(&value, &collection, &index)
    } else {
        SearchContextObservation::unavailable()
    };
    Ok((value, observation))
}

pub(super) fn search_context_observation(
    value: &Value,
    collection: &SearchCollection,
    index: &VerifiedIndex,
) -> SearchContextObservation {
    if collection.result_window.hits.is_empty() {
        return SearchContextObservation::unavailable();
    }
    let Some(delivered_context_bytes) =
        value
            .get("results")
            .and_then(Value::as_array)
            .and_then(|results| {
                results.iter().try_fold(0_usize, |total, result| {
                    total.checked_add(result.get("snippet")?.as_str()?.len())
                })
            })
    else {
        return SearchContextObservation::unavailable();
    };
    let session_ids = collection
        .result_window
        .hits
        .iter()
        .map(|hit| hit.event.session_id)
        .collect::<BTreeSet<_>>();
    let mut matched_normalized_session_bytes = 0_usize;
    for session_id in session_ids {
        let Ok(Some(session_bytes)) = index.core_content_bytes_for_session_if_bounded(
            session_id,
            MAX_USAGE_CONTEXT_EVENTS_PER_SESSION,
        ) else {
            return SearchContextObservation::unavailable();
        };
        let Some(total) = matched_normalized_session_bytes.checked_add(session_bytes) else {
            return SearchContextObservation::unavailable();
        };
        matched_normalized_session_bytes = total;
    }
    SearchContextObservation::complete(delivered_context_bytes, matched_normalized_session_bytes)
        .unwrap_or_else(SearchContextObservation::unavailable)
}

pub(super) fn refresh_for_search(
    request: &SourceSearchRequest,
    data_root: &Path,
) -> Result<RefreshOutcome> {
    refresh_for_search_with(request, data_root, coordinate_source_backed_refresh)
}

pub(super) fn refresh_for_search_with<Coordinate>(
    request: &SourceSearchRequest,
    data_root: &Path,
    coordinate: Coordinate,
) -> Result<RefreshOutcome>
where
    Coordinate: FnOnce(&Path, SourceBackedRefreshMode) -> Result<SourceBackedRefreshObservation>,
{
    validate_search_request(request)?;
    let mode = source_backed_refresh_mode(request.refresh);
    let observation = coordinate(data_root, mode)?;
    if observation.mode != mode {
        return Err(anyhow!(
            "source-backed refresh coordinator returned mode {:?} for requested mode {:?}",
            observation.mode,
            mode
        ));
    }
    let status = match mode {
        SourceBackedRefreshMode::Off => "existing_generation",
        SourceBackedRefreshMode::Background if observation.daemon_available => "daemon_background",
        SourceBackedRefreshMode::Background => "daemon_unavailable",
        SourceBackedRefreshMode::Wait => "completed",
    };
    Ok(RefreshOutcome {
        pin: observation.pin,
        status,
        source_count: observation.source_count,
    })
}

pub(super) fn source_backed_refresh_mode(refresh: RefreshArg) -> SourceBackedRefreshMode {
    match refresh {
        RefreshArg::Off => SourceBackedRefreshMode::Off,
        RefreshArg::Background => SourceBackedRefreshMode::Background,
        RefreshArg::Wait => SourceBackedRefreshMode::Wait,
    }
}

fn search_pinned_generation(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
    refresh: RefreshOutcome,
) -> Result<(Value, SearchCollection, VerifiedIndex, &'static str, usize)> {
    let RefreshOutcome {
        pin,
        status,
        source_count,
    } = refresh;
    let (value, collection, index) = search_existing_generation(
        request,
        pin.into_index(),
        data_root,
        semantic_weight,
        status,
        source_count,
    )?;
    Ok((value, collection, index, status, source_count))
}

pub(super) fn search_existing_generation(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    refresh_status: &str,
    refresh_source_count: usize,
) -> Result<(Value, SearchCollection, VerifiedIndex)> {
    validate_search_request(request)?;
    let filters = index_search_filters(request, &index)?;
    let query_started = Instant::now();
    let collection =
        collect_search_hits_with_backend(request, &index, data_root, semantic_weight, &filters)?;
    let query_duration = query_started.elapsed();
    let presentations = presentations_for_search_hits(
        &index,
        &collection.result_window.hits,
        &NormalizedSearchQuery::from_request(request),
    )?;
    let value = search_json(
        request,
        data_root,
        &index,
        &collection,
        &filters,
        &presentations,
        refresh_status,
        refresh_source_count,
        query_duration,
    )?;
    Ok((value, collection, index))
}

pub(super) fn collect_search_hits_with_backend(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    filters: &EventSearchFilters,
) -> Result<SearchCollection> {
    let mut semantic_pin = None;
    collect_search_hits_with_backend_using(
        request,
        index,
        data_root,
        semantic_weight,
        filters,
        |index, data_root, query, filters, candidate_limit| {
            if semantic_pin.is_none() {
                semantic_pin = Some(
                    PinnedSourceBackedGeneration::pin_semantic_query_for_source_generation(
                        index, data_root,
                    )?,
                );
            }
            let pin = semantic_pin
                .as_ref()
                .ok_or_else(|| anyhow!("source-backed semantic query pin is unavailable"))?;
            PinnedSourceBackedGeneration::semantic_candidates_for_pinned_source_generation(
                index,
                data_root,
                query,
                filters,
                candidate_limit,
                pin,
            )
        },
    )
}

pub(super) fn collect_search_hits_with_backend_using<SemanticSearch>(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    filters: &EventSearchFilters,
    semantic_search: SemanticSearch,
) -> Result<SearchCollection>
where
    SemanticSearch: FnMut(
        &VerifiedIndex,
        &Path,
        &str,
        &EventSearchFilters,
        usize,
    ) -> Result<(Vec<EventSearchCandidate>, Value)>,
{
    let requested_backend = request.backend.unwrap_or(SearchBackendArg::Lexical);
    if !semantic_weight.is_finite() || !(0.0..=1.0).contains(&semantic_weight) {
        return Err(anyhow!(
            "semantic weight must be finite and between 0.0 and 1.0"
        ));
    }
    if requested_backend == SearchBackendArg::Lexical
        || (requested_backend == SearchBackendArg::Hybrid && semantic_weight == 0.0)
    {
        let normalized_query = NormalizedSearchQuery::from_request(request);
        let queries = normalized_query.texts();
        let mut collection =
            collect_search_hits(index, &queries, request.limit, request.events, filters)?;
        collection.requested_backend = requested_backend;
        collection.semantic_weight = 0.0;
        return Ok(collection);
    }
    if !request.semantic_enabled {
        let not_ready = SourceBackedSemanticNotReady::new(
            "semantic_disabled",
            "local semantic retrieval is disabled",
        );
        if requested_backend == SearchBackendArg::Semantic {
            return Err(anyhow::Error::new(not_ready));
        }
        let normalized_query = NormalizedSearchQuery::from_request(request);
        let queries = normalized_query.texts();
        let mut collection =
            collect_search_hits(index, &queries, request.limit, request.events, filters)?;
        let fallback = SemanticFallbackDiagnostics {
            code: not_ready.code(),
            detail: not_ready.detail().to_owned(),
        };
        collection.requested_backend = requested_backend;
        collection.effective_backend = SearchBackendArg::Lexical;
        collection.semantic_weight = semantic_weight;
        collection.semantic_status = "disabled";
        collection.semantic_fallback = Some(fallback.clone());
        collection.semantic_diagnostics = Some(json!({
            "query_count": queries.len(),
            "queries": [],
            "fallback": {
                "code": fallback.code,
                "detail": fallback.detail,
            },
        }));
        return Ok(collection);
    }

    let normalized_query = NormalizedSearchQuery::from_request(request);
    let queries = normalized_query.texts();
    let mut semantic_by_event = BTreeMap::<Uuid, EventSearchCandidate>::new();
    let mut semantic_query_diagnostics = Vec::with_capacity(queries.len());
    let mut semantic_search = semantic_search;
    for query in &queries {
        let semantic_result =
            semantic_search(index, data_root, query, filters, SOURCE_FUSION_CANDIDATES);
        let (candidates, diagnostics) = match semantic_result {
            Ok(value) => value,
            Err(error) if requested_backend == SearchBackendArg::Hybrid => {
                let fallback = semantic_fallback_diagnostics(&error);
                let mut collection =
                    collect_search_hits(index, &queries, request.limit, request.events, filters)?;
                collection.requested_backend = requested_backend;
                collection.effective_backend = SearchBackendArg::Lexical;
                collection.semantic_weight = semantic_weight;
                collection.semantic_status = "unavailable";
                collection.semantic_fallback = Some(fallback.clone());
                collection.semantic_diagnostics = Some(json!({
                    "query_count": queries.len(),
                    "queries": semantic_query_diagnostics,
                    "fallback": {
                        "code": fallback.code,
                        "detail": fallback.detail,
                    },
                }));
                return Ok(collection);
            }
            Err(error) => return Err(error),
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

    let candidates = if requested_backend == SearchBackendArg::Semantic {
        semantic_candidates
    } else {
        let lexical_candidates = index.search_event_candidates_any_with_filters(
            &queries,
            filters,
            SOURCE_FUSION_CANDIDATES,
        )?;
        fuse_source_candidates(lexical_candidates, semantic_candidates, semantic_weight)
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
        semantic_weight: if requested_backend == SearchBackendArg::Semantic {
            1.0
        } else {
            semantic_weight
        },
        semantic_status: "ready",
        semantic_fallback: None,
        semantic_diagnostics: Some(semantic_diagnostics),
    })
}

fn semantic_fallback_diagnostics(error: &anyhow::Error) -> SemanticFallbackDiagnostics {
    if let Some(not_ready) = error.downcast_ref::<SourceBackedSemanticNotReady>() {
        return SemanticFallbackDiagnostics {
            code: not_ready.code(),
            detail: not_ready.detail().to_owned(),
        };
    }
    SemanticFallbackDiagnostics {
        code: "semantic_query_failed",
        detail: format!("{error:#}"),
    }
}

fn collect_search_hits(
    index: &VerifiedIndex,
    queries: &[&str],
    limit: usize,
    event_results: bool,
    filters: &EventSearchFilters,
) -> Result<SearchCollection> {
    let document_count = usize::try_from(index.document_count()).unwrap_or(usize::MAX);
    let maximum = document_count
        .min(MAX_SESSION_DIVERSITY_CANDIDATES)
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
        let exhausted = candidates.len() < candidate_limit || candidate_limit >= document_count;
        let result_window = shape_search_result_window(candidates.iter(), limit, event_results);
        let enough = result_window.more_available;
        if enough || exhausted {
            return Ok(SearchCollection {
                result_window,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: false,
                requested_backend: SearchBackendArg::Lexical,
                effective_backend: SearchBackendArg::Lexical,
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
                requested_backend: SearchBackendArg::Lexical,
                effective_backend: SearchBackendArg::Lexical,
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

struct SourceFusionEvidence {
    event: EventRecord,
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
}

fn fuse_source_candidates(
    lexical: Vec<EventSearchCandidate>,
    semantic: Vec<EventSearchCandidate>,
    semantic_weight: f32,
) -> Vec<EventSearchCandidate> {
    let mut evidence = BTreeMap::<Uuid, SourceFusionEvidence>::new();
    for (rank, candidate) in lexical.into_iter().enumerate() {
        evidence.insert(
            candidate.event.event_id.as_uuid(),
            SourceFusionEvidence {
                event: candidate.event,
                lexical_rank: Some(rank.saturating_add(1)),
                semantic_rank: None,
            },
        );
    }
    for (rank, candidate) in semantic.into_iter().enumerate() {
        let semantic_rank = rank.saturating_add(1);
        evidence
            .entry(candidate.event.event_id.as_uuid())
            .and_modify(|entry| entry.semantic_rank = Some(semantic_rank))
            .or_insert(SourceFusionEvidence {
                event: candidate.event,
                lexical_rank: None,
                semantic_rank: Some(semantic_rank),
            });
    }
    let mut candidates = evidence
        .into_values()
        .map(|evidence| EventSearchCandidate {
            score: weighted_rrf_score(
                evidence.lexical_rank,
                evidence.semantic_rank,
                semantic_weight,
            ),
            event: evidence.event,
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| {
                right
                    .event
                    .occurred_at_unix_ms
                    .cmp(&left.event.occurred_at_unix_ms)
            })
            .then_with(|| right.event.event_sequence.cmp(&left.event.event_sequence))
            .then_with(|| {
                left.event
                    .event_id
                    .as_uuid()
                    .cmp(&right.event.event_id.as_uuid())
            })
    });
    candidates
}

fn weighted_rrf_score(
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
    semantic_weight: f32,
) -> f32 {
    let reciprocal_rank = |rank: usize| 1.0 / (60.0 + rank.max(1) as f32);
    let lexical = lexical_rank.map(reciprocal_rank).unwrap_or(0.0);
    let semantic = semantic_rank.map(reciprocal_rank).unwrap_or(0.0);
    ((1.0 - semantic_weight) * lexical) + (semantic_weight * semantic)
}

pub(super) fn shape_search_result_window<'a>(
    candidates: impl IntoIterator<Item = &'a EventSearchCandidate>,
    limit: usize,
    event_results: bool,
) -> SearchResultWindow {
    let shape_limit = limit.saturating_add(1);
    let mut hits = if event_results {
        candidates
            .into_iter()
            .take(shape_limit)
            .map(|candidate| SearchHit {
                event: SearchEventMetadata::from(&candidate.event),
                score: candidate.score,
                more_matches_in_session: 0,
            })
            .collect()
    } else {
        let mut positions = BTreeMap::<Uuid, usize>::new();
        let mut hits = Vec::<SearchHit>::new();
        for candidate in candidates {
            let session_id = candidate.event.session_id.as_uuid();
            if let Some(position) = positions.get(&session_id).copied() {
                if let Some(hit) = hits.get_mut(position) {
                    hit.more_matches_in_session = hit.more_matches_in_session.saturating_add(1);
                }
                continue;
            }
            if hits.len() == shape_limit {
                continue;
            }
            positions.insert(session_id, hits.len());
            hits.push(SearchHit {
                event: SearchEventMetadata::from(&candidate.event),
                score: candidate.score,
                more_matches_in_session: 0,
            });
        }
        hits
    };
    let more_available = hits.len() > limit;
    hits.truncate(limit);
    SearchResultWindow {
        limit,
        hits,
        more_available,
    }
}
