use super::*;

pub(super) fn retained_catalog_witness(
    retained_generation: Option<&VerifiedIndex>,
) -> Result<(
    Option<ExplicitSourceCatalogAuthority>,
    Vec<crate::commands::import::ExplicitSourceCatalogRouteBinding>,
)> {
    let Some(generation) = retained_generation else {
        return Ok((None, Vec::new()));
    };
    if generation.publication_metadata().is_none() {
        return Ok((None, Vec::new()));
    }
    let metadata = SourceBackedPublicationMetadata::decode(generation)
        .context("decode retained explicit catalog generation witness")?;
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), generation)
        .context("validate retained explicit catalog generation witness")?;
    Ok((
        receipt.published_explicit_source_catalog,
        receipt.catalog_route_bindings,
    ))
}

pub(super) fn reconcile_published_catalog_witness(
    previous_catalog: Option<&ExplicitSourceCatalogAuthority>,
    previous_bindings: &[crate::commands::import::ExplicitSourceCatalogRouteBinding],
    requested_catalog: Option<&ExplicitSourceCatalogAuthority>,
    requested_bindings: &[crate::commands::import::ExplicitSourceCatalogRouteBinding],
    manifest: &GenerationManifest,
    route_results: &[SourceBackedRefreshRouteResult],
) -> Result<(
    Option<ExplicitSourceCatalogAuthority>,
    Vec<crate::commands::import::ExplicitSourceCatalogRouteBinding>,
)> {
    let route_results_by_identity = route_results
        .iter()
        .map(|result| (result.route_identity.as_str(), result))
        .collect::<BTreeMap<_, _>>();
    if requested_bindings
        .iter()
        .any(|binding| !route_results_by_identity.contains_key(binding.route_identity.as_str()))
    {
        bail!("requested explicit catalog lineage has no selected terminal route result");
    }
    let retained_routes = manifest
        .source_routes()
        .iter()
        .map(|route| route.route_identity().clone())
        .collect::<BTreeSet<_>>();
    let mut published_requested_routes = BTreeSet::new();
    for binding in requested_bindings {
        let result = route_results_by_identity
            .get(binding.route_identity.as_str())
            .expect("requested route result presence checked above");
        if !result.outcome.is_success() {
            continue;
        }
        let route =
            ctx_history_index::SourceRouteIdentity::from_sha256(binding.route_identity.clone())
                .context("validate successful requested explicit catalog route")?;
        if !retained_routes.contains(&route) {
            bail!("successful requested explicit catalog route is absent from the manifest");
        }
        published_requested_routes.insert(route);
    }
    let (catalog, mut bindings) = ExplicitSourceCatalogAuthority::reconcile_generation_witness(
        previous_catalog.map(|catalog| (catalog, previous_bindings)),
        requested_catalog.map(|catalog| (catalog, requested_bindings)),
        &retained_routes,
        &published_requested_routes,
    )?;
    for binding in requested_bindings {
        if bindings
            .iter()
            .any(|retained| retained.catalog_lineage == binding.catalog_lineage)
        {
            continue;
        }
        let result = route_results_by_identity
            .get(binding.route_identity.as_str())
            .expect("requested route result presence checked above");
        if !matches!(
            result.outcome,
            SourceBackedRefreshRouteOutcome::Failed {
                carried_forward: false,
                ..
            }
        ) {
            bail!("unretained explicit catalog route has no terminal cold failure");
        }
        bindings.push(binding.clone());
    }
    bindings.sort_by(|left, right| left.catalog_lineage.cmp(&right.catalog_lineage));
    Ok((catalog, bindings))
}
