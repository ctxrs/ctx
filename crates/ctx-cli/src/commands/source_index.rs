use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_capture::{
    build_automatic_source_backed_registry, source_backed_route_inventory, DiscoveryContext,
    SourceBackedHydrationSupport, SourceBackedResolverRegistry,
};
use ctx_history_core::{CaptureProvider, ContentSourceResolver, EventHydrationRequest, EventType};
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
    output::{compact_json, print_json, JsonOutputFormat, OutputFormat},
    provider_sources::home_dir,
    search_filters::parse_since_filter,
    semantic::{
        coordinate_source_backed_refresh, PinnedSourceBackedGeneration, SourceBackedRefreshMode,
        SourceBackedRefreshObservation,
    },
    transcript::{normalize_uuid_prefix, shell_quote_arg, write_output, TranscriptMode},
    RefreshArg, SearchArgs, SearchBackendArg, ShowArgs, ShowTarget,
};

const INDEX_DIRECTORY: &str = "source-backed-lexical-v0";
const LEGACY_ACTIVE_SESSION_PROVIDER_ENV: &str = "CODEX_THREAD_ID";
const LEGACY_ACTIVE_SESSION_PROVIDER: CaptureProvider = CaptureProvider::Codex;
const MAX_SESSION_DIVERSITY_CANDIDATES: usize = 64 * 1024;
const MIN_CANDIDATE_BATCH: usize = 256;
const CANDIDATE_OVERSAMPLE: usize = 8;

#[derive(Debug)]
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
            refresh: args.refresh,
        }
    }
}

#[derive(Debug)]
struct SearchCollection {
    hits: Vec<SearchHit>,
    candidate_pool: usize,
    candidate_pool_truncated: bool,
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
    complete_content_available: bool,
    complete: bool,
    source_verified: bool,
}

pub(crate) fn index_is_available(data_root: &Path) -> bool {
    index_root(data_root).join("meta.json").is_file()
}

pub(crate) fn should_run_search(args: &SearchArgs, data_root: &Path) -> bool {
    should_route_source_backed_search(
        index_is_available(data_root),
        args.refresh,
        args.provider.map(|provider| provider.capture_provider()),
        args.source_format.as_deref(),
    )
}

fn should_route_source_backed_search(
    index_available: bool,
    refresh: RefreshArg,
    provider: Option<CaptureProvider>,
    source_format: Option<&str>,
) -> bool {
    index_available
        || (refresh != RefreshArg::Off
            && automatic_source_backed_route_supported(provider, source_format))
}

