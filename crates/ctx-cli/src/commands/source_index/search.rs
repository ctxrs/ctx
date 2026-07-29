use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_index::{
    AgentScope, EventRecord, EventSearchCandidate, EventSearchFilters, ExcludedSessionTree,
    VerifiedIndex,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{
        count_bucket, duration_bucket, text_length_bucket, RefreshStatus, SearchTelemetry,
    },
    config,
    local_usage::{CliUsage, ResultObservationAction},
    output::{print_json, JsonOutputFormat},
    search_filters::{
        normalize_source_identity_filters, parse_since_filter, SourceIdentityFilterArgs,
        SourceIdentityFilters,
    },
    semantic::{
        coordinate_source_backed_refresh, semantic_query_service_supported,
        PinnedSourceBackedGeneration, SourceBackedRefreshMode, SourceBackedRefreshObservation,
        SourceBackedSemanticNotReady,
    },
    transcript::{shell_quote_arg, write_output},
    RefreshArg, SearchArgs, SearchBackendArg,
};

use super::{
    render::{pretty_json_stdout_bytes, render_search_text, search_json, stdout_body_bytes},
    shared::resolve_session,
};

const LEGACY_ACTIVE_SESSION_PROVIDER_ENV: &str = "CODEX_THREAD_ID";
const LEGACY_ACTIVE_SESSION_PROVIDER: CaptureProvider = CaptureProvider::Codex;
const MAX_SESSION_DIVERSITY_CANDIDATES: usize = 64 * 1024;
const MIN_CANDIDATE_BATCH: usize = 256;
const CANDIDATE_OVERSAMPLE: usize = 8;
const SOURCE_FUSION_CANDIDATES: usize = 1_600;

#[derive(Debug, Clone)]
pub(crate) struct SourceSearchRequest {
    pub(crate) query: String,
    pub(crate) terms: Vec<String>,
    pub(crate) limit: usize,
    pub(crate) provider: Option<CaptureProvider>,
    pub(crate) history_source: Option<String>,
    pub(crate) provider_key: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) workspace: Option<String>,
    pub(crate) since: Option<String>,
    pub(crate) primary_only: bool,
    pub(crate) include_subagents: bool,
    pub(crate) event_type: Option<String>,
    pub(crate) file: Option<PathBuf>,
    pub(crate) session: Option<String>,
    pub(crate) events: bool,
    pub(crate) include_current_session: bool,
    pub(crate) backend: Option<SearchBackendArg>,
    pub(crate) semantic_weight: f32,
    pub(crate) semantic_enabled: bool,
    pub(crate) refresh: RefreshArg,
}

