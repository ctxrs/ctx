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

pub(super) fn source_backed_refresh_failure_outcome(
    error: &anyhow::Error,
    attempted_routes: &BTreeSet<SourceRouteIdentity>,
) -> SourceBackedRefreshFailureOutcome {
    if let Some(failed_routes) = error.chain().find_map(|cause| {
        let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } =
            cause.downcast_ref::<SourceBackedCoordinatorError>()?
        else {
            return None;
        };
        Some(failed_routes)
    }) {
        let classes = [
            SourceBackedSourceFailureClass::Unavailable,
            SourceBackedSourceFailureClass::SourceChanged,
            SourceBackedSourceFailureClass::Unreadable,
            SourceBackedSourceFailureClass::Incompatible,
        ]
        .into_iter()
        .filter(|class| failed_routes.class_total(*class) != 0)
        .collect::<Vec<_>>();
        let (code, class) = match classes.as_slice() {
            [SourceBackedSourceFailureClass::Unavailable] => ("source_unavailable", "unavailable"),
            [SourceBackedSourceFailureClass::SourceChanged] => ("source_changed", "source_changed"),
            [SourceBackedSourceFailureClass::Unreadable] => ("malformed_source", "unreadable"),
            [SourceBackedSourceFailureClass::Incompatible] => {
                ("unsupported_schema", "incompatible")
            }
            _ => ("source_failures", "mixed"),
        };
        let retryable = classes.iter().any(|class| {
            matches!(
                class,
                SourceBackedSourceFailureClass::Unavailable
                    | SourceBackedSourceFailureClass::SourceChanged
            )
        });
        let known = failed_routes.failures().iter().map(|failure| {
            (
                failure.route_identity.clone(),
                matches!(
                    failure.class,
                    SourceBackedSourceFailureClass::Unavailable
                        | SourceBackedSourceFailureClass::SourceChanged
                ),
            )
        });
        let (retryable_routes, blocked_routes) =
            authoritative_route_dispositions(attempted_routes, known, retryable);
        return SourceBackedRefreshFailureOutcome::with_route_dispositions(
            code,
            class,
            retryable,
            retryable_routes,
            blocked_routes,
            Some(if retryable {
                "retry_affected_routes"
            } else {
                "inspect_sources"
            }),
        );
    }

    if let Some(failed_sources) = error.chain().find_map(|cause| {
        let SourceBackedCoordinatorError::NoUsableLogicalSources { failed_sources } =
            cause.downcast_ref::<SourceBackedCoordinatorError>()?
        else {
            return None;
        };
        Some(failed_sources)
    }) {
        let retained_classes = [
            SourceBackedSourceFailureClass::Unavailable,
            SourceBackedSourceFailureClass::SourceChanged,
            SourceBackedSourceFailureClass::Unreadable,
            SourceBackedSourceFailureClass::Incompatible,
        ]
        .into_iter()
        .filter(|class| {
            failed_sources
                .failures()
                .iter()
                .any(|failure| failure.class == *class)
        })
        .collect::<Vec<_>>();
        let diagnostics_complete = failed_sources.total() == failed_sources.failures().len();
        let retryable = !diagnostics_complete
            || retained_classes.iter().any(|class| {
                matches!(
                    class,
                    SourceBackedSourceFailureClass::Unavailable
                        | SourceBackedSourceFailureClass::SourceChanged
                )
            });
        let (code, class) = if diagnostics_complete && retained_classes.len() == 1 {
            source_failure_code_and_class(retained_classes[0])
        } else {
            ("logical_source_failures", "mixed")
        };
        let known = failed_sources.failures().iter().map(|failure| {
            (
                failure.route_identity.clone(),
                matches!(
                    failure.class,
                    SourceBackedSourceFailureClass::Unavailable
                        | SourceBackedSourceFailureClass::SourceChanged
                ),
            )
        });
        let (retryable_routes, blocked_routes) =
            authoritative_route_dispositions(attempted_routes, known, retryable);
        return SourceBackedRefreshFailureOutcome::with_route_dispositions(
            code,
            class,
            retryable,
            retryable_routes,
            blocked_routes,
            Some(if retryable {
                "retry_affected_routes"
            } else {
                "inspect_sources"
            }),
        );
    }

    if let Some(route_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SourceBackedRouteError>())
    {
        let (code, class, retryable, retry_advice) = match route_error.kind {
            SourceBackedRouteErrorKind::Unavailable => (
                "source_unavailable",
                "unavailable",
                true,
                "retry_affected_routes",
            ),
            SourceBackedRouteErrorKind::SourceChanged => (
                "source_changed",
                "source_changed",
                true,
                "retry_affected_routes",
            ),
            SourceBackedRouteErrorKind::InvalidSource => {
                ("malformed_source", "unreadable", false, "inspect_sources")
            }
            SourceBackedRouteErrorKind::Unsupported => (
                "unsupported_schema",
                "incompatible",
                false,
                "upgrade_or_reconfigure",
            ),
            SourceBackedRouteErrorKind::ResourceUnavailable => (
                "resource_unavailable",
                "resource_unavailable",
                true,
                "retry_affected_routes",
            ),
            SourceBackedRouteErrorKind::Internal => {
                ("source_refresh_failed", "internal", true, "retry_request")
            }
        };
        return SourceBackedRefreshFailureOutcome::new(
            code,
            class,
            retryable,
            attempted_routes.clone(),
            Some(retry_advice),
        );
    }

    if let Some(index_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<IndexError>())
    {
        let (code, class, retryable, retry_advice) = match index_error {
            IndexError::SourceInvalidated(_) | IndexError::CompleteInventoryInvalidated { .. } => (
                "source_changed",
                "source_changed",
                true,
                "retry_affected_routes",
            ),
            IndexError::Io(_)
            | IndexError::IndexMemoryTooSmall { .. }
            | IndexError::VerificationScratchLimitExceeded { .. } => (
                "resource_unavailable",
                "resource_unavailable",
                true,
                "retry_request",
            ),
            corruption if index_error_is_corruption(corruption) => {
                ("index_corruption", "corruption", false, "rebuild_index")
            }
            incompatible if generation_incompatibility_requires_rebuild(incompatible) => {
                ("index_incompatible", "incompatible", false, "rebuild_index")
            }
            _ => ("source_refresh_failed", "internal", true, "retry_request"),
        };
        return SourceBackedRefreshFailureOutcome::new(
            code,
            class,
            retryable,
            attempted_routes.clone(),
            Some(retry_advice),
        );
    }

    if let Some(coordinator_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SourceBackedCoordinatorError>())
    {
        let (code, class, retryable, retry_advice) = match coordinator_error {
            SourceBackedCoordinatorError::UnavailableRoute { .. } => (
                "source_unavailable",
                "unavailable",
                true,
                "retry_affected_routes",
            ),
            SourceBackedCoordinatorError::InvalidRoute { .. }
            | SourceBackedCoordinatorError::InvalidRefreshScope { .. } => (
                "unsupported_schema",
                "incompatible",
                false,
                "upgrade_or_reconfigure",
            ),
            _ => ("source_refresh_failed", "internal", true, "retry_request"),
        };
        return SourceBackedRefreshFailureOutcome::new(
            code,
            class,
            retryable,
            attempted_routes.clone(),
            Some(retry_advice),
        );
    }

    SourceBackedRefreshFailureOutcome::new(
        "source_refresh_failed",
        "internal",
        true,
        attempted_routes.clone(),
        Some(if attempted_routes.is_empty() {
            "retry_request"
        } else {
            "retry_affected_routes"
        }),
    )
}

