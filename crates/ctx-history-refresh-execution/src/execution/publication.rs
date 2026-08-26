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

pub(super) struct PublicationMetadataEvidence<'a> {
    pub(super) committed_rejection_diagnostics: &'a [SourceBackedRefreshRecordRejection],
    pub(super) route_observations: BTreeMap<SourceRouteIdentity, String>,
    pub(super) route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

pub(super) fn encode_publication_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    previous_generation: Option<&str>,
    publication: &SourceBackedRefreshPublication,
    evidence: PublicationMetadataEvidence<'_>,
) -> Result<Vec<u8>> {
    let terminal = SourceBackedRefreshReceipt::from_verified_publication(
        previous_generation.map(str::to_owned),
        publication.generation_id.clone(),
        publication,
    )?;
    let metadata = SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: request_id.to_owned(),
        operation,
        refresh_scope: scope.clone(),
        receipt: terminal.to_json(),
        route_observations: evidence.route_observations,
        route_controls: evidence.route_controls,
    };
    let mut committed_rejection_diagnostics = evidence.committed_rejection_diagnostics.to_vec();
    loop {
        let ledger_json = rejection_diagnostics_ledger_json(&committed_rejection_diagnostics)?;
        match metadata.encode_with_committed_rejection_diagnostics(&ledger_json) {
            Ok(encoded) => return Ok(encoded),
            Err(IndexError::PublicationMetadataTooLarge { .. })
                if committed_rejection_diagnostics.pop().is_some() => {}
            Err(error) => return Err(error.into()),
        }
    }
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

/// Replays only rejection evidence already authenticated by the retained
/// generation and still backed by an identical source certificate. Automatic
/// route identities may change while a source is carried unchanged, so source
/// certificates—not transient route identity—own this evidence.
pub(super) fn preserve_carried_rejection_diagnostics(
    route_results: &mut [SourceBackedRefreshRouteResult],
    snapshot: &(impl ImmutableCaptureSnapshot + ?Sized),
    retained_generation: Option<&VerifiedIndex>,
) -> Result<Vec<SourceBackedRefreshRecordRejection>> {
    let authority = current_rejection_authority(snapshot)?;
    let (previous_diagnostics, stable_sources) = match retained_generation
        .filter(|retained| retained.publication_metadata().is_some())
    {
        Some(retained_generation) => {
            let (metadata, committed_rejection_diagnostics) =
                SourceBackedPublicationMetadata::decode_with_committed_rejection_diagnostics(
                    retained_generation,
                )?;
            let previous = published_refresh_receipt_for_index(
                &metadata.response_value(),
                retained_generation,
            )?;
            if previous.published_generation != retained_generation.generation_id() {
                bail!("carried Core rejection diagnostics do not match the retained generation");
            }
            (
                committed_rejection_diagnostics.unwrap_or_else(|| {
                    previous
                        .route_results
                        .iter()
                        .filter(|result| result.outcome.is_success())
                        .flat_map(|result| result.rejection_diagnostics.iter().cloned())
                        .collect()
                }),
                stable_source_identities(snapshot, retained_generation),
            )
        }
        None => (Vec::new(), BTreeSet::new()),
    };
    let current_diagnostics = route_results
        .iter()
        .filter(|result| result.outcome.is_success())
        .flat_map(|result| result.rejection_diagnostics.iter().cloned())
        .collect::<Vec<_>>();
    let ledger = build_committed_rejection_ledger(
        &current_diagnostics,
        &previous_diagnostics,
        &stable_sources,
        &authority,
    )?;
    for result in route_results {
        if !result.outcome.is_success()
            || result.rejected_record_total == 0
            || result.rejection_diagnostics.len() as u64 >= result.rejected_record_total
        {
            continue;
        }
        carry_route_rejection_diagnostics(result, &ledger, &authority)?;
    }
    Ok(ledger)
}

struct CurrentRejectionAuthority {
    sources: HashMap<String, RejectionSourceAuthority>,
}

struct RejectionSourceAuthority {
    route_identity: String,
    capacity: u64,
}

