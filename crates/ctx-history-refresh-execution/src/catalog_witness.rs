use super::*;

pub(super) fn reconcile_published_catalog_witness(
    snapshot: &impl ImmutableCaptureSnapshot,
    previous_catalog: Option<&ExplicitSourceCatalogAuthority>,
    previous_bindings: &[ExplicitSourceCatalogRouteBinding],
    requested_catalog: Option<&ExplicitSourceCatalogAuthority>,
    requested_bindings: &[ExplicitSourceCatalogRouteBinding],
    route_results: &[SourceBackedRefreshRouteResult],
) -> Result<(
    Option<ExplicitSourceCatalogAuthority>,
    Vec<ExplicitSourceCatalogRouteBinding>,
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
    let retained_routes = snapshot
        .source_routes()
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
        let SourceBackedRefreshRouteOutcome::Failed {
            carried_forward, ..
        } = result.outcome
        else {
            bail!("unretained explicit catalog route has no terminal failure");
        };
        let route =
            ctx_history_index::SourceRouteIdentity::from_sha256(binding.route_identity.clone())
                .context("validate failed requested explicit catalog route")?;
        if retained_routes.contains(&route) != carried_forward {
            bail!("failed explicit catalog route retention disagrees with its terminal outcome");
        }
        bindings.push(binding.clone());
    }
    bindings.sort_by(|left, right| left.catalog_lineage.cmp(&right.catalog_lineage));
    Ok((catalog, bindings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn retained_route_failure_keeps_transient_requested_lineage_binding() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let sessions = temp.path().join("configured-codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let source =
            ctx_history_capture::provider_source_for_path(CaptureProvider::Codex, sessions);
        let request = upsert_explicit_source(&data_root, &source).unwrap();
        let route = ctx_history_capture::SourceBackedRoute::automatic(
            source,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            ctx_history_capture::SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .unwrap();
        let route_identity = route.metadata().route_identity.clone().unwrap();
        let binding = ExplicitSourceCatalogRouteBinding {
            catalog_lineage: request.catalog_lineage_hex(),
            route_identity: route_identity.as_str().to_owned(),
        };
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        let receipt = ctx_history_capture::refresh_source_backed_generation(
            temp.path().join("index"),
            &registry,
            WriterOptions::default(),
        )
        .unwrap();
        let result = SourceBackedRefreshRouteResult::failed(
            route_identity.as_str().to_owned(),
            "source_changed".to_owned(),
            true,
        );

        let (catalog, bindings) = reconcile_published_catalog_witness(
            receipt.commit.snapshot(),
            None,
            &[],
            Some(&request.authority),
            std::slice::from_ref(&binding),
            &[result],
        )
        .unwrap();

        assert!(
            catalog.is_none(),
            "a failed request does not become durable authority"
        );
        assert_eq!(bindings, vec![binding]);
    }
}
