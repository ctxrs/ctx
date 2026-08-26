use super::*;
use ctx_history_capture_model::ProviderRootSourceIdentity;
use ctx_history_index::AppliedProviderRootSourceMembership;
use std::collections::BTreeMap;

fn configured_source(
    mut source: ProviderSource,
    root: &ProviderRootDefinition,
    route_role: &'static str,
) -> ProviderSource {
    source.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: root.id.clone(),
        root_path: root.path.clone(),
        route_role: ProviderRouteRole::from_static(route_role),
        automatic_route_role: None,
    };
    source
}

#[test]
fn configured_compound_roots_register_from_arbitrary_paths_without_automatic_authority() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let cases = [
        (
            CaptureProvider::Crush,
            "crush_sqlite",
            "crush-project-database",
        ),
        (
            CaptureProvider::Goose,
            "goose_sessions_sqlite",
            "goose-sessions-database",
        ),
        (
            CaptureProvider::AstrBot,
            "astrbot_data_v4_sqlite",
            "astrbot-instance-database",
        ),
        (
            CaptureProvider::Lingma,
            "lingma_sqlite",
            "lingma-client-profile-database",
        ),
        (
            CaptureProvider::Warp,
            "warp_sqlite",
            "warp-surface-database",
        ),
    ];
    let roots = cases
        .iter()
        .map(|(provider, _, _)| ProviderRootDefinition {
            id: format!("configured-{}", provider.as_str()),
            provider: *provider,
            path: temp.path().join(format!("arbitrary-{}", provider.as_str())),
            group: None,
            kind: None,
        })
        .collect::<Vec<_>>();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(roots.clone());
    let sources = cases
        .iter()
        .zip(&roots)
        .map(|((provider, format, role), root)| {
            configured_source(
                fixture_provider_source_at(
                    *provider,
                    format,
                    ProviderImportSupport::Native,
                    root.path.join("not-selected-by-automatic.sqlite"),
                ),
                root,
                role,
            )
        })
        .collect();
    let build = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &test_provider_probes(),
        &context,
        &temp.path().join("ctx-data"),
        DiscoveryReport {
            sources,
            issues: Vec::new(),
        },
        &BTreeMap::new(),
    );

    assert!(build.issues.is_empty(), "{:?}", build.issues);
    assert_eq!(build.executable_route_count(), cases.len());
    assert!(build
        .registry
        .applied_provider_roots()
        .unwrap()
        .2
        .iter()
        .all(|root| root.source_identity() == ProviderRootSourceIdentity::NamedV1));
}

#[test]
fn configured_exact_roots_register_with_named_identity_without_automatic_authority() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let cases = [
        (
            CaptureProvider::KiroCli,
            "kiro_cli_sqlite",
            "kiro-cli-database",
        ),
        (
            CaptureProvider::Antigravity,
            "antigravity_cli_transcript_jsonl_tree",
            "antigravity-brain",
        ),
        (
            CaptureProvider::FactoryAiDroid,
            "factory_ai_droid_sessions_jsonl",
            "factory-droid-sessions",
        ),
        (
            CaptureProvider::Auggie,
            "auggie_session_json",
            "auggie-sessions",
        ),
        (
            CaptureProvider::Firebender,
            "firebender_chat_history_sqlite",
            "firebender-chat-history-database",
        ),
        (
            CaptureProvider::DeepAgents,
            "deepagents_sessions_sqlite",
            "deepagents-sessions-database",
        ),
        (
            CaptureProvider::Qoder,
            "qoder_transcript_jsonl_tree",
            "qoder-projects",
        ),
    ];
    let roots = cases
        .iter()
        .map(|(provider, _, _)| ProviderRootDefinition {
            id: format!("configured-{}", provider.as_str()),
            provider: *provider,
            path: temp.path().join(provider.as_str()),
            group: None,
            kind: None,
        })
        .collect::<Vec<_>>();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(roots.clone());
    let sources = cases
        .iter()
        .zip(&roots)
        .map(|((provider, format, role), root)| {
            configured_source(
                fixture_provider_source_at(
                    *provider,
                    format,
                    ProviderImportSupport::Native,
                    root.path.clone(),
                ),
                root,
                role,
            )
        })
        .collect();
    let build = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &test_provider_probes(),
        &context,
        &temp.path().join("ctx-data"),
        DiscoveryReport {
            sources,
            issues: Vec::new(),
        },
        &BTreeMap::new(),
    );

    assert!(build.issues.is_empty(), "{:?}", build.issues);
    assert_eq!(build.executable_route_count(), cases.len());
    assert!(build.registry.routes().all(|route| {
        route.source.route_provenance.configured_root().is_some() && route.route_identity.is_some()
    }));
    assert!(build
        .registry
        .applied_provider_roots()
        .unwrap()
        .2
        .iter()
        .all(|root| root.source_identity() == ProviderRootSourceIdentity::NamedV1));
}

