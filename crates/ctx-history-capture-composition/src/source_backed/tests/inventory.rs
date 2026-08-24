use super::*;
use ctx_history_capture_model::ProviderRootDefinition;
use std::str::FromStr;

#[test]
fn only_one_configured_root_per_provider_can_own_the_released_namespace() {
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
        })
        .collect::<Vec<_>>();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(roots.clone());
    let report = DiscoveryReport {
        sources: roots
            .iter()
            .map(|root| {
                crate::provider_source_for_path(CaptureProvider::Claude, root.path.join("projects"))
            })
            .collect(),
        issues: Vec::new(),
    };
    let retained = BTreeMap::from([
        ("first".to_owned(), ProviderRootSourceIdentity::Released),
        ("second".to_owned(), ProviderRootSourceIdentity::Released),
    ]);

    let build = build_automatic_source_backed_registry_from_report_with_probes_and_root_identities(
        &crate::test_provider_probes(),
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
fn configured_claude_roots_register_as_independent_routes_and_aliases() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let personal = temp.path().join("claude-personal");
    let work = temp.path().join("claude-work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(personal.join("projects")).unwrap();
    let roots = vec![
        ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: personal.clone(),
            group: Some("personal".to_owned()),
        },
        ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: work.clone(),
            group: Some("work".to_owned()),
        },
    ];
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(roots.clone());
    let sources = vec![
        crate::provider_source_for_path(CaptureProvider::Claude, personal.join("projects")),
        crate::provider_source_for_path(CaptureProvider::Claude, work.join("projects")),
    ];

    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        sources,
        Vec::new(),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    assert_eq!(build.registry.routes().len(), 2);
    assert_eq!(build.executable_route_count(), 1);
    let route_ids = build
        .registry
        .routes()
        .map(|route| route.route_identity.clone().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(route_ids.len(), 2);
    let watch_catalog = build.registry.watch_catalog();
    assert_eq!(watch_catalog.route_ids().len(), 2);
    assert!(watch_catalog
        .target_paths()
        .any(|path| path == personal.join("projects")));
    assert!(watch_catalog
        .target_paths()
        .any(|path| path == work.join("projects")));
    let (automatic, digest, applied) = build.registry.applied_provider_roots().unwrap();
    assert!(*automatic);
    assert_eq!(digest, &provider_source_config_digest(true, &roots));
    assert_eq!(
        applied
            .iter()
            .map(|root| (root.definition().id.as_str(), root.routes().len()))
            .collect::<Vec<_>>(),
        vec![("personal", 1), ("work", 1)]
    );
}

#[test]
fn nested_claude_homes_keep_exact_root_alias_membership() {
    let temp = tempdir().unwrap();
    let outer = temp.path().join("claude-outer");
    let inner = outer.join("projects/claude-inner");
    let outer_projects = outer.join("projects");
    let inner_projects = inner.join("projects");
    fs::create_dir_all(&inner_projects).unwrap();
    let definitions = vec![
        ProviderRootDefinition {
            id: "outer".to_owned(),
            provider: CaptureProvider::Claude,
            path: outer,
            group: None,
        },
        ProviderRootDefinition {
            id: "inner".to_owned(),
            provider: CaptureProvider::Claude,
            path: inner,
            group: None,
        },
    ];
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(definitions);
    let sources = vec![
        crate::provider_source_for_path(CaptureProvider::Claude, outer_projects.clone()),
        crate::provider_source_for_path(CaptureProvider::Claude, inner_projects.clone()),
    ];
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("data"),
        sources,
        Vec::new(),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    let route_for = |path: &Path| {
        build
            .registry
            .routes()
            .find(|route| route.source.path == path)
            .and_then(|route| route.route_identity.clone())
            .unwrap()
    };
    let (_, _, roots) = build.registry.applied_provider_roots().unwrap();
    let owned = |id: &str| {
        roots
            .iter()
            .find(|root| root.definition().id == id)
            .unwrap()
            .routes()
            .to_vec()
    };
    assert_eq!(owned("outer"), [route_for(&outer_projects)]);
    assert_eq!(owned("inner"), [route_for(&inner_projects)]);
}

