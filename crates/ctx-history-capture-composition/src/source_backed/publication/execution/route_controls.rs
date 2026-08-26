use super::super::*;

pub(super) fn successful_route_controls(
    registry: &SourceBackedProviderRegistry,
    successful_routes: &BTreeSet<SourceRouteIdentity>,
    base_route_controls: &BTreeMap<SourceRouteIdentity, Vec<u8>>,
) -> SourceBackedCoordinatorResult<BTreeMap<SourceRouteIdentity, Vec<u8>>> {
    let mut route_controls = base_route_controls.clone();
    route_controls.retain(|route, _| !registry.provider_root_route_withdrawals.contains(route));
    for route in &registry.routes {
        let Some(route_identity) = route.metadata.route_identity.as_ref() else {
            continue;
        };
        if !successful_routes.contains(route_identity) {
            continue;
        }
        route_controls.remove(route_identity);
        if let Some(witness) = route.automatic_split_bridge_control.as_ref() {
            route_controls.insert(route_identity.clone(), witness.clone());
            continue;
        }
        let Some(control) = route
            .driver
            .as_ref()
            .and_then(|driver| driver.publication_control.as_ref())
        else {
            continue;
        };
        let Some(control) =
            control().map_err(|source| SourceBackedCoordinatorError::RouteScan {
                provider: route.metadata.source.provider,
                source,
            })?
        else {
            continue;
        };
        if control.len() > MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES {
            return Err(SourceBackedCoordinatorError::RouteScan {
                provider: route.metadata.source.provider,
                source: SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "route publication control exceeds its bounded contract",
                ),
            });
        }
        route_controls.insert(route_identity.clone(), control);
    }
    Ok(route_controls)
}