fn automatic_source_backed_route_supported(
    provider: Option<CaptureProvider>,
    source_format: Option<&str>,
) -> bool {
    source_backed_route_inventory().iter().any(|route| {
        route.automatic
            && route.unsupported_reason.is_none()
            && provider.is_none_or(|provider| route.provider == provider)
            && source_format.is_none_or(|source_format| {
                route.source_format == source_format
                    || route.certified_source_format == source_format
            })
    })
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
) -> Result<()> {
    let request = SourceSearchRequest::from(&args);
    let refresh_started = Instant::now();
    let refresh = refresh_for_search(&request, &data_root)?;
    telemetry.refresh_duration = Some(duration_bucket(refresh_started.elapsed()));
    telemetry.refresh_mode = Some(request.refresh);
    telemetry.refresh_status = Some(RefreshStatus::from_safe_summary(refresh.status));
    telemetry.refresh_source_count = Some(count_bucket(refresh.source_count as u64));

    let query_started = Instant::now();
    let (value, collection, index) = search_existing_generation(
        &request,
        refresh.pin.into_index(),
        refresh.status,
        refresh.source_count,
    )?;
    let query_duration = query_started.elapsed();
    telemetry.query_duration = Some(duration_bucket(query_duration));
    telemetry.query_length = Some(text_length_bucket(request.query.chars().count()));
    telemetry.query_term_count = Some(count_bucket(
        request
            .query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .count() as u64,
    ));
    telemetry.backend_requested = Some(SearchBackendArg::Lexical);
    telemetry.backend_effective = Some(SearchBackendArg::Lexical);
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

pub(crate) fn mcp_search(request: SourceSearchRequest, data_root: &Path) -> Result<Value> {
    let (value, _, _) =
        search_existing_generation(&request, open_index(data_root)?, "existing_generation", 0)?;
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

fn search_existing_generation(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    refresh_status: &str,
    refresh_source_count: usize,
) -> Result<(Value, SearchCollection, VerifiedIndex)> {
    validate_search_request(request)?;
    let filters = index_search_filters(request, &index)?;
    let query_started = Instant::now();
    let collection = collect_search_hits(
        &index,
        &request.query,
        request.limit,
        request.events,
        &filters,
    )?;
    let query_duration = query_started.elapsed();
    let value = search_json(
        request,
        &index,
        &collection,
        refresh_status,
        refresh_source_count,
        query_duration,
    );
    Ok((value, collection, index))
}

fn validate_search_request(request: &SourceSearchRequest) -> Result<()> {
    if matches!(
        request.backend,
        Some(SearchBackendArg::Hybrid | SearchBackendArg::Semantic)
    ) {
        return Err(anyhow!(
            "the source-backed Core generation currently supports lexical retrieval only; no legacy backend was used"
        ));
    }
    if request.terms.iter().any(|term| !term.trim().is_empty()) {
        return Err(anyhow!(
            "OR-composed --term search is not yet exposed by the source-backed index query API"
        ));
    }
    if request.history_source.is_some()
        || request.provider_key.is_some()
        || request.source_id.is_some()
    {
        return Err(anyhow!(
            "custom history source identity filters are not yet exposed by the source-backed index query API"
        ));
    }
    if request.query.trim().is_empty() {
        if request.file.is_some() {
            return Err(anyhow!(
                "file-only search is not yet exposed by the source-backed index query API; add a text query"
            ));
        }
        return Err(anyhow!("source-backed search needs a non-empty text query"));
    }
    Ok(())
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

fn collect_search_hits(
    index: &VerifiedIndex,
    query: &str,
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
        let candidates =
            index.search_event_candidates_with_filters(query.trim(), filters, candidate_limit)?;
        let exhausted = candidates.len() < candidate_limit || candidate_limit >= document_count;
        let hits = shape_search_hits(candidates.iter(), limit, event_results);
        let enough = hits.len() >= limit;
        if enough || exhausted {
            return Ok(SearchCollection {
                hits,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: false,
            });
        }
        if candidate_limit >= maximum {
            return Ok(SearchCollection {
                hits,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: true,
            });
        }
        candidate_limit = candidate_limit
            .saturating_mul(2)
            .min(maximum)
            .max(candidate_limit.saturating_add(1));
    }
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
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Value {
    let result_scope = if request.events { "event" } else { "session" };
    let results = collection
        .hits
        .iter()
        .map(|hit| search_result_json(hit, result_scope, &request.query))
        .collect::<Vec<_>>();
    let phase_attribution = phase_attribution(query_duration);
    compact_json(json!({
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
            "requested_mode": "lexical",
            "effective_mode": "lexical",
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
    }))
}

fn search_result_json(hit: &SearchHit, result_scope: &str, query: &str) -> Value {
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
        "snippet": event.preview,
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
    let rendered = render_event_values(&selected, content, output_limit_bytes)?;
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
    selected: &EventRecord,
    events: &[EventRecord],
    content: ContentPolicy,
    format: OutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let references = events.iter().collect::<Vec<_>>();
    let rendered = render_event_values(&references, content, output_limit_bytes)?;
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
    events: &[&EventRecord],
    policy: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    let resolved = resolve_contents(events, policy, output_limit_bytes)?;
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
                "preview": event.preview,
                "text": resolved.text,
                "content": {
                    "requested": policy.as_str(),
                    "complete": resolved.complete,
                    "origin": if resolved.source_verified { "provider_source" } else { "ctx_index" },
                    "stored_truncated": true,
                    "source_verified": resolved.source_verified,
                    "complete_content_available": resolved.complete_content_available,
                },
            })))
        })
        .collect()
}

