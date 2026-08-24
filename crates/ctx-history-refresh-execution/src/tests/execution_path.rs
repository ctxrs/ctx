//! Explicit executor-path coverage owned by physical refresh execution.

use super::*;
use ctx_history_capture::{SourceBackedRoute, SourceBackedRouteDriver};
use ctx_history_index::EventSearchFilters;
use rusqlite::Connection;

#[test]
fn requested_watch_observations_preserve_present_and_missing_routes() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("history.jsonl");
    std::fs::write(&source_path, b"one\n").unwrap();
    let source = provider_source_for_path(CaptureProvider::OpenCode, source_path.clone());
    let route = SourceBackedRoute::automatic(
        source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let present = route.metadata().route_identity.clone().unwrap();
    let missing = SourceRouteIdentity::from_sha256("fe".repeat(32)).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let catalog = registry.watch_catalog();

    let observations = source_backed_requested_route_observations(
        &catalog,
        &BTreeSet::from([present.clone(), missing.clone()]),
    );

    let admitted = AdmittedRefresh::from_exact_catalog_authority(
        BTreeSet::from([present.clone()]),
        StdDuration::ZERO,
        catalog.clone(),
    )
    .unwrap();
    assert_eq!(admitted.exact_routes(), &BTreeSet::from([present.clone()]));
    let error = AdmittedRefresh::from_exact_catalog_authority(
        BTreeSet::from([missing.clone()]),
        StdDuration::ZERO,
        catalog.clone(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("absent from catalog authority"));

    assert_eq!(observations.len(), 2);
    assert!(observations[&present].is_some());
    assert_eq!(observations[&missing], None);

    std::fs::write(source_path, b"one\ntwo\n").unwrap();
    let changed = source_backed_requested_route_observations(
        &catalog,
        &BTreeSet::from([present.clone(), missing.clone()]),
    );
    assert_ne!(changed[&present], observations[&present]);
    assert_eq!(changed[&missing], None);
    assert_eq!(
        changed.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([present, missing])
    );
}

#[test]
fn complete_catalog_execution_reuses_admission_and_preserves_progress_order() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(home.join(".forge")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let forge = home.join(".forge/.forge.db");
    let forge_writer = Connection::open(&forge).unwrap();
    forge_writer
        .pragma_update(None, "journal_mode", "wal")
        .unwrap();
    forge_writer
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    forge_writer
        .execute_batch("create table conversations (id text primary key);")
        .unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let forge_source = provider_source_for_path(CaptureProvider::ForgeCode, forge.clone());
    let forge_route = automatic_source_backed_route_identity(&forge_source).unwrap();
    let updates = std::sync::Mutex::new(Vec::new());
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        updates.lock().unwrap().push((
            update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
            update.completed_records,
            update.completed_bytes,
            update.providers,
            update.processed_sessions,
            update.processed_messages,
            update.processed_tool_calls,
            update.processed_bytes,
            update.elapsed_millis,
        ));
        Ok(())
    };
    let admitted = source_backed_admitted_discovery_from_report(
        &discovery,
        DiscoveryReport {
            sources: vec![forge_source],
            issues: Vec::new(),
        },
        StdDuration::from_millis(7),
        &data_root,
        AdmittedRefreshCoverage::CompleteCatalog,
        None,
        &TestPublishedState,
    )
    .unwrap()
    .with_execution_facts(BTreeMap::new())
    .unwrap();
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "all-provider-request",
        RefreshOperation::Refresh,
        None,
        admitted,
        &discovery,
        &TestPublishedState,
        &report_progress,
    );
    let mut capture_calls = 0;

    let publication = execute_capture_owned_refresh_with(
        execution,
        &discovery,
        |observed_discovery,
         observed_report,
         observed_discovery_duration,
         observed_request_id,
         observed_operation,
         observed_data_root,
         observed_index_root,
         observed_explicit_source_catalog,
         observed_scope,
         observed_physical_scope,
         observed_exact_catalog_members,
         _observed_published_state,
         progress| {
            capture_calls += 1;
            assert_eq!(observed_discovery.home(), discovery.home());
            assert_eq!(observed_discovery.cwd(), discovery.cwd());
            assert_eq!(observed_discovery.data_root(), Some(data_root.as_path()));
            assert!(observed_report.sources.iter().any(|source| {
                source.provider == CaptureProvider::ForgeCode
                    && source.path == forge
                    && source.status == ProviderSourceStatus::Available
            }));
            assert_eq!(observed_discovery_duration, StdDuration::from_millis(7));
            assert_eq!(observed_request_id, "all-provider-request");
            assert_eq!(observed_operation, RefreshOperation::Refresh);
            assert_eq!(observed_data_root, data_root);
            assert_eq!(observed_index_root, index_root);
            assert!(observed_explicit_source_catalog.is_none());
            assert_eq!(observed_scope, SourceBackedRefreshScope::All);
            assert_eq!(
                observed_physical_scope,
                SourceBackedRefreshScope::exact([forge_route.clone()])
            );
            assert!(!observed_exact_catalog_members);
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: ctx_history_capture::SourceBackedRefreshProgress {
                    phase: "discovering",
                    completed_sources: 0,
                    total_sources: 2,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    providers: vec![CaptureProvider::Codex, CaptureProvider::Claude],
                    elapsed: StdDuration::from_secs(1),
                    ..Default::default()
                },
                current_source_progress: None,
                exact_scan_progress: None,
            })?;
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: ctx_history_capture::SourceBackedRefreshProgress {
                    phase: "refreshing",
                    completed_sources: 1,
                    total_sources: 2,
                    current_source: Some("provider-wide-route".to_owned()),
                    completed_records: Some(11),
                    completed_bytes: Some(4_096),
                    providers: vec![CaptureProvider::Codex, CaptureProvider::Claude],
                    processed_sessions: 3,
                    processed_messages: 8,
                    processed_tool_calls: 3,
                    processed_bytes: 4_096,
                    elapsed: StdDuration::from_millis(2_500),
                    ..Default::default()
                },
                current_source_progress: None,
                exact_scan_progress: None,
            })?;
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: ctx_history_capture::SourceBackedRefreshProgress {
                    phase: "verifying",
                    completed_sources: 2,
                    total_sources: 2,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    providers: vec![CaptureProvider::Codex, CaptureProvider::Claude],
                    processed_sessions: 3,
                    processed_messages: 8,
                    processed_tool_calls: 3,
                    processed_bytes: 4_096,
                    elapsed: StdDuration::from_secs(3),
                    ..Default::default()
                },
                current_source_progress: None,
                exact_scan_progress: None,
            })?;
            Ok(test_publication("all-provider-generation"))
        },
    )
    .unwrap();
    drop(forge_writer);

    assert_eq!(capture_calls, 1);
    assert_eq!(publication.generation_id, "all-provider-generation");
    assert_eq!(
        updates.into_inner().unwrap(),
        vec![
            (
                "discovering".to_owned(),
                0,
                2,
                None,
                None,
                None,
                vec!["codex".to_owned(), "claude".to_owned()],
                0,
                0,
                0,
                0,
                Some(1_000),
            ),
            (
                "refreshing".to_owned(),
                1,
                2,
                Some("provider-wide-route".to_owned()),
                Some(11),
                Some(4_096),
                vec!["codex".to_owned(), "claude".to_owned()],
                3,
                8,
                3,
                4_096,
                Some(2_500),
            ),
            (
                "verifying".to_owned(),
                2,
                2,
                None,
                None,
                None,
                vec!["codex".to_owned(), "claude".to_owned()],
                3,
                8,
                3,
                4_096,
                Some(3_000),
            ),
        ]
    );
}

