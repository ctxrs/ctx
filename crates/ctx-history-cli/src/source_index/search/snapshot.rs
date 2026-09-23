//! Read-only history query projection shared by CLI composition and MCP.

use super::*;

pub fn mcp_search_with_compact(
    request: SourceSearchRequest,
    data_root: &Path,
    config: HistoryCliConfig,
) -> std::result::Result<
    (
        Value,
        SearchContextObservation,
        Value,
        SearchExecutionObservation,
    ),
    McpSearchExecutionFailure,
> {
    snapshot_search(request, data_root, config, None)
}

/// Read existing lexical history without refresh, preserving CLI active-session exclusion.
pub fn cli_snapshot_search(
    request: SourceSearchRequest,
    data_root: &Path,
    config: HistoryCliConfig,
) -> SnapshotSearchResult {
    snapshot_search(
        request,
        data_root,
        config,
        super::super::detected_active_session(),
    )
}

type SnapshotSearchResult = std::result::Result<
    (
        Value,
        SearchContextObservation,
        Value,
        SearchExecutionObservation,
    ),
    McpSearchExecutionFailure,
>;

fn snapshot_search(
    request: SourceSearchRequest,
    data_root: &Path,
    config: HistoryCliConfig,
    active_session: Option<ctx_history_read_application::ActiveSessionExclusion>,
) -> SnapshotSearchResult {
    let config = config::AppConfig::from_snapshot(config);
    let semantic_port = crate::semantic::SemanticQueryAdapter::new(data_root);
    let policy = source_search_policy(&config, false);
    let mut observation = initial_search_observation();
    match mcp_search_inner(
        request,
        data_root,
        &config,
        policy,
        &semantic_port,
        active_session,
        &mut observation,
    ) {
        Ok((value, context, compact)) => Ok((value, context, compact, observation)),
        Err(error) => Err(McpSearchExecutionFailure {
            error: error.into_mcp(),
            observation: Box::new(observation),
        }),
    }
}

pub fn normalize_mcp_search_request(
    request: &mut SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    ctx_history_read_application::normalize_search_request(request)
        .map_err(|error| SourceSearchFailure::from(error).into_mcp())
}

fn mcp_search_inner<P: HistorySemanticPort>(
    request: SourceSearchRequest,
    data_root: &Path,
    config: &config::AppConfig,
    policy: ctx_history_read_application::SearchPolicy,
    semantic_port: &P,
    active_session: Option<ctx_history_read_application::ActiveSessionExclusion>,
    observation: &mut SearchExecutionObservation,
) -> SourceSearchResult<(Value, SearchContextObservation, Value)> {
    let plan = ctx_history_read_application::plan_search(request, policy)?;
    let requested_backend = plan.request().backend.unwrap_or(policy.default_backend);
    observation.backend_requested = Some(requested_backend);
    let retained_peer =
        ctx_history_read_application::retained_peer_read_for_search(plan.request(), true);
    let refresh = observed_refresh_for_search(
        plan.request(),
        RefreshArg::Off,
        data_root,
        retained_peer,
        observation,
    )?;
    let result = search_pinned_generation(
        plan,
        data_root,
        RefreshArg::Off,
        refresh,
        true,
        semantic_port,
        active_session,
        observation,
    );
    let (value, application) = result?;
    let collection = &application.query().collection;
    let context = if config.local_usage.enabled {
        search_context_observation(&value, collection, application.index())
    } else {
        SearchContextObservation::unavailable()
    };
    observation.result_count = value["results"]
        .as_array()
        .map(|results| results.len() as u64);
    observation.citation_count = Some(collection.result_window.hits.len() as u64);
    observation.zero_result = Some(collection.result_window.hits.is_empty());
    observation.has_indexed_content_after = Some(application.index().document_count() > 0);
    observation.failure_phase = Some(SearchFailurePhase::ResultProjection);
    let compact_value = match application.project_read_model(&value) {
        Ok(value) => value,
        Err(error) => return Err(SourceSearchFailure::from(error)),
    };
    observation.failure_phase = None;
    Ok((value, context, compact_value))
}
