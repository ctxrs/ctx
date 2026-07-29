use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_index::{
    AgentScope, EventRecord, EventSearchCandidate, EventSearchFilters, ExcludedSessionTree,
    SessionRecord, VerifiedIndex,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{
        count_bucket, duration_bucket, text_length_bucket, RefreshStatus, SearchTelemetry,
        ShowTelemetry,
    },
    complete_content::{
        enforce_complete_content_cli_output_limit, enforce_complete_content_output_limit,
        ContentPolicy, CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
    },
    config,
    output::{compact_json, print_json, JsonOutputFormat, OutputFormat},
    provider_args::ProviderArg,
    search_filters::parse_since_filter,
    semantic::{
        coordinate_source_backed_refresh, semantic_query_service_supported,
        PinnedSourceBackedGeneration, SourceBackedRefreshMode, SourceBackedRefreshObservation,
        SourceBackedSemanticNotReady,
    },
    transcript::{
        normalize_uuid_prefix, print_locate_event_text, print_locate_session_text,
        provider_resume_json, shell_quote_arg, write_output, TranscriptMode,
    },
    LocateArgs, LocateTarget, RefreshArg, SearchArgs, SearchBackendArg, ShowArgs, ShowTarget,
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
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

#[derive(Debug)]
struct SearchCollection {
    hits: Vec<SearchHit>,
    candidate_pool: usize,
    candidate_pool_truncated: bool,
    requested_backend: SearchBackendArg,
    effective_backend: SearchBackendArg,
    semantic_weight: f32,
    semantic_status: &'static str,
    semantic_fallback: Option<SemanticFallbackDiagnostics>,
    semantic_diagnostics: Option<Value>,
}

#[derive(Debug, Clone)]
struct SemanticFallbackDiagnostics {
    code: &'static str,
    detail: String,
}

#[derive(Debug, Clone)]
struct SearchHit {
    event: EventRecord,
    score: f32,
    more_matches_in_session: usize,
}

struct RefreshOutcome {
    pin: PinnedSourceBackedGeneration,
    status: &'static str,
    source_count: usize,
}

#[derive(Debug)]
struct ResolvedIndexContent {
    text: String,
}

pub(crate) fn index_is_available(data_root: &Path) -> bool {
    index_root(data_root).join("meta.json").is_file()
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
) -> Result<()> {
    let config = config::AppConfig::load(&data_root)?;
    let mut request = SourceSearchRequest::from(&args);
    let requested_backend = resolve_source_search_backend(&request, &config)?;
    request.backend = Some(requested_backend);
    request.semantic_enabled = config.semantic_search_enabled();
    let semantic_weight = request.semantic_weight;
    let json_output = args.format == JsonOutputFormat::Json;
    if request.refresh == RefreshArg::Background && config.daemon.enabled && !json_output {
        crate::semantic::maybe_autostart_daemon_for_search(&data_root, &config);
    }
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
    telemetry.result_count = Some(count_bucket(collection.hits.len() as u64));
    telemetry.citation_count = Some(count_bucket(collection.hits.len() as u64));
    telemetry.zero_result = Some(collection.hits.is_empty());

    let render_started = Instant::now();
    if args.format == JsonOutputFormat::Json {
        print_json(value)?;
    } else {
        write_output(render_search_text(&value, args.verbose), None)?;
    }
    telemetry.render_duration = Some(duration_bucket(render_started.elapsed()));
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

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
) -> Result<()> {
    let index = open_index(&data_root)?;
    match args.target {
        ShowTarget::Event(args) => {
            let selected = resolve_event(&index, &args.id)?;
            let events = event_window(&index, &selected, args.before, args.after, args.window)?;
            telemetry.events_returned = Some(count_bucket(events.len() as u64));
            let value = event_window_json(
                &index,
                &data_root,
                &selected,
                &events,
                args.content,
                args.format,
                CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
            )?;
            write_show_value(
                value,
                args.format,
                args.content,
                None,
                selected.event_id.as_uuid(),
            )
        }
        ShowTarget::Session(args) => {
            if args.provider_session.is_some() {
                return Err(anyhow!(
                    "provider-session lookup is not yet exposed by the source-backed index; pass a ctx session ID or prefix"
                ));
            }
            let id = args.id.as_deref().ok_or_else(|| {
                anyhow!(
                    "source-backed session lookup requires a ctx session ID or unambiguous prefix"
                )
            })?;
            let session = resolve_session(&index, id)?;
            if let Some(provider) = args.provider {
                let requested = provider.capture_provider().as_str();
                if session.provider != requested {
                    return Err(anyhow!(
                        "source-backed session {} belongs to provider {}, not {}",
                        session.session_id,
                        session.provider,
                        requested
                    ));
                }
            }
            let value = session_json(
                &index,
                &data_root,
                &session,
                args.mode,
                args.content,
                args.format,
                None,
                CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
            )?;
            telemetry.events_returned = value["events"]
                .as_array()
                .map(|events| count_bucket(events.len() as u64));
            let event_id = value["events"]
                .as_array()
                .and_then(|events| events.last())
                .and_then(|event| event["ctx_event_id"].as_str())
                .and_then(|id| Uuid::parse_str(id).ok())
                .unwrap_or_else(|| session.session_id.as_uuid());
            write_show_value(value, args.format, args.content, args.out, event_id)
        }
    }
}

pub(crate) fn mcp_show_session(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    content: ContentPolicy,
    max_events: usize,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let session = resolve_session(&index, id)?;
    let value = session_json(
        &index,
        data_root,
        &session,
        mode,
        content,
        OutputFormat::Json,
        Some(max_events),
        output_limit_bytes,
    )?;
    let event_id = value["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["ctx_event_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or_else(|| session.session_id.as_uuid());
    enforce_json_output_limit(content, &value, output_limit_bytes, event_id)?;
    Ok(value)
}

pub(crate) fn mcp_show_event(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    content: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let selected = resolve_event(&index, id)?;
    let events = event_window(&index, &selected, before, after, window)?;
    let value = event_window_json(
        &index,
        data_root,
        &selected,
        &events,
        content,
        OutputFormat::Json,
        output_limit_bytes,
    )?;
    enforce_json_output_limit(
        content,
        &value,
        output_limit_bytes,
        selected.event_id.as_uuid(),
    )?;
    Ok(value)
}

pub(crate) fn run_locate(args: LocateArgs, data_root: PathBuf) -> Result<()> {
    let index = open_index(&data_root)?;
    let (value, json_output) = match args.target {
        LocateTarget::Session(args) => {
            let provider = args.provider.map(ProviderArg::capture_provider);
            let session = match (args.id.as_deref(), args.provider_session.as_deref()) {
                (Some(id), None) => resolve_session(&index, id)?,
                (None, Some(provider_session_id)) => {
                    let matches = index.sessions_by_provider_session_id(
                        provider_session_id,
                        provider.map(CaptureProvider::as_str),
                    )?;
                    match matches.as_slice() {
                        [] => {
                            return Err(anyhow!(
                                "provider session {provider_session_id:?} was not found in the source-backed Core generation"
                            ));
                        }
                        [session] => session.clone(),
                        matches => {
                            return Err(anyhow!(
                                "provider session {provider_session_id:?} is ambiguous; first matches are {} and {}; pass --provider or a ctx session ID",
                                matches[0].session_id,
                                matches[1].session_id
                            ));
                        }
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(anyhow!(
                        "pass either a ctx session ID or --provider-session, not both"
                    ));
                }
                (None, None) => {
                    return Err(anyhow!(
                        "source-backed session lookup requires a ctx session ID or --provider-session"
                    ));
                }
            };
            if let Some(provider) = provider {
                if session.provider != provider.as_str() {
                    return Err(anyhow!(
                        "source-backed session {} belongs to provider {}, not {}",
                        session.session_id,
                        session.provider,
                        provider
                    ));
                }
            }
            let value = locate_session_value(&session);
            (value, args.format.is_json())
        }
        LocateTarget::Event(args) => {
            let event = resolve_event(&index, &args.id)?;
            let value = locate_event_value(&event);
            (value, args.format.is_json())
        }
    };
    if json_output {
        print_json(value)
    } else if value["target"] == "session" {
        print_locate_session_text(&value)
    } else {
        print_locate_event_text(&value)
    }
}

fn locate_session_value(session: &SessionRecord) -> Value {
    let provider = session
        .provider
        .parse::<CaptureProvider>()
        .unwrap_or(CaptureProvider::Unknown);
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_location",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": session.root_session_id.as_uuid(),
        "agent_type": session.agent_type,
        "started_at": timestamp_json(session.first_occurred_at_unix_ms),
        "source": {
            "path": session.source_path,
            "exists": session.source_path.as_deref().map(|path| Path::new(path).exists()),
            "source_format": session.source_format,
            "workspace": session.workspace,
            "cwd": session.cwd,
        },
        "resume": provider_resume_json(provider, session.provider_session_id.as_deref()),
    }))
}