fn resolve_contents(
    events: &[&EventRecord],
    policy: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<Vec<ResolvedIndexContent>> {
    if policy == ContentPolicy::Indexed {
        return Ok(events
            .iter()
            .map(|event| ResolvedIndexContent {
                text: event.preview.clone(),
                complete_content_available: exact_route_supported(event),
                complete: false,
                source_verified: false,
            })
            .collect());
    }
    if events.is_empty() {
        return Ok(Vec::new());
    }

    let registry = automatic_exact_content_resolver_registry()?;
    resolve_complete_contents(events, output_limit_bytes, &registry)
}

fn resolve_complete_contents(
    events: &[&EventRecord],
    output_limit_bytes: usize,
    resolver: &dyn ContentSourceResolver,
) -> Result<Vec<ResolvedIndexContent>> {
    let mut output_bytes = 0usize;
    let mut resolved = Vec::with_capacity(events.len());
    for event in events {
        let request = EventHydrationRequest::new(event.event_id, event.locator.clone())
            .with_context(|| format!("validate typed locator for event {}", event.event_id))?;
        let hydrated = resolver.hydrate_event(&request).map_err(|failure| {
            anyhow!(
                "hydrate source-backed {} event {} through the provider registry: {:?}: {}",
                event.provider,
                event.event_id,
                failure.kind,
                failure.detail
            )
        })?;
        if hydrated.event_id != event.event_id {
            return Err(anyhow!(
                "provider registry returned event {} while hydrating event {}",
                hydrated.event_id,
                event.event_id
            ));
        }
        let text = String::from_utf8(hydrated.provider_bytes).map_err(|error| {
            anyhow!(
                "provider registry returned non-UTF-8 exact content for {} event {}: {}",
                event.provider,
                event.event_id,
                error.utf8_error()
            )
        })?;
        output_bytes = output_bytes.saturating_add(text.len());
        if output_bytes > output_limit_bytes {
            return Err(anyhow!(
                "source-backed complete content exceeds the {output_limit_bytes}-byte output limit at event {}",
                event.event_id
            ));
        }
        resolved.push(ResolvedIndexContent {
            text,
            complete_content_available: true,
            complete: true,
            source_verified: true,
        });
    }
    Ok(resolved)
}

fn automatic_exact_content_resolver_registry() -> Result<SourceBackedResolverRegistry> {
    let home = home_dir().ok_or_else(|| {
        anyhow!("cannot discover provider sources because the user home directory is unavailable")
    })?;
    let build = build_automatic_source_backed_registry(&DiscoveryContext::from_process(home));
    Ok(build.registry.resolver_registry())
}

fn exact_route_supported(event: &EventRecord) -> bool {
    exact_route_supported_for(&event.provider, &event.source_format)
}

fn exact_route_supported_for(provider: &str, source_format: &str) -> bool {
    source_backed_route_inventory().iter().any(|route| {
        route.provider.as_str() == provider
            && route.certified_source_format == source_format
            && route.automatic
            && route.unsupported_reason.is_none()
            && matches!(
                route.exact_hydration,
                SourceBackedHydrationSupport::Full | SourceBackedHydrationSupport::Partial
            )
    })
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
    data_root.join(INDEX_DIRECTORY)
}

fn timestamp_json(timestamp: Option<i64>) -> Option<String> {
    timestamp
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
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
        derive_event_id, derive_session_id, EventIdentityInput, HydratedProviderRecord,
        HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
        NativeRecordCoordinate, NativeSessionKey, SessionHydrationRequest, SessionIdentityInput,
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

        fn hydrate_session(
            &self,
            request: &SessionHydrationRequest,
        ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
            request
                .events()
                .iter()
                .map(|event| self.hydrate_event(event))
                .collect()
        }
    }

    fn fixture_event(
        provider: CaptureProvider,
        source_format: &str,
        lineage: u8,
        sequence: u64,
        preview: &str,
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
            preview: preview.to_owned(),
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
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": format!("{TEST_QUERY} sentinel")
                    }],
                    "phase": "final_answer"
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
    fn cold_search_routes_non_codex_automatic_provider() {
        assert!(should_route_source_backed_search(
            false,
            RefreshArg::Background,
            Some(CaptureProvider::Claude),
            None,
        ));
        assert!(should_route_source_backed_search(
            false,
            RefreshArg::Wait,
            Some(CaptureProvider::Warp),
            Some("warp_sqlite"),
        ));
    }

    #[test]
    fn cold_search_without_provider_routes_all_automatic_providers() {
        assert!(should_route_source_backed_search(
            false,
            RefreshArg::Background,
            None,
            None,
        ));
        assert!(should_route_source_backed_search(
            false,
            RefreshArg::Wait,
            None,
            Some("claude_projects_jsonl_tree"),
        ));
    }

    #[test]
    fn cold_search_rejects_manual_only_and_unsupported_routes() {
        assert!(!should_route_source_backed_search(
            false,
            RefreshArg::Wait,
            Some(CaptureProvider::Custom),
            Some("ctx_history_jsonl_v1"),
        ));
        assert!(!should_route_source_backed_search(
            false,
            RefreshArg::Wait,
            Some(CaptureProvider::Codex),
            Some("codex_history_jsonl"),
        ));
    }

    #[test]
    fn refresh_off_only_routes_an_existing_source_backed_generation() {
        assert!(!should_route_source_backed_search(
            false,
            RefreshArg::Off,
            Some(CaptureProvider::Claude),
            None,
        ));
        assert!(!should_route_source_backed_search(
            false,
            RefreshArg::Off,
            None,
            None,
        ));
        assert!(should_route_source_backed_search(
            true,
            RefreshArg::Off,
            Some(CaptureProvider::Custom),
            Some("ctx_history_jsonl_v1"),
        ));
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
        let (value, collection, index) = search_existing_generation(
            &request(RefreshArg::Off),
            outcome.pin.into_index(),
            outcome.status,
            outcome.source_count,
        )
        .unwrap();

        assert_eq!(index.generation_id(), generation);
        assert_eq!(value["retrieval"]["generation_id"], generation);
        assert_eq!(collection.hits.len(), 1);
    }

    #[test]
    fn exact_route_support_comes_from_automatic_provider_metadata() {
        assert!(exact_route_supported_for("codex", "codex_session_jsonl"));
        assert!(exact_route_supported_for("warp", "warp_sqlite"));
        assert!(exact_route_supported_for("mux", "mux_session_jsonl"));
        assert!(!exact_route_supported_for("codex", "codex_history_jsonl"));
        assert!(!exact_route_supported_for(
            "unknown",
            "unknown_source_format"
        ));
    }

    #[test]
    fn complete_content_hydrates_typed_locators_for_multiple_providers() {
        let codex = fixture_event(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            1,
            1,
            "stored Codex preview",
        );
        let warp = fixture_event(
            CaptureProvider::Warp,
            "warp_sqlite",
            2,
            2,
            "stored Warp preview",
        );
        let resolver = MockContentResolver::default()
            .with_body(&codex, "complete Codex source")
            .with_body(&warp, "complete Warp source");
        let resolved = resolve_complete_contents(&[&codex, &warp], usize::MAX, &resolver).unwrap();

        assert_eq!(resolved[0].text, "complete Codex source");
        assert_eq!(resolved[1].text, "complete Warp source");
        assert!(resolved.iter().all(|content| {
            content.complete && content.source_verified && content.complete_content_available
        }));
        assert_eq!(
            resolver.calls.into_inner(),
            vec![
                ("codex".to_owned(), "codex_session_jsonl".to_owned()),
                ("warp".to_owned(), "warp_sqlite".to_owned()),
            ]
        );
    }

    #[test]
    fn complete_content_never_falls_back_to_the_index_preview() {
        let event = fixture_event(
            CaptureProvider::Warp,
            "warp_sqlite",
            3,
            3,
            "indexed fallback must not be returned",
        );
        let error =
            resolve_complete_contents(&[&event], usize::MAX, &MockContentResolver::default())
                .unwrap_err();

        assert!(format!("{error:#}").contains("mock provider record is absent"));
    }

    #[test]
    fn complete_content_rejects_non_utf8_provider_bytes() {
        let event = fixture_event(
            CaptureProvider::Warp,
            "warp_sqlite",
            4,
            4,
            "bounded preview",
        );
        let resolver = MockContentResolver::default().with_body(&event, vec![b'o', b'k', 0x80]);
        let error = resolve_complete_contents(&[&event], usize::MAX, &resolver).unwrap_err();

        assert!(format!("{error:#}").contains("non-UTF-8 exact content"));
    }

    #[test]
    fn complete_content_preserves_the_cumulative_output_limit() {
        let first = fixture_event(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            5,
            5,
            "bounded preview",
        );
        let second = fixture_event(
            CaptureProvider::Warp,
            "warp_sqlite",
            6,
            6,
            "bounded preview",
        );
        let resolver = MockContentResolver::default()
            .with_body(&first, "four")
            .with_body(&second, "five");
        let error = resolve_complete_contents(&[&first, &second], 7, &resolver).unwrap_err();

        assert!(format!("{error:#}").contains("exceeds the 7-byte output limit"));
        assert!(format!("{error:#}").contains(&second.event_id.to_string()));
    }
}
