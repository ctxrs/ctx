use super::*;

pub(super) fn automatic_carried_route_retirements(
    registry: &SourceBackedProviderRegistry,
    selected_routes: &BTreeSet<SourceRouteIdentity>,
    base_routes: &BTreeSet<SourceRouteIdentity>,
) -> SourceBackedCoordinatorResult<BTreeMap<SourceRouteIdentity, Vec<SourceRouteIdentity>>> {
    let mut owners = BTreeMap::<SourceRouteIdentity, SourceRouteIdentity>::new();
    let mut retirements = BTreeMap::<SourceRouteIdentity, Vec<SourceRouteIdentity>>::new();
    for route in registry.routes.iter().filter(|route| {
        route.driver.is_some()
            && route
                .metadata
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected_routes.contains(identity))
    }) {
        let replacement = route.metadata.route_identity.as_ref().ok_or_else(|| {
            index_writer_invariant("selected automatic replacement has no route identity")
        })?;
        for retired in route
            .automatic_retire_after_success
            .iter()
            .filter(|candidate| {
                base_routes.contains(*candidate)
                    && !selected_routes.contains(*candidate)
                    && !route.retire_after_success.contains(*candidate)
            })
        {
            if let Some(other) = owners.insert(retired.clone(), replacement.clone()) {
                if other != *replacement {
                    return Err(SourceBackedCoordinatorError::InvalidRoute {
                        provider: route.metadata.source.provider,
                        detail: format!(
                            "automatic routes {} and {} both claim carried route {}",
                            other.as_str(),
                            replacement.as_str(),
                            retired.as_str()
                        ),
                    });
                }
            }
            retirements
                .entry(replacement.clone())
                .or_default()
                .push(retired.clone());
        }
    }
    Ok(retirements)
}

pub(super) fn capture_staged_source_route_revalidation_receipts(
    lifecycle: &impl CaptureLifecycleSink<Error = IndexError>,
    route_index: usize,
    owners: &mut HashMap<[u8; 32], SourceOwner>,
) -> SourceBackedCoordinatorResult<()> {
    lifecycle
        .visit_revalidation_targets(|target| {
            let (source, receipt) = match target {
                CaptureRevalidationTarget::Source(certificate) => (
                    certificate.observation().source(),
                    SourceBackedRouteRevalidation::Source(certificate.clone()),
                ),
                CaptureRevalidationTarget::Deletion(deletion) => (
                    deletion.source(),
                    SourceBackedRouteRevalidation::Deletion(Box::new(deletion.clone())),
                ),
            };
            let owner = owners
                .get_mut(&source.identity().digest())
                .filter(|owner| {
                    owner.route_index == route_index && owner.source.exact_descriptor_eq(source)
                })
                .ok_or_else(|| {
                    index_writer_invariant("active route certificate has no matching source owner")
                })?;
            match (&owner.revalidation, &receipt) {
                (None, _) => owner.revalidation = Some(receipt),
                (
                    Some(SourceBackedRouteRevalidation::Source(expected)),
                    SourceBackedRouteRevalidation::Source(actual),
                ) if expected == actual => {}
                (
                    Some(SourceBackedRouteRevalidation::Deletion(expected)),
                    SourceBackedRouteRevalidation::Deletion(actual),
                ) if expected == actual => {}
                _ => {
                    return Err(index_writer_invariant(
                        "active route certificate disagrees with its staged receipt",
                    ));
                }
            }
            Ok(())
        })?
        .map_err(SourceBackedCoordinatorError::Index)
}

pub(super) fn revalidate_staged_source_route(
    provider: CaptureProvider,
    route_index: usize,
    driver: &SourceBackedRouteDriver,
    owners: &HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &[CompleteInventoryOwner],
) -> SourceBackedCoordinatorResult<bool> {
    // Keep the route savepoint active until its complete source authority has
    // passed once. Accepted compact receipts are checked structurally at the
    // global commit, so a later failed route can roll back without rescanning
    // or revalidating any earlier successful route. Source churn after this
    // route-local fence belongs to a future refresh; it does not reopen work
    // already completed and certified by this refresh.
    for owner in owners
        .values()
        .filter(|owner| owner.route_index == route_index)
    {
        let revalidation = owner.revalidation.as_ref().ok_or_else(|| {
            index_writer_invariant("completed source route has no route-local revalidation receipt")
        })?;
        let valid = match revalidation {
            SourceBackedRouteRevalidation::Source(certificate) if owner.present => {
                let source = certificate.observation().source();
                owner.source.exact_descriptor_eq(source)
                    && route_callback(provider, (driver.owns_source)(source))?
                    && route_callback(
                        provider,
                        (driver.revalidate)(SourceBackedRevalidationTarget::Source(certificate)),
                    )?
            }
            SourceBackedRouteRevalidation::Deletion(deletion) if !owner.present => {
                let source = deletion.source();
                owner.source.exact_descriptor_eq(source)
                    && route_callback(provider, (driver.owns_source)(source))?
                    && route_callback(
                        provider,
                        (driver.revalidate)(SourceBackedRevalidationTarget::Deletion(deletion)),
                    )?
            }
            _ => {
                return Err(index_writer_invariant(
                    "source route revalidation receipt disagrees with staged ownership",
                )
                .into());
            }
        };
        if !valid {
            return Ok(false);
        }
    }

    for owner in complete_inventory_owners
        .iter()
        .filter(|owner| owner.route_index == route_index)
    {
        let valid = match driver.revalidate_complete_inventory.as_ref() {
            Some(revalidate) => route_callback(provider, revalidate(&owner.inventory))?,
            None => false,
        };
        if !valid {
            return Ok(false);
        }
    }
    Ok(true)
}