fn locate_event_value(event: &EventRecord) -> Value {
    let provider = event
        .provider
        .parse::<CaptureProvider>()
        .unwrap_or(CaptureProvider::Unknown);
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_location",
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "source": {
            "source_key": event.locator.source(),
            "path": event.source_path,
            "exists": event.source_path.as_deref().map(|path| Path::new(path).exists()),
            "source_format": event.source_format,
            "workspace": event.workspace,
            "cwd": event.cwd,
        },
        "source_record": {
            "locator": event.locator,
            "locator_version": event.locator.locator_version(),
            "record_digest": encode_hex(event.locator.record_digest()),
        },
        "complete_content": {
            "available": true,
            "source_authority": "provider",
            "locator_kind": format!("{:?}", event.locator.coordinate()),
        },
        "resume": provider_resume_json(provider, event.provider_session_id.as_deref()),
    }))
}

fn refresh_for_search(request: &SourceSearchRequest, data_root: &Path) -> Result<RefreshOutcome> {
    refresh_for_search_with(request, data_root, coordinate_source_backed_refresh)
}

fn refresh_for_search_with<Coordinate>(
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

fn source_backed_refresh_mode(refresh: RefreshArg) -> SourceBackedRefreshMode {
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
fn search_with_hydration_retry_with<Run, Refresh>(
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

fn search_existing_generation(
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
fn search_existing_generation_with_hydrator<Hydrate>(
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
        .hits
        .iter()
        .map(|hit| &hit.event)
        .collect::<Vec<_>>();
    let snippets = hydrate(&index, data_root, &events)?;
    let value = search_json(
        request,
        &index,
        &collection,
        &snippets,
        refresh_status,
        refresh_source_count,
        query_duration,
    )?;
    Ok((value, collection, index))
}

fn validate_search_request(request: &SourceSearchRequest) -> Result<()> {
    if request.history_source.is_some()
        || request.provider_key.is_some()
        || request.source_id.is_some()
    {
        return Err(anyhow!(
            "custom history source identity filters are not yet exposed by the source-backed index query API"
        ));
    }
    let has_query = !search_query_texts(request).is_empty();
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

fn search_query_texts(request: &SourceSearchRequest) -> Vec<&str> {
    std::iter::once(request.query.as_str())
        .chain(request.terms.iter().map(String::as_str))
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .collect()
}

fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    if request.backend.is_none() && search_query_texts(request).is_empty() && request.file.is_some()
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

fn index_search_filters(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
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
            .map(|provider| provider.as_str().to_owned()),
        source_format: request.source_format.clone(),
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

fn collect_search_hits_with_backend(
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

fn collect_search_hits_with_backend_using<SemanticSearch>(
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
        let queries = search_query_texts(request);
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
        let mut collection = collect_search_hits(
            index,
            &search_query_texts(request),
            request.limit,
            request.events,
            filters,
        )?;
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
            "query_count": search_query_texts(request).len(),
            "queries": [],
            "fallback": {
                "code": fallback.code,
                "detail": fallback.detail,
            },
        }));
        return Ok(collection);
    }

    let queries = search_query_texts(request);
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
    let hits = shape_search_hits(candidates.iter(), request.limit, request.events);
    Ok(SearchCollection {
        hits,
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
        let hits = shape_search_hits(candidates.iter(), limit, event_results);
        let enough = hits.len() >= limit;
        if enough || exhausted {
            return Ok(SearchCollection {
                hits,
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
                hits,
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

fn shape_search_hits<'a>(
    candidates: impl IntoIterator<Item = &'a EventSearchCandidate>,
    limit: usize,
    event_results: bool,
) -> Vec<SearchHit> {
    if event_results {
        return candidates
            .into_iter()
            .take(limit)
            .map(|candidate| SearchHit {
                event: candidate.event.clone(),
                score: candidate.score,
                more_matches_in_session: 0,
            })
            .collect();
    }

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
        if hits.len() == limit {
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
}

fn search_json(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    snippets: &HashMap<Uuid, String>,
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    let result_scope = if request.events { "event" } else { "session" };
    let results = collection
        .hits
        .iter()
        .map(|hit| {
            let snippet = snippets
                .get(&hit.event.event_id.as_uuid())
                .filter(|snippet| !snippet.is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "generation-bound source hydration omitted search event {}",
                        hit.event.event_id
                    )
                })?;
            Ok(search_result_json(
                hit,
                snippet,
                result_scope,
                &request.query,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let phase_attribution = phase_attribution(query_duration);
    Ok(compact_json(json!({
        "schema_version": 1,
        "payload_type": "search_results",
        "query": request.query.trim(),
        "filters": {
            "provider": request.provider.map(CaptureProvider::as_str),
            "source_format": request.source_format,
            "workspace": request.workspace,
            "since": request.since,
            "event_type": request.event_type,
            "file": request.file.as_ref().map(|path| path.display().to_string()),
            "session": request.session,
            "primary_only": request.primary_only.then_some(true),
            "include_subagents": request.include_subagents.then_some(true),
            "include_current_session": request.include_current_session.then_some(true),
        },
        "freshness": {
            "mode": request.refresh.as_str(),
            "status": refresh_status,
            "source_count": refresh_source_count,
        },
        "retrieval": {
            "requested_mode": collection.requested_backend.as_str(),
            "effective_mode": collection.effective_backend.as_str(),
            "semantic_weight": collection.semantic_weight,
            "semantic_status": collection.semantic_status,
            "semantic_fallback_code": collection.semantic_fallback.as_ref().map(|fallback| fallback.code),
            "semantic_fallback": collection.semantic_fallback.as_ref().map(|fallback| fallback.detail.as_str()),
            "semantic_diagnostics": collection.semantic_diagnostics,
            "index": "source_backed",
            "generation_id": index.generation_id(),
            "indexed_documents": index.document_count(),
            "phase_attribution": phase_attribution,
        },
        "phase_attribution": phase_attribution,
        "results": results,
        "pagination": {
            "limit": request.limit,
            "returned": results.len(),
        },
        "truncation": {
            "candidate_pool": collection.candidate_pool,
            "candidate_pool_truncated": collection.candidate_pool_truncated,
        },
    })))
}

fn search_result_json(hit: &SearchHit, snippet: &str, result_scope: &str, query: &str) -> Value {
    let event = &hit.event;
    let event_id = event.event_id.as_uuid();
    let session_id = event.session_id.as_uuid();
    let item_id = if result_scope == "session" {
        session_id
    } else {
        event_id
    };
    let title = match event.role.as_deref() {
        Some(role) => format!("{} {role} {}", event.provider, event.event_type),
        None => format!("{} {}", event.provider, event.event_type),
    };
    let mut next = vec![format!("ctx show event {event_id} --window 10")];
    if result_scope == "session" {
        next.insert(0, format!("ctx show session {session_id}"));
    }
    next.push(format!(
        "ctx search {} --session {session_id}",
        shell_quote_arg(query)
    ));
    compact_json(json!({
        "item_id": item_id,
        "result_type": if result_scope == "session" { "session_result" } else { "event" },
        "ctx_event_id": event_id,
        "ctx_session_id": session_id,
        "session_id": session_id,
        "event_id": event_id,
        "event_seq": event.event_sequence,
        "title": title,
        "snippet": snippet,
        "rank": hit.score,
        "result_scope": result_scope,
        "session_importance": (result_scope == "session").then_some(hit.score),
        "more_matches_in_session": (result_scope == "session")
            .then_some(hit.more_matches_in_session),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "source_path": event.source_path,
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "branch": event.branch,
        "agent_type": event.agent_type,
        "is_primary": event.is_primary,
        "timestamp": timestamp_json(event.occurred_at_unix_ms),
        "workspace": event.workspace,
        "cwd": event.cwd,
        "suggested_next_commands": next,
        "citations": [{
            "item_id": event_id,
            "target_type": "event",
            "ctx_event_id": event_id,
            "ctx_session_id": session_id,
            "provider": event.provider,
            "session_id": session_id,
            "event_seq": event.event_sequence,
            "source_path": event.source_path,
        }],
        "visibility": "local",
    }))
}

fn render_search_text(value: &Value, verbose: bool) -> String {
    let mut output = String::new();
    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if results.is_empty() {
        let query = value["query"].as_str().unwrap_or_default();
        output.push_str(&format!("no results for {}\n", shell_quote_arg(query)));
        return output;
    }
    for (position, result) in results.iter().enumerate() {
        let title = result["title"].as_str().unwrap_or("indexed event");
        if verbose {
            output.push_str(title);
            output.push('\n');
            for key in [
                "ctx_event_id",
                "ctx_session_id",
                "provider_session_id",
                "source_format",
            ] {
                if let Some(value) = result[key].as_str() {
                    output.push_str(&format!("  {key}: {value}\n"));
                }
            }
            if let Some(snippet) = result["snippet"].as_str() {
                output.push_str(&format!("  {snippet}\n"));
            }
            if let Some(rank) = result["rank"].as_f64() {
                output.push_str(&format!("  rank: {rank:.2}\n"));
            }
            if let Some(importance) = result["session_importance"].as_f64() {
                output.push_str(&format!("  session_importance: {importance:.2}\n"));
            }
            if let Some(commands) = result["suggested_next_commands"].as_array() {
                for command in commands.iter().filter_map(Value::as_str).take(3) {
                    output.push_str(&format!("  next: {command}\n"));
                }
            }
            if let Some(event_id) = result["ctx_event_id"].as_str() {
                output.push_str(&format!("  citation: event {event_id}\n"));
            }
        } else {
            output.push_str(&format!("{}. {title}\n", position + 1));
            let provider = result["provider"].as_str().unwrap_or("unknown");
            let scope = result["result_scope"].as_str().unwrap_or("event");
            let score = result["session_importance"]
                .as_f64()
                .or_else(|| result["rank"].as_f64())
                .unwrap_or_default();
            output.push_str(&format!("   {provider} | {scope} {score:.2}\n"));
            if let Some(snippet) = result["snippet"].as_str() {
                output.push_str(&format!("   {snippet}\n"));
            }
            if let Some(command) = result["suggested_next_commands"]
                .as_array()
                .and_then(|commands| commands.first())
                .and_then(Value::as_str)
            {
                output.push_str(&format!("   inspect: {command}\n"));
            }
        }
    }
    if value["truncation"]["candidate_pool_truncated"] == true {
        output.push_str(
            "warning: source-backed session diversity was bounded by the current index query API\n",
        );
    }
    output
}

fn session_json(
    index: &VerifiedIndex,
    data_root: &Path,
    session: &SessionRecord,
    mode: TranscriptMode,
    content: ContentPolicy,
    format: OutputFormat,
    max_events: Option<usize>,
    output_limit_bytes: usize,
) -> Result<Value> {
    let mut events = index.events_for_session(session.session_id.as_uuid())?;
    let truncated = max_events.is_some_and(|limit| events.len() > limit);
    if let Some(limit) = max_events {
        events.truncate(limit);
    }
    let selected = select_session_events(&events, mode);
    let rendered = render_event_values(index, data_root, &selected, content, output_limit_bytes)?;
    Ok(compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_transcript",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "mode": mode.as_str(),
        "content_policy": content.as_str(),
        "format": format.as_str(),
        "session": {
            "ctx_session_id": session.session_id.as_uuid(),
            "provider": session.provider,
            "provider_session_id": session.provider_session_id,
            "source_format": session.source_format,
            "source_path": session.source_path,
            "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
            "root_ctx_session_id": session.root_session_id.as_uuid(),
            "branch": session.branch,
            "agent_type": session.agent_type,
            "is_primary": session.is_primary,
            "workspace": session.workspace,
            "cwd": session.cwd,
        },
        "events": rendered,
        "truncated": truncated.then(|| json!({
            "events": true,
            "max_events": max_events,
        })),
    })))
}

fn event_window_json(
    index: &VerifiedIndex,
    data_root: &Path,
    selected: &EventRecord,
    events: &[EventRecord],
    content: ContentPolicy,
    format: OutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let references = events.iter().collect::<Vec<_>>();
    let rendered = render_event_values(index, data_root, &references, content, output_limit_bytes)?;
    let selected_value = rendered
        .iter()
        .find(|event| {
            event["ctx_event_id"].as_str() == Some(&selected.event_id.as_uuid().to_string())
        })
        .cloned()
        .ok_or_else(|| anyhow!("selected source-backed event is absent from its event window"))?;
    Ok(compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_window",
        "ctx_event_id": selected.event_id.as_uuid(),
        "ctx_session_id": selected.session_id.as_uuid(),
        "content_policy": content.as_str(),
        "format": format.as_str(),
        "event": selected_value,
        "events": rendered,
    })))
}

fn render_event_values(
    index: &VerifiedIndex,
    data_root: &Path,
    events: &[&EventRecord],
    policy: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    let resolved = resolve_contents(index, data_root, events, output_limit_bytes)?;
    events
        .iter()
        .zip(resolved)
        .map(|(event, resolved)| {
            Ok(compact_json(json!({
                "ctx_event_id": event.event_id.as_uuid(),
                "item_id": event.event_id.as_uuid(),
                "record_type": "event",
                "ctx_session_id": event.session_id.as_uuid(),
                "provider": event.provider,
                "provider_session_id": event.provider_session_id,
                "source_format": event.source_format,
                "source_path": event.source_path,
                "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
                "root_ctx_session_id": event.root_session_id.as_uuid(),
                "branch": event.branch,
                "agent_type": event.agent_type,
                "is_primary": event.is_primary,
                "sequence": event.event_sequence,
                "event_type": event.event_type,
                "role": event.role,
                "occurred_at": timestamp_json(event.occurred_at_unix_ms),
                "workspace": event.workspace,
                "cwd": event.cwd,
                "touched_files": event.touched_files,
                "preview": resolved.text.chars().take(2_048).collect::<String>(),
                "text": resolved.text,
                "content": {
                    "requested": policy.as_str(),
                    "complete": true,
                    "origin": "provider_source",
                    "stored_truncated": false,
                    "source_verified": true,
                    "complete_content_available": true,
                },
            })))
        })
        .collect()
}

fn resolve_contents(
    index: &VerifiedIndex,
    data_root: &Path,
    events: &[&EventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<ResolvedIndexContent>> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let hydrated =
        PinnedSourceBackedGeneration::hydrate_source_complete_events(index, data_root, events)?;
    resolved_contents_from_map(events, output_limit_bytes, hydrated)
}

fn resolved_contents_from_map(
    events: &[&EventRecord],
    output_limit_bytes: usize,
    mut hydrated: HashMap<Uuid, String>,
) -> Result<Vec<ResolvedIndexContent>> {
    let mut output_bytes = 0usize;
    let mut resolved = Vec::with_capacity(events.len());
    for event in events {
        let text = hydrated
            .remove(&event.event_id.as_uuid())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "generation-bound source hydration omitted complete event {}",
                    event.event_id
                )
            })?;
        output_bytes = output_bytes.saturating_add(text.len());
        if output_bytes > output_limit_bytes {
            return Err(anyhow!(
                "source-backed complete content exceeds the {output_limit_bytes}-byte output limit at event {}",
                event.event_id
            ));
        }
        resolved.push(ResolvedIndexContent { text });
    }
    if !hydrated.is_empty() {
        return Err(anyhow!(
            "generation-bound source hydration returned unrequested events"
        ));
    }
    Ok(resolved)
}

#[cfg(test)]
fn resolve_complete_contents(
    events: &[&EventRecord],
    output_limit_bytes: usize,
    resolver: &dyn ctx_history_core::ContentSourceResolver,
) -> Result<Vec<ResolvedIndexContent>> {
    use ctx_history_core::{BatchHydrationRequest, EventHydrationRequest};

    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let request = BatchHydrationRequest::new(requests)?;
    let result = resolver.hydrate_batch(&request).map_err(|failure| {
        anyhow!(
            "hydrate ordered generation-bound source batch: {:?}: {}",
            failure.kind,
            failure.detail
        )
    })?;
    result
        .validate_for_request(&request)
        .map_err(|failure| anyhow!("validate generation-bound source batch: {}", failure.detail))?;
    let mut hydrated = HashMap::with_capacity(events.len());
    for (event, record) in events.iter().zip(result.into_records()) {
        let text = String::from_utf8(record.provider_bytes).map_err(|error| {
            anyhow!(
                "provider registry returned non-UTF-8 exact content for {} event {}: {}",
                event.provider,
                event.event_id,
                error.utf8_error()
            )
        })?;
        if hydrated.insert(event.event_id.as_uuid(), text).is_some() {
            return Err(anyhow!(
                "generation-bound source batch duplicated event {}",
                event.event_id
            ));
        }
    }
    resolved_contents_from_map(events, output_limit_bytes, hydrated)
}

fn write_show_value(
    value: Value,
    format: OutputFormat,
    content: ContentPolicy,
    out: Option<PathBuf>,
    event_id: Uuid,
) -> Result<()> {
    let body = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&value)?,
        OutputFormat::Jsonl => render_show_jsonl(&value)?,
        OutputFormat::Text => render_show_text(&value),
        OutputFormat::Markdown => render_show_markdown(&value),
    };
    enforce_complete_content_cli_output_limit(
        content,
        &body,
        out.is_none(),
        CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
        event_id,
    )?;
    write_output(body, out)
}

fn render_show_jsonl(value: &Value) -> Result<String> {
    let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let lines = events
        .iter()
        .map(|event| {
            if value["target"] == "session" {
                serde_json::to_string(&compact_json(json!({
                    "schema_version": 1,
                    "payload_type": "session_transcript_event",
                    "mode": value["mode"],
                    "content_policy": value["content_policy"],
                    "ctx_session_id": value["ctx_session_id"],
                    "provider": value["provider"],
                    "provider_session_id": value["provider_session_id"],
                    "event": event,
                })))
            } else {
                serde_json::to_string(event)
            }
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(lines.join("\n") + "\n")
    }
}

fn enforce_json_output_limit(
    policy: ContentPolicy,
    value: &Value,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<()> {
    let serialized_bytes = serde_json::to_vec(value)?.len();
    enforce_complete_content_output_limit(policy, serialized_bytes, output_limit_bytes, event_id)?;
    Ok(())
}

fn render_show_text(value: &Value) -> String {
    let mut output = String::new();
    match value["target"].as_str() {
        Some("session") => {
            output.push_str(&format!(
                "ctx_session_id: {}\nprovider: {}\n",
                value["ctx_session_id"].as_str().unwrap_or("unknown"),
                value["provider"].as_str().unwrap_or("unknown")
            ));
            if let Some(provider_session_id) = value["provider_session_id"].as_str() {
                output.push_str(&format!("provider_session_id: {provider_session_id}\n"));
            }
            output.push_str(&format!(
                "mode: {}\ncontent: {}\nformat: text\n\n",
                value["mode"].as_str().unwrap_or("lite"),
                value["content_policy"].as_str().unwrap_or("indexed")
            ));
        }
        _ => {
            output.push_str(&format!(
                "ctx_event_id: {}\nctx_session_id: {}\ncontent: {}\n\n",
                value["ctx_event_id"].as_str().unwrap_or("unknown"),
                value["ctx_session_id"].as_str().unwrap_or("unknown"),
                value["content_policy"].as_str().unwrap_or("indexed")
            ));
        }
    }
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "[{}] {} {} {}\n{}\n\n",
            event["occurred_at"].as_str().unwrap_or("-"),
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
            event["text"].as_str().unwrap_or_default()
        ));
    }
    output
}

