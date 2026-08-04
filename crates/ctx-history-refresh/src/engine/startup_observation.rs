use super::*;

pub(super) fn startup_routes_requiring_refresh(
    catalog: &SourceBackedWatchCatalog,
    expected: Option<&BTreeMap<SourceRouteIdentity, String>>,
    missing_routes: &BTreeSet<SourceRouteIdentity>,
    budget: StdDuration,
) -> Vec<SourceRouteIdentity> {
    let started = StdInstant::now();
    catalog
        .route_ids()
        .filter(|route| {
            let Some(expected) = expected else {
                return true;
            };
            if started.elapsed() >= budget || missing_routes.contains(*route) {
                return true;
            }
            !matches!(
                catalog.observe_route(route, expected.get(*route).map(String::as_str)),
                RouteObservation::Unchanged
            )
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_capture::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRoute,
        SourceBackedRouteDriver,
    };

    fn watch_catalog(path: PathBuf) -> (SourceBackedWatchCatalog, SourceRouteIdentity) {
        let source = ProviderSource {
            provider: CaptureProvider::Codex,
            exists: true,
            path,
            source_format: "codex_history_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        };
        let route = SourceBackedRoute::automatic(
            source,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .unwrap();
        let identity = route.metadata().route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        (registry.watch_catalog(), identity)
    }

    #[test]
    fn warm_exact_noop_schedules_zero_parser_or_writer_work() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let (catalog, route) = watch_catalog(path);
        let expected = BTreeMap::from([(
            route.clone(),
            catalog.certify_route_observation(&route).unwrap(),
        )]);

        assert!(startup_routes_requiring_refresh(
            &catalog,
            Some(&expected),
            &BTreeSet::new(),
            StdDuration::from_secs(1),
        )
        .is_empty());
    }

    #[test]
    fn changed_unavailable_indeterminate_and_budget_expiry_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let (catalog, route) = watch_catalog(path.clone());
        let token = catalog.certify_route_observation(&route).unwrap();
        let expected = BTreeMap::from([(route.clone(), token)]);

        fs::write(&path, b"one\ntwo\n").unwrap();
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::new(),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::new(),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                None,
                &BTreeSet::new(),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::new(),
                StdDuration::ZERO,
            ),
            vec![route]
        );
    }

    #[test]
    fn missing_grace_never_skips_and_watcher_race_reenters_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let (catalog, route) = watch_catalog(path);
        let expected = BTreeMap::from([(
            route.clone(),
            catalog.certify_route_observation(&route).unwrap(),
        )]);
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::from([route.clone()]),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );

        let engine = test_refresh_engine();
        engine.initialize_watch_route_authority([route.clone()]);
        assert!(startup_routes_requiring_refresh(
            &catalog,
            Some(&expected),
            &BTreeSet::new(),
            StdDuration::from_secs(1),
        )
        .is_empty());
        engine.record_watch_routes(
            [(route.clone(), EventWatermark::new(7, 1))],
            source_route_ledger_now_ms(),
        );
        assert_eq!(
            engine.scheduled_route_ids_for_test(),
            BTreeSet::from([route])
        );
    }
}