#[test]
fn selected_route_facts_cannot_attach_unadmitted_work() {
    let selected = SourceRouteIdentity::from_sha256("cc".repeat(32)).unwrap();
    let outside = SourceRouteIdentity::from_sha256("cd".repeat(32)).unwrap();
    let discovery = SourceBackedAdmittedDiscovery::new(
        DiscoveryReport {
            sources: Vec::new(),
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        SourceBackedProviderRegistry::new().watch_catalog(),
    );
    let workset_error = AdmittedRefresh::for_test(
        AdmittedRefreshCoverage::SelectedRoutes,
        BTreeSet::from([selected]),
        discovery,
    )
    .unwrap()
    .with_execution_facts(BTreeMap::from([(
        outside,
        SourceBackedRefreshWorkset::Exhaustive,
    )]))
    .unwrap_err();
    assert!(format!("{workset_error:#}")
        .contains("source refresh workset references a route outside physical admission"));
}

#[test]
fn selected_execution_never_widens_beyond_its_admitted_report() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (home, _, discovery) = discovery_fixture(temp.path());
    let unrelated_forge = home.join(".forge/.forge.db");
    fs::create_dir_all(unrelated_forge.parent().unwrap()).unwrap();
    fs::write(&unrelated_forge, b"discoverable but not admitted").unwrap();

    let selected_path = temp.path().join("selected-history.jsonl");
    fs::write(&selected_path, b"selected\n").unwrap();
    let selected_source = provider_source_for_path(CaptureProvider::OpenCode, selected_path);
    let selected_route = SourceBackedRoute::automatic(
        selected_source.clone(),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let selected_route_id = selected_route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(selected_route);
    let admitted = AdmittedRefresh::for_test(
        AdmittedRefreshCoverage::SelectedRoutes,
        BTreeSet::from([selected_route_id.clone()]),
        SourceBackedAdmittedDiscovery::new(
            DiscoveryReport {
                sources: vec![selected_source.clone()],
                issues: Vec::new(),
            },
            StdDuration::from_millis(3),
            registry.watch_catalog(),
        ),
    )
    .unwrap()
    .with_execution_facts(BTreeMap::new())
    .unwrap();
    let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "selected-no-widening",
        RefreshOperation::Refresh,
        None,
        admitted,
        &discovery,
        &TestPublishedState,
        &progress,
    );

    let publication = execute_capture_owned_refresh_with(
        execution,
        &discovery,
        |_,
         report,
         discovery_duration,
         _,
         _,
         _,
         _,
         _,
         publication_scope,
         physical_scope,
         exact_catalog_members,
         _,
         _| {
            assert_eq!(report.sources, vec![selected_source]);
            assert!(report
                .sources
                .iter()
                .all(|source| source.path != unrelated_forge));
            assert_eq!(discovery_duration, StdDuration::from_millis(3));
            let exact_scope = SourceBackedRefreshScope::exact([selected_route_id]);
            assert_eq!(publication_scope, exact_scope);
            assert_eq!(physical_scope, exact_scope);
            assert!(!exact_catalog_members);
            Ok(test_publication("selected-no-widening-generation"))
        },
    )
    .unwrap();
    assert_eq!(publication.generation_id, "selected-no-widening-generation");
}

