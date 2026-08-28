use std::path::PathBuf;

use ctx_history_capture_model::ProviderSourceRouteProvenance;

use super::*;
use crate::ProviderCatalogSupport;

fn source_with_role(
    provider: CaptureProvider,
    source_format: &'static str,
    path: impl Into<PathBuf>,
    role: ProviderRouteRole,
    status: ProviderSourceStatus,
) -> ProviderSource {
    ProviderSource {
        provider,
        path: path.into(),
        exists: status != ProviderSourceStatus::Missing,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status,
        unsupported_reason: None,
        route_provenance: ProviderSourceRouteProvenance::Automatic { route_role: role },
    }
}

fn source(role: &'static str) -> ProviderSource {
    source_with_role(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        format!("/fixture/{role}"),
        ProviderRouteRole::from_static(role),
        ProviderSourceStatus::Available,
    )
}

fn executable_route(source: ProviderSource) -> SourceBackedRoute {
    SourceBackedRoute::automatic(
        source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| true, |_| true),
    )
    .unwrap()
}

fn route(role: &'static str) -> SourceBackedRoute {
    executable_route(source(role))
}

fn missing_route(source: ProviderSource) -> SourceBackedRoute {
    SourceBackedRoute::certified_missing(source, SourceBackedSelectorAuthority::DiscoveredWinner)
        .unwrap()
}

fn route_id(route: &SourceBackedRoute) -> SourceRouteIdentity {
    route.metadata.route_identity.clone().unwrap()
}

fn fail_before_scan(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let owns = std::sync::Arc::clone(&original.owns_source);
    route.driver = Some(SourceBackedRouteDriver::new_fallible(
        |_| {
            Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "injected split cohort failure",
            ))
        },
        move |source| owns(source),
        |_| Ok(false),
    ));
    route
}

