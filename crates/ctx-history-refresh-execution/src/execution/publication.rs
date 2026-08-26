use super::*;

#[doc(hidden)]
pub fn exclusive_scan_stage_duration(
    scan_stage_duration: StdDuration,
    commit_duration: StdDuration,
) -> StdDuration {
    // The capture receipt measures scan-stage wall time from before the
    // writer opens through commit, and also reports commit independently.
    // Keep the exported buckets disjoint without creating a telemetry layer.
    scan_stage_duration.saturating_sub(commit_duration)
}

pub(super) fn encode_publication_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    previous_generation: Option<&str>,
    publication: &SourceBackedRefreshPublication,
    route_observations: BTreeMap<SourceRouteIdentity, String>,
    route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
) -> Result<Vec<u8>> {
    let terminal = SourceBackedRefreshReceipt::from_verified_publication(
        previous_generation.map(str::to_owned),
        publication.generation_id.clone(),
        publication,
    )?;
    SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: request_id.to_owned(),
        operation,
        refresh_scope: scope.clone(),
        receipt: terminal.to_json(),
        route_observations,
        route_controls,
    }
    .encode()
    .map_err(Into::into)
}

pub(super) fn publication_from_verified_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    timings: SourceBackedRefreshTimings,
    verified_index: Arc<VerifiedIndex>,
) -> Result<SourceBackedRefreshPublication> {
    let metadata = SourceBackedPublicationMetadata::decode(&verified_index)?;
    if metadata.request_id != request_id
        || metadata.operation != operation
        || metadata.refresh_scope != *scope
    {
        bail!("published Core source-refresh metadata does not match its exact request");
    }
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), &verified_index)?;
    let unsupported_routes = receipt
        .route_results
        .iter()
        .filter(|result| result.outcome.failure_class() == Some("incompatible"))
        .count();
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.published_generation,
        published_explicit_source_catalog: receipt.published_explicit_source_catalog,
        unsupported_routes,
        certified_source_count: receipt.current.source_count,
        certified_source_bytes: receipt.current.certified_source_bytes,
        current: receipt.current,
        route_results: receipt.route_results,
        zero_source_authority: receipt.zero_source_authority,
        catalog_route_bindings: receipt.catalog_route_bindings,
        timings,
        verified_index: Some(verified_index),
    })
}

pub(super) fn validate_recertified_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    verified_index: &VerifiedIndex,
) -> Result<()> {
    let metadata = SourceBackedPublicationMetadata::decode(verified_index)?;
    if metadata.request_id != request_id
        || metadata.operation != operation
        || metadata.refresh_scope != *scope
        || !metadata.certifies_generation(verified_index)
    {
        bail!("recertified Core source-refresh metadata does not match its exact request");
    }
    Ok(())
}

pub(super) struct ProviderPublicationFacts<'a, S: ImmutableCaptureSnapshot + ?Sized> {
    pub(super) selected_route_ids: &'a [SourceRouteIdentity],
    pub(super) successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
    pub(super) failed_routes: &'a [SourceBackedFailedRouteOutcome],
    pub(super) source_failures: &'a SourceBackedSourceFailures,
    pub(super) logical_source_failures: &'a SourceBackedLogicalSourceFailures,
    pub(super) record_rejections: &'a SourceBackedRecordRejections,
    pub(super) snapshot: &'a S,
}