#[test]
fn warm_exact_carries_unselected_routes_while_receipt_stays_selected() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());

    let codex_root = temp.path().join("codex-sessions");
    fs::create_dir_all(&codex_root).unwrap();
    let codex_session = codex_root.join("rollout.jsonl");
    fs::write(
        &codex_session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-17T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000701",
                    "timestamp": "2026-08-17T00:00:00Z",
                    "cwd": "/repo/exact-carry",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-17T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "codex warm"}]
                }
            })
        ),
    )
    .unwrap();
    let claude_root = temp.path().join("claude-projects");
    let claude_session = claude_root.join("project/session.jsonl");
    fs::create_dir_all(claude_session.parent().unwrap()).unwrap();
    fs::write(
        &claude_session,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "literal-claude-warm",
                "sessionId": "019fb700-0000-7000-8000-000000000702",
                "message": {"role": "user", "content": "claude warm"}
            })
        ),
    )
    .unwrap();
    let codex_source = provider_source_for_path(CaptureProvider::Codex, codex_root);
    let claude_source = provider_source_for_path(CaptureProvider::Claude, claude_root);
    let codex_route = automatic_source_backed_route_identity(&codex_source).unwrap();
    let claude_route = automatic_source_backed_route_identity(&claude_source).unwrap();
    let report = DiscoveryReport {
        sources: vec![codex_source.clone(), claude_source],
        issues: Vec::new(),
    };
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    let cold = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert_eq!(cold.route_results.len(), 2);

    fs::write(
        &codex_session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-17T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000701",
                    "timestamp": "2026-08-17T00:00:00Z",
                    "cwd": "/repo/exact-carry",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-17T00:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "codex exact"}]
                }
            })
        ),
    )
    .unwrap();
    let exact_routes = BTreeSet::from([codex_route.clone()]);
    let mut exact_progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    let exact = refresh_all_provider_sources_route_local(
        &discovery,
        DiscoveryReport {
            sources: vec![codex_source],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        "warm-exact-carry",
        RefreshOperation::Refresh,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::Exact(exact_routes.clone()),
        &TestPublishedState,
        &mut exact_progress,
    )
    .unwrap();

    assert_eq!(exact.route_results.len(), 1);
    assert_eq!(exact.route_results[0].route_identity, codex_route.as_str());
    let published = VerifiedIndex::open(&index_root).unwrap();
    assert!(published.manifest().source_route(&codex_route).is_some());
    assert!(published.manifest().source_route(&claude_route).is_some());
}