#[test]
fn only_one_root_can_claim_the_same_released_automatic_route() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let first = temp.path().join("claude-first");
    let second = temp.path().join("claude-second");
    for path in [&home, &cwd, &first, &second] {
        fs::create_dir_all(path).unwrap();
    }
    let roots = [(&first, "first"), (&second, "second")]
        .into_iter()
        .map(|(path, id)| ProviderRootDefinition {
            id: id.to_owned(),
            provider: CaptureProvider::Claude,
            path: path.to_path_buf(),
            group: None,
            kind: None,
        })
        .collect::<Vec<_>>();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(roots.clone());
    let report = DiscoveryReport {
        sources: roots
            .iter()
            .map(|root| {
                configured_source(
                    provider_sources::provider_source_for_path(
                        CaptureProvider::Claude,
                        root.path.join("projects"),
                    ),
                    root,
                    "claude-projects",
                )
            })
            .collect(),
        issues: Vec::new(),
    };
    let retained = roots
        .iter()
        .map(|root| {
            (
                root.id.clone(),
                AppliedProviderRoot::with_source_identity(
                    root.clone(),
                    ProviderRootSourceIdentity::Released,
                    Vec::new(),
                )
                .unwrap()
                .retained_authority()
                .unwrap(),
            )
        })
        .collect();
    let build = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &test_provider_probes(),
        &context,
        &temp.path().join("ctx-data"),
        report,
        &retained,
    );
    let applied = &build.registry.applied_provider_roots().unwrap().2;

    assert_eq!(
        applied
            .iter()
            .filter(|root| root.source_identity() == ProviderRootSourceIdentity::Released)
            .count(),
        1
    );
    assert_eq!(
        applied[0].source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(
        applied[1].source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
}

#[test]
fn unavailable_static_child_restores_only_its_prior_exact_membership() {
    let definition = ProviderRootDefinition {
        id: "codex-work".to_owned(),
        provider: CaptureProvider::Codex,
        path: PathBuf::from("/fixture/codex-work"),
        group: Some("work".to_owned()),
        kind: None,
    };
    let sessions = SourceRouteIdentity::from_sha256("31".repeat(32)).unwrap();
    let history = SourceRouteIdentity::from_sha256("32".repeat(32)).unwrap();
    let history_source = "41".repeat(32);
    let prior =
        AppliedProviderRoot::new(definition.clone(), vec![sessions.clone(), history.clone()])
            .unwrap()
            .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
                history.clone(),
                vec![history_source.clone()],
            )
            .unwrap()])
            .unwrap();
    let current =
        AppliedProviderRoot::new(definition, vec![sessions.clone(), history.clone()]).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry
        .set_applied_provider_roots(false, "fixture".to_owned(), vec![current])
        .unwrap();

    registry
        .retain_unavailable_provider_root_routes(&[prior])
        .unwrap();

    let retained = &registry.applied_provider_roots().unwrap().2[0];
    assert_eq!(retained.routes(), &[sessions, history.clone()]);
    assert_eq!(
        retained.exact_source_tokens_for_route(&history),
        Some(std::slice::from_ref(&history_source))
    );
}

#[test]
fn removed_dynamic_child_is_not_restored_from_prior_membership() {
    let definition = ProviderRootDefinition {
        id: "openclaw-state".to_owned(),
        provider: CaptureProvider::OpenClaw,
        path: PathBuf::from("/fixture/openclaw-state"),
        group: None,
        kind: None,
    };
    let alpha = SourceRouteIdentity::from_sha256("51".repeat(32)).unwrap();
    let beta = SourceRouteIdentity::from_sha256("52".repeat(32)).unwrap();
    let prior = AppliedProviderRoot::new(definition.clone(), vec![alpha.clone(), beta.clone()])
        .unwrap()
        .with_exact_source_memberships(
            [(&alpha, "61"), (&beta, "62")]
                .into_iter()
                .map(|(route, token)| {
                    AppliedProviderRootSourceMembership::exact(
                        route.clone(),
                        vec![token.repeat(32)],
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap();
    let current = AppliedProviderRoot::new(definition, vec![alpha.clone()]).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry
        .set_applied_provider_roots(false, "fixture".to_owned(), vec![current])
        .unwrap();

    registry
        .retain_unavailable_provider_root_routes(&[prior])
        .unwrap();

    let retained = &registry.applied_provider_roots().unwrap().2[0];
    assert_eq!(retained.routes(), std::slice::from_ref(&alpha));
    assert!(retained.exact_source_tokens_for_route(&alpha).is_some());
    assert!(retained.exact_source_tokens_for_route(&beta).is_none());
}