#[test]
fn nested_codex_homes_keep_exact_root_alias_membership() {
    let temp = tempdir().unwrap();
    let outer = temp.path().join("codex-outer");
    let inner = outer.join("sessions/codex-inner");
    let outer_sessions = outer.join("sessions");
    let inner_sessions = inner.join("sessions");
    fs::create_dir_all(&inner_sessions).unwrap();
    let definitions = vec![
        ProviderRootDefinition {
            id: "outer".to_owned(),
            provider: CaptureProvider::Codex,
            path: outer,
            group: None,
        },
        ProviderRootDefinition {
            id: "inner".to_owned(),
            provider: CaptureProvider::Codex,
            path: inner,
            group: None,
        },
    ];
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(definitions);
    let sources = vec![
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            outer_sessions.clone(),
        ),
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            inner_sessions.clone(),
        ),
    ];
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("data"),
        sources,
        Vec::new(),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    let route_for = |path: &Path| {
        build
            .registry
            .routes()
            .find(|route| route.source.path == path)
            .and_then(|route| route.route_identity.clone())
            .unwrap()
    };
    let (_, _, roots) = build.registry.applied_provider_roots().unwrap();
    let owned = |id: &str| {
        roots
            .iter()
            .find(|root| root.definition().id == id)
            .unwrap()
            .routes()
            .to_vec()
    };
    assert_eq!(owned("outer"), [route_for(&outer_sessions)]);
    assert_eq!(owned("inner"), [route_for(&inner_sessions)]);
}

#[test]
fn configured_codex_root_keeps_its_route_alias_for_a_present_session_tree() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = temp.path().join("codex-personal");
    let sessions = root.join("sessions");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let definition = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Codex,
        path: root,
        group: Some("personal".to_owned()),
    };
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![definition.clone()]);
    let source = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        sessions,
    );

    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        vec![source],
        Vec::new(),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    let (_, _, roots) = build.registry.applied_provider_roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].definition(), &definition);
    assert_eq!(roots[0].routes().len(), 1);
}

#[test]
fn naming_the_released_codex_home_preserves_the_compound_session_route() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = temp.path().join("codex-released");
    let sessions = root.join("sessions");
    let archived = root.join("archived_sessions");
    let history = root.join("history.jsonl");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();
    fs::write(&history, b"").unwrap();
    let definition = ProviderRootDefinition {
        id: "released".to_owned(),
        provider: CaptureProvider::Codex,
        path: root.clone(),
        group: Some("released".to_owned()),
    };
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("CODEX_HOME", &root)
    .with_configured_provider_roots(vec![definition.clone()]);
    let session_source = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        sessions,
    );
    let archived_source = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        archived,
    );
    let history_source = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_history_jsonl",
        ProviderImportSupport::Native,
        history,
    );
    let expected_session_route = automatic_source_backed_route_identity(&session_source).unwrap();

    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        vec![session_source, archived_source, history_source],
        Vec::new(),
    );

    assert!(build.issues.is_empty(), "{:?}", build.issues);
    assert_eq!(
        build
            .registry
            .watch_catalog()
            .catalog_coverage_route_registration_sources(&expected_session_route)
            .unwrap()
            .len(),
        2
    );
    let (_, _, roots) = build.registry.applied_provider_roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(
        roots[0].source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(roots[0].routes().len(), 2);
}