#[test]
fn configured_claude_home_is_additive_and_naming_the_automatic_home_deduplicates() {
    for same_home in [true, false] {
        for automatic_enabled in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let fixture = fs::canonicalize(temp.path()).unwrap();
            let data_root = fixture.join("data");
            let index_root = source_backed_index_root(&data_root);
            ctx_history_platform::platform_security::establish_private_data_root(&data_root)
                .unwrap();
            let (_, _, automatic_discovery) = discovery_fixture(&fixture);
            let automatic_home = fixture.join("claude-automatic");
            let automatic_discovery =
                automatic_discovery.with_env("CLAUDE_CONFIG_DIR", &automatic_home);
            let automatic_projects = automatic_home.join("projects");
            let automatic_session = automatic_projects.join("project/session.jsonl");
            fs::create_dir_all(automatic_session.parent().unwrap()).unwrap();
            fs::write(
                &automatic_session,
                format!(
                    "{}\n",
                    json!({
                        "type": "user",
                        "uuid": "automatic-message",
                        "sessionId": "019fb700-0000-7000-8000-000000000711",
                        "message": {"role": "user", "content": "automatic claude"}
                    })
                ),
            )
            .unwrap();
            let automatic_source =
                provider_source_for_path(CaptureProvider::Claude, automatic_projects.clone());
            let automatic_route =
                automatic_source_backed_route_identity(&automatic_source).unwrap();
            let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
            refresh_all_provider_sources(
                &automatic_discovery,
                DiscoveryReport {
                    sources: vec![automatic_source.clone()],
                    issues: Vec::new(),
                },
                StdDuration::ZERO,
                &data_root,
                &index_root,
                None,
                SourceBackedRefreshScope::All,
                &mut progress,
            )
            .unwrap();
            let automatic_source_key = VerifiedIndex::open(&index_root).unwrap().manifest().sources
                [0]
            .observation()
            .source()
            .clone();

            let configured_home = if same_home {
                automatic_home.clone()
            } else {
                fixture.join("claude-configured")
            };
            let configured_projects = configured_home.join("projects");
            let configured_session = configured_projects.join("project/session.jsonl");
            if !same_home {
                fs::create_dir_all(configured_session.parent().unwrap()).unwrap();
                fs::write(
                    &configured_session,
                    format!(
                        "{}\n",
                        json!({
                            "type": "user",
                            "uuid": "configured-message",
                            "sessionId": "019fb700-0000-7000-8000-000000000712",
                            "message": {"role": "user", "content": "configured claude"}
                        })
                    ),
                )
                .unwrap();
            }
            let definition = ctx_history_capture::ProviderRootDefinition {
                id: "work".to_owned(),
                provider: CaptureProvider::Claude,
                path: configured_home,
                group: Some("work".to_owned()),
            };
            let configured_discovery = automatic_discovery
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![definition]);
            let configured_source =
                provider_source_for_path(CaptureProvider::Claude, configured_projects);
            let sources = if automatic_enabled && !same_home {
                vec![automatic_source, configured_source]
            } else {
                vec![configured_source]
            };
            let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
            refresh_all_provider_sources(
                &configured_discovery,
                DiscoveryReport {
                    sources,
                    issues: Vec::new(),
                },
                StdDuration::ZERO,
                &data_root,
                &index_root,
                None,
                SourceBackedRefreshScope::All,
                &mut progress,
            )
            .unwrap();

            let published = VerifiedIndex::open(&index_root).unwrap();
            // Disabling inference changes future selection; it does not use a
            // config toggle as deletion authority for already indexed history.
            let expected_route_count = if same_home { 1 } else { 2 };
            assert!(published
                .manifest()
                .source_route(&automatic_route)
                .is_some());
            assert_eq!(
                published.manifest().source_routes().len(),
                expected_route_count
            );
            assert_eq!(published.manifest().sources.len(), expected_route_count);
            assert_eq!(published.manifest().provider_roots().len(), 1);
            assert_eq!(
                published.manifest().provider_roots()[0].source_identity(),
                if same_home {
                    ProviderRootSourceIdentity::Released
                } else {
                    ProviderRootSourceIdentity::NamedV1
                }
            );
            if same_home {
                assert!(published.manifest().sources[0]
                    .observation()
                    .source()
                    .exact_descriptor_eq(&automatic_source_key));
            }
            assert_eq!(
                published.manifest().provider_roots()[0].definition().id,
                "work"
            );
            assert_eq!(
                published.manifest().automatic_provider_discovery(),
                automatic_enabled
            );
        }
    }
}