impl From<&SearchArgs> for SourceSearchRequest {
    fn from(args: &SearchArgs) -> Self {
        Self {
            query: args.query.clone().unwrap_or_default(),
            terms: args.term.clone(),
            limit: args.limit,
            provider: args.provider.map(|provider| provider.capture_provider()),
            history_source: args.history_source.clone(),
            provider_key: args.provider_key.clone(),
            source_id: args.source_id.clone(),
            source_format: args.source_format.clone(),
            workspace: args.workspace.clone(),
            since: args.since.clone(),
            primary_only: args.primary_only,
            include_subagents: args.include_subagents,
            event_type: args.event_type.clone(),
            file: args.file.clone(),
            session: args.session.clone(),
            events: args.events || args.session.is_some(),
            include_current_session: args.include_current_session,
            backend: args.backend,
            semantic_weight: args.semantic_weight,
            semantic_enabled: false,
            refresh: args.refresh,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NormalizedSearchQuery {
    positional: Option<String>,
    terms: Vec<String>,
    alternatives: Vec<String>,
    display: String,
}

impl NormalizedSearchQuery {
    pub(super) fn from_request(request: &SourceSearchRequest) -> Self {
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

    pub(super) fn is_empty(&self) -> bool {
        self.alternatives.is_empty()
    }

    pub(super) fn texts(&self) -> Vec<&str> {
        self.alternatives.iter().map(String::as_str).collect()
    }

    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) fn shell_arguments(&self) -> String {
        let mut arguments = Vec::with_capacity(self.alternatives.len().saturating_mul(2));
        if let Some(positional) = self.positional.as_deref() {
            arguments.push(shell_quote_arg(positional));
        }
        for term in &self.terms {
            arguments.push(format!("--term={}", shell_quote_arg(term)));
        }
        arguments.join(" ")
    }
}

fn normalized_query_alternative(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_owned())
}

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
    pub(super) event: EventRecord,
    pub(super) score: f32,
    pub(super) more_matches_in_session: usize,
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
) -> Result<()> {
    let config = config::AppConfig::load(&data_root)?;
    let mut request = SourceSearchRequest::from(&args);
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
    let (value, collection, index, refresh_status, refresh_source_count, retry_refresh_duration) =
        search_with_hydration_retry(&request, &data_root, semantic_weight, refresh)?;
    if !json_output {
        if let Some(fallback) = collection.semantic_fallback.as_ref() {
            eprintln!(
                "warning: semantic retrieval is unavailable ({}); falling back to lexical search",
                fallback.code
            );
        }
    }
    let query_duration = query_started.elapsed();
    telemetry.refresh_duration = Some(duration_bucket(
        initial_refresh_duration.saturating_add(retry_refresh_duration),
    ));
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
    let citation_count = results
        .iter()
        .map(|result| {
            result["citations"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default()
        })
        .sum();
    let content_bytes = serde_json::to_vec(&value["results"])?.len();
    let render_started = Instant::now();
    let output_bytes = if args.format == JsonOutputFormat::Json {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let body = render_search_text(&value, args.verbose);
        let output_bytes = stdout_body_bytes(&body);
        write_output(body, None)?;
        output_bytes
    };
    telemetry.render_duration = Some(duration_bucket(render_started.elapsed()));
    local_usage.set_result_observation(
        ResultObservationAction::Search,
        result_count,
        citation_count,
        content_bytes,
    );
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

pub(crate) fn mcp_search(mut request: SourceSearchRequest, data_root: &Path) -> Result<Value> {
    let config = config::AppConfig::load(data_root)?;
    request.backend = Some(resolve_source_search_backend(&request, &config)?);
    request.semantic_enabled = config.semantic_search_enabled();
    let semantic_weight = request.semantic_weight;
    let refresh = refresh_for_search(&request, data_root)?;
    let (value, _, _, _, _, _) =
        search_with_hydration_retry(&request, data_root, semantic_weight, refresh)?;
    Ok(value)
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

fn search_with_hydration_retry(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
    refresh: RefreshOutcome,
) -> Result<(
    Value,
    SearchCollection,
    VerifiedIndex,
    &'static str,
    usize,
    Duration,
)> {
    search_with_hydration_retry_with(
        request,
        data_root,
        semantic_weight,
        refresh,
        search_existing_generation,
        |request, data_root| {
            let mut wait_request = request.clone();
            wait_request.refresh = RefreshArg::Wait;
            refresh_for_search(&wait_request, data_root)
        },
    )
}

#[allow(clippy::type_complexity)]
pub(super) fn search_with_hydration_retry_with<Run, Refresh>(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
    refresh: RefreshOutcome,
    mut run: Run,
    mut wait_refresh: Refresh,
) -> Result<(
    Value,
    SearchCollection,
    VerifiedIndex,
    &'static str,
    usize,
    Duration,
)>
where
    Run: FnMut(
        &SourceSearchRequest,
        VerifiedIndex,
        &Path,
        f32,
        &'static str,
        usize,
    ) -> Result<(Value, SearchCollection, VerifiedIndex)>,
    Refresh: FnMut(&SourceSearchRequest, &Path) -> Result<RefreshOutcome>,
{
    let RefreshOutcome {
        pin,
        status,
        source_count,
    } = refresh;
    match run(
        request,
        pin.into_index(),
        data_root,
        semantic_weight,
        status,
        source_count,
    ) {
        Ok((value, collection, index)) => Ok((
            value,
            collection,
            index,
            status,
            source_count,
            Duration::ZERO,
        )),
        Err(error)
            if request.refresh != RefreshArg::Off
                && PinnedSourceBackedGeneration::source_hydration_retryable(&error) =>
        {
            let retry_started = Instant::now();
            let retry = wait_refresh(request, data_root)?;
            let retry_duration = retry_started.elapsed();
            let (value, collection, index) = run(
                request,
                retry.pin.into_index(),
                data_root,
                semantic_weight,
                retry.status,
                retry.source_count,
            )?;
            Ok((
                value,
                collection,
                index,
                retry.status,
                retry.source_count,
                retry_duration,
            ))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn search_existing_generation(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    refresh_status: &str,
    refresh_source_count: usize,
) -> Result<(Value, SearchCollection, VerifiedIndex)> {
    search_existing_generation_with_hydrator(
        request,
        index,
        data_root,
        semantic_weight,
        refresh_status,
        refresh_source_count,
        |index, data_root, events| {
            PinnedSourceBackedGeneration::hydrate_source_search_page(index, data_root, events)
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_existing_generation_with_hydrator<Hydrate>(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    refresh_status: &str,
    refresh_source_count: usize,
    hydrate: Hydrate,
) -> Result<(Value, SearchCollection, VerifiedIndex)>
where
    Hydrate: FnOnce(&VerifiedIndex, &Path, &[&EventRecord]) -> Result<HashMap<Uuid, String>>,
{
    validate_search_request(request)?;
    let filters = index_search_filters(request, &index)?;
    let query_started = Instant::now();
    let collection =
        collect_search_hits_with_backend(request, &index, data_root, semantic_weight, &filters)?;
    let query_duration = query_started.elapsed();
    let events = collection
        .result_window
        .hits
        .iter()
        .map(|hit| &hit.event)
        .collect::<Vec<_>>();
    let snippets = hydrate(&index, data_root, &events)?;
    let value = search_json(
        request,
        &index,
        &collection,
        &filters,
        &snippets,
        refresh_status,
        refresh_source_count,
        query_duration,
    )?;
    Ok((value, collection, index))
}

fn validate_search_request(request: &SourceSearchRequest) -> Result<()> {
    let source_identity = normalized_source_identity_filters(request)?;
    if !source_identity.is_empty()
        && request
            .provider
            .is_some_and(|provider| provider != CaptureProvider::Custom)
    {
        return Err(anyhow!(
            "custom history source filters can only be combined with --provider custom"
        ));
    }
    let has_query = !NormalizedSearchQuery::from_request(request).is_empty();
    if !has_query && request.file.is_none() {
        return Err(anyhow!("source-backed search needs a non-empty text query"));
    }
    if !has_query
        && request
            .backend
            .is_some_and(|backend| backend != SearchBackendArg::Lexical)
    {
        return Err(anyhow!(
            "semantic and hybrid search need a non-empty text query"
        ));
    }
    Ok(())
}

fn normalized_source_identity_filters(
    request: &SourceSearchRequest,
) -> Result<SourceIdentityFilters> {
    normalize_source_identity_filters(SourceIdentityFilterArgs {
        history_source: request.history_source.clone(),
        provider_key: request.provider_key.clone(),
        source_id: request.source_id.clone(),
        source_format: request.source_format.clone(),
    })
}

fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    if request.backend.is_none()
        && NormalizedSearchQuery::from_request(request).is_empty()
        && request.file.is_some()
    {
        return Ok(SearchBackendArg::Lexical);
    }
    let semantic_enabled = config.semantic_search_enabled();
    match request.backend {
        Some(SearchBackendArg::Semantic) if !semantic_enabled => Err(anyhow!(
            "semantic search is disabled. Set [search] semantic = true in ctx config to enable local semantic search"
        )),
        Some(SearchBackendArg::Semantic) if !semantic_query_service_supported() => Err(anyhow!(
            "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical"
        )),
        Some(SearchBackendArg::Semantic) if !config.daemon.enabled => Err(anyhow!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical"
        )),
        value
            if semantic_enabled
                && semantic_query_service_supported()
                && !config.daemon.enabled
                && !matches!(value, Some(SearchBackendArg::Lexical)) =>
        {
            Err(anyhow!(
                "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical"
            ))
        }
        Some(value) => Ok(value),
        None if semantic_enabled => Ok(SearchBackendArg::Hybrid),
        None => Ok(SearchBackendArg::Lexical),
    }
}

pub(super) fn index_search_filters(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
    let source_identity = normalized_source_identity_filters(request)?;
    let session_id = request
        .session
        .as_deref()
        .map(|id| resolve_session(index, id).map(|session| session.session_id.as_uuid()))
        .transpose()?;
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
        .then(|| std::env::var(LEGACY_ACTIVE_SESSION_PROVIDER_ENV).ok())
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|provider_session_id| ExcludedSessionTree {
            provider: LEGACY_ACTIVE_SESSION_PROVIDER.as_str().to_owned(),
            provider_session_id,
            session_id: None,
        });
    Ok(EventSearchFilters {
        session_id,
        provider: request
            .provider
            .or_else(|| (!source_identity.is_empty()).then_some(CaptureProvider::Custom))
            .map(|provider| provider.as_str().to_owned()),
        history_source: source_identity.history_source,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        workspace: request.workspace.clone(),
        since_unix_ms,
        event_type,
        agent_scope: if request.primary_only || !request.include_subagents {
            AgentScope::Primary
        } else {
            AgentScope::All
        },
        file: request.file.as_ref().map(|path| path.display().to_string()),
        exclude_session_tree,
        ..EventSearchFilters::default()
    })
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
    let maximum = document_count.min(MAX_SESSION_DIVERSITY_CANDIDATES);
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
                event: candidate.event.clone(),
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
                event: candidate.event.clone(),
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
