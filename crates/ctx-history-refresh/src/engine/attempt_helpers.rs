use super::read_model::SourceBackedRefreshFailureType;
use super::*;

pub(super) fn source_backed_refresh_failure_type(
    error: &anyhow::Error,
) -> Option<SourceBackedRefreshFailureType> {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<ZeroSourcePublicationBlocked>()
            .is_some()
    }) {
        return Some(SourceBackedRefreshFailureType::AllProviderTerminalCoverageUnavailable);
    }
    error.chain().find_map(|cause| {
        if let Some(route) = cause.downcast_ref::<SourceBackedRouteError>() {
            return match route.kind {
                SourceBackedRouteErrorKind::Unsupported => {
                    Some(SourceBackedRefreshFailureType::UnsupportedSchema)
                }
                SourceBackedRouteErrorKind::InvalidSource => {
                    Some(SourceBackedRefreshFailureType::MalformedSource)
                }
                SourceBackedRouteErrorKind::Unavailable => {
                    Some(SourceBackedRefreshFailureType::SourceUnavailable)
                }
                SourceBackedRouteErrorKind::SourceChanged => {
                    Some(SourceBackedRefreshFailureType::SourceChanged)
                }
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
            return Some(SourceBackedRefreshFailureType::SourceFailures);
        };
        Some(match *first {
            SourceBackedSourceFailureClass::Unavailable => {
                SourceBackedRefreshFailureType::SourceUnavailable
            }
            SourceBackedSourceFailureClass::SourceChanged => {
                SourceBackedRefreshFailureType::SourceChanged
            }
            SourceBackedSourceFailureClass::Unreadable => {
                SourceBackedRefreshFailureType::MalformedSource
            }
            SourceBackedSourceFailureClass::Incompatible => {
                SourceBackedRefreshFailureType::UnsupportedSchema
            }
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
    if let Some(registration_failures) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SourceBackedAdmissionRouteFailures>())
    {
        let failures = registration_failures.failures();
        let affected_routes = failures
            .iter()
            .map(|failure| failure.route_identity().clone())
            .collect::<BTreeSet<_>>();
        if failures
            .iter()
            .any(|failure| failure.kind() == SourceBackedRouteErrorKind::Internal)
        {
            return SourceBackedRefreshFailureOutcome::new(
                RefreshOutcomeCode::SourceRefreshFailed,
                RefreshOutcomeClass::Internal,
                true,
                affected_routes,
                Some(RefreshRetryAdvice::RetryRequest),
            );
        }
        let (retryable_routes, blocked_routes): (BTreeSet<_>, BTreeSet<_>) = failures
            .iter()
            .map(|failure| failure.route_identity().clone())
            .partition(|route| {
                failures.iter().any(|failure| {
                    failure.route_identity() == route
                        && matches!(
                            failure.kind(),
                            SourceBackedRouteErrorKind::Unavailable
                                | SourceBackedRouteErrorKind::SourceChanged
                                | SourceBackedRouteErrorKind::ResourceUnavailable
                        )
                })
            });
        let resource_unavailable = failures
            .iter()
            .any(|failure| failure.kind() == SourceBackedRouteErrorKind::ResourceUnavailable);
        let first_kind = failures[0].kind();
        let homogeneous = failures.iter().all(|failure| failure.kind() == first_kind);
        let (code, class) = if resource_unavailable {
            (
                RefreshOutcomeCode::ResourceUnavailable,
                RefreshOutcomeClass::ResourceUnavailable,
            )
        } else if homogeneous {
            match first_kind {
                SourceBackedRouteErrorKind::Unavailable => (
                    RefreshOutcomeCode::SourceUnavailable,
                    RefreshOutcomeClass::Unavailable,
                ),
                SourceBackedRouteErrorKind::SourceChanged => (
                    RefreshOutcomeCode::SourceChanged,
                    RefreshOutcomeClass::SourceChanged,
                ),
                SourceBackedRouteErrorKind::InvalidSource => (
                    RefreshOutcomeCode::MalformedSource,
                    RefreshOutcomeClass::Unreadable,
                ),
                SourceBackedRouteErrorKind::Unsupported => (
                    RefreshOutcomeCode::UnsupportedSchema,
                    RefreshOutcomeClass::Incompatible,
                ),
                SourceBackedRouteErrorKind::ResourceUnavailable
                | SourceBackedRouteErrorKind::Internal => unreachable!(),
            }
        } else {
            (
                RefreshOutcomeCode::SourceFailures,
                RefreshOutcomeClass::Mixed,
            )
        };
        let retryable = !retryable_routes.is_empty();
        let retry_advice = if retryable {
            RefreshRetryAdvice::RetryAffectedRoutes
        } else if homogeneous && first_kind == SourceBackedRouteErrorKind::Unsupported {
            RefreshRetryAdvice::UpgradeOrReconfigure
        } else {
            RefreshRetryAdvice::InspectSources
        };
        return SourceBackedRefreshFailureOutcome::with_route_dispositions(
            code,
            class,
            retryable,
            retryable_routes,
            blocked_routes,
            Some(retry_advice),
        );
    }
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<ZeroSourcePublicationBlocked>()
            .is_some()
    }) {
        return SourceBackedRefreshFailureOutcome::new(
            RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable,
            RefreshOutcomeClass::Coverage,
            true,
            attempted_routes.clone(),
            Some(RefreshRetryAdvice::RetryRequest),
        );
    }
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
            [SourceBackedSourceFailureClass::Unavailable] => (
                RefreshOutcomeCode::SourceUnavailable,
                RefreshOutcomeClass::Unavailable,
            ),
            [SourceBackedSourceFailureClass::SourceChanged] => (
                RefreshOutcomeCode::SourceChanged,
                RefreshOutcomeClass::SourceChanged,
            ),
            [SourceBackedSourceFailureClass::Unreadable] => (
                RefreshOutcomeCode::MalformedSource,
                RefreshOutcomeClass::Unreadable,
            ),
            [SourceBackedSourceFailureClass::Incompatible] => (
                RefreshOutcomeCode::UnsupportedSchema,
                RefreshOutcomeClass::Incompatible,
            ),
            _ => (
                RefreshOutcomeCode::SourceFailures,
                RefreshOutcomeClass::Mixed,
            ),
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
                RefreshRetryAdvice::RetryAffectedRoutes
            } else {
                RefreshRetryAdvice::InspectSources
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
            (
                RefreshOutcomeCode::LogicalSourceFailures,
                RefreshOutcomeClass::Mixed,
            )
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
                RefreshRetryAdvice::RetryAffectedRoutes
            } else {
                RefreshRetryAdvice::InspectSources
            }),
        );
    }

    if let Some(route_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SourceBackedRouteError>())
    {
        let (code, class, retryable, retry_advice) = match route_error.kind {
            SourceBackedRouteErrorKind::Unavailable => (
                RefreshOutcomeCode::SourceUnavailable,
                RefreshOutcomeClass::Unavailable,
                true,
                RefreshRetryAdvice::RetryAffectedRoutes,
            ),
            SourceBackedRouteErrorKind::SourceChanged => (
                RefreshOutcomeCode::SourceChanged,
                RefreshOutcomeClass::SourceChanged,
                true,
                RefreshRetryAdvice::RetryAffectedRoutes,
            ),
            SourceBackedRouteErrorKind::InvalidSource => (
                RefreshOutcomeCode::MalformedSource,
                RefreshOutcomeClass::Unreadable,
                false,
                RefreshRetryAdvice::InspectSources,
            ),
            SourceBackedRouteErrorKind::Unsupported => (
                RefreshOutcomeCode::UnsupportedSchema,
                RefreshOutcomeClass::Incompatible,
                false,
                RefreshRetryAdvice::UpgradeOrReconfigure,
            ),
            SourceBackedRouteErrorKind::ResourceUnavailable => (
                RefreshOutcomeCode::ResourceUnavailable,
                RefreshOutcomeClass::ResourceUnavailable,
                true,
                RefreshRetryAdvice::RetryAffectedRoutes,
            ),
            SourceBackedRouteErrorKind::Internal => (
                RefreshOutcomeCode::SourceRefreshFailed,
                RefreshOutcomeClass::Internal,
                true,
                RefreshRetryAdvice::RetryRequest,
            ),
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
                RefreshOutcomeCode::SourceChanged,
                RefreshOutcomeClass::SourceChanged,
                true,
                RefreshRetryAdvice::RetryAffectedRoutes,
            ),
            IndexError::Io(_)
            | IndexError::IndexMemoryTooSmall { .. }
            | IndexError::VerificationScratchLimitExceeded { .. } => (
                RefreshOutcomeCode::ResourceUnavailable,
                RefreshOutcomeClass::ResourceUnavailable,
                true,
                RefreshRetryAdvice::RetryRequest,
            ),
            corruption if index_error_is_corruption(corruption) => (
                RefreshOutcomeCode::IndexCorruption,
                RefreshOutcomeClass::Corruption,
                false,
                RefreshRetryAdvice::RebuildIndex,
            ),
            incompatible if generation_incompatibility_requires_rebuild(incompatible) => (
                RefreshOutcomeCode::IndexIncompatible,
                RefreshOutcomeClass::Incompatible,
                false,
                RefreshRetryAdvice::RebuildIndex,
            ),
            _ => (
                RefreshOutcomeCode::SourceRefreshFailed,
                RefreshOutcomeClass::Internal,
                true,
                RefreshRetryAdvice::RetryRequest,
            ),
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
        if let SourceBackedCoordinatorError::UnclaimedBaseSource {
            route_identity,
            route_failures,
            logical_source_failures,
            ..
        } = coordinator_error
        {
            let mut known = BTreeMap::<SourceRouteIdentity, bool>::new();
            for failure in route_failures {
                let retryable = source_failure_class_is_retryable(failure.class);
                known
                    .entry(failure.route_identity.clone())
                    .and_modify(|current| *current |= retryable)
                    .or_insert(retryable);
            }
            for (route, retryable) in logical_source_failures.route_retryability() {
                known
                    .entry(route.clone())
                    .and_modify(|current| *current |= retryable)
                    .or_insert(retryable);
            }
            known.insert(route_identity.clone(), false);
            let (retryable_routes, blocked_routes) =
                authoritative_route_dispositions(attempted_routes, known, true);
            let retryable = !retryable_routes.is_empty();
            return SourceBackedRefreshFailureOutcome::with_route_dispositions(
                RefreshOutcomeCode::SourceUnclaimed,
                RefreshOutcomeClass::Coverage,
                retryable,
                retryable_routes,
                blocked_routes,
                Some(if retryable {
                    RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
                } else {
                    RefreshRetryAdvice::InspectSources
                }),
            );
        }
        let (code, class, retryable, retry_advice) = match coordinator_error {
            SourceBackedCoordinatorError::Index(error) if index_error_is_corruption(error) => (
                RefreshOutcomeCode::IndexCorruption,
                RefreshOutcomeClass::Corruption,
                false,
                RefreshRetryAdvice::RebuildIndex,
            ),
            SourceBackedCoordinatorError::UnavailableRoute { .. } => (
                RefreshOutcomeCode::SourceUnavailable,
                RefreshOutcomeClass::Unavailable,
                true,
                RefreshRetryAdvice::RetryAffectedRoutes,
            ),
            SourceBackedCoordinatorError::InvalidRoute { .. }
            | SourceBackedCoordinatorError::InvalidRefreshScope { .. } => (
                RefreshOutcomeCode::UnsupportedSchema,
                RefreshOutcomeClass::Incompatible,
                false,
                RefreshRetryAdvice::UpgradeOrReconfigure,
            ),
            _ => (
                RefreshOutcomeCode::SourceRefreshFailed,
                RefreshOutcomeClass::Internal,
                true,
                RefreshRetryAdvice::RetryRequest,
            ),
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
        RefreshOutcomeCode::SourceRefreshFailed,
        RefreshOutcomeClass::Internal,
        true,
        attempted_routes.clone(),
        Some(if attempted_routes.is_empty() {
            RefreshRetryAdvice::RetryRequest
        } else {
            RefreshRetryAdvice::RetryAffectedRoutes
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

fn source_failure_class_is_retryable(class: SourceBackedSourceFailureClass) -> bool {
    matches!(
        class,
        SourceBackedSourceFailureClass::Unavailable | SourceBackedSourceFailureClass::SourceChanged
    )
}

fn source_failure_code_and_class(
    class: SourceBackedSourceFailureClass,
) -> (RefreshOutcomeCode, RefreshOutcomeClass) {
    match class {
        SourceBackedSourceFailureClass::Unavailable => (
            RefreshOutcomeCode::SourceUnavailable,
            RefreshOutcomeClass::Unavailable,
        ),
        SourceBackedSourceFailureClass::SourceChanged => (
            RefreshOutcomeCode::SourceChanged,
            RefreshOutcomeClass::SourceChanged,
        ),
        SourceBackedSourceFailureClass::Unreadable => (
            RefreshOutcomeCode::MalformedSource,
            RefreshOutcomeClass::Unreadable,
        ),
        SourceBackedSourceFailureClass::Incompatible => (
            RefreshOutcomeCode::UnsupportedSchema,
            RefreshOutcomeClass::Incompatible,
        ),
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
        attempt.whole_run_eta.disable();
    }
    merge_trigger_ownership(attempt, &metadata);
    attempt.coalesced_requests = attempt.coalesced_requests.saturating_add(1);
    attempt.to_json()
}

pub(super) fn merge_trigger_ownership(
    attempt: &mut SourceBackedRefreshAttempt,
    metadata: &SourceRefreshRuntimeMetadata,
) {
    let incoming_priority = trigger_ownership_priority(metadata.trigger);
    let current_priority = trigger_ownership_priority(attempt.trigger);
    let explicit_import_upgrade = metadata.operation == SourceBackedRefreshOperation::Import
        && metadata.trigger == "import"
        && attempt.trigger == "import";
    if incoming_priority > current_priority || explicit_import_upgrade {
        attempt.trigger = metadata.trigger;
        attempt.trigger_provenance = metadata.trigger_provenance;
    }
}

fn trigger_ownership_priority(trigger: &str) -> u8 {
    match trigger {
        "import" => 2,
        "setup" => 1,
        _ => 0,
    }
}

pub(super) fn new_refresh_attempt(
    observed_generation: Option<String>,
    metadata: SourceRefreshRuntimeMetadata,
    intent: RefreshIntent,
    refresh_scope: SourceBackedRefreshScope,
) -> SourceBackedRefreshAttempt {
    let request_id = Uuid::now_v7().to_string();
    let eta_eligible = observed_generation.is_none()
        && intent == RefreshIntent::AutomaticMaintenance
        && matches!(&refresh_scope, SourceBackedRefreshScope::All);
    let reconciliation_demand = intent.reconciliation_demand();
    SourceBackedRefreshAttempt {
        request_id: request_id.clone(),
        state: SourceBackedRefreshState::Queued,
        requested_at_ms: utc_now().timestamp_millis(),
        started_at_ms: None,
        finished_at_ms: None,
        previous_generation: observed_generation.clone(),
        published_generation: observed_generation,
        intent,
        refresh_scope,
        reconciliation_demand,
        admitted_authority: None,
        request_fingerprint: None,
        admission_durability_indeterminate: false,
        coalesced_requests: 0,
        progress: SourceBackedRefreshProgress::default(),
        attempt_history_progress: None,
        progress_total_sources_known: false,
        whole_run_eta: WholeRunEtaEstimator::new(eta_eligible),
        scanned_routes: None,
        unsupported_routes: None,
        request_source_count: None,
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

pub(super) fn recover_reconciliation_demand(
    job: &Value,
    operation: SourceBackedRefreshOperation,
) -> Result<SourceBackedReconciliationDemand> {
    match job.get("reconciliation_demand") {
        Some(Value::String(value)) => SourceBackedReconciliationDemand::parse(value)
            .ok_or_else(|| anyhow!("durable source refresh has invalid reconciliation demand")),
        Some(_) => bail!("durable source refresh has invalid reconciliation demand"),
        None => Ok(match operation {
            SourceBackedRefreshOperation::Refresh => SourceBackedReconciliationDemand::Incremental,
            SourceBackedRefreshOperation::Import => SourceBackedReconciliationDemand::Exhaustive,
        }),
    }
}

pub(super) fn recover_refresh_intent(
    job: &Value,
    operation: SourceBackedRefreshOperation,
    allow_legacy_terminal_catalog_omission: bool,
    ignore_obsolete_root_catalog: bool,
) -> Result<RefreshIntent> {
    if let Some(value) = job.get("refresh_intent") {
        let intent = RefreshIntent::from_json(value)?;
        if intent.operation() != operation {
            bail!("durable source refresh intent disagrees with its operation");
        }
        if let Some(legacy_catalog) = job
            .get("requested_explicit_source_catalog")
            .filter(|value| !value.is_null())
            .map(ExplicitSourceCatalogAuthority::from_json)
            .transpose()?
        {
            if intent.explicit_source_authority() != Some(&legacy_catalog) {
                bail!("durable source refresh intent disagrees with its legacy exact-source authority");
            }
        }
        return Ok(intent);
    }

    // One isolated decoder for pre-canonical durable jobs. Legacy fields are
    // normalized immediately and never enter scheduling or execution as an
    // independent request representation.
    // Pre-overlay periodic roots can carry obsolete catalog-shaped data. It
    // was never request authority for refresh operations, so retain that
    // compatibility without weakening import or successor validation.
    let explicit_catalog = if ignore_obsolete_root_catalog
        && operation == SourceBackedRefreshOperation::Refresh
        && job.get("refresh_selector").is_none()
    {
        None
    } else {
        job.get("requested_explicit_source_catalog")
            .filter(|value| !value.is_null())
            .map(ExplicitSourceCatalogAuthority::from_json)
            .transpose()?
    };
    let explicit_catalog = explicit_catalog.as_ref();
    let has_explicit_catalog = explicit_catalog.is_some();
    let fresh = job
        .get("fresh_after_admitted_snapshot")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(selector) = job.get("refresh_selector") else {
        return match (operation, explicit_catalog) {
            (SourceBackedRefreshOperation::Refresh, Some(_)) => {
                bail!("durable refresh carries legacy explicit source authority")
            }
            (SourceBackedRefreshOperation::Import, Some(authority)) => Ok(
                RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority.clone())),
            ),
            (SourceBackedRefreshOperation::Import, None)
                if !allow_legacy_terminal_catalog_omission =>
            {
                bail!("durable import source refresh has no explicit source authority")
            }
            (_, None) if fresh || operation == SourceBackedRefreshOperation::Import => {
                Ok(RefreshIntent::SelectedImport(RefreshSelection::All))
            }
            (_, None) => Ok(RefreshIntent::AutomaticMaintenance),
        };
    };
    let fields = selector
        .as_object()
        .ok_or_else(|| anyhow!("durable source refresh selector is not an object"))?;
    match fields.get("kind").and_then(Value::as_str) {
        Some("all_automatic") if fields.len() == 1 => match explicit_catalog {
            Some(authority) if operation == SourceBackedRefreshOperation::Import => Ok(
                RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority.clone())),
            ),
            Some(_) => bail!("durable refresh carries explicit source authority"),
            None if fresh => Ok(RefreshIntent::SelectedImport(RefreshSelection::All)),
            None => Ok(RefreshIntent::AutomaticMaintenance),
        },
        Some("automatic_provider") if fields.len() == 2 && !has_explicit_catalog => {
            let provider = fields
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("durable provider selector has no provider"))?
                .parse()
                .context("parse durable provider selector")?;
            if provider == CaptureProvider::Unknown {
                bail!("durable provider selector has an unknown provider");
            }
            Ok(RefreshIntent::SelectedImport(RefreshSelection::Provider(
                provider,
            )))
        }
        Some("explicit_catalog") if fields.len() == 1 => Ok(RefreshIntent::SelectedImport(
            RefreshSelection::ExactSource(
                explicit_catalog
                    .cloned()
                    .ok_or_else(|| anyhow!("durable exact-source refresh has no authority"))?,
            ),
        )),
        Some("automatic_provider") if has_explicit_catalog => {
            bail!("durable provider selector carries explicit source authority")
        }
        Some(kind) => bail!("durable source refresh selector `{kind}` is malformed"),
        None => bail!("durable source refresh selector kind is missing"),
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
mod tests;