pub(super) fn provider_route_results<S: ImmutableCaptureSnapshot + ?Sized>(
    facts: ProviderPublicationFacts<'_, S>,
    registry_failures: &[SourceBackedFailedRoute],
    expected_selected_route_ids: &BTreeSet<String>,
) -> Result<Vec<SourceBackedRefreshRouteResult>> {
    let selected_route_ids = facts
        .selected_route_ids
        .iter()
        .chain(
            registry_failures
                .iter()
                .map(|failure| &failure.route_identity),
        )
        .map(|identity| identity.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if selected_route_ids.len()
        != facts
            .selected_route_ids
            .len()
            .saturating_add(registry_failures.len())
        || &selected_route_ids != expected_selected_route_ids
    {
        bail!("capture-owned source refresh receipt omitted, duplicated, or added selected route outcomes");
    }
    let mut source_failures = facts.source_failures.clone();
    source_failures.extend(registry_failures.iter().cloned());
    let failed_route_outcomes = facts
        .failed_routes
        .iter()
        .map(|failure| {
            (
                failure.route_identity.as_str().to_owned(),
                (failure.class.as_str().to_owned(), failure.carried_forward),
            )
        })
        .chain(registry_failures.iter().map(|failure| {
            (
                failure.route_identity.as_str().to_owned(),
                (failure.class.as_str().to_owned(), failure.carried_forward),
            )
        }))
        .collect::<BTreeMap<_, _>>();
    if failed_route_outcomes.len()
        != facts
            .failed_routes
            .len()
            .saturating_add(registry_failures.len())
    {
        bail!("capture-owned source refresh receipt contains duplicate failed routes");
    }
    let successful_route_changes = facts
        .successful_route_outcomes
        .iter()
        .map(|outcome| {
            (
                outcome.route_identity.as_str().to_owned(),
                (
                    outcome.changed,
                    outcome.logical_source_failure_total,
                    outcome.logical_source_retryable_failure_total,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let failed_routes = failed_route_outcomes
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if successful_route_changes.len() != facts.successful_route_outcomes.len()
        || !successful_route_changes
            .keys()
            .all(|route| selected_route_ids.contains(route))
        || !successful_route_changes
            .keys()
            .all(|route| !failed_routes.contains(route))
        || successful_route_changes
            .len()
            .saturating_add(failed_routes.len())
            != selected_route_ids.len()
    {
        bail!("capture-owned source refresh receipt has an incomplete or overlapping terminal route-result partition");
    }
    let committed_rejections = committed_rejected_records(facts.snapshot)?;
    let successful_route_rejections = facts
        .successful_route_outcomes
        .iter()
        .map(|outcome| {
            (
                outcome.route_identity.as_str().to_owned(),
                committed_rejections
                    .get(&outcome.route_identity)
                    .map(|rejections| rejections.total)
                    .unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut diagnostics_by_route = BTreeMap::<String, Vec<_>>::new();
    for failure in source_failures.failures() {
        diagnostics_by_route
            .entry(failure.route_identity.as_str().to_owned())
            .or_default()
            .push(SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: failure.source_identity.clone(),
                provider: failure.provider.as_str().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: failure.source_selector.clone(),
                detail: failure.detail.clone(),
            });
    }
    for failure in facts.logical_source_failures.failures() {
        let source_identity = source_key_identity(&failure.source);
        diagnostics_by_route
            .entry(failure.route_identity.as_str().to_owned())
            .or_default()
            .push(SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: source_identity.clone(),
                provider: failure.source.provider().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: format!("logical-source:{source_identity}"),
                detail: failure.detail.clone(),
            });
    }
    let mut rejections_by_route = BTreeMap::<String, Vec<_>>::new();
    let mut reported_rejections_by_source = HashMap::new();
    for rejection in facts.record_rejections.rejections() {
        if !rejection.is_committed() {
            continue;
        }
        let source_digest = rejection.source.exact_descriptor_digest();
        let committed_total = committed_rejections
            .get(&rejection.route_identity)
            .and_then(|route| route.by_source.get(&source_digest))
            .copied()
            .ok_or_else(|| anyhow!("committed rejection has no exact certified source"))?;
        let reported = reported_rejections_by_source
            .entry((rejection.route_identity.clone(), source_digest))
            .or_insert(0_u64);
        *reported = reported
            .checked_add(1)
            .ok_or_else(|| anyhow!("committed source rejection diagnostic total overflow"))?;
        if *reported > committed_total {
            bail!("committed source has more rejection diagnostics than rejected records");
        }
        let route_identity = rejection.route_identity.as_str().to_owned();
        rejections_by_route
            .entry(route_identity.clone())
            .or_default()
            .push(SourceBackedRefreshRecordRejection {
                route_identity,
                source_identity: source_key_identity(&rejection.source),
                provider: rejection.provider.as_str().to_owned(),
                source_selector: rejection.source_selector.clone(),
                line: rejection.line_number,
                payload_type: rejection
                    .payload_type
                    .clone()
                    .unwrap_or_else(|| "unspecified".to_owned()),
                class: rejection.class.as_str().to_owned(),
                detail: rejection.detail.clone(),
            });
    }
    let route_results = selected_route_ids
        .iter()
        .map(|route_identity| {
            let mut result = successful_route_changes
                .get(route_identity)
                .copied()
                .map(
                    |(changed, source_failure_total, source_retryable_failure_total)| {
                        let mut result = SourceBackedRefreshRouteResult::succeeded(
                            route_identity.clone(),
                            changed,
                        );
                        result.source_failure_total = source_failure_total;
                        result.source_retryable_failure_total = source_retryable_failure_total;
                        result
                    },
                )
                .or_else(|| {
                    failed_route_outcomes
                        .get(route_identity)
                        .map(|(class, carried)| {
                            SourceBackedRefreshRouteResult::failed(
                                route_identity.clone(),
                                class.clone(),
                                *carried,
                            )
                        })
                })
                .ok_or_else(|| anyhow!("selected route has no terminal outcome"))?;
            result.source_failures = diagnostics_by_route
                .remove(route_identity)
                .unwrap_or_default();
            result.rejected_record_total = successful_route_rejections
                .get(route_identity)
                .copied()
                .unwrap_or_default();
            result.rejection_diagnostics = rejections_by_route
                .remove(route_identity)
                .unwrap_or_default();
            result.validate_source_failures()?;
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;
    if !diagnostics_by_route.is_empty() || !rejections_by_route.is_empty() {
        bail!("capture-owned source refresh diagnostics name an unselected route");
    }
    Ok(route_results)
}

fn source_key_identity(source: &ctx_history_core::SourceKey) -> String {
    source
        .identity()
        .digest()
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct CommittedRouteRejectedRecords {
    total: u64,
    by_source: HashMap<[u8; 32], u64>,
}

fn committed_rejected_records(
    snapshot: &(impl ImmutableCaptureSnapshot + ?Sized),
) -> Result<HashMap<SourceRouteIdentity, CommittedRouteRejectedRecords>> {
    let certificates = snapshot
        .sources()
        .iter()
        .map(|source| (source.observation().source().identity().digest(), source))
        .collect::<HashMap<_, _>>();
    let mut by_route = HashMap::new();
    for route in snapshot.source_routes() {
        let mut by_source = HashMap::new();
        let total = route.sources().iter().try_fold(0_u64, |total, source| {
            let certificate = certificates
                .get(&source.identity().digest())
                .filter(|candidate| candidate.observation().source().exact_descriptor_eq(source))
                .ok_or_else(|| {
                    anyhow!(
                        "committed route {} names a source without an exact certificate",
                        route.route_identity().as_str()
                    )
                })?;
            let rejected_records = certificate.counts().rejected_records;
            if by_source
                .insert(source.exact_descriptor_digest(), rejected_records)
                .is_some()
            {
                bail!("committed route contains a duplicate exact source");
            }
            total
                .checked_add(rejected_records)
                .ok_or_else(|| anyhow!("committed route rejected-record total overflow"))
        })?;
        by_route.insert(
            route.route_identity().clone(),
            CommittedRouteRejectedRecords { total, by_source },
        );
    }
    Ok(by_route)
}