#[test]
fn provider_inventory_covers_supported_automatic_routes() {
    let unsupported = LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .filter(|route| route.unsupported_reason.is_some())
        .collect::<Vec<_>>();
    assert!(unsupported.is_empty());
    let providers_with_automatic_routes = LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .filter(|route| route.automatic && route.unsupported_reason.is_none())
        .map(|route| route.provider)
        .collect::<HashSet<_>>();
    let importable_providers = crate::provider_source_specs()
        .iter()
        .filter(|spec| spec.import_support.is_importable())
        .map(|spec| spec.provider)
        .collect::<HashSet<_>>();
    assert_eq!(providers_with_automatic_routes, importable_providers);
    let mut formats = HashSet::new();
    for route in LANDED_SOURCE_BACKED_ROUTES {
        assert!(
            formats.insert((route.provider, route.source_format)),
            "{} {} is registered more than once",
            route.provider.as_str(),
            route.source_format
        );
        assert!(!route.source_format.is_empty());
        assert!(!route.certified_source_format.is_empty());
        assert!(route.unsupported_reason.is_none());
    }

    for spec in crate::provider_source_specs() {
        let routes = LANDED_SOURCE_BACKED_ROUTES
            .iter()
            .filter(|route| route.provider == spec.provider)
            .collect::<Vec<_>>();
        assert!(
            !routes.is_empty(),
            "{} must have at least one central source-backed format route",
            spec.provider.as_str()
        );
        assert!(
            source_backed_route_constructor(spec.provider).is_some(),
            "{} must have a mechanical driver constructor",
            spec.provider.as_str()
        );
        assert_eq!(
            providers_with_automatic_routes.contains(&spec.provider),
            spec.import_support.is_importable(),
            "{} support classification disagrees with its landed route",
            spec.provider.as_str()
        );
        for location in spec.default_locations {
            let matching = routes
                .iter()
                .filter(|route| route.source_format == location.source_format)
                .collect::<Vec<_>>();
            assert_eq!(
                matching.len(),
                1,
                "{} default format {} must have exactly one central format route",
                spec.provider.as_str(),
                location.source_format
            );
            assert!(
                matching[0].automatic,
                "{} default format {} is not automatic",
                spec.provider.as_str(),
                location.source_format
            );
        }
    }

    let root_leaf_variants = [
        (
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::Codex,
            "codex_history_jsonl",
            "codex_history_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::Codex,
            "codex_session_jsonl",
            "codex_session_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::GrokBuild,
            "grok_build_session_updates_jsonl_tree",
            "grok_build_session_updates_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::GrokBuild,
            "grok_build_session_updates_jsonl",
            "grok_build_session_updates_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::Cursor,
            "cursor_agent_transcript_jsonl_tree",
            "cursor_agent_transcript_jsonl_tree",
            true,
            true,
        ),
        (
            CaptureProvider::Cursor,
            "cursor_agent_transcript_jsonl",
            "cursor_agent_transcript_jsonl_tree",
            false,
            true,
        ),
        (
            CaptureProvider::QwenCode,
            "qwen_code_chat_jsonl_tree",
            "qwen_code_chat_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::QwenCode,
            "qwen_code_chat_jsonl",
            "qwen_code_chat_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::KimiCodeCli,
            "kimi_code_cli_wire_jsonl_tree",
            "kimi_code_cli_wire_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::KimiCodeCli,
            "kimi_code_cli_wire_jsonl",
            "kimi_code_cli_wire_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::MistralVibe,
            "mistral_vibe_session_jsonl_tree",
            "mistral_vibe_session_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::MistralVibe,
            "mistral_vibe_session_jsonl",
            "mistral_vibe_session_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::Mux,
            "mux_session_jsonl_tree",
            "mux_session_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::Mux,
            "mux_session_jsonl",
            "mux_session_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::Qoder,
            "qoder_transcript_jsonl_tree",
            "qoder_transcript_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::Qoder,
            "qoder_transcript_jsonl",
            "qoder_transcript_jsonl",
            false,
            true,
        ),
        (
            CaptureProvider::Junie,
            "junie_session_events_jsonl",
            "junie_session_events_jsonl_tree",
            false,
            true,
        ),
    ];
    for (provider, selected, certified, automatic, explicit) in root_leaf_variants {
        let route = landed_format_route(provider, selected).unwrap();
        assert_eq!(route.certified_source_format, certified);
        assert_eq!(route.automatic, automatic);
        assert_eq!(route.explicit_manual, explicit);
    }
}

#[test]
fn shelley_keeps_exact_cwd_automatic_authority_and_admits_explicit_paths() {
    let route = LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .find(|route| {
            route.provider == CaptureProvider::Shelley && route.source_format == "shelley_sqlite"
        })
        .expect("Shelley route");

    assert!(route.automatic);
    assert!(route.explicit_manual);
    assert_eq!(
        route.selector_authority,
        SourceBackedSelectorAuthority::ExactCwd
    );
    assert_eq!(route.constructor, SourceBackedRouteConstructor::ExactCwd);
}