fn render_show_markdown(value: &Value) -> String {
    let mut output = match value["target"].as_str() {
        Some("session") => format!(
            "# {} session {}\n\n- ctx_session_id: `{}`\n- content: `{}`\n",
            value["provider"].as_str().unwrap_or("unknown"),
            value["provider_session_id"]
                .as_str()
                .or_else(|| value["ctx_session_id"].as_str())
                .unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown"),
            value["content_policy"].as_str().unwrap_or("indexed")
        ),
        _ => format!(
            "# Event {}\n\n- ctx_session_id: `{}`\n- content: `{}`\n",
            value["ctx_event_id"].as_str().unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown"),
            value["content_policy"].as_str().unwrap_or("indexed")
        ),
    };
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "\n## {} - {} - {}\n\nctx_event_id: `{}`\n\n{}\n",
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["occurred_at"].as_str().unwrap_or("-"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
            event["text"].as_str().unwrap_or_default()
        ));
    }
    output
}

fn event_window(
    index: &VerifiedIndex,
    selected: &EventRecord,
    before: usize,
    after: usize,
    window: Option<usize>,
) -> Result<Vec<EventRecord>> {
    let events = index.events_for_session(selected.session_id.as_uuid())?;
    let position = events
        .iter()
        .position(|event| event.event_id == selected.event_id)
        .ok_or_else(|| anyhow!("selected source-backed event is absent from its session"))?;
    let (before, after) = window
        .map(|window| (window, window))
        .unwrap_or((before, after));
    let start = position.saturating_sub(before);
    let end = position
        .saturating_add(after)
        .saturating_add(1)
        .min(events.len());
    Ok(events[start..end].to_vec())
}