fn route_callback(
    provider: CaptureProvider,
    result: SourceBackedRouteResult<bool>,
) -> SourceBackedCoordinatorResult<bool> {
    result.map_err(|source| SourceBackedCoordinatorError::RouteScan { provider, source })
}

pub(super) struct BaseSourceOwnershipEvidence<'a> {
    pub(super) carried_routes: &'a BTreeSet<SourceRouteIdentity>,
    pub(super) partial_routes: &'a BTreeSet<SourceRouteIdentity>,
    pub(super) successful_routes: &'a BTreeSet<SourceRouteIdentity>,
    pub(super) failed_routes: &'a BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    pub(super) logical_source_failures: &'a SourceBackedLogicalSourceFailures,
}

pub(super) fn require_complete_base_source_ownership(
    lifecycle: &impl CaptureLifecycleSink<Error = IndexError>,
    registry: &SourceBackedProviderRegistry,
    owners: &HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &[CompleteInventoryOwner],
    evidence: BaseSourceOwnershipEvidence<'_>,
) -> SourceBackedCoordinatorResult<()> {
    let Some(base) = lifecycle.base_snapshot() else {
        return Ok(());
    };
    for snapshot in base.source_routes() {
        let covered_by_missing_route = registry.routes.iter().any(|route| {
            !route.certified_missing_paths.is_empty()
                && route.metadata.route_identity.as_ref() == Some(snapshot.route_identity())
        });
        if covered_by_missing_route
            || evidence.carried_routes.contains(snapshot.route_identity())
            || evidence.partial_routes.contains(snapshot.route_identity())
            || registry
                .provider_root_route_retirements
                .contains(snapshot.route_identity())
        {
            continue;
        }
        for source in snapshot.sources() {
            let owner = owners.get(&source.identity().digest());
            if !owner.is_some_and(|owner| {
                source_owner_covers_base_source(source, owner, complete_inventory_owners)
            }) {
                let route_identity = match owner {
                    Some(owner) => registry
                        .routes
                        .get(owner.route_index)
                        .and_then(|route| route.metadata.route_identity.as_ref()),
                    None => Some(snapshot.route_identity()),
                }
                .filter(|route_identity| evidence.successful_routes.contains(*route_identity))
                .cloned()
                .ok_or_else(|| {
                    SourceBackedCoordinatorError::Index(index_writer_invariant(
                        "unclaimed base source has no unique successful provider route",
                    ))
                })?;
                return Err(SourceBackedCoordinatorError::UnclaimedBaseSource {
                    source_id: source.identity().to_string(),
                    route_identity,
                    route_failures: evidence
                        .failed_routes
                        .values()
                        .map(SourceBackedFailedRouteOutcome::from)
                        .collect(),
                    logical_source_failures: Box::new(evidence.logical_source_failures.clone()),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn source_owner_covers_base_source(
    base: &SourceKey,
    owner: &SourceOwner,
    complete_inventory_owners: &[CompleteInventoryOwner],
) -> bool {
    if owner.source.exact_descriptor_eq(base) {
        return true;
    }
    if !base.is_same_lineage_descriptor_replacement(&owner.source) {
        return false;
    }

    let mut matching_inventories = complete_inventory_owners.iter().filter(|candidate| {
        candidate.route_index == owner.route_index
            && candidate.inventory.observation().provider() == owner.source.provider()
            && candidate.inventory.validate_contract().is_ok()
            && candidate.inventory.contains(&owner.source)
    });
    matching_inventories.next().is_some() && matching_inventories.next().is_none()
}