#[test]
fn hermes_route_is_executable_for_automatic_registration() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let database = home.join(".hermes/state.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(&database, b"registration must not open provider content").unwrap();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let source = crate::provider_source_for_path(CaptureProvider::Hermes, database);
    let route_identity = automatic_source_backed_route_identity(&source).unwrap();

    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        vec![source.clone()],
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 1);
    assert_eq!(build.unsupported_route_count(), 0);
    assert!(build.issues.is_empty());
    assert_eq!(
        automatic_source_backed_route_identity(&source).unwrap(),
        route_identity
    );

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route_with_data_root(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        &temp.path().join("ctx-data"),
    )
    .unwrap();
    assert_eq!(registry.executable_route_count(), 1);
}

#[test]
fn public_supported_formats_have_one_exact_hydratable_landed_route() {
    let matrix = crate::test_support_paths::provider_support_matrix();

    for provider in matrix["providers"].as_array().unwrap() {
        let capture_provider =
            CaptureProvider::from_str(provider["capture_provider"].as_str().unwrap()).unwrap();
        for path in provider["implemented_paths"].as_array().unwrap() {
            let source_format = path["source_format"].as_str().unwrap();
            let routes = LANDED_SOURCE_BACKED_ROUTES
                .iter()
                .filter(|route| {
                    route.provider == capture_provider && route.source_format == source_format
                })
                .collect::<Vec<_>>();
            assert_eq!(
                routes.len(),
                1,
                "{} {} must have exactly one landed source-backed route",
                capture_provider.as_str(),
                source_format
            );
            assert!(
                routes[0].unsupported_reason.is_none(),
                "{} {} is publicly supported but its landed route is unsupported",
                capture_provider.as_str(),
                source_format
            );
        }
    }
}

#[test]
fn automatic_builder_executes_typed_warp_crush_and_lingma_authorities() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("work");
    let state = temp.path().join("state");
    let config = temp.path().join("config");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();
    let warp = state.join("warp-terminal/warp.sqlite");
    std::fs::create_dir_all(warp.parent().unwrap()).unwrap();
    std::fs::write(&warp, b"sqlite").unwrap();
    let crush = cwd.join(".crush/crush.db");
    std::fs::create_dir_all(crush.parent().unwrap()).unwrap();
    std::fs::write(&crush, b"sqlite").unwrap();
    let lingma = home.join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    std::fs::create_dir_all(lingma.parent().unwrap()).unwrap();
    rusqlite::Connection::open(&lingma)
        .unwrap()
        .execute_batch(
            "create table chat_record (\
                    session_id text, request_id text, chat_prompt text, summary text, \
                    error_result text, gmt_create integer, extra text);",
        )
        .unwrap();
    let codex_history = home.join(".codex/history.jsonl");
    std::fs::create_dir_all(codex_history.parent().unwrap()).unwrap();
    std::fs::write(
        &codex_history,
        b"{\"session_id\":\"session-a\",\"ts\":1785139200,\"text\":\"automatic prompt\"}\n",
    )
    .unwrap();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs {
            config: Some(config),
            state: Some(state),
            ..crate::DiscoveryPlatformDirs::default()
        },
    );
    let mut missing_mux = fixture_provider_source(
        CaptureProvider::Mux,
        "mux_session_jsonl_tree",
        ProviderImportSupport::Native,
    );
    missing_mux.exists = false;
    missing_mux.status = ProviderSourceStatus::Missing;
    let sources = vec![
        fixture_provider_source(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            ProviderImportSupport::Native,
        ),
        fixture_provider_source_at(
            CaptureProvider::Warp,
            "warp_sqlite",
            ProviderImportSupport::Native,
            &warp,
        ),
        fixture_provider_source_at(
            CaptureProvider::Goose,
            "goose_sessions_sqlite",
            ProviderImportSupport::Native,
            home.join(".local/share/goose/sessions/sessions.db"),
        ),
        fixture_provider_source(
            CaptureProvider::AstrBot,
            "astrbot_data_v4_sqlite",
            ProviderImportSupport::Native,
        ),
        fixture_provider_source_at(
            CaptureProvider::AstrBot,
            "astrbot_data_v4_sqlite",
            ProviderImportSupport::Native,
            "/home/test/.astrbot_launcher/instances/one/data/data_v4.db",
        ),
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_history_jsonl",
            ProviderImportSupport::Native,
            &codex_history,
        ),
        fixture_provider_source_at(
            CaptureProvider::Crush,
            "crush_sqlite",
            ProviderImportSupport::Native,
            &crush,
        ),
        fixture_provider_source_at(
            CaptureProvider::Lingma,
            "lingma_sqlite",
            ProviderImportSupport::Native,
            &lingma,
        ),
        fixture_provider_source(
            CaptureProvider::Unknown,
            "unknown_detected_format",
            ProviderImportSupport::Unsupported,
        ),
        missing_mux,
    ];

    let data_root = temp.path().join("ctx-data");
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        sources,
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 7);
    assert_eq!(build.unsupported_route_count(), 1);
    assert_eq!(build.issues.len(), 1);
    for provider in [
        CaptureProvider::Codex,
        CaptureProvider::Warp,
        CaptureProvider::Crush,
        CaptureProvider::Lingma,
    ] {
        assert!(build.registry.routes().any(|route| {
            route.source.provider == provider
                && route.selection == Some(SourceBackedRouteSelection::Automatic)
                && route.unsupported_reason.is_none()
        }));
    }
    assert!(!build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable { source, .. }
            if source.provider == CaptureProvider::Codex
    )));
    assert!(!build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable {
            source,
            reason: SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. },
        } if matches!(
            source.provider,
            CaptureProvider::Warp | CaptureProvider::Crush | CaptureProvider::Lingma
        )
    )));
    assert!(build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable {
            source,
            reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. },
        } if source.provider == CaptureProvider::Unknown
            && source.source_format == "unknown_detected_format"
    )));
}

