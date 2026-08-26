use super::*;

#[test]
fn unavailable_openclaw_root_restores_all_prior_agent_routes() {
    let prior_definition = ProviderRootDefinition {
        id: "openclaw-state".to_owned(),
        provider: CaptureProvider::OpenClaw,
        path: PathBuf::from("/fixture/old-openclaw-state"),
        group: Some("old".to_owned()),
        kind: None,
    };
    let current_definition = ProviderRootDefinition {
        path: PathBuf::from("/fixture/moved-openclaw-state"),
        group: Some("moved".to_owned()),
        ..prior_definition.clone()
    };
    let alpha = SourceRouteIdentity::from_sha256("a".repeat(64)).unwrap();
    let beta = SourceRouteIdentity::from_sha256("b".repeat(64)).unwrap();
    let prior = AppliedProviderRoot::with_source_identity(
        prior_definition,
        ProviderRootSourceIdentity::Released,
        vec![alpha.clone(), beta.clone()],
    )
    .unwrap()
    .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
        alpha.clone(),
        vec!["11".repeat(32)],
    )
    .unwrap()])
    .unwrap();
    let current = AppliedProviderRoot::with_source_identity(
        current_definition.clone(),
        ProviderRootSourceIdentity::Released,
        Vec::new(),
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry
        .set_applied_provider_roots(true, "fixture".to_owned(), vec![current])
        .unwrap();

    registry
        .retain_unavailable_provider_root_routes(std::slice::from_ref(&prior))
        .unwrap();

    let restored = &registry.applied_provider_roots().unwrap().2[0];
    assert_eq!(restored.definition(), &current_definition);
    assert_eq!(
        restored.source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(restored.routes(), &[alpha, beta]);
    assert_eq!(
        restored.exact_source_memberships(),
        prior.exact_source_memberships()
    );
}

#[test]
fn unavailable_root_retention_rejects_duplicate_stable_ids() {
    let definition = |provider| ProviderRootDefinition {
        id: "duplicate".to_owned(),
        provider,
        path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
        group: None,
        kind: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    registry
        .set_applied_provider_roots(
            false,
            "fixture".to_owned(),
            [CaptureProvider::OpenClaw, CaptureProvider::Hermes]
                .map(|provider| AppliedProviderRoot::new(definition(provider), Vec::new()).unwrap())
                .to_vec(),
        )
        .unwrap();

    let error = registry
        .retain_unavailable_provider_root_routes(&[])
        .unwrap_err();
    assert!(error.to_string().contains("not unique"), "{error}");
}

#[test]
fn unavailable_root_retention_requires_matching_provider() {
    let definition = |provider| ProviderRootDefinition {
        id: "provider-stable-id".to_owned(),
        provider,
        path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
        group: None,
        kind: None,
    };
    let prior_route = SourceRouteIdentity::from_sha256("4c".repeat(32)).unwrap();
    let prior = AppliedProviderRoot::with_source_identity(
        definition(CaptureProvider::OpenClaw),
        ProviderRootSourceIdentity::Released,
        vec![prior_route],
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry
        .set_applied_provider_roots(
            false,
            "fixture".to_owned(),
            vec![AppliedProviderRoot::with_source_identity(
                definition(CaptureProvider::Hermes),
                ProviderRootSourceIdentity::Released,
                Vec::new(),
            )
            .unwrap()],
        )
        .unwrap();

    registry
        .retain_unavailable_provider_root_routes(&[prior])
        .unwrap();

    assert!(registry.applied_provider_roots().unwrap().2[0]
        .routes()
        .is_empty());
    assert!(registry.applied_provider_roots().unwrap().2[0]
        .exact_source_memberships()
        .is_empty());
}

fn route_identity_source(provider: CaptureProvider, source_format: &'static str) -> ProviderSource {
    ProviderSource {
        provider,
        path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
        exists: true,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: crate::ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

#[test]
fn configured_claude_and_codex_route_identities_preserve_released_goldens() {
    let golden = |provider: CaptureProvider,
                  selected_format: &'static str,
                  certified_format: &str,
                  role: &'static str,
                  expected: &str| {
        let root = ProviderRootDefinition {
            id: "personal".to_owned(),
            provider,
            path: PathBuf::from("/machine-specific-provider-home"),
            group: None,
            kind: None,
        };
        let lineage = ProviderRootSourceIdentity::NamedV1.lineage(&root).unwrap();
        let identity = provider_root_source_backed_route_identity(
            &route_identity_source(provider, selected_format),
            certified_format,
            lineage,
            &ProviderRouteRole::from_static(role),
        )
        .unwrap();

        assert_eq!(identity.as_str(), expected);
    };

    golden(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        "claude_projects_jsonl_tree",
        "claude-projects",
        "3ceb120cc19d687091cb951a28b9d173be1b1a7594e2f7822c0729573ed3a7c6",
    );
    golden(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        "codex_session_jsonl",
        "codex-sessions",
        "28482f00a92e00b796afb7c00fbce791e1f1765f4fcfc1f8e26cf831c1feda73",
    );
    golden(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        "codex_session_jsonl",
        "codex-archived-sessions",
        "39ef7a5d71542cac12636347be66fc65e9cccd2df424043a33f8af5c9940a3f8",
    );
    golden(
        CaptureProvider::Codex,
        "codex_history_jsonl",
        "codex_history_jsonl",
        "codex-prompt-history",
        "0ce68ccb4a181a282d068a706b2f6e2d36d551b54206a5c72497e2f5be89e15c",
    );
}

#[test]
fn automatic_route_role_is_opt_in_and_legacy_identity_is_unchanged() {
    let mut source = route_identity_source(CaptureProvider::Codex, "codex_session_jsonl_tree");
    let unroled = automatic_source_backed_route_identity(&source).unwrap();
    assert_eq!(
        unroled.as_str(),
        "d81b73c4985c02368ce9652682df1200a293f4c124ff94d1e57a079c9963364b"
    );

    source.route_provenance = ctx_history_capture_model::ProviderSourceRouteProvenance::Automatic {
        route_role: ProviderRouteRole::from_static("same-format-slot-a"),
    };
    let role_specific = automatic_source_backed_route_identity(&source).unwrap();
    assert_eq!(
        role_specific.as_str(),
        "3323b51108b227a05b476ce100f19ec57d3a34886e19ddfc16944b3624f6fda5"
    );
    assert_ne!(unroled, role_specific);
}

#[test]
fn route_identity_validation_uses_the_canonical_index_conversion() {
    let error = index_source_route_identity(SourceRouteIdentity::from_sha256("AB".repeat(32)))
        .map_err(SourceBackedCoordinatorError::from)
        .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::InvalidSourceRouteIdentity)
    ));
}

#[test]
fn unavailable_configured_root_retention_requires_matching_kind() {
    use ctx_history_capture_model::ProviderRootKind;

    let definition = |kind| ProviderRootDefinition {
        id: "openhands".to_owned(),
        provider: CaptureProvider::OpenHands,
        path: PathBuf::from("/configured/openhands"),
        group: None,
        kind: Some(kind),
    };
    let legacy = definition(ProviderRootKind::OpenHandsLegacyPersistence);
    let current = definition(ProviderRootKind::OpenHandsCurrentConversations);
    let route = SourceRouteIdentity::from_sha256("5a".repeat(32)).unwrap();
    let retained = AppliedProviderRoot::new(legacy, vec![route.clone()])
        .unwrap()
        .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
            route,
            vec!["22".repeat(32)],
        )
        .unwrap()])
        .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry
        .set_applied_provider_roots(
            false,
            "digest".to_owned(),
            vec![AppliedProviderRoot::new(current, Vec::new()).unwrap()],
        )
        .unwrap();

    registry
        .retain_unavailable_provider_root_routes(&[retained])
        .unwrap();

    assert!(registry.applied_provider_roots().unwrap().2[0]
        .routes()
        .is_empty());
    assert!(registry.applied_provider_roots().unwrap().2[0]
        .exact_source_memberships()
        .is_empty());
}
