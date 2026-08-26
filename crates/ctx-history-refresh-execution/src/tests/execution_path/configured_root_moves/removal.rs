use super::*;

#[derive(Debug, PartialEq, Eq)]
struct ExactHistoryBytes {
    route: Vec<u8>,
    source_token: String,
    source: Vec<u8>,
    session_id: Vec<u8>,
    event_id: Vec<u8>,
    record: Vec<u8>,
    certificate: Vec<u8>,
}

impl ExactHistoryBytes {
    fn assert_core_history_eq(&self, other: &Self) {
        assert_eq!(self.route, other.route);
        assert_eq!(self.source_token, other.source_token);
        assert_eq!(self.source, other.source);
        assert_eq!(self.session_id, other.session_id);
        assert_eq!(self.event_id, other.event_id);
        assert_eq!(self.record, other.record);
    }
}

fn exact_history_bytes(index: &VerifiedIndex, marker: &str) -> ExactHistoryBytes {
    let hits = index.search_event_candidates(marker, 8).unwrap();
    assert_eq!(hits.len(), 1, "{marker}");
    let event = &hits[0].event;
    assert!(index
        .session_by_id(event.session_id.as_uuid())
        .unwrap()
        .is_some());
    assert!(index
        .core_event_by_id(event.event_id.as_uuid())
        .unwrap()
        .is_some());
    let source = &event.source;
    let route = index
        .manifest()
        .source_routes()
        .iter()
        .find(|route| route.sources().iter().any(|member| member == source))
        .unwrap();
    let certificate = index
        .manifest()
        .sources
        .iter()
        .find(|candidate| candidate.observation().source() == source)
        .unwrap();
    ExactHistoryBytes {
        route: serde_json::to_vec(route.route_identity()).unwrap(),
        source_token: ctx_history_index::source_token(source),
        source: serde_json::to_vec(source).unwrap(),
        session_id: serde_json::to_vec(&event.session_id).unwrap(),
        event_id: serde_json::to_vec(&event.event_id).unwrap(),
        record: serde_json::to_vec(
            &index
                .core_record_by_id(event.event_id.as_uuid())
                .unwrap()
                .unwrap(),
        )
        .unwrap(),
        certificate: serde_json::to_vec(certificate).unwrap(),
    }
}

fn write_codex_session(sessions: &Path, session_id: &str, marker: &str) {
    fs::create_dir_all(sessions).unwrap();
    fs::write(
        sessions.join("rollout.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-25T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": "2026-08-25T00:00:00Z",
                    "cwd": "/repo/partial-released-codex",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-25T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": marker}]
                }
            })
        ),
    )
    .unwrap();
}

fn refresh_discovered_codex(discovery: &DiscoveryContext, data_root: &Path, index_root: &Path) {
    let report = ctx_history_capture::discover_provider_sources_for_provider_with_context(
        discovery,
        CaptureProvider::Codex,
    );
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        discovery,
        report,
        StdDuration::ZERO,
        data_root,
        index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
}