fn authoritative_route_dispositions(
    attempted_routes: &BTreeSet<SourceRouteIdentity>,
    known: impl IntoIterator<Item = (SourceRouteIdentity, bool)>,
    default_retryable: bool,
) -> (BTreeSet<SourceRouteIdentity>, BTreeSet<SourceRouteIdentity>) {
    let known = known.into_iter().collect::<BTreeMap<_, _>>();
    let affected_routes = if attempted_routes.is_empty() {
        known.keys().cloned().collect::<BTreeSet<_>>()
    } else {
        attempted_routes.clone()
    };
    affected_routes
        .into_iter()
        .partition(|route| known.get(route).copied().unwrap_or(default_retryable))
}

fn source_failure_code_and_class(
    class: SourceBackedSourceFailureClass,
) -> (&'static str, &'static str) {
    match class {
        SourceBackedSourceFailureClass::Unavailable => ("source_unavailable", "unavailable"),
        SourceBackedSourceFailureClass::SourceChanged => ("source_changed", "source_changed"),
        SourceBackedSourceFailureClass::Unreadable => ("malformed_source", "unreadable"),
        SourceBackedSourceFailureClass::Incompatible => ("unsupported_schema", "incompatible"),
    }
}

fn index_error_is_corruption(error: &IndexError) -> bool {
    matches!(
        error,
        IndexError::MissingCommitPayload
            | IndexError::MissingActiveGenerationPointer
            | IndexError::InvalidActiveGenerationPointer
            | IndexError::NonCanonicalCommitPayload
            | IndexError::InvalidPublicationMetadataEncoding
            | IndexError::UnboundIndexState
            | IndexError::PinnedGenerationMismatch { .. }
            | IndexError::MissingManifest(_)
            | IndexError::ManifestDigestMismatch { .. }
            | IndexError::InvalidGenerationId
            | IndexError::NonCanonicalManifest
            | IndexError::NonCanonicalManifestSources
            | IndexError::InvalidSourceRouteIdentity
            | IndexError::NonCanonicalSourceRoutes
            | IndexError::NonCanonicalSourceRouteMembers(_)
            | IndexError::InvalidSourceRouteMissingState(_)
            | IndexError::EmptyMissingSourceRoute(_)
            | IndexError::SourceRouteMemberNotRetained { .. }
            | IndexError::SourceNotOwnedByRoute(_)
            | IndexError::SourceOwnedByMultipleRoutes(_)
            | IndexError::InvalidManifestTotals { .. }
            | IndexError::MissingSchemaField(_)
            | IndexError::InvalidStoredDocumentField(_)
            | IndexError::ChecksumMismatch
    )
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
    let request_id = Uuid::now_v7().to_string();
    SourceBackedRefreshAttempt {
        request_id: request_id.clone(),
        state: SourceBackedRefreshState::Queued,
        requested_at_ms: utc_now().timestamp_millis(),
        started_at_ms: None,
        finished_at_ms: None,
        previous_generation: observed_generation.clone(),
        published_generation: observed_generation,
        refresh_scope,
        operation: metadata.operation,
        requested_explicit_source_catalog: requested_catalog,
        fresh_after_admitted_snapshot: false,
        request_fingerprint: None,
        admission_durability_indeterminate: false,
        coalesced_into_request_id: None,
        coalesced_logical_demands: 0,
        coalesced_requests: 0,
        progress: SourceBackedRefreshProgress::default(),
        progress_total_sources_known: false,
        physical_attempt_id: Some(request_id),
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
        failure_outcome: None,
        last_error: None,
    }
}