#[test]
fn watch_catalog_retains_released_identity_after_named_default_home_moves() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let released_home = fixture.join("claude-released");
    let released_projects = released_home.join("projects");
    let released_session = released_projects.join("project/session.jsonl");
    fs::create_dir_all(released_session.parent().unwrap()).unwrap();
    fs::write(
        &released_session,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "released-move-message",
                "sessionId": "019fb700-0000-7000-8000-000000000713",
                "message": {"role": "user", "content": "released move"}
            })
        ),
    )
    .unwrap();
    let definition = |path| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Claude,
        path,
        group: Some("work".to_owned()),
    };
    let initial_discovery = discovery
        .with_env("CLAUDE_CONFIG_DIR", &released_home)
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(released_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial_discovery,
        DiscoveryReport {
            sources: vec![provider_source_for_path(
                CaptureProvider::Claude,
                released_projects,
            )],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let published = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        published.manifest().provider_roots()[0].source_identity(),
        ProviderRootSourceIdentity::Released
    );
    let published_route = published.manifest().provider_roots()[0].routes()[0].clone();
    drop(published);

    let moved_home = fixture.join("claude-moved");
    fs::rename(&released_home, &moved_home).unwrap();
    let moved_discovery =
        initial_discovery.with_configured_provider_roots(vec![definition(moved_home)]);
    let catalog = source_backed_watch_catalog(&data_root, &moved_discovery).unwrap();

    assert_eq!(
        catalog.route_ids().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([published_route])
    );
}

