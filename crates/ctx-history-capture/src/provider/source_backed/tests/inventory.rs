use super::*;
use std::str::FromStr;

#[test]
fn importable_provider_inventory_covers_default_and_explicit_formats() {
    assert_eq!(LANDED_SOURCE_BACKED_ROUTES.len(), 52);
    assert_eq!(
        LANDED_SOURCE_BACKED_ROUTES
            .iter()
            .filter(|route| route.automatic)
            .count(),
        41
    );
    assert_eq!(
        LANDED_SOURCE_BACKED_ROUTES
            .iter()
            .filter(|route| route.automatic && route.unsupported_reason.is_some())
            .count(),
        0
    );
    assert_eq!(
        LANDED_SOURCE_BACKED_ROUTES
            .iter()
            .filter(|route| route.automatic && route.unsupported_reason.is_none())
            .count(),
        41
    );
    let unsupported = LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .filter(|route| route.unsupported_reason.is_some())
        .collect::<Vec<_>>();
    assert!(unsupported.is_empty());
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
        match route.exact_hydration {
            SourceBackedHydrationSupport::Full => {
                assert!(route.hydration_limitation.is_none());
                assert!(route.unsupported_reason.is_none());
            }
            SourceBackedHydrationSupport::Unsupported => {
                assert!(route.unsupported_reason.is_some());
            }
        }
    }

    for spec in crate::provider_source_specs()
        .iter()
        .filter(|spec| spec.import_support.is_importable())
    {
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
            if spec.import_support == ProviderImportSupport::Native {
                assert!(
                    matching[0].automatic,
                    "{} default format {} is not automatic",
                    spec.provider.as_str(),
                    location.source_format
                );
            }
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
            CaptureProvider::Windsurf,
            "windsurf_cascade_hook_transcript_jsonl_tree",
            "windsurf_cascade_hook_transcript_jsonl",
            true,
            true,
        ),
        (
            CaptureProvider::Windsurf,
            "windsurf_cascade_hook_transcript_jsonl",
            "windsurf_cascade_hook_transcript_jsonl",
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
fn public_supported_formats_have_one_exact_hydratable_landed_route() {
    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../docs/provider-support-matrix.json"
    ))
    .unwrap();

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
            assert_eq!(
                routes[0].exact_hydration,
                SourceBackedHydrationSupport::Full,
                "{} {} must hydrate exact provider-owned content",
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
    assert_eq!(build.issues.len(), 2);
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
    assert_eq!(unavailable_build.unsupported_route_count(), 1);
    assert!(unavailable_build.issues.iter().any(|issue| matches!(
        issue,
        SourceBackedAutomaticRegistryIssue::Unavailable {
            source,
            reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Missing
            ),
        } if !source.exists && source.path == sessions
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
