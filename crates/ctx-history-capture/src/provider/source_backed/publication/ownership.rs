use super::*;

pub(super) fn capture_staged_source_route_revalidation_receipts(
    writer: &GenerationWriter,
    route_index: usize,
    owners: &mut HashMap<[u8; 32], SourceOwner>,
) -> SourceBackedCoordinatorResult<()> {
    for target in writer.active_source_route_revalidation_targets()? {
        let (source, receipt) = match target {
            RevalidationTarget::Source(certificate) => (
                certificate.observation().source(),
                SourceBackedRouteRevalidation::Source(certificate.clone()),
            ),
            RevalidationTarget::Deletion(deletion) => (
                deletion.source(),
                SourceBackedRouteRevalidation::Deletion(deletion.clone()),
            ),
        };
        let owner = owners
            .get_mut(&source.identity().digest())
            .filter(|owner| {
                owner.route_index == route_index && owner.source.exact_descriptor_eq(source)
            })
            .ok_or(IndexError::WriterInvariant(
                "active route certificate has no matching source owner",
            ))?;
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
                return Err(IndexError::WriterInvariant(
                    "active route certificate disagrees with its staged receipt",
                )
                .into());
            }
        }
    }
    Ok(())
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
        let revalidation = owner
            .revalidation
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "completed source route has no route-local revalidation receipt",
            ))?;
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
                return Err(IndexError::WriterInvariant(
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

pub(super) fn require_complete_base_source_ownership(
    writer: &GenerationWriter,
    registry: &SourceBackedProviderRegistry,
    owners: &HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &[CompleteInventoryOwner],
    carried_routes: &BTreeSet<SourceRouteIdentity>,
) -> SourceBackedCoordinatorResult<()> {
    let Some(base) = writer.base_manifest() else {
        return Ok(());
    };
    for source in base
        .sources
        .iter()
        .map(|source| source.observation().source())
    {
        let claimed = owners
            .get(&source.identity().digest())
            .is_some_and(|owner| {
                source_owner_covers_base_source(source, owner, complete_inventory_owners)
            });
        let covered_by_missing_route = base.source_routes().iter().any(|snapshot| {
            snapshot
                .sources()
                .iter()
                .any(|member| member.exact_descriptor_eq(source))
                && registry.routes.iter().any(|route| {
                    !route.certified_missing_paths.is_empty()
                        && route.metadata.route_identity.as_ref() == Some(snapshot.route_identity())
                })
        });
        let covered_by_carried_route = base.source_routes().iter().any(|snapshot| {
            carried_routes.contains(snapshot.route_identity())
                && snapshot
                    .sources()
                    .iter()
                    .any(|member| member.exact_descriptor_eq(source))
        });
        if !claimed && !covered_by_missing_route && !covered_by_carried_route {
            return Err(SourceBackedCoordinatorError::UnclaimedBaseSource {
                source_id: source.identity().to_string(),
            });
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