#[test]
fn moved_released_root_wins_if_the_old_automatic_location_reappears() {
    for automatic_first in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let fixture = fs::canonicalize(temp.path()).unwrap();
        let data_root = fixture.join("data");
        let index_root = source_backed_index_root(&data_root);
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        let (_, _, discovery) = discovery_fixture(&fixture);
        let released_home = fixture.join("claude-released");
        let released_projects = released_home.join("projects");
        let write_session = |projects: &Path, session: &str, message: &str| {
            let path = projects.join("project").join(format!("{session}.jsonl"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!(
                    "{}\n",
                    json!({
                        "type": "user",
                        "uuid": format!("message-{session}"),
                        "sessionId": session,
                        "message": {"role": "user", "content": message}
                    })
                ),
            )
            .unwrap();
        };
        write_session(
            &released_projects,
            "019fb700-0000-7000-8000-000000000715",
            "releasedfirstcanary",
        );
        write_session(
            &released_projects,
            "019fb700-0000-7000-8000-000000000716",
            "releasedsecondcanary",
        );
        let definition = |path| ctx_history_capture::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path,
            group: Some("work".to_owned()),
        };
        let initial_discovery = discovery
            .with_env("CLAUDE_CONFIG_DIR", &released_home)
            .with_automatic_provider_discovery(false)
            .with_configured_provider_roots(vec![definition(released_home.clone())]);
        let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
        refresh_all_provider_sources(
            &initial_discovery,
            DiscoveryReport {
                sources: vec![provider_source_for_path(
                    CaptureProvider::Claude,
                    released_projects.clone(),
                )],
                issues: Vec::new(),
            },
            StdDuration::ZERO,
            &data_root,
            &index_root,
            None,
            SourceBackedRefreshScope::All,
            &mut progress,
        )
        .unwrap();

        let moved_home = fixture.join("claude-moved");
        fs::rename(&released_home, &moved_home).unwrap();
        let moved_projects = moved_home.join("projects");
        let recreated_projects = released_home.join("projects");
        write_session(
            &recreated_projects,
            "019fb700-0000-7000-8000-000000000717",
            "oldautomaticcanary",
        );
        let automatic_source =
            provider_source_for_path(CaptureProvider::Claude, recreated_projects);
        let configured_source = provider_source_for_path(CaptureProvider::Claude, moved_projects);
        let sources = if automatic_first {
            vec![automatic_source, configured_source]
        } else {
            vec![configured_source, automatic_source]
        };
        let moved_discovery = initial_discovery
            .with_automatic_provider_discovery(true)
            .with_configured_provider_roots(vec![definition(moved_home)]);
        refresh_all_provider_sources(
            &moved_discovery,
            DiscoveryReport {
                sources,
                issues: Vec::new(),
            },
            StdDuration::ZERO,
            &data_root,
            &index_root,
            None,
            SourceBackedRefreshScope::All,
            &mut progress,
        )
        .unwrap();

        let published = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(published.manifest().sources.len(), 2);
        assert_eq!(published.manifest().source_routes().len(), 1);
        assert_eq!(published.manifest().provider_roots().len(), 1);
        assert_eq!(
            published.manifest().provider_roots()[0].source_identity(),
            ProviderRootSourceIdentity::Released
        );
        assert_eq!(published.manifest().provider_roots()[0].routes().len(), 1);
        let allowed_source_keys = published
            .manifest()
            .provider_root_source_tokens(&["work".to_owned()], &[])
            .unwrap();
        let work_filter = EventSearchFilters {
            allowed_source_keys: Some(allowed_source_keys),
            ..EventSearchFilters::default()
        };
        assert_eq!(
            published
                .search_event_candidates_with_filters("releasedfirstcanary", &work_filter, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            published
                .search_event_candidates_with_filters("releasedsecondcanary", &work_filter, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(published
            .search_event_candidates_with_filters("oldautomaticcanary", &work_filter, 10)
            .unwrap()
            .is_empty());
        assert!(published
            .search_event_candidates("oldautomaticcanary", 10)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn naming_a_failing_automatic_home_carries_it_while_named_peer_advances() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let automatic_home = fixture.join("claude-automatic-failing");
    let peer_home = fixture.join("claude-peer");
    let automatic_projects = automatic_home.join("projects");
    let peer_projects = peer_home.join("projects");
    let automatic_session =
        automatic_projects.join("project/019fb700-0000-7000-8000-000000000714.jsonl");
    let peer_session = peer_projects.join("project/019fb700-0000-7000-8000-000000000715.jsonl");
    fs::create_dir_all(automatic_session.parent().unwrap()).unwrap();
    fs::create_dir_all(peer_session.parent().unwrap()).unwrap();
    let claude_message = |uuid: &str, session_id: &str, content: &str| {
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": uuid,
                "sessionId": session_id,
                "message": {"role": "user", "content": content}
            })
        )
    };
    fs::write(
        &automatic_session,
        claude_message(
            "019fb710-0000-7000-8000-000000000714",
            "019fb700-0000-7000-8000-000000000714",
            "retainedautomaticfixture",
        ),
    )
    .unwrap();
    fs::write(
        &peer_session,
        claude_message(
            "019fb710-0000-7000-8000-000000000715",
            "019fb700-0000-7000-8000-000000000715",
            "peer initial",
        ),
    )
    .unwrap();
    let definition = |id: &str, path: PathBuf| ctx_history_capture::ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Claude,
        path,
        group: Some(id.to_owned()),
    };
    let peer_definition = definition("peer", peer_home.clone());
    let initial_discovery = discovery
        .with_env("CLAUDE_CONFIG_DIR", &automatic_home)
        .with_configured_provider_roots(vec![peer_definition.clone()]);
    let automatic_source =
        provider_source_for_path(CaptureProvider::Claude, automatic_projects.clone());
    let automatic_route = automatic_source_backed_route_identity(&automatic_source).unwrap();
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial_discovery,
        DiscoveryReport {
            sources: vec![
                automatic_source,
                provider_source_for_path(CaptureProvider::Claude, peer_projects.clone()),
            ],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    fs::write(
        &peer_session,
        format!(
            "{}{}",
            claude_message(
                "019fb710-0000-7000-8000-000000000715",
                "019fb700-0000-7000-8000-000000000715",
                "peer initial",
            ),
            claude_message(
                "019fb710-0000-7000-8000-000000000716",
                "019fb700-0000-7000-8000-000000000715",
                "advancedpeerfixture",
            )
        ),
    )
    .unwrap();
    let displaced_home = fixture.join("claude-automatic-displaced");
    fs::rename(&automatic_home, &displaced_home).unwrap();
    fs::write(&automatic_home, b"temporarily not a directory").unwrap();
    let automatic_definition = definition("automatic", automatic_home.clone());
    let configured_discovery = initial_discovery
        .with_configured_provider_roots(vec![automatic_definition, peer_definition]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    let publication = refresh_all_provider_sources(
        &configured_discovery,
        DiscoveryReport {
            sources: vec![
                provider_source_for_path(CaptureProvider::Claude, automatic_projects),
                provider_source_for_path(CaptureProvider::Claude, peer_projects),
            ],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    assert!(!publication.route_results.is_empty());
    let published = VerifiedIndex::open(&index_root).unwrap();
    let automatic_root = published
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "automatic")
        .unwrap();
    assert_eq!(
        automatic_root.source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(automatic_root.routes(), &[automatic_route]);
    assert_eq!(
        published
            .search_event_candidates("retainedautomaticfixture", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        published
            .search_event_candidates("advancedpeerfixture", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn removing_last_configured_claude_home_returns_to_one_automatic_route() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, automatic_discovery) = discovery_fixture(&fixture);
    let home = fixture.join("claude-home");
    let automatic_discovery = automatic_discovery.with_env("CLAUDE_CONFIG_DIR", &home);
    let projects = home.join("projects");
    let session = projects.join("project/session.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "fallback-message",
                "sessionId": "019fb700-0000-7000-8000-000000000713",
                "message": {"role": "user", "content": "fallback claude"}
            })
        ),
    )
    .unwrap();
    let source = provider_source_for_path(CaptureProvider::Claude, projects);
    let automatic_route = automatic_source_backed_route_identity(&source).unwrap();
    let configured_discovery = automatic_discovery
        .clone()
        .with_configured_provider_roots(vec![ctx_history_capture::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: home,
            group: Some("personal".to_owned()),
        }]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &configured_discovery,
        DiscoveryReport {
            sources: vec![source.clone()],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let configured_source_key = VerifiedIndex::open(&index_root).unwrap().manifest().sources[0]
        .observation()
        .source()
        .clone();

    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &automatic_discovery,
        DiscoveryReport {
            sources: vec![source],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    let published = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(published.manifest().source_routes().len(), 1);
    assert!(published
        .manifest()
        .source_route(&automatic_route)
        .is_some());
    assert!(published.manifest().sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&configured_source_key));
    assert!(published.manifest().provider_roots().is_empty());
}

#[test]
fn moving_a_named_claude_home_preserves_route_and_source_identity() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let first_home = fixture.join("claude-work-old");
    let first_projects = first_home.join("projects");
    let session = first_projects.join("project/session.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "moved-message",
                "sessionId": "019fb700-0000-7000-8000-000000000715",
                "message": {"role": "user", "content": "moved claude"}
            })
        ),
    )
    .unwrap();
    let definition = |path| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Claude,
        path,
        group: Some("work".to_owned()),
    };
    let first_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(first_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &first_discovery,
        DiscoveryReport {
            sources: vec![provider_source_for_path(
                CaptureProvider::Claude,
                first_projects,
            )],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let first = VerifiedIndex::open(&index_root).unwrap();
    let first_route = first.manifest().provider_roots()[0].routes()[0].clone();
    let first_source = first.manifest().sources[0].observation().source().clone();
    drop(first);

    let second_home = fixture.join("claude-work-new");
    fs::rename(&first_home, &second_home).unwrap();
    let second_projects = second_home.join("projects");
    let second_discovery = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(second_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &second_discovery,
        DiscoveryReport {
            sources: vec![provider_source_for_path(
                CaptureProvider::Claude,
                second_projects,
            )],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    let moved = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(moved.manifest().source_routes().len(), 1);
    assert_eq!(
        moved.manifest().provider_roots()[0].routes(),
        &[first_route]
    );
    assert_eq!(
        moved.manifest().provider_roots()[0].definition().path,
        second_home
    );
    assert_eq!(
        moved.manifest().provider_roots()[0].source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
    assert!(moved.manifest().sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&first_source));
}

#[test]
fn moving_a_named_codex_home_preserves_route_and_source_identity() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let first_home = fixture.join("codex-work-old");
    let first_sessions = first_home.join("sessions");
    let session = first_sessions.join("rollout.jsonl");
    fs::create_dir_all(&first_sessions).unwrap();
    fs::write(
        &session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-17T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000716",
                    "timestamp": "2026-08-17T00:00:00Z",
                    "cwd": "/repo/moved-codex",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-17T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "moved codex"}]
                }
            })
        ),
    )
    .unwrap();
    let definition = |path| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Codex,
        path,
        group: Some("work".to_owned()),
    };
    let first_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(first_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &first_discovery,
        DiscoveryReport {
            sources: vec![provider_source_for_path(
                CaptureProvider::Codex,
                first_sessions,
            )],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let first = VerifiedIndex::open(&index_root).unwrap();
    let first_route = first.manifest().provider_roots()[0].routes()[0].clone();
    let first_source = first.manifest().sources[0].observation().source().clone();
    drop(first);

    let second_home = fixture.join("codex-work-new");
    fs::rename(&first_home, &second_home).unwrap();
    let second_sessions = second_home.join("sessions");
    let second_discovery = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(second_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &second_discovery,
        DiscoveryReport {
            sources: vec![provider_source_for_path(
                CaptureProvider::Codex,
                second_sessions,
            )],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    let moved = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(moved.manifest().source_routes().len(), 1);
    assert_eq!(
        moved.manifest().provider_roots()[0].routes(),
        &[first_route]
    );
    assert_eq!(
        moved.manifest().provider_roots()[0].definition().path,
        second_home
    );
    assert!(moved.manifest().sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(&first_source));
}

#[test]
fn disabling_automatic_discovery_stops_selection_without_deleting_retained_history() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, automatic_discovery) = discovery_fixture(&fixture);
    let sessions = fixture.join("codex-automatic/sessions");
    let session = sessions.join("rollout.jsonl");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        &session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-17T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000714",
                    "timestamp": "2026-08-17T00:00:00Z",
                    "cwd": "/repo/automatic-disable",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-17T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "retained automatic history"}]
                }
            })
        ),
    )
    .unwrap();
    let source = provider_source_for_path(CaptureProvider::Codex, sessions);
    let route = automatic_source_backed_route_identity(&source).unwrap();
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &automatic_discovery,
        DiscoveryReport {
            sources: vec![source],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .manifest()
            .indexed_documents,
        1
    );

    let disabled = automatic_discovery.with_automatic_provider_discovery(false);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &disabled,
        DiscoveryReport::default(),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    let published = VerifiedIndex::open(&index_root).unwrap();
    assert!(!published.manifest().automatic_provider_discovery());
    assert!(published.manifest().source_route(&route).is_some());
    assert_eq!(published.manifest().sources.len(), 1);
    assert_eq!(published.manifest().indexed_documents, 1);
}