pub(super) fn recover_admission_durability(job: &Value, context: &str) -> Result<bool> {
    match (
        job.get("admission_acknowledgement").and_then(Value::as_str),
        job.get("admission_durability").and_then(Value::as_str),
    ) {
        (None, None) => Ok(false),
        (Some("retained_after_durability_error"), Some("replacement_visible_or_indeterminate")) => {
            Ok(true)
        }
        _ => bail!("{context} has invalid admission durability state"),
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

    #[test]
    fn bounded_failure_diagnostics_do_not_bound_affected_routes_or_retryability() {
        let attempted_routes = (0..70)
            .map(|index| SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap())
            .collect::<BTreeSet<_>>();
        let failures = attempted_routes.iter().enumerate().map(|(index, route)| {
            SourceBackedFailedRoute::new(
                route.clone(),
                format!("{index:064x}"),
                CaptureProvider::Codex,
                if index == 69 {
                    SourceBackedSourceFailureClass::Unavailable
                } else {
                    SourceBackedSourceFailureClass::Incompatible
                },
                false,
                "fixture source",
                "fixture failure",
            )
        });
        let failed_routes = SourceBackedSourceFailures::from_failures(failures);
        assert_eq!(failed_routes.failures().len(), 64);
        assert_eq!(failed_routes.omitted(), 6);
        let error: anyhow::Error =
            SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }.into();

        let outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);

        assert_eq!(outcome.affected_routes, attempted_routes);
        assert!(outcome.retryable);
        assert_eq!(outcome.blocked_routes.len(), 64);
        assert_eq!(outcome.retryable_routes.len(), 6);
    }

    #[test]
    fn route_index_and_internal_failures_have_stable_retry_classes() {
        let route = SourceRouteIdentity::from_sha256("aa".repeat(32)).unwrap();
        let attempted_routes = BTreeSet::from([route]);
        let cases: Vec<(anyhow::Error, &str, &str, bool)> = vec![
            (
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::ResourceUnavailable,
                    "fixture resource pressure",
                )
                .into(),
                "resource_unavailable",
                "resource_unavailable",
                true,
            ),
            (
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    "fixture malformed source",
                )
                .into(),
                "malformed_source",
                "unreadable",
                false,
            ),
            (
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unsupported,
                    "fixture incompatible source",
                )
                .into(),
                "unsupported_schema",
                "incompatible",
                false,
            ),
            (
                IndexError::MissingCommitPayload.into(),
                "index_corruption",
                "corruption",
                false,
            ),
            (
                IndexError::SchemaMismatch(1).into(),
                "index_incompatible",
                "incompatible",
                false,
            ),
            (
                anyhow!("fixture internal failure"),
                "source_refresh_failed",
                "internal",
                true,
            ),
        ];

        for (error, code, class, retryable) in cases {
            let outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);
            assert_eq!(outcome.code, code);
            assert_eq!(outcome.class, class);
            assert_eq!(outcome.retryable, retryable);
        }
    }
}
