use super::*;

pub(super) fn source_backed_refresh_failure_type(error: &anyhow::Error) -> Option<&'static str> {
    error.chain().find_map(|cause| {
        if let Some(route) = cause.downcast_ref::<SourceBackedRouteError>() {
            return match route.kind {
                SourceBackedRouteErrorKind::Unsupported => Some("unsupported_schema"),
                SourceBackedRouteErrorKind::InvalidSource => Some("malformed_source"),
                SourceBackedRouteErrorKind::Unavailable => Some("source_unavailable"),
                SourceBackedRouteErrorKind::SourceChanged => Some("source_changed"),
                SourceBackedRouteErrorKind::ResourceUnavailable
                | SourceBackedRouteErrorKind::Internal => None,
            };
        }
        let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } =
            cause.downcast_ref::<SourceBackedCoordinatorError>()?
        else {
            return None;
        };
        let classes = [
            SourceBackedSourceFailureClass::Unavailable,
            SourceBackedSourceFailureClass::SourceChanged,
            SourceBackedSourceFailureClass::Unreadable,
            SourceBackedSourceFailureClass::Incompatible,
        ];
        let present = classes
            .into_iter()
            .filter(|class| failed_routes.class_total(*class) != 0)
            .collect::<Vec<_>>();
        let [first] = present.as_slice() else {
            return Some("source_failures");
        };
        Some(match *first {
            SourceBackedSourceFailureClass::Unavailable => "source_unavailable",
            SourceBackedSourceFailureClass::SourceChanged => "source_changed",
            SourceBackedSourceFailureClass::Unreadable => "malformed_source",
            SourceBackedSourceFailureClass::Incompatible => "unsupported_schema",
        })
    })
}

pub(super) fn source_backed_refresh_error_summary(error: &anyhow::Error) -> String {
    let failed_routes = error.chain().find_map(|cause| {
        let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } =
            cause.downcast_ref::<SourceBackedCoordinatorError>()?
        else {
            return None;
        };
        Some(failed_routes)
    });
    let Some(failed_routes) = failed_routes else {
        return format!("{error:#}");
    };
    format!("source-backed refresh retained no usable source: {failed_routes}")
}

pub(super) fn find_attempt<'a>(
    state: &'a CoreRefreshEngineState,
    request_id: &str,
) -> Option<&'a SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter()
        .find(|attempt| attempt.request_id == request_id)
}

pub(super) fn find_attempt_mut<'a>(
    state: &'a mut CoreRefreshEngineState,
    request_id: &str,
) -> Option<&'a mut SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.request_id == request_id)
}

pub(super) fn coalesce_attempt(
    attempt: &mut SourceBackedRefreshAttempt,
    metadata: SourceRefreshRuntimeMetadata,
) -> Value {
    if metadata.operation == SourceBackedRefreshOperation::Import {
        attempt.operation = SourceBackedRefreshOperation::Import;
        attempt.trigger = metadata.trigger;
        attempt.trigger_provenance = metadata.trigger_provenance;
    }
    attempt.coalesced_requests = attempt.coalesced_requests.saturating_add(1);
    attempt.to_json()
}

pub(super) fn new_refresh_attempt(
    observed_generation: Option<String>,
    metadata: SourceRefreshRuntimeMetadata,
    requested_catalog: Option<ExplicitSourceCatalogAuthority>,
    refresh_scope: SourceBackedRefreshScope,
) -> SourceBackedRefreshAttempt {
    SourceBackedRefreshAttempt {
        request_id: Uuid::now_v7().to_string(),
        state: SourceBackedRefreshState::Queued,
        requested_at_ms: utc_now().timestamp_millis(),
        started_at_ms: None,
        finished_at_ms: None,
        previous_generation: observed_generation.clone(),
        published_generation: observed_generation,
        refresh_scope,
        operation: metadata.operation,
        requested_explicit_source_catalog: requested_catalog,
        coalesced_requests: 0,
        progress: SourceBackedRefreshProgress::default(),
        scanned_routes: None,
        unsupported_routes: None,
        certified_source_count: None,
        certified_source_bytes: None,
        receipt: None,
        publication_receipt: None,
        route_observations: BTreeMap::new(),
        timings: None,
        publication_probe_us: 0,
        daemon_mode: metadata.daemon_mode,
        trigger: metadata.trigger,
        trigger_provenance: metadata.trigger_provenance,
        failure_type: None,
        last_error: None,
    }
}