#[test]
fn cold_roled_routes_remain_direct_and_unroled_legacy_hash_is_stable() {
    let roled = route("surface-cli");
    let current = route_id(&roled);
    let legacy = legacy_automatic_source_backed_route_identity(&roled.metadata.source).unwrap();
    assert_ne!(current, legacy);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(roled);
    let plan = prepare_automatic_route_splits(
        &mut registry,
        &BTreeSet::new(),
        &BTreeMap::new(),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    assert!(!plan.requires_exhaustive_publication());
    assert_eq!(route_id(registry.routes.first().unwrap()), current);
    assert!(registry
        .watch_catalog()
        .has_automatic_split_legacy_route(&legacy));
}

#[test]
fn first_warm_publication_bridges_the_first_winner_and_writes_a_witness() {
    let first = route("surface-cli");
    let second = route("surface-ide");
    let legacy = legacy_automatic_source_backed_route_identity(&first.metadata.source).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first);
    registry.register(second);
    let plan = prepare_automatic_route_splits(
        &mut registry,
        &BTreeSet::from([legacy.clone()]),
        &BTreeMap::new(),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    assert!(plan.requires_exhaustive_publication());
    assert_eq!(registry.routes.len(), 1);
    let bridge = registry.routes.first().unwrap();
    assert_eq!(route_id(bridge), legacy);
    let witness =
        decode_witness(bridge.automatic_split_bridge_control.as_deref().unwrap()).unwrap();
    assert_eq!(witness.role, ProviderRouteRole::from_static("surface-cli"));
}

#[test]
fn bridge_conflicts_replace_missing_with_first_executable_for_fixed_and_dynamic_roles() {
    let cases = [
        (
            CaptureProvider::Antigravity,
            "antigravity_cli_transcript_jsonl_tree",
            ProviderRouteRole::from_dynamic([b"surface".as_slice(), b"cli".as_slice()]).unwrap(),
            ProviderRouteRole::from_dynamic([b"surface".as_slice(), b"ide".as_slice()]).unwrap(),
        ),
        (
            CaptureProvider::OpenClaw,
            "openclaw_session_jsonl_tree",
            ProviderRouteRole::from_dynamic([b"agent".as_slice(), b"missing".as_slice()]).unwrap(),
            ProviderRouteRole::from_dynamic([b"agent".as_slice(), b"available".as_slice()])
                .unwrap(),
        ),
    ];
    for (provider, format, missing_role, available_role) in cases {
        let missing_source = source_with_role(
            provider,
            format,
            format!("/fixture/{}/missing", provider.as_str()),
            missing_role,
            ProviderSourceStatus::Missing,
        );
        let available_path = PathBuf::from(format!("/fixture/{}/available", provider.as_str()));
        let available_source = source_with_role(
            provider,
            format,
            available_path.clone(),
            available_role.clone(),
            ProviderSourceStatus::Available,
        );
        let legacy = legacy_automatic_source_backed_route_identity(&available_source).unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(missing_route(missing_source));
        registry.register(executable_route(available_source));
        prepare_automatic_route_splits(
            &mut registry,
            &BTreeSet::from([legacy.clone()]),
            &BTreeMap::new(),
            &SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
        )
        .unwrap();
        let [bridge] = registry.routes.as_ref() else {
            panic!("one released bridge route expected");
        };
        assert!(bridge.driver.is_some());
        assert_eq!(bridge.metadata.source.path, available_path);
        assert!(bridge.certified_missing_paths.is_empty());
        let witness =
            decode_witness(bridge.automatic_split_bridge_control.as_deref().unwrap()).unwrap();
        assert_eq!(witness.role, available_role);
    }
}

#[test]
fn all_missing_bridge_candidates_merge_paths_like_the_released_registry() {
    let first_path = PathBuf::from("/fixture/missing-z");
    let second_path = PathBuf::from("/fixture/missing-a");
    let first = source_with_role(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        first_path.clone(),
        ProviderRouteRole::from_static("missing-z"),
        ProviderSourceStatus::Missing,
    );
    let second = source_with_role(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        second_path.clone(),
        ProviderRouteRole::from_static("missing-a"),
        ProviderSourceStatus::Missing,
    );
    let legacy = legacy_automatic_source_backed_route_identity(&first).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(missing_route(first));
    registry.register(missing_route(second));
    prepare_automatic_route_splits(
        &mut registry,
        &BTreeSet::from([legacy]),
        &BTreeMap::new(),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    let [bridge] = registry.routes.as_ref() else {
        panic!("one merged missing bridge expected");
    };
    assert_eq!(
        bridge.certified_missing_paths,
        vec![second_path, first_path]
    );
}

#[test]
fn witnessed_successor_alone_receives_the_legacy_alias_and_retirement_barrier() {
    let first = route("surface-cli");
    let second = route("surface-ide");
    let legacy = legacy_automatic_source_backed_route_identity(&first.metadata.source).unwrap();
    let cohort = split_cohort(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
    )
    .unwrap();
    let witness = encode_witness(&SplitWitness {
        cohort,
        role: ProviderRouteRole::from_static("surface-ide"),
    })
    .unwrap();
    let first_id = route_id(&first);
    let second_id = route_id(&second);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first);
    registry.register(second);
    prepare_automatic_route_splits(
        &mut registry,
        &BTreeSet::from([legacy.clone()]),
        &BTreeMap::from([(legacy.clone(), witness)]),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    let first = registry
        .routes
        .iter()
        .find(|route| route_id(route) == first_id)
        .unwrap();
    let second = registry
        .routes
        .iter()
        .find(|route| route_id(route) == second_id)
        .unwrap();
    assert!(first.base_route_aliases.is_empty());
    assert_eq!(second.base_route_aliases, BTreeSet::from([legacy.clone()]));
    assert_eq!(registry.automatic_split_cohort_barriers.len(), 1);
    assert_eq!(
        registry.automatic_split_cohort_barriers[0].cohort,
        BTreeSet::from([first_id, second_id])
    );
}

#[test]
fn every_roled_nonowner_blocks_retirement_except_certified_missing() {
    let owner_role = ProviderRouteRole::from_static("surface-cli");
    let owner_source = source_with_role(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        "/fixture/owner",
        owner_role.clone(),
        ProviderSourceStatus::Available,
    );
    let legacy = legacy_automatic_source_backed_route_identity(&owner_source).unwrap();
    let witness = encode_witness(&SplitWitness {
        cohort: split_cohort(
            CaptureProvider::Antigravity,
            "antigravity_cli_transcript_jsonl_tree",
        )
        .unwrap(),
        role: owner_role,
    })
    .unwrap();

    for (name, status) in [
        ("unavailable", ProviderSourceStatus::Unknown),
        ("unsupported", ProviderSourceStatus::Unsupported),
        ("registration-failed", ProviderSourceStatus::Available),
    ] {
        let blocked = source_with_role(
            CaptureProvider::Antigravity,
            "antigravity_cli_transcript_jsonl_tree",
            format!("/fixture/{name}"),
            ProviderRouteRole::from_static(name),
            status,
        );
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(executable_route(owner_source.clone()));
        registry.register(SourceBackedRoute::unsupported(
            blocked,
            format!("injected {name} candidate"),
        ));
        let error = prepare_automatic_route_splits(
            &mut registry,
            &BTreeSet::from([legacy.clone()]),
            &BTreeMap::from([(legacy.clone(), witness.clone())]),
            &SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
        )
        .expect_err("known unusable nonowner must block retirement");
        assert!(error.to_string().contains("unavailable or unsupported"));
    }

    let missing = source_with_role(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        "/fixture/certified-missing",
        ProviderRouteRole::from_static("certified-missing"),
        ProviderSourceStatus::Missing,
    );
    let missing_id = automatic_source_backed_route_identity(&missing).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(executable_route(owner_source));
    registry.register(missing_route(missing));
    prepare_automatic_route_splits(
        &mut registry,
        &BTreeSet::from([legacy.clone()]),
        &BTreeMap::from([(legacy, witness)]),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    assert_eq!(registry.automatic_split_cohort_barriers.len(), 1);
    assert!(registry.automatic_split_cohort_barriers[0]
        .cohort
        .contains(&missing_id));
}

#[test]
fn malformed_stale_mixed_and_partial_split_states_fail_closed() {
    let fixture = route("surface-cli");
    let legacy = legacy_automatic_source_backed_route_identity(&fixture.metadata.source).unwrap();
    let current = route_id(&fixture);
    let attempt = |base: BTreeSet<SourceRouteIdentity>,
                   controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
                   scope: SourceBackedRefreshScope,
                   demand| {
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route("surface-cli"));
        prepare_automatic_route_splits(&mut registry, &base, &controls, &scope, demand)
    };
    assert!(attempt(
        BTreeSet::from([legacy.clone()]),
        BTreeMap::from([(legacy.clone(), WITNESS_MAGIC.to_vec())]),
        SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .is_err());
    assert!(attempt(
        BTreeSet::from([legacy.clone(), current]),
        BTreeMap::new(),
        SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .is_err());
    assert!(attempt(
        BTreeSet::from([legacy.clone()]),
        BTreeMap::new(),
        SourceBackedRefreshScope::exact([legacy.clone()]),
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .is_err());
    assert!(attempt(
        BTreeSet::from([legacy]),
        BTreeMap::new(),
        SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Incremental,
    )
    .is_err());
}

#[test]
fn split_cohorts_are_limited_to_the_six_roled_certified_formats() {
    for (provider, format) in [
        (CaptureProvider::OpenClaw, "openclaw_session_jsonl_tree"),
        (CaptureProvider::Warp, "warp_sqlite"),
        (CaptureProvider::Cline, "cline_task_directory_json"),
        (
            CaptureProvider::Antigravity,
            "antigravity_cli_transcript_jsonl_tree",
        ),
        (CaptureProvider::CodeBuddy, "codebuddy_history_json"),
        (CaptureProvider::RooCode, "roo_task_directory_json"),
    ] {
        assert!(
            split_cohort(provider, format).is_some(),
            "{provider:?} {format}"
        );
    }
    assert!(split_cohort(CaptureProvider::OpenClaw, "openclaw_agent_sqlite").is_none());
    assert!(split_cohort(CaptureProvider::Cline, "cline_sdk_session_store").is_none());
    assert!(split_cohort(CaptureProvider::RooCode, "roo_task_json").is_none());
}

#[test]
fn split_witness_enforces_the_role_and_control_byte_bounds() {
    let component = vec![b'r'; 247];
    let role = ProviderRouteRole::from_dynamic([component.as_slice()]).unwrap();
    assert_eq!(role.as_bytes().len(), 256);
    let encoded = encode_witness(&SplitWitness {
        cohort: [7; 32],
        role,
    })
    .unwrap();
    assert_eq!(encoded.len(), MAX_AUTOMATIC_ROUTE_SPLIT_WITNESS_BYTES);
    assert!(decode_witness(&encoded).is_ok());
    assert!(decode_witness(&vec![0; MAX_AUTOMATIC_ROUTE_SPLIT_WITNESS_BYTES + 1]).is_err());
}

#[test]
fn two_publication_bridge_keeps_the_legacy_route_then_retires_it_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let initial_current = route("surface-cli");
    let legacy =
        legacy_automatic_source_backed_route_identity(&initial_current.metadata.source).unwrap();
    let mut released = initial_current.clone();
    released.metadata.route_identity = Some(legacy.clone());
    let mut released_registry = SourceBackedProviderRegistry::new();
    released_registry.register(released);
    let initial =
        refresh_source_backed_generation(&index_root, &released_registry, WriterOptions::default())
            .unwrap();
    assert!(initial.commit.manifest().source_route(&legacy).is_some());

    let mut bridge_registry = SourceBackedProviderRegistry::new();
    bridge_registry.register(route("surface-cli"));
    bridge_registry.register(route("surface-ide"));
    prepare_automatic_route_splits(
        &mut bridge_registry,
        &BTreeSet::from([legacy.clone()]),
        &BTreeMap::new(),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    let bridge = SourceBackedRefreshExecutor::new(bridge_registry, WriterOptions::default())
        .refresh(&index_root, |_| Ok(()))
        .unwrap();
    assert!(bridge.commit.manifest().source_route(&legacy).is_some());
    assert!(bridge.route_controls.contains_key(&legacy));

    let first = route("surface-cli");
    let second = route("surface-ide");
    let first_id = route_id(&first);
    let second_id = route_id(&second);
    let mut successor_registry = SourceBackedProviderRegistry::new();
    successor_registry.register(first);
    successor_registry.register(second);
    prepare_automatic_route_splits(
        &mut successor_registry,
        &BTreeSet::from([legacy.clone()]),
        &bridge.route_controls,
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    let successor = SourceBackedRefreshExecutor::new(successor_registry, WriterOptions::default())
        .with_base_route_controls(bridge.route_controls)
        .refresh(&index_root, |_| Ok(()))
        .unwrap();
    assert!(successor.commit.manifest().source_route(&legacy).is_none());
    assert!(successor
        .commit
        .manifest()
        .source_route(&first_id)
        .is_some());
    assert!(successor
        .commit
        .manifest()
        .source_route(&second_id)
        .is_some());
    assert!(!successor.route_controls.contains_key(&legacy));
}

#[test]
fn failed_witnessed_cohort_keeps_the_bridge_generation_active() {
    let temp = tempfile::tempdir().unwrap();
    let index_root = temp.path().join("index");
    let current = route("surface-cli");
    let legacy = legacy_automatic_source_backed_route_identity(&current.metadata.source).unwrap();
    let mut released = current.clone();
    released.metadata.route_identity = Some(legacy.clone());
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(released);
    refresh_source_backed_generation(&index_root, &initial_registry, WriterOptions::default())
        .unwrap();

    let mut bridge_registry = SourceBackedProviderRegistry::new();
    bridge_registry.register(route("surface-cli"));
    bridge_registry.register(route("surface-ide"));
    prepare_automatic_route_splits(
        &mut bridge_registry,
        &BTreeSet::from([legacy.clone()]),
        &BTreeMap::new(),
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    let bridge = SourceBackedRefreshExecutor::new(bridge_registry, WriterOptions::default())
        .refresh(&index_root, |_| Ok(()))
        .unwrap();

    let mut failed_registry = SourceBackedProviderRegistry::new();
    failed_registry.register(route("surface-cli"));
    failed_registry.register(fail_before_scan(route("surface-ide")));
    prepare_automatic_route_splits(
        &mut failed_registry,
        &BTreeSet::from([legacy.clone()]),
        &bridge.route_controls,
        &SourceBackedRefreshScope::All,
        SourceBackedReconciliationDemand::Exhaustive,
    )
    .unwrap();
    assert!(
        SourceBackedRefreshExecutor::new(failed_registry, WriterOptions::default())
            .with_base_route_controls(bridge.route_controls)
            .refresh(&index_root, |_| Ok(()))
            .is_err()
    );
    let retained = ctx_history_index::VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(retained.generation_id(), bridge.commit.generation_id);
    assert!(retained.manifest().source_route(&legacy).is_some());
}