fn current_rejection_authority(
    snapshot: &(impl ImmutableCaptureSnapshot + ?Sized),
) -> Result<CurrentRejectionAuthority> {
    let current_certificates = snapshot
        .sources()
        .iter()
        .map(|certificate| {
            (
                certificate.observation().source().identity().digest(),
                certificate,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut sources = HashMap::<String, Option<RejectionSourceAuthority>>::new();
    for route in snapshot.source_routes() {
        let route_identity = route.route_identity().as_str().to_owned();
        for source in route.sources() {
            let certificate = current_certificates
                .get(&source.identity().digest())
                .filter(|candidate| candidate.observation().source().exact_descriptor_eq(source))
                .ok_or_else(|| {
                    anyhow!("committed route names a source without an exact certificate")
                })?;
            let source_identity = source_key_identity(source);
            let capacity = certificate.counts().rejected_records;
            match sources.entry(source_identity) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Some(RejectionSourceAuthority {
                        route_identity: route_identity.clone(),
                        capacity,
                    }));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
    }
    Ok(CurrentRejectionAuthority {
        sources: sources
            .into_iter()
            .filter_map(|(source, authority)| authority.map(|authority| (source, authority)))
            .collect(),
    })
}

fn stable_source_identities(
    snapshot: &(impl ImmutableCaptureSnapshot + ?Sized),
    retained_generation: &VerifiedIndex,
) -> BTreeSet<String> {
    let retained_certificates = retained_generation
        .manifest()
        .sources
        .iter()
        .map(|certificate| {
            (
                certificate.observation().source().identity().digest(),
                certificate,
            )
        })
        .collect::<HashMap<_, _>>();
    snapshot
        .sources()
        .iter()
        .filter(|certificate| {
            retained_certificates
                .get(&certificate.observation().source().identity().digest())
                .is_some_and(|retained| *retained == *certificate)
        })
        .map(|certificate| source_key_identity(certificate.observation().source()))
        .collect()
}

fn build_committed_rejection_ledger(
    current_diagnostics: &[SourceBackedRefreshRecordRejection],
    previous_diagnostics: &[SourceBackedRefreshRecordRejection],
    stable_sources: &BTreeSet<String>,
    authority: &CurrentRejectionAuthority,
) -> Result<Vec<SourceBackedRefreshRecordRejection>> {
    let mut ledger = Vec::new();
    let mut reported_by_source = HashMap::<String, u64>::new();
    let mut exact_diagnostics = BTreeSet::new();
    for rejection in current_diagnostics.iter().chain(
        previous_diagnostics
            .iter()
            .filter(|rejection| stable_sources.contains(&rejection.source_identity)),
    ) {
        if ledger.len() >= ctx_history_capture_runtime::MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS
        {
            break;
        }
        let Some(source_authority) = authority.sources.get(&rejection.source_identity) else {
            continue;
        };
        let identity = rejection_diagnostic_identity(rejection);
        if exact_diagnostics.contains(&identity) {
            continue;
        }
        let reported = reported_by_source
            .entry(rejection.source_identity.clone())
            .or_default();
        if *reported >= source_authority.capacity {
            continue;
        }
        *reported = reported
            .checked_add(1)
            .ok_or_else(|| anyhow!("committed source rejection diagnostic total overflow"))?;
        exact_diagnostics.insert(identity);
        let mut rejection = rejection.clone();
        rejection
            .route_identity
            .clone_from(&source_authority.route_identity);
        ledger.push(rejection);
    }
    Ok(ledger)
}

fn rejection_diagnostics_ledger_json(
    diagnostics: &[SourceBackedRefreshRecordRejection],
) -> Result<Value> {
    let mut by_route = BTreeMap::<String, SourceBackedRefreshRouteResult>::new();
    for rejection in diagnostics {
        let result = by_route
            .entry(rejection.route_identity.clone())
            .or_insert_with(|| {
                SourceBackedRefreshRouteResult::succeeded(rejection.route_identity.clone(), false)
            });
        result.rejected_record_total = result
            .rejected_record_total
            .checked_add(1)
            .ok_or_else(|| anyhow!("committed rejection-diagnostic ledger total overflow"))?;
        result.rejection_diagnostics.push(rejection.clone());
    }
    let mut routes = serde_json::Map::new();
    for (route_identity, result) in by_route {
        result.validate_source_failures()?;
        routes.insert(route_identity, result.compact_json());
    }
    Ok(Value::Object(routes))
}

fn carry_route_rejection_diagnostics(
    result: &mut SourceBackedRefreshRouteResult,
    previous_diagnostics: &[SourceBackedRefreshRecordRejection],
    authority: &CurrentRejectionAuthority,
) -> Result<()> {
    let mut reported_by_source = HashMap::<String, u64>::new();
    let mut exact_diagnostics = result
        .rejection_diagnostics
        .iter()
        .map(rejection_diagnostic_identity)
        .collect::<BTreeSet<_>>();
    for rejection in &result.rejection_diagnostics {
        let reported = reported_by_source
            .entry(rejection.source_identity.clone())
            .or_default();
        *reported = reported
            .checked_add(1)
            .ok_or_else(|| anyhow!("committed source rejection diagnostic total overflow"))?;
    }
    for rejection in previous_diagnostics {
        if result.rejection_diagnostics.len() as u64 >= result.rejected_record_total {
            break;
        }
        let identity = rejection_diagnostic_identity(rejection);
        if exact_diagnostics.contains(&identity) {
            continue;
        }
        let Some(source_authority) = authority
            .sources
            .get(&rejection.source_identity)
            .filter(|authority| authority.route_identity == result.route_identity)
        else {
            continue;
        };
        let reported = reported_by_source
            .entry(rejection.source_identity.clone())
            .or_default();
        if *reported >= source_authority.capacity {
            continue;
        }
        *reported = reported
            .checked_add(1)
            .ok_or_else(|| anyhow!("carried source rejection diagnostic total overflow"))?;
        exact_diagnostics.insert(identity);
        let mut rejection = rejection.clone();
        rejection.route_identity.clone_from(&result.route_identity);
        result.rejection_diagnostics.push(rejection);
    }
    result.validate_source_failures()
}

fn rejection_diagnostic_identity(
    rejection: &SourceBackedRefreshRecordRejection,
) -> (String, String, String, u64, String, String, String) {
    (
        rejection.source_identity.clone(),
        rejection.provider.clone(),
        rejection.source_selector.clone(),
        rejection.line,
        rejection.payload_type.clone(),
        rejection.class.clone(),
        rejection.detail.clone(),
    )
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

#[cfg(test)]
mod carried_rejection_tests {
    use super::*;

    fn rejection(source_identity: &str, line: u64) -> SourceBackedRefreshRecordRejection {
        SourceBackedRefreshRecordRejection {
            route_identity: "a".repeat(64),
            source_identity: source_identity.to_owned(),
            provider: "codex".to_owned(),
            source_selector: format!("source:{source_identity}"),
            line,
            payload_type: "message".to_owned(),
            class: "malformed_record".to_owned(),
            detail: format!("rejected source {source_identity} line {line}"),
        }
    }

    #[test]
    fn carried_diagnostics_cannot_consume_another_sources_capacity() {
        let source_a = "b".repeat(64);
        let source_b = "c".repeat(64);
        let mut current = SourceBackedRefreshRouteResult::succeeded("a".repeat(64), false);
        current.rejected_record_total = 2;
        current.rejection_diagnostics = vec![rejection(&source_a, 2)];
        let previous_diagnostics = vec![rejection(&source_a, 1), rejection(&source_b, 1)];
        let authority = CurrentRejectionAuthority {
            sources: HashMap::from([
                (
                    source_a.clone(),
                    RejectionSourceAuthority {
                        route_identity: current.route_identity.clone(),
                        capacity: 1,
                    },
                ),
                (
                    source_b.clone(),
                    RejectionSourceAuthority {
                        route_identity: current.route_identity.clone(),
                        capacity: 1,
                    },
                ),
            ]),
        };

        carry_route_rejection_diagnostics(&mut current, &previous_diagnostics, &authority).unwrap();

        assert_eq!(
            current
                .rejection_diagnostics
                .iter()
                .map(|rejection| (rejection.source_identity.as_str(), rejection.line))
                .collect::<Vec<_>>(),
            vec![(source_a.as_str(), 2), (source_b.as_str(), 1)]
        );
    }

    #[test]
    fn metadata_size_truncation_is_deterministic_and_does_not_revive_evidence() {
        let route_identity = "a".repeat(64);
        let source_identity = "b".repeat(64);
        let route = SourceRouteIdentity::from_sha256(route_identity.clone()).unwrap();
        let diagnostics = (1..=64)
            .map(|line| {
                let mut rejection = rejection(&source_identity, line);
                rejection.source_selector = "s".repeat(512);
                rejection.payload_type = "p".repeat(128);
                rejection.detail = "d".repeat(512);
                rejection
            })
            .collect::<Vec<_>>();
        let mut result = SourceBackedRefreshRouteResult::succeeded(route_identity.clone(), false);
        result.rejected_record_total = diagnostics.len() as u64;
        result.rejection_diagnostics = diagnostics.clone();
        let publication = SourceBackedRefreshPublication {
            generation_id: "c".repeat(64),
            published_explicit_source_catalog: None,
            unsupported_routes: 0,
            certified_source_count: 1,
            certified_source_bytes: 1,
            current: SourceBackedRefreshCurrent {
                source_count: 1,
                complete_records: diagnostics.len() as u64,
                rejected_records: diagnostics.len() as u64,
                certified_source_bytes: 1,
                sources_with_rejections: 1,
                ..SourceBackedRefreshCurrent::default()
            },
            route_results: vec![result],
            zero_source_authority: Vec::new(),
            catalog_route_bindings: Vec::new(),
            timings: SourceBackedRefreshTimings::default(),
            verified_index: None,
        };
        let encode = || {
            encode_publication_metadata(
                "metadata-ledger-size-test",
                RefreshOperation::Refresh,
                &SourceBackedRefreshScope::exact([route.clone()]),
                Some(&publication.generation_id),
                &publication,
                PublicationMetadataEvidence {
                    committed_rejection_diagnostics: &diagnostics,
                    route_observations: BTreeMap::new(),
                    route_controls: BTreeMap::from([(
                        route.clone(),
                        vec![0; MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES],
                    )]),
                },
            )
            .unwrap()
        };

        let first = encode();
        let second = encode();
        assert_eq!(first, second);
        let encoded: Value = serde_json::from_slice(&first).unwrap();
        let persisted = required_route_results(
            encoded.get(crate::metadata::COMMITTED_REJECTION_DIAGNOSTICS_FIELD),
        )
        .unwrap()
        .into_iter()
        .flat_map(|result| result.rejection_diagnostics)
        .collect::<Vec<_>>();
        assert!(!persisted.is_empty());
        assert!(persisted.len() < diagnostics.len());

        let authority = CurrentRejectionAuthority {
            sources: HashMap::from([(
                source_identity.clone(),
                RejectionSourceAuthority {
                    route_identity: route_identity.clone(),
                    capacity: diagnostics.len() as u64,
                },
            )]),
        };
        let carried = build_committed_rejection_ledger(
            &[],
            &persisted,
            &BTreeSet::from([source_identity]),
            &authority,
        )
        .unwrap();
        assert_eq!(carried, persisted);
        assert!(!carried.iter().any(|rejection| rejection.line == 64));
        let mut next = SourceBackedRefreshRouteResult::succeeded(route_identity, false);
        next.rejected_record_total = diagnostics.len() as u64;
        carry_route_rejection_diagnostics(&mut next, &carried, &authority).unwrap();
        assert_eq!(next.rejection_diagnostics, persisted);
        assert_eq!(
            next.rejected_record_total - next.rejection_diagnostics.len() as u64,
            (diagnostics.len() - persisted.len()) as u64
        );
    }
}
