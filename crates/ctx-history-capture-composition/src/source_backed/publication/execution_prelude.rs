use super::*;

pub(super) struct RefreshPrelude {
    pub(super) selected_route_ids: BTreeSet<SourceRouteIdentity>,
    pub(super) scanned_routes: usize,
    pub(super) providers: Vec<CaptureProvider>,
    pub(super) unsupported_routes: Vec<SourceBackedRouteMetadata>,
}

pub(super) fn configured_provider_root_route_ids(
    registry: &SourceBackedProviderRegistry,
) -> BTreeSet<SourceRouteIdentity> {
    registry
        .applied_provider_roots
        .iter()
        .flat_map(|(_, _, roots)| roots)
        .flat_map(|root| root.routes().iter().cloned())
        .collect()
}

pub(super) fn provider_roots_for_publication(
    registry: &SourceBackedProviderRegistry,
) -> ctx_history_index::Result<Option<(bool, String, Vec<AppliedProviderRoot>)>> {
    let Some((automatic, digest, roots)) = registry.applied_provider_roots.as_ref() else {
        return Ok(None);
    };
    // This is the requested topology. Generation construction intersects its
    // route memberships with the exact final source-route snapshot, after
    // cold scan and terminal-revalidation failures are known.
    Ok(Some((*automatic, digest.clone(), roots.clone())))
}

pub(super) fn publication_selected_route_ids(
    registry: &SourceBackedProviderRegistry,
    selected_route_ids: &BTreeSet<SourceRouteIdentity>,
    base_route_ids: &BTreeSet<SourceRouteIdentity>,
    configured_route_ids: &BTreeSet<SourceRouteIdentity>,
) -> BTreeSet<SourceRouteIdentity> {
    selected_route_ids
        .iter()
        .filter(|route_identity| {
            base_route_ids.contains(*route_identity)
                || !omit_empty_automatic_route(registry, route_identity, configured_route_ids)
                || !registry.routes.iter().any(|route| {
                    route.metadata.route_identity.as_ref() == Some(*route_identity)
                        && !route.certified_missing_paths.is_empty()
                })
        })
        .cloned()
        .collect()
}

pub(super) fn omit_empty_automatic_route(
    registry: &SourceBackedProviderRegistry,
    route_identity: &SourceRouteIdentity,
    configured_route_ids: &BTreeSet<SourceRouteIdentity>,
) -> bool {
    registry.applied_provider_roots.is_some()
        && !configured_route_ids.contains(route_identity)
        && registry.routes.iter().any(|route| {
            route.metadata.route_identity.as_ref() == Some(route_identity)
                && route.metadata.selection == Some(SourceBackedRouteSelection::Automatic)
        })
}

pub(super) fn prepare_refresh(
    registry: &SourceBackedProviderRegistry,
    plan: &SourceBackedRefreshPlan,
) -> SourceBackedCoordinatorResult<RefreshPrelude> {
    if matches!(&plan.scope, SourceBackedRefreshScope::All) {
        if let Some(unavailable) = registry.routes.iter().find(|route| {
            route.driver.is_none()
                && route.certified_missing_paths.is_empty()
                && route.metadata.source.status == ProviderSourceStatus::Unknown
                && route.metadata.route_identity.is_none()
        }) {
            return Err(SourceBackedCoordinatorError::UnavailableRoute {
                provider: unavailable.metadata.source.provider,
                detail: unavailable
                    .metadata
                    .unsupported_reason
                    .clone()
                    .unwrap_or_else(|| "route state is unavailable".to_owned()),
            });
        }
    }
    let executable_route_ids = registry
        .routes
        .iter()
        .filter(|route| {
            route.driver.is_some()
                || !route.certified_missing_paths.is_empty()
                || (matches!(
                    route.metadata.source.status,
                    ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown
                ) && route.metadata.route_identity.is_some())
        })
        .filter_map(|route| route.metadata.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let selected_route_ids = match &plan.scope {
        SourceBackedRefreshScope::All => executable_route_ids,
        SourceBackedRefreshScope::Exact(selected) => {
            if let Some(unknown) = selected.difference(&executable_route_ids).next() {
                return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                    route_id: unknown.as_str().to_owned(),
                });
            }
            selected.clone()
        }
    };
    let selected_routes = registry.routes.iter().filter(|route| {
        route.driver.is_some()
            && route
                .metadata
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected_route_ids.contains(identity))
    });
    let scanned_routes = selected_routes.clone().count();
    let mut selected_provider_set = HashSet::new();
    let providers = selected_routes
        .filter_map(|route| {
            selected_provider_set
                .insert(route.metadata.source.provider)
                .then_some(route.metadata.source.provider)
        })
        .collect::<Vec<_>>();
    let unsupported_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_none() && route.metadata.route_identity.is_none())
        .map(|route| route.metadata.clone())
        .collect();
    Ok(RefreshPrelude {
        selected_route_ids,
        scanned_routes,
        providers,
        unsupported_routes,
    })
}

pub(super) fn discovery_started_progress(
    scanned_routes: usize,
    providers: Vec<CaptureProvider>,
    discovery_duration: Duration,
) -> SourceBackedDetailedRefreshProgress {
    source_level_progress(SourceBackedRefreshProgress {
        phase: "discovering",
        completed_sources: 0,
        total_sources: scanned_routes,
        current_source: None,
        completed_records: None,
        completed_bytes: None,
        providers,
        processed_sessions: 0,
        processed_messages: 0,
        processed_tool_calls: 0,
        processed_bytes: 0,
        stage_duration: discovery_duration,
        elapsed: discovery_duration,
        certified_source_count: None,
        certified_source_bytes: None,
    })
}

pub(super) fn committed_progress(
    scanned_routes: usize,
    providers: Vec<CaptureProvider>,
    history: ctx_history_capture_model::AttemptHistoryProgressSnapshot,
    commit_duration: Duration,
    elapsed: Duration,
    certified_source_count: usize,
    certified_source_bytes: u64,
) -> SourceBackedDetailedRefreshProgress {
    source_level_progress(SourceBackedRefreshProgress {
        phase: "committed",
        completed_sources: scanned_routes,
        total_sources: scanned_routes,
        current_source: None,
        completed_records: None,
        completed_bytes: None,
        providers,
        processed_sessions: history.processed_sessions,
        processed_messages: history.processed_messages,
        processed_tool_calls: history.processed_tool_calls,
        processed_bytes: history.processed_bytes,
        stage_duration: commit_duration,
        elapsed,
        certified_source_count: Some(certified_source_count),
        certified_source_bytes: Some(certified_source_bytes),
    })
}