#[test]
fn partial_released_codex_overlap_move_remove_readd_preserves_exact_identity() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let first_home = fixture.join("codex-released-first");
    let first_sessions = first_home.join("sessions");
    let session_id = "019fb700-0000-7000-8000-000000000721";
    let marker = "partialreleasedcodexlifecycle";
    write_codex_session(&first_sessions, session_id, marker);
    let automatic_discovery = discovery
        .clone()
        .with_env("CODEX_HOME", first_home.as_os_str());
    let automatic_report = ctx_history_capture::discover_provider_sources_for_provider_with_context(
        &automatic_discovery,
        CaptureProvider::Codex,
    );
    assert_eq!(automatic_report.sources.len(), 3);
    assert_eq!(
        automatic_report
            .sources
            .iter()
            .filter(|source| source.status == ProviderSourceStatus::Available)
            .count(),
        1
    );
    assert_eq!(
        automatic_report
            .sources
            .iter()
            .filter(|source| source.status == ProviderSourceStatus::Missing)
            .count(),
        2
    );
    let session_route = automatic_source_backed_route_identity(
        automatic_report
            .sources
            .iter()
            .find(|source| source.source_format == "codex_session_jsonl_tree")
            .unwrap(),
    )
    .unwrap();
    let prompt_history_route = automatic_source_backed_route_identity(
        automatic_report
            .sources
            .iter()
            .find(|source| source.source_format == "codex_history_jsonl")
            .unwrap(),
    )
    .unwrap();
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &automatic_discovery,
        automatic_report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let automatic = VerifiedIndex::open(&index_root).unwrap();
    let baseline = exact_history_bytes(&automatic, marker);
    assert_eq!(automatic.manifest().sources.len(), 1);
    drop(automatic);

    let definition = |path| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Codex,
        path,
        group: Some("work-group".to_owned()),
        kind: None,
    };
    let overlapping_discovery = automatic_discovery
        .clone()
        .with_configured_provider_roots(vec![definition(first_home.clone())]);
    refresh_discovered_codex(&overlapping_discovery, &data_root, &index_root);
    let overlapping = VerifiedIndex::open(&index_root).unwrap();
    exact_history_bytes(&overlapping, marker).assert_core_history_eq(&baseline);
    assert_eq!(overlapping.manifest().sources.len(), 1);
    let overlapping_routes = overlapping
        .manifest()
        .provider_root("work")
        .unwrap()
        .routes()
        .to_vec();
    assert_eq!(overlapping_routes, vec![session_route]);
    assert_eq!(
        overlapping
            .manifest()
            .provider_root("work")
            .unwrap()
            .source_identity(),
        ProviderRootSourceIdentity::Released
    );
    drop(overlapping);

    let moved_home = fixture.join("codex-released-moved");
    fs::rename(&first_home, &moved_home).unwrap();
    fs::create_dir(&first_home).unwrap();
    let moved_discovery = discovery
        .clone()
        .with_env("CODEX_HOME", first_home.as_os_str())
        .with_configured_provider_roots(vec![definition(moved_home.clone())]);
    refresh_discovered_codex(&moved_discovery, &data_root, &index_root);
    let moved = VerifiedIndex::open(&index_root).unwrap();
    exact_history_bytes(&moved, marker).assert_core_history_eq(&baseline);
    assert_eq!(moved.manifest().sources.len(), 1);
    let moved_routes = moved
        .manifest()
        .provider_root("work")
        .unwrap()
        .routes()
        .to_vec();
    assert_eq!(moved_routes, overlapping_routes);
    assert!(moved
        .manifest()
        .source_route(&prompt_history_route)
        .is_none());
    assert_eq!(
        moved
            .manifest()
            .provider_root("work")
            .unwrap()
            .source_identity(),
        ProviderRootSourceIdentity::Released
    );
    drop(moved);

    let removed_discovery = discovery
        .clone()
        .with_env("CODEX_HOME", first_home.as_os_str());
    refresh_discovered_codex(&removed_discovery, &data_root, &index_root);
    let removed = VerifiedIndex::open(&index_root).unwrap();
    exact_history_bytes(&removed, marker).assert_core_history_eq(&baseline);
    assert!(removed.manifest().provider_roots().is_empty());
    assert_eq!(
        removed.manifest().detached_released_provider_roots().len(),
        1
    );
    assert_eq!(
        removed.manifest().detached_released_provider_roots()[0].id(),
        "work"
    );
    assert!(matches!(
        removed
            .manifest()
            .provider_root_source_tokens(&["work".to_owned()], &[]),
        Err(ctx_history_index::IndexError::UnknownProviderRootSelector(id)) if id == "work"
    ));
    drop(removed);

    let readded_discovery = discovery
        .with_env("CODEX_HOME", first_home.as_os_str())
        .with_configured_provider_roots(vec![definition(moved_home)]);
    refresh_discovered_codex(&readded_discovery, &data_root, &index_root);
    let readded = VerifiedIndex::open(&index_root).unwrap();
    exact_history_bytes(&readded, marker).assert_core_history_eq(&baseline);
    assert_eq!(readded.manifest().sources.len(), 1);
    assert!(readded
        .manifest()
        .detached_released_provider_roots()
        .is_empty());
    let root = readded.manifest().provider_root("work").unwrap();
    assert_eq!(root.source_identity(), ProviderRootSourceIdentity::Released);
    assert_eq!(root.routes(), moved_routes);
    let tokens = readded
        .manifest()
        .provider_root_source_tokens(&["work".to_owned()], &["work-group".to_owned()])
        .unwrap();
    assert_eq!(tokens, vec![baseline.source_token]);
    assert_eq!(
        readded
            .search_event_candidates_with_filters(
                marker,
                &EventSearchFilters {
                    allowed_source_keys: Some(tokens),
                    ..EventSearchFilters::default()
                },
                8,
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn removed_standalone_named_codex_root_retains_exact_history_until_readded() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let first_home = fixture.join("codex-work-first");
    let first_sessions = first_home.join("sessions");
    let session_id = "019fb700-0000-7000-8000-000000000719";
    let session = first_sessions.join("rollout.jsonl");
    fs::create_dir_all(&first_sessions).unwrap();
    fs::write(
        &session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-25T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": "2026-08-25T00:00:00Z",
                    "cwd": "/repo/retained-codex",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-25T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "standaloneremovalretention"
                    }]
                }
            })
        ),
    )
    .unwrap();
    let peer_home = fixture.join("codex-personal");
    let peer_sessions = peer_home.join("sessions");
    fs::create_dir_all(&peer_sessions).unwrap();
    fs::write(
        peer_sessions.join("rollout.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-25T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000720",
                    "timestamp": "2026-08-25T00:00:00Z",
                    "cwd": "/repo/personal-codex",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-25T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "retainednamedpeer"}]
                }
            })
        ),
    )
    .unwrap();
    let definition = |id: &str, path| ctx_history_capture::ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Codex,
        path,
        group: Some(format!("{id}-group")),
        kind: None,
    };
    let configured_source = |id, sessions, home| {
        configured_provider_source_for_path(
            CaptureProvider::Codex,
            sessions,
            id,
            home,
            "codex-sessions",
        )
    };
    let initial_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            definition("work", first_home.clone()),
            definition("personal", peer_home.clone()),
        ]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial_discovery,
        DiscoveryReport {
            sources: vec![
                configured_source("work", first_sessions, first_home.clone()),
                configured_source("personal", peer_sessions.clone(), peer_home.clone()),
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
    let initial = VerifiedIndex::open(&index_root).unwrap();
    let initial_exact = exact_history_bytes(&initial, "standaloneremovalretention");
    let route = initial.manifest().provider_root("work").unwrap().routes()[0].clone();
    let peer_route = initial
        .manifest()
        .provider_root("personal")
        .unwrap()
        .routes()[0]
        .clone();
    assert_eq!(initial.manifest().sources.len(), 2);
    drop(initial);

    let moved_home = fixture.join("codex-work-moved");
    fs::rename(&first_home, &moved_home).unwrap();
    let moved_sessions = moved_home.join("sessions");
    let moved_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            definition("work", moved_home.clone()),
            definition("personal", peer_home.clone()),
        ]);
    refresh_all_provider_sources(
        &moved_discovery,
        DiscoveryReport {
            sources: vec![
                configured_source("work", moved_sessions.clone(), moved_home.clone()),
                configured_source("personal", peer_sessions.clone(), peer_home.clone()),
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
    let moved = VerifiedIndex::open(&index_root).unwrap();
    let moved_exact = exact_history_bytes(&moved, "standaloneremovalretention");
    moved_exact.assert_core_history_eq(&initial_exact);
    assert_eq!(
        moved.manifest().provider_root("work").unwrap().routes(),
        std::slice::from_ref(&route)
    );
    drop(moved);

    let removed_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition("personal", peer_home.clone())]);
    refresh_all_provider_sources(
        &removed_discovery,
        DiscoveryReport {
            sources: vec![configured_source(
                "personal",
                peer_sessions.clone(),
                peer_home.clone(),
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
    let removed = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(removed.manifest().provider_roots().len(), 1);
    assert_eq!(
        removed.manifest().provider_roots()[0].definition().id,
        "personal"
    );
    assert_eq!(removed.manifest().source_routes().len(), 2);
    assert_eq!(
        exact_history_bytes(&removed, "standaloneremovalretention"),
        moved_exact
    );
    assert!(matches!(
        removed
            .manifest()
            .provider_root_source_tokens(&["work".to_owned()], &[]),
        Err(ctx_history_index::IndexError::UnknownProviderRootSelector(id)) if id == "work"
    ));
    assert!(matches!(
        removed
            .manifest()
            .provider_root_source_tokens(&[], &["work-group".to_owned()]),
        Err(ctx_history_index::IndexError::UnknownProviderRootGroup(group))
            if group == "work-group"
    ));
    let removed_generation = removed.generation_id().to_owned();
    drop(removed);
    let removed_catalog = source_backed_watch_catalog(&data_root, &removed_discovery).unwrap();
    assert!(!removed_catalog
        .route_ids()
        .any(|candidate| candidate == &route));
    assert!(removed_catalog
        .route_ids()
        .any(|candidate| candidate == &peer_route));
    assert!(removed_catalog
        .route_targets()
        .flat_map(|(_, targets)| targets)
        .all(|path| !path.starts_with(&first_home) && !path.starts_with(&moved_home)));

    let readded_home = fixture.join("codex-work-readded");
    fs::rename(&moved_home, &readded_home).unwrap();
    for (home, marker) in [
        (&first_home, "removedoldpathdecoy"),
        (&moved_home, "removedmovedpathdecoy"),
    ] {
        let sessions = home.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("decoy.jsonl"), marker).unwrap();
    }
    refresh_all_provider_sources(
        &removed_discovery,
        DiscoveryReport {
            sources: vec![configured_source(
                "personal",
                peer_sessions.clone(),
                peer_home.clone(),
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
    let restarted = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(restarted.generation_id(), removed_generation);
    assert_eq!(
        exact_history_bytes(&restarted, "standaloneremovalretention"),
        moved_exact
    );
    assert!(restarted
        .search_event_candidates("removedoldpathdecoy", 8)
        .unwrap()
        .is_empty());
    assert!(restarted
        .search_event_candidates("removedmovedpathdecoy", 8)
        .unwrap()
        .is_empty());
    drop(restarted);

    let readded_sessions = readded_home.join("sessions");
    let readded_discovery = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            definition("work", readded_home.clone()),
            definition("personal", peer_home.clone()),
        ]);
    refresh_all_provider_sources(
        &readded_discovery,
        DiscoveryReport {
            sources: vec![
                configured_source("work", readded_sessions, readded_home.clone()),
                configured_source("personal", peer_sessions, peer_home),
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
    let readded = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(readded.manifest().sources.len(), 2);
    assert_eq!(readded.manifest().source_routes().len(), 2);
    let readded_exact = exact_history_bytes(&readded, "standaloneremovalretention");
    readded_exact.assert_core_history_eq(&moved_exact);
    assert_eq!(
        readded.manifest().provider_root("work").unwrap().routes(),
        std::slice::from_ref(&route)
    );
    let source_tokens = readded
        .manifest()
        .provider_root_source_tokens(&["work".to_owned()], &["work-group".to_owned()])
        .unwrap();
    assert_eq!(source_tokens, vec![moved_exact.source_token.clone()]);
    assert_eq!(
        readded
            .search_event_candidates_with_filters(
                "standaloneremovalretention",
                &EventSearchFilters {
                    allowed_source_keys: Some(source_tokens),
                    ..EventSearchFilters::default()
                },
                8,
            )
            .unwrap()
            .len(),
        1
    );
    drop(readded);
    let catalog = source_backed_watch_catalog(&data_root, &readded_discovery).unwrap();
    assert!(catalog
        .route_ids()
        .any(|candidate| candidate == &peer_route));
    let targets = catalog
        .route_targets()
        .find_map(|(candidate, targets)| (candidate == &route).then_some(targets))
        .unwrap();
    assert!(targets.iter().all(|path| path.starts_with(&readded_home)));
}