#[test]
fn automatic_registry_keeps_present_empty_roots_executable_and_other_statuses_typed() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );

    let mut empty = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        &sessions,
    );
    empty.status = ProviderSourceStatus::Empty;
    empty.unsupported_reason = Some("path exists but has no sessions");

    let data_root = temp.path().join("ctx-data");
    let empty_build = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        vec![empty],
        Vec::new(),
    );
    assert_eq!(empty_build.executable_route_count(), 1);
    assert_eq!(empty_build.unsupported_route_count(), 0);
    assert!(empty_build.issues.is_empty());
    let empty_route = empty_build
        .registry
        .routes()
        .find(|route| route.source.path == sessions)
        .expect("present empty Codex root must retain its landed route");
    assert_eq!(empty_route.source.status, ProviderSourceStatus::Empty);
    assert_eq!(empty_route.unsupported_reason, None);

    fs::rename(&sessions, home.join(".codex/sessions-renamed")).unwrap();

    let mut missing = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        &sessions,
    );
    missing.exists = false;
    missing.status = ProviderSourceStatus::Missing;

    let mut unknown = fixture_provider_source_at(
        CaptureProvider::Codex,
        "codex_history_jsonl",
        ProviderImportSupport::Native,
        home.join(".codex/history.jsonl"),
    );
    unknown.status = ProviderSourceStatus::Unknown;

    let unsupported = fixture_provider_source(
        CaptureProvider::Unknown,
        "unknown_detected_format",
        ProviderImportSupport::Unsupported,
    );
    let unavailable_build = build_automatic_source_backed_registry_from_parts(
        &context,
        &data_root,
        vec![missing, unknown, unsupported],
        Vec::new(),
    );

    assert_eq!(unavailable_build.executable_route_count(), 0);
    assert_eq!(unavailable_build.unsupported_route_count(), 2);
    assert!(unavailable_build.registry.routes().any(|route| {
        route.source.status == ProviderSourceStatus::Missing
            && !route.source.exists
            && route.source.path == sessions
            && route.selection == Some(SourceBackedRouteSelection::Automatic)
            && route.unsupported_reason.is_none()
    }));
    assert!(!unavailable_build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable { source, .. }
            if source.status == ProviderSourceStatus::Missing
    )));
    assert!(unavailable_build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable {
            reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Unknown
            ),
            ..
        }
    )));
    assert!(unavailable_build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable {
            source,
            reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. },
        } if source.status == ProviderSourceStatus::Unsupported
    )));
}