pub(super) fn durable_queue_entry_count(state: &CoreRefreshEngineState) -> usize {
    let active = state
        .attempts
        .iter()
        .filter(|attempt| attempt.state.is_active())
        .count();
    let terminal_root_id = state
        .pending_terminal_persistence
        .as_ref()
        .map(|pending| pending.request_id.as_str())
        .or(state.pending_scheduler_retry_root_id.as_deref());
    let terminal_root = terminal_root_id.is_some_and(|request_id| {
        find_attempt(state, request_id).is_some_and(|attempt| !attempt.state.is_active())
    });
    active.saturating_add(usize::from(terminal_root))
}

pub(super) fn trim_terminal_attempt_history(state: &mut CoreRefreshEngineState) {
    let mut terminal_count = state
        .attempts
        .iter()
        .filter(|attempt| !attempt.state.is_active())
        .count();
    while terminal_count > SOURCE_REFRESH_ATTEMPT_HISTORY {
        let pending_terminal_root = state
            .pending_terminal_persistence
            .as_ref()
            .map(|pending| pending.request_id.as_str());
        let pending_scheduler_root = state.pending_scheduler_retry_root_id.as_deref();
        let Some(oldest_terminal) = state.attempts.iter().position(|attempt| {
            !attempt.state.is_active()
                && Some(attempt.request_id.as_str()) != pending_terminal_root
                && Some(attempt.request_id.as_str()) != pending_scheduler_root
        }) else {
            break;
        };
        state.attempts.remove(oldest_terminal);
        terminal_count = terminal_count.saturating_sub(1);
    }
}

pub(super) fn advance_after_terminal_attempt(
    state: &mut CoreRefreshEngineState,
    request_id: &str,
    observed_generation: Option<String>,
) {
    if state.active_request_id.as_deref() != Some(request_id) {
        return;
    }
    state.active_request_id = state.pending_request_ids.pop_front();
    let Some(next_request_id) = state.active_request_id.clone() else {
        return;
    };
    if state
        .manual_all_continuations
        .contains_key(&next_request_id)
    {
        return;
    }
    if let Some(next_attempt) = find_attempt_mut(state, &next_request_id) {
        if observed_generation.is_some() {
            next_attempt.previous_generation = observed_generation.clone();
            next_attempt.published_generation = observed_generation;
        }
    }
}

pub(super) fn source_route_ledger_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_preserves_route_local_change_in_catalog_outcome() {
        let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
        let mut continuation = ManualAllContinuation::new(
            "predecessor".to_owned(),
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        continuation.covered_route_results.insert(
            route.clone(),
            SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false),
        );
        let mut publication = SourceBackedRefreshPublication {
            generation_id: "generation".to_owned(),
            published_explicit_source_catalog: None,
            unsupported_routes: 0,
            certified_source_count: 0,
            certified_source_bytes: 0,
            current: SourceBackedRefreshCurrent::default(),
            timings: SourceBackedRefreshTimings::default(),
            route_results: Vec::new(),
            catalog_route_bindings: vec![ExplicitSourceCatalogRouteBinding {
                catalog_lineage: "cd".repeat(32),
                route_identity: route.as_str().to_owned(),
            }],
            verified_index: None,
        };

        continuation.covered_publication().apply(&mut publication);

        assert_eq!(publication.route_results.len(), 1);
        assert_eq!(publication.route_results[0].route_identity, route.as_str());
        assert_eq!(publication.route_results[0].outcome.changed(), Some(false));
    }

    #[test]
    fn continuation_overlap_remains_visible_to_canonical_duplicate_rejection() {
        let route = SourceRouteIdentity::from_sha256("ef".repeat(32)).unwrap();
        let mut continuation = ManualAllContinuation::new(
            "predecessor".to_owned(),
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        continuation.covered_route_results.insert(
            route.clone(),
            SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false),
        );
        let mut publication = SourceBackedRefreshPublication {
            generation_id: "generation".to_owned(),
            published_explicit_source_catalog: None,
            unsupported_routes: 0,
            certified_source_count: 0,
            certified_source_bytes: 0,
            current: SourceBackedRefreshCurrent::default(),
            timings: SourceBackedRefreshTimings::default(),
            route_results: vec![SourceBackedRefreshRouteResult::succeeded(
                route.as_str().to_owned(),
                true,
            )],
            catalog_route_bindings: Vec::new(),
            verified_index: None,
        };

        continuation.covered_publication().apply(&mut publication);

        let error = SourceBackedRefreshReceipt::from_verified_publication(
            None,
            publication.generation_id.clone(),
            &publication,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("duplicate route result"));
    }
}