fn select_session_events(events: &[EventRecord], mode: TranscriptMode) -> Vec<&EventRecord> {
    match mode {
        TranscriptMode::Log => events.iter().collect(),
        TranscriptMode::Full => events
            .iter()
            .filter(|event| {
                event.event_type == EventType::Message.as_str()
                    && matches!(event.role.as_deref(), Some("user" | "assistant" | "system"))
            })
            .collect(),
        TranscriptMode::Lite => {
            let mut selected = Vec::new();
            let mut pending_assistant = None;
            for event in events {
                if event.event_type != EventType::Message.as_str() {
                    continue;
                }
                match event.role.as_deref() {
                    Some("user") => {
                        if let Some(assistant) = pending_assistant.take() {
                            selected.push(assistant);
                        }
                        selected.push(event);
                    }
                    Some("assistant") => pending_assistant = Some(event),
                    _ => {}
                }
            }
            if let Some(assistant) = pending_assistant {
                selected.push(assistant);
            }
            selected
        }
    }
}

fn resolve_event(index: &VerifiedIndex, id: &str) -> Result<EventRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index.event_by_id(uuid)?.ok_or_else(|| {
            anyhow!("event {uuid} was not found in the source-backed Core generation")
        });
    }
    let prefix = normalize_uuid_prefix(id, "event")?;
    match index.events_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(anyhow!(
            "event id prefix {prefix:?} was not found in the source-backed Core generation"
        )),
        [event] => Ok(event.clone()),
        matches => Err(anyhow!(
            "event id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_event_id",
            matches[0].event_id,
            matches[1].event_id
        )),
    }
}

fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<SessionRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index.session_by_id(uuid)?.ok_or_else(|| {
            anyhow!("session {uuid} was not found in the source-backed Core generation")
        });
    }
    let prefix = normalize_uuid_prefix(id, "session")?;
    match index.sessions_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(anyhow!(
            "session id prefix {prefix:?} was not found in the source-backed Core generation"
        )),
        [session] => Ok(session.clone()),
        matches => Err(anyhow!(
            "session id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_session_id",
            matches[0].session_id,
            matches[1].session_id
        )),
    }
}

fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    let root = index_root(data_root);
    if !root.join("meta.json").is_file() {
        return Err(anyhow!(
            "source-backed Core index is not initialized at {}",
            root.display()
        ));
    }
    VerifiedIndex::open(&root)
        .with_context(|| format!("open verified source-backed Core index {}", root.display()))
}

fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

fn timestamp_json(timestamp: Option<i64>) -> Option<String> {
    timestamp
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn phase_attribution(query: Duration) -> Value {
    json!({
        "discovery_seconds": 0.0,
        "writer_open_seconds": 0.0,
        "scan_and_stage_seconds": 0.0,
        "scanner_worker_busy_seconds": 0.0,
        "writer_add_document_seconds": 0.0,
        "certification_seconds": 0.0,
        "index_commit_seconds": 0.0,
        "refresh_total_seconds": 0.0,
        "query_seconds": query.as_secs_f64(),
        "catalog_sources": 0,
        "catalog_source_bytes": 0,
        "cold_sources": 0,
        "appended_sources": 0,
        "replaced_sources": 0,
        "replayed_sources": 0,
        "deleted_sources": 0,
        "scanner_bytes_read": 0,
        "checkpoint_validation_bytes": 0,
        "scanner_workers": 0,
        "complete_records_scanned": 0,
        "retained_records_scanned": 0,
        "rejected_records_scanned": 0,
        "ignored_records_scanned": 0,
        "staged_documents": 0,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::HashMap,
        fs,
    };

    use ctx_history_capture::ingest_codex_source_backed_v0;
    use ctx_history_core::{
        database_path, derive_event_id, derive_session_id, BatchHydrationRequest,
        BatchHydrationResult, ContentSourceResolver, EventHydrationRequest, EventIdentityInput,
        HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
        NativeItemKey, NativeRecordCoordinate, NativeSessionKey, SessionIdentityInput,
        SourceAnchor, SourceKey, SourceRecordLocator, StableEntityId, TypedKey,
    };
    use serde_json::json;
    use tempfile::tempdir;

    use crate::semantic::SourceBackedRefreshDaemonUnavailable;

    use super::*;

    const TEST_SESSION_ID: &str = "019fa000-0000-7000-8000-0000000000d1";
    const TEST_QUERY: &str = "pinnedgenerationrouting";

    #[derive(Default)]
    struct MockContentResolver {
        bodies: HashMap<StableEntityId, Vec<u8>>,
        calls: RefCell<Vec<(String, String)>>,
        batch_calls: Cell<usize>,
    }

    impl MockContentResolver {
        fn with_body(mut self, event: &EventRecord, body: impl Into<Vec<u8>>) -> Self {
            self.bodies.insert(event.event_id, body.into());
            self
        }
    }

    impl ContentSourceResolver for MockContentResolver {
        fn hydrate_event(
            &self,
            request: &EventHydrationRequest,
        ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
            self.calls.borrow_mut().push((
                request.locator().source().provider().to_owned(),
                request.locator().source().source_format().to_owned(),
            ));
            let provider_bytes =
                self.bodies
                    .get(&request.event_id())
                    .cloned()
                    .ok_or_else(|| HydrationFailure {
                        kind: HydrationFailureKind::MissingRecord,
                        detail: "mock provider record is absent".to_owned(),
                    })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            })
        }

        fn hydrate_batch(
            &self,
            request: &BatchHydrationRequest,
        ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
            self.batch_calls
                .set(self.batch_calls.get().saturating_add(1));
            let records = request
                .events()
                .iter()
                .map(|event| self.hydrate_event(event))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let result = BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: error.to_string(),
            })?;
            result.validate_for_request(request)?;
            Ok(result)
        }
    }

    fn fixture_event(
        provider: CaptureProvider,
        source_format: &str,
        lineage: u8,
        sequence: u64,
    ) -> EventRecord {
        let source = SourceKey::derive(
            provider.as_str(),
            source_format,
            "fixture",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap();
        let native_session_key = NativeSessionKey::native_id(
            "session",
            TypedKey::utf8(format!("fixture-session-{lineage}")).unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let locator = SourceRecordLocator::new(
            source,
            NativeRecordCoordinate::ProviderNative {
                namespace: "fixture".to_owned(),
                coordinate: TypedKey::U64(sequence),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some([lineage; 32]),
            [sequence as u8; 32],
        )
        .unwrap();
        EventRecord {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            locator,
            provider: provider.as_str().to_owned(),
            source_format: source_format.to_owned(),
            provider_session_id: Some(format!("fixture-session-{lineage}")),
            branch: None,
            source_path: None,
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: None,
            event_type: "message".to_owned(),
            role: Some("assistant".to_owned()),
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        }
    }

    fn request(refresh: RefreshArg) -> SourceSearchRequest {
        SourceSearchRequest {
            query: TEST_QUERY.to_owned(),
            terms: Vec::new(),
            limit: 10,
            provider: Some(CaptureProvider::Codex),
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            workspace: None,
            since: None,
            primary_only: false,
            include_subagents: false,
            event_type: None,
            file: None,
            session: None,
            events: false,
            include_current_session: true,
            backend: Some(SearchBackendArg::Lexical),
            semantic_weight: 0.35,
            semantic_enabled: true,
            refresh,
        }
    }

    fn write_test_generation(data_root: &Path) {
        let sessions = data_root.join("sessions");
        let source = sessions.join(format!("rollout-{TEST_SESSION_ID}.jsonl"));
        fs::create_dir_all(&sessions).unwrap();
        let records = [
            json!({
                "timestamp": "2026-07-28T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": TEST_SESSION_ID,
                    "timestamp": "2026-07-28T12:00:00Z",
                    "cwd": "/workspace/pinned",
                    "originator": "codex_cli_rs",
                    "cli_version": "0.1.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-07-28T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("{TEST_QUERY} sentinel")
                    }]
                }
            }),
        ];
        let body = records
            .iter()
            .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
            .collect::<String>();
        fs::write(source, body).unwrap();
        ingest_codex_source_backed_v0(&sessions, index_root(data_root)).unwrap();
    }

    #[test]
    fn core_refresh_modes_map_to_the_daemon_contract() {
        assert_eq!(
            source_backed_refresh_mode(RefreshArg::Off),
            SourceBackedRefreshMode::Off
        );
        assert_eq!(
            source_backed_refresh_mode(RefreshArg::Background),
            SourceBackedRefreshMode::Background
        );
        assert_eq!(
            source_backed_refresh_mode(RefreshArg::Wait),
            SourceBackedRefreshMode::Wait
        );
    }

    #[test]
    fn daemon_unavailable_error_remains_typed_through_core_routing() {
        let temp = tempdir().unwrap();
        let error = match refresh_for_search(&request(RefreshArg::Wait), temp.path()) {
            Ok(_) => panic!("refresh unexpectedly succeeded without a daemon"),
            Err(error) => error,
        };
        assert!(error
            .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
            .is_some());
        assert!(format!("{error:#}").contains("no foreground writer was started"));
    }

    #[test]
    fn core_search_consumes_the_coordinator_pin_without_reopening() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let requested_mode = Cell::new(None);
        let outcome =
            refresh_for_search_with(&request(RefreshArg::Off), temp.path(), |data_root, mode| {
                requested_mode.set(Some(mode));
                Ok(SourceBackedRefreshObservation {
                    mode,
                    status: "off".to_owned(),
                    request_id: None,
                    daemon_available: false,
                    source_count: 0,
                    pin: PinnedSourceBackedGeneration::from_index(open_index(data_root)?),
                })
            })
            .unwrap();
        assert_eq!(requested_mode.get(), Some(SourceBackedRefreshMode::Off));
        let generation = outcome.pin.generation_id().to_owned();

        fs::remove_file(index_root(temp.path()).join("meta.json")).unwrap();
        let (value, collection, index) = search_existing_generation_with_hydrator(
            &request(RefreshArg::Off),
            outcome.pin.into_index(),
            temp.path(),
            0.35,
            outcome.status,
            outcome.source_count,
            |_index, _data_root, events| {
                Ok(events
                    .iter()
                    .map(|event| {
                        (
                            event.event_id.as_uuid(),
                            "exact injected search body".to_owned(),
                        )
                    })
                    .collect())
            },
        )
        .unwrap();

        assert_eq!(index.generation_id(), generation);
        assert_eq!(value["retrieval"]["generation_id"], generation);
        assert_eq!(collection.hits.len(), 1);
    }

    #[test]
    fn refresh_off_surfaces_typed_resolver_unavailable_without_retrying() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let wait_calls = Cell::new(0);
        let error = match search_with_hydration_retry_with(
            &request(RefreshArg::Off),
            temp.path(),
            0.35,
            RefreshOutcome {
                pin: PinnedSourceBackedGeneration::from_index(open_index(temp.path()).unwrap()),
                status: "existing_generation",
                source_count: 0,
            },
            search_existing_generation,
            |_request, _data_root| {
                wait_calls.set(wait_calls.get() + 1);
                panic!("refresh off must not retry source discovery")
            },
        ) {
            Ok(_) => panic!("refresh-off hydration unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(PinnedSourceBackedGeneration::source_hydration_retryable(
            &error
        ));
        assert!(format!("{error:#}").contains("resolver_service_unavailable"));
        assert_eq!(wait_calls.get(), 0);
    }

    #[test]
    fn background_search_retries_hydration_once_after_daemon_wait_repin() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let run_calls = Cell::new(0);
        let wait_calls = Cell::new(0);
        let outcome = search_with_hydration_retry_with(
            &request(RefreshArg::Background),
            temp.path(),
            0.35,
            RefreshOutcome {
                pin: PinnedSourceBackedGeneration::from_index(open_index(temp.path()).unwrap()),
                status: "daemon_background",
                source_count: 1,
            },
            |request, index, data_root, semantic_weight, status, source_count| {
                run_calls.set(run_calls.get() + 1);
                if run_calls.get() == 1 {
                    search_existing_generation(
                        request,
                        index,
                        data_root,
                        semantic_weight,
                        status,
                        source_count,
                    )
                } else {
                    search_existing_generation_with_hydrator(
                        request,
                        index,
                        data_root,
                        semantic_weight,
                        status,
                        source_count,
                        |_index, _data_root, events| {
                            Ok(events
                                .iter()
                                .map(|event| {
                                    (
                                        event.event_id.as_uuid(),
                                        "exact source after daemon repin".to_owned(),
                                    )
                                })
                                .collect())
                        },
                    )
                }
            },
            |_request, data_root| {
                wait_calls.set(wait_calls.get() + 1);
                Ok(RefreshOutcome {
                    pin: PinnedSourceBackedGeneration::from_index(open_index(data_root)?),
                    status: "published",
                    source_count: 1,
                })
            },
        )
        .unwrap();

        assert_eq!(outcome.3, "published");
        assert_eq!(
            outcome.0["results"][0]["snippet"],
            "exact source after daemon repin"
        );
        assert_eq!(run_calls.get(), 2);
        assert_eq!(wait_calls.get(), 1);
    }

    #[test]
    fn generation_only_semantic_is_typed_and_hybrid_falls_back_without_exact_projection() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        assert!(!database_path(temp.path().to_path_buf()).exists());

        let mut lexical_request = request(RefreshArg::Off);
        lexical_request.backend = Some(SearchBackendArg::Lexical);
        let filters = index_search_filters(&lexical_request, &index).unwrap();
        let lexical =
            collect_search_hits_with_backend(&lexical_request, &index, temp.path(), 0.35, &filters)
                .unwrap();
        assert_eq!(lexical.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(lexical.hits.len(), 1);

        let mut hybrid_request = request(RefreshArg::Off);
        hybrid_request.backend = Some(SearchBackendArg::Hybrid);
        let filters = index_search_filters(&hybrid_request, &index).unwrap();
        let fallback =
            collect_search_hits_with_backend(&hybrid_request, &index, temp.path(), 0.35, &filters)
                .unwrap();
        assert_eq!(fallback.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(fallback.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(fallback.semantic_weight, 0.35);
        assert_eq!(fallback.semantic_status, "unavailable");
        assert_eq!(
            fallback.semantic_fallback.as_ref().map(|value| value.code),
            Some("semantic_store_missing")
        );
        assert_eq!(fallback.hits.len(), 1);

        let mut semantic_request = request(RefreshArg::Off);
        semantic_request.backend = Some(SearchBackendArg::Semantic);
        let filters = index_search_filters(&semantic_request, &index).unwrap();
        let missing = collect_search_hits_with_backend(
            &semantic_request,
            &index,
            temp.path(),
            0.35,
            &filters,
        )
        .unwrap_err();
        let not_ready = missing
            .downcast_ref::<SourceBackedSemanticNotReady>()
            .expect("semantic-only errors remain typed");
        assert_eq!(not_ready.code(), "semantic_store_missing");
        assert!(not_ready.detail().contains("flat-F32"));

        let mut embedding = vec![0.0; 384];
        embedding[0] = 1.0;
        let exact_source_texts = index
            .semantic_event_page(None, ctx_history_index::MAX_SEMANTIC_EVENT_PAGE_ITEMS)
            .unwrap()
            .items
            .into_iter()
            .map(|event| {
                (
                    event.event_id.as_uuid(),
                    format!("exact provider fixture text containing {TEST_QUERY}"),
                )
            })
            .collect();
        PinnedSourceBackedGeneration::install_source_generation_flat_fixture(
            &index,
            temp.path(),
            &embedding,
            exact_source_texts,
        )
        .unwrap();
        assert!(temp
            .path()
            .join("search")
            .join("semantic")
            .is_dir());
        assert!(
            !temp.path().join("semantic-vectors").exists(),
            "the fresh source epoch must not open or reuse the legacy vector root"
        );

        for backend in [SearchBackendArg::Semantic, SearchBackendArg::Hybrid] {
            let mut source_request = request(RefreshArg::Off);
            source_request.backend = Some(backend);
            let filters = index_search_filters(&source_request, &index).unwrap();
            let collection = collect_search_hits_with_backend_using(
                &source_request,
                &index,
                temp.path(),
                0.35,
                &filters,
                |index, data_root, _query, filters, candidate_limit| {
                    PinnedSourceBackedGeneration::semantic_candidates_for_source_generation_with_embedding(
                        index,
                        data_root,
                        filters,
                        candidate_limit,
                        &embedding,
                    )
                },
            )
            .unwrap();
            assert_eq!(collection.requested_backend, backend);
            assert_eq!(collection.effective_backend, backend);
            assert_eq!(collection.semantic_status, "ready");
            assert_eq!(collection.hits.len(), 1);
            let diagnostics = collection.semantic_diagnostics.unwrap();
            assert_eq!(diagnostics["query_count"], 1);
            let query_diagnostics = &diagnostics["queries"][0]["diagnostics"];
            assert_eq!(query_diagnostics["vector_backend"], "flat_f32");
            assert_eq!(
                query_diagnostics["core_generation_id"],
                index.generation_id()
            );
            assert!(query_diagnostics["flat_generation"].as_u64().unwrap() > 0);
            assert!(query_diagnostics["flat_generation_hash"]
                .as_str()
                .is_some_and(|value| !value.is_empty()));
        }

        assert!(
            !database_path(temp.path().to_path_buf()).exists(),
            "generation-only semantic/hybrid must not create or open the legacy Store"
        );
    }

    #[test]
    fn zero_weight_hybrid_performs_no_semantic_callback_or_store_work() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let index = open_index(temp.path()).unwrap();
        let mut hybrid_request = request(RefreshArg::Off);
        hybrid_request.backend = Some(SearchBackendArg::Hybrid);
        let filters = index_search_filters(&hybrid_request, &index).unwrap();

        let collection = collect_search_hits_with_backend_using(
            &hybrid_request,
            &index,
            temp.path(),
            0.0,
            &filters,
            |_index, _data_root, _query, _filters, _candidate_limit| {
                panic!("zero-weight hybrid must not pin vectors, embed, or use IPC")
            },
        )
        .unwrap();

        assert_eq!(collection.requested_backend, SearchBackendArg::Hybrid);
        assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
        assert_eq!(collection.semantic_weight, 0.0);
        assert_eq!(collection.semantic_status, "skipped");
        assert!(collection.semantic_fallback.is_none());
        assert_eq!(collection.hits.len(), 1);
        assert!(!temp
            .path()
            .join("search")
            .join("semantic")
            .exists());
        assert!(!database_path(temp.path().to_path_buf()).exists());
    }

    #[test]
    fn mcp_source_route_applies_the_semantic_config_default_to_source_generations() {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let mut source_request = request(RefreshArg::Off);
        source_request.query = "query-with-no-fixture-match".to_owned();
        source_request.backend = None;

        let lexical = mcp_search(source_request.clone(), temp.path()).unwrap();
        assert_eq!(lexical["retrieval"]["requested_mode"], "lexical");
        assert_eq!(lexical["retrieval"]["effective_mode"], "lexical");

        config::set_semantic_search_enabled(temp.path(), true).unwrap();
        let hybrid = mcp_search(source_request, temp.path()).unwrap();
        assert_eq!(hybrid["retrieval"]["requested_mode"], "hybrid");
        assert_eq!(hybrid["retrieval"]["effective_mode"], "lexical");
        assert_eq!(
            hybrid["retrieval"]["semantic_fallback_code"],
            "semantic_store_missing"
        );

        let mut file_only = request(RefreshArg::Off);
        file_only.query.clear();
        file_only.backend = None;
        file_only.file = Some(PathBuf::from("/fixture/no-match.rs"));
        let file_only = mcp_search(file_only, temp.path()).unwrap();
        assert_eq!(file_only["retrieval"]["requested_mode"], "lexical");
        assert_eq!(file_only["retrieval"]["effective_mode"], "lexical");
        assert!(!database_path(temp.path().to_path_buf()).exists());
    }

    #[test]
    fn complete_content_hydrates_typed_locators_for_multiple_providers() {
        let codex = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 1, 1);
        let warp = fixture_event(CaptureProvider::Warp, "warp_sqlite", 2, 2);
        let resolver = MockContentResolver::default()
            .with_body(&codex, "complete Codex source")
            .with_body(&warp, "complete Warp source");
        let resolved = resolve_complete_contents(&[&codex, &warp], usize::MAX, &resolver).unwrap();

        assert_eq!(resolved[0].text, "complete Codex source");
        assert_eq!(resolved[1].text, "complete Warp source");
        assert_eq!(resolver.batch_calls.get(), 1);
        assert_eq!(
            resolver.calls.into_inner(),
            vec![
                ("codex".to_owned(), "codex_session_jsonl".to_owned()),
                ("warp".to_owned(), "warp_sqlite".to_owned()),
            ]
        );
    }

    #[test]
    fn complete_content_fails_when_exact_source_is_unavailable() {
        let event = fixture_event(CaptureProvider::Warp, "warp_sqlite", 3, 3);
        let error =
            resolve_complete_contents(&[&event], usize::MAX, &MockContentResolver::default())
                .unwrap_err();

        assert!(format!("{error:#}").contains("mock provider record is absent"));
    }

    #[test]
    fn complete_content_rejects_non_utf8_provider_bytes() {
        let event = fixture_event(CaptureProvider::Warp, "warp_sqlite", 4, 4);
        let resolver = MockContentResolver::default().with_body(&event, vec![b'o', b'k', 0x80]);
        let error = resolve_complete_contents(&[&event], usize::MAX, &resolver).unwrap_err();

        assert!(format!("{error:#}").contains("non-UTF-8 exact content"));
    }

    #[test]
    fn complete_content_preserves_the_cumulative_output_limit() {
        let first = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 5, 5);
        let second = fixture_event(CaptureProvider::Warp, "warp_sqlite", 6, 6);
        let resolver = MockContentResolver::default()
            .with_body(&first, "four")
            .with_body(&second, "five");
        let error = resolve_complete_contents(&[&first, &second], 7, &resolver).unwrap_err();

        assert!(format!("{error:#}").contains("exceeds the 7-byte output limit"));
        assert!(format!("{error:#}").contains(&second.event_id.to_string()));
    }
}
