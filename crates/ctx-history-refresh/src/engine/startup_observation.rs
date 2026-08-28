use super::*;

pub(super) fn overdue_hermes_exact_routes(
    index: &VerifiedIndex,
    now_ms: i64,
    expectation: impl Fn(&SourceRouteIdentity) -> Option<SourceBackedRouteControlExpectation>,
) -> BTreeSet<SourceRouteIdentity> {
    let manifest = index.manifest();
    let route_controls = SourceBackedPublicationMetadata::decode(index)
        .map(|metadata| metadata.route_controls)
        .unwrap_or_default();
    manifest
        .source_routes()
        .iter()
        .filter_map(|route| {
            let expectation = expectation(route.route_identity())?;
            let control_due = route_controls
                .get(route.route_identity())
                .and_then(|control| expectation.exact_due(control, now_ms));
            control_due
                .unwrap_or(true)
                .then(|| route.route_identity().clone())
        })
        .collect()
}

pub(super) fn hermes_routes_requiring_control_recovery(
    catalog: &SourceBackedWatchCatalog,
    controls: &BTreeMap<SourceRouteIdentity, Vec<u8>>,
    now_ms: i64,
) -> BTreeSet<SourceRouteIdentity> {
    catalog
        .route_ids()
        .filter_map(|route| {
            let expectation = catalog.route_control_expectation(route)?;
            let valid_and_future = controls
                .get(route)
                .is_some_and(|control| expectation.exact_due(control, now_ms) == Some(false));
            (!valid_and_future).then(|| route.clone())
        })
        .collect()
}

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
        build_automatic_source_backed_registry_from_report, DiscoveryContext,
        SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    };
    use ctx_history_capture_model::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus,
    };
    use ctx_history_core::{
        CertifiedSource, ScannedSourceCounts, SourceKey, SourceObservation, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, SourceRouteSnapshot, VerifiedIndex, WriterOptions};

    fn hermes_route_control_index(
        root: &Path,
        route: &SourceRouteIdentity,
        profile_source_descriptor: [u8; 32],
        exact_due_at_ms: i64,
    ) -> VerifiedIndex {
        let source = SourceKey::derive_provider_native(
            CaptureProvider::Hermes.as_str(),
            "hermes_state_sqlite",
            "hermes-state-session-v1",
            1,
            "hermes-test-profile\u{1f}session-1",
            TypedKey::U64(1),
        )
        .unwrap();
        let database_identity = [1_u8; 32];
        let schema_evidence = [2_u8; 32];
        let revision = serde_json::to_vec(&serde_json::json!({
            "kind": "hermes-route-control-v1",
            "version": 2,
            "profile_source_descriptor": profile_source_descriptor,
            "database_identity": database_identity,
            "schema_evidence": schema_evidence,
            "session_rowid": 4,
            "message_rowid": 9,
            "last_successful_exhaustive_at_ms": 100,
            "exact_due_at_ms": exact_due_at_ms,
            "exhaustive_sequence": 1,
            "mode": "exhaustive",
            "outcome": "successful",
        }))
        .unwrap();
        let observation = SourceObservation::new(
            source.clone(),
            "hermes-source-backed-v3",
            b"session-revision".to_vec(),
        )
        .unwrap();
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            "hermes-source-backed-v3",
            [3; 32],
            ScannedSourceCounts::default(),
        )
        .unwrap();
        let mut writer = GenerationWriter::open(root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.certify_source(certificate).unwrap();
        writer
            .set_present_source_routes(vec![SourceRouteSnapshot::present(
                route.clone(),
                vec![source],
            )
            .unwrap()])
            .unwrap();
        let request_route = route.clone();
        writer
            .commit_with_publication_metadata(
                |_| true,
                move |context| {
                    let publication = SourceBackedRefreshPublication {
                        generation_id: context.generation_id().to_owned(),
                        published_explicit_source_catalog: None,
                        unsupported_routes: 0,
                        certified_source_count: 1,
                        certified_source_bytes: 0,
                        current: SourceBackedRefreshCurrent {
                            source_count: 1,
                            ..SourceBackedRefreshCurrent::default()
                        },
                        timings: SourceBackedRefreshTimings::default(),
                        route_results: vec![SourceBackedRefreshRouteResult::succeeded(
                            request_route.as_str().to_owned(),
                            true,
                        )],
                        zero_source_authority: Vec::new(),
                        catalog_route_bindings: Vec::new(),
                        verified_index: None,
                    };
                    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                        None,
                        context.generation_id().to_owned(),
                        &publication,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                    SourceBackedPublicationMetadata {
                        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                        request_id: "hermes-route-control-test".to_owned(),
                        operation: SourceBackedRefreshOperation::Refresh,
                        refresh_scope: SourceBackedRefreshScope::All,
                        receipt: receipt.to_json(),
                        route_observations: BTreeMap::new(),
                        route_controls: BTreeMap::from([(request_route.clone(), revision)]),
                    }
                    .encode()
                },
            )
            .unwrap();
        VerifiedIndex::open_pinned(root).unwrap()
    }

    fn empty_hermes_route_index(
        root: &Path,
        route: &SourceRouteIdentity,
        control: Option<Vec<u8>>,
    ) -> VerifiedIndex {
        let mut writer = GenerationWriter::open(root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer
            .set_present_source_routes(vec![SourceRouteSnapshot::present(
                route.clone(),
                Vec::new(),
            )
            .unwrap()])
            .unwrap();
        let request_route = route.clone();
        writer
            .commit_with_publication_metadata(
                |_| true,
                move |context| {
                    let publication = SourceBackedRefreshPublication {
                        generation_id: context.generation_id().to_owned(),
                        published_explicit_source_catalog: None,
                        unsupported_routes: 0,
                        certified_source_count: 0,
                        certified_source_bytes: 0,
                        current: SourceBackedRefreshCurrent::default(),
                        timings: SourceBackedRefreshTimings::default(),
                        route_results: vec![SourceBackedRefreshRouteResult::succeeded(
                            request_route.as_str().to_owned(),
                            true,
                        )],
                        zero_source_authority: vec![SourceBackedZeroSourceAuthority {
                            generation_id: context.generation_id().to_owned(),
                            route_identity: request_route.clone(),
                            kind: SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                        }],
                        catalog_route_bindings: Vec::new(),
                        verified_index: None,
                    };
                    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                        None,
                        context.generation_id().to_owned(),
                        &publication,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                    SourceBackedPublicationMetadata {
                        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                        request_id: "empty-hermes-route-control-test".to_owned(),
                        operation: SourceBackedRefreshOperation::Refresh,
                        refresh_scope: SourceBackedRefreshScope::All,
                        receipt: receipt.to_json(),
                        route_observations: BTreeMap::new(),
                        route_controls: control
                            .map(|control| BTreeMap::from([(request_route.clone(), control)]))
                            .unwrap_or_default(),
                    }
                    .encode()
                },
            )
            .unwrap();
        VerifiedIndex::open_pinned(root).unwrap()
    }

    #[test]
    fn persisted_hermes_deadline_selects_only_overdue_exact_routes() {
        let temp = tempfile::tempdir().unwrap();
        let route = SourceRouteIdentity::from_sha256("a7".repeat(32)).unwrap();
        let profile_source_descriptor = [4_u8; 32];
        let future = hermes_route_control_index(
            &temp.path().join("future"),
            &route,
            profile_source_descriptor,
            1_001,
        );
        let expectation = SourceBackedRouteControlExpectation::new(
            "hermes-route-control-v1",
            profile_source_descriptor,
            ctx_history_capture::hermes_route_control_exact_due_for_profile,
            Some(ctx_history_capture::hermes_route_control_database_identity),
        );
        assert!(overdue_hermes_exact_routes(&future, 1_000, |_| { Some(expectation) }).is_empty());
        let overdue = hermes_route_control_index(
            &temp.path().join("overdue"),
            &route,
            profile_source_descriptor,
            1_000,
        );
        assert_eq!(
            overdue_hermes_exact_routes(&overdue, 1_000, |_| Some(expectation)),
            BTreeSet::from([route])
        );
    }

    #[test]
    fn empty_hermes_route_missing_or_malformed_control_is_admitted_for_exact_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data-root");
        let path = temp.path().join("state.db");
        fs::write(&path, b"not opened by startup control recovery").unwrap();
        let source = ProviderSource {
            provider: CaptureProvider::Hermes,
            exists: true,
            path,
            source_format: "hermes_state_sqlite",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        };
        let build = build_automatic_source_backed_registry_from_report(
            &DiscoveryContext::from_process(&data_root),
            &data_root,
            DiscoveryReport {
                sources: vec![source],
                issues: Vec::new(),
            },
        );
        assert!(build.issues.is_empty());
        let catalog = build.registry.watch_catalog();
        let route = catalog.route_ids().next().cloned().expect("Hermes route");

        for (case, control) in [
            ("missing", None),
            ("malformed", Some(b"malformed".to_vec())),
        ] {
            let persisted = empty_hermes_route_index(&temp.path().join(case), &route, control);
            let persisted_route = persisted.manifest().source_route(&route).unwrap();
            assert!(persisted_route.sources().is_empty());
            let controls = SourceBackedPublicationMetadata::decode(&persisted)
                .unwrap()
                .route_controls;
            let recovery = hermes_routes_requiring_control_recovery(&catalog, &controls, 1_000);
            assert_eq!(recovery, BTreeSet::from([route.clone()]));
            assert_eq!(
                overdue_hermes_exact_routes(&persisted, 1_000, |_| {
                    catalog.route_control_expectation(&route).copied()
                }),
                BTreeSet::from([route.clone()])
            );
        }
        let engine = test_refresh_engine();
        engine.initialize_watch_route_authority([route.clone()]);
        engine.schedule_startup_route_observation(&catalog, EventWatermark::new(1, 1), 1_000);
        assert_eq!(
            engine.scheduled_route_ids_for_test(),
            BTreeSet::from([route.clone()])
        );
        assert!(engine
            .enqueue_next_dirty_route(temp.path(), 1_000_000)
            .unwrap());
        assert_eq!(
            engine.active_reconciliation_demand_for_test(),
            Some(SourceBackedReconciliationDemand::Exhaustive)
        );
    }

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
            route_provenance: Default::default(),
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
