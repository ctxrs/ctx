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

pub(super) fn aggregate_manual_all_continuation(
    publication: &mut SourceBackedRefreshPublication,
    continuation: &ManualAllContinuation,
) {
    if continuation.covered_route_ids.is_empty() {
        return;
    }
    let covered = continuation
        .covered_route_ids
        .iter()
        .map(|route| route.as_str().to_owned());
    publication.selected_route_ids.extend(covered.clone());
    publication.successful_route_ids.extend(covered);
    publication.selected_route_ids.sort();
    publication.selected_route_ids.dedup();
    publication.successful_route_ids.sort();
    publication.successful_route_ids.dedup();
    publication.successful_route_changes.extend(
        continuation
            .covered_route_changes
            .iter()
            .map(|(route, changed)| (route.as_str().to_owned(), *changed)),
    );
    for outcome in &mut publication.catalog_route_outcomes {
        let Some(changed) =
            continuation
                .covered_route_changes
                .iter()
                .find_map(|(route, changed)| {
                    (route.as_str() == outcome.route_identity).then_some(*changed)
                })
        else {
            continue;
        };
        outcome.outcome = "succeeded".to_owned();
        outcome.failure_class = None;
        outcome.changed = Some(changed);
    }
    publication.scanned_routes = publication
        .scanned_routes
        .saturating_add(continuation.covered_scanned_routes);
    publication.current.removed_source_count = publication
        .current
        .removed_source_count
        .saturating_add(continuation.covered_removed_source_count);
    publication.timings.discovery_us = publication
        .timings
        .discovery_us
        .saturating_add(continuation.covered_timings.discovery_us);
    publication.timings.scan_stage_us = publication
        .timings
        .scan_stage_us
        .saturating_add(continuation.covered_timings.scan_stage_us);
    publication.timings.commit_us = publication
        .timings
        .commit_us
        .saturating_add(continuation.covered_timings.commit_us);
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
        published_explicit_source_catalog: None,
        coalesced_requests: 0,
        progress: SourceBackedRefreshProgress::default(),
        scanned_routes: None,
        unsupported_routes: None,
        certified_source_count: None,
        certified_source_bytes: None,
        receipt: None,
        timings: None,
        publication_probe_us: 0,
        daemon_mode: metadata.daemon_mode,
        trigger: metadata.trigger,
        trigger_provenance: metadata.trigger_provenance,
        failure_type: None,
        last_error: None,
        post_publication_error: None,
    }
}

pub(super) fn active_attempt_count(state: &CoreRefreshEngineState) -> usize {
    state
        .attempts
        .iter()
        .filter(|attempt| attempt.state.is_active())
        .count()
}

pub(super) fn trim_terminal_attempt_history(state: &mut CoreRefreshEngineState) {
    let mut terminal_count = state
        .attempts
        .iter()
        .filter(|attempt| !attempt.state.is_active())
        .count();
    while terminal_count > SOURCE_REFRESH_ATTEMPT_HISTORY {
        let Some(oldest_terminal) = state
            .attempts
            .iter()
            .position(|attempt| !attempt.state.is_active())
        else {
            break;
        };
        state.attempts.remove(oldest_terminal);
        terminal_count = terminal_count.saturating_sub(1);
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
        let mut continuation = ManualAllContinuation::new("predecessor".to_owned());
        continuation.covered_route_ids.insert(route.clone());
        continuation
            .covered_route_changes
            .insert(route.clone(), false);
        let mut publication = SourceBackedRefreshPublication {
            generation_id: "generation".to_owned(),
            published_explicit_source_catalog: load_explicit_source_catalog_authority(
                tempfile::tempdir().unwrap().path(),
            )
            .unwrap(),
            scanned_routes: 0,
            unsupported_routes: 0,
            certified_source_count: 0,
            certified_source_bytes: 0,
            current: SourceBackedRefreshCurrent::default(),
            timings: SourceBackedRefreshTimings::default(),
            selected_route_ids: Vec::new(),
            successful_route_ids: Vec::new(),
            successful_route_changes: BTreeMap::new(),
            failed_route_outcomes: Vec::new(),
            catalog_route_outcomes: vec![SourceBackedRefreshCatalogRouteOutcome {
                catalog_lineage: "cd".repeat(32),
                route_identity: route.as_str().to_owned(),
                outcome: "not_selected".to_owned(),
                failure_class: None,
                changed: None,
            }],
            source_failures: Vec::new(),
        };

        aggregate_manual_all_continuation(&mut publication, &continuation);

        assert_eq!(publication.selected_route_ids, [route.as_str()]);
        assert_eq!(publication.successful_route_ids, [route.as_str()]);
        assert!(!publication.successful_route_changes[route.as_str()]);
        assert_eq!(publication.catalog_route_outcomes[0].outcome, "succeeded");
        assert_eq!(publication.catalog_route_outcomes[0].changed, Some(false));
    }
}
