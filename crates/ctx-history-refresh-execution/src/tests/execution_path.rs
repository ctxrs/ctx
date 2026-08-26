//! Explicit executor-path coverage owned by physical refresh execution.

#[path = "execution_path/compound_root_lifecycle.rs"]
mod compound_root_lifecycle;
#[path = "execution_path/configured_root_moves.rs"]
mod configured_root_moves;

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
