use super::*;

#[test]
fn moving_named_qoder_transcript_tree_preserves_supported_layouts_and_identity() {
    const ROOT_ID: &str = "qoder-archive";
    const DIRECT_MARKER: &str = "qoderrenameddirectcanary";
    const LEGACY_MARKER: &str = "qoderrenamedlegacycanary";
    const NESTED_MARKER: &str = "qoderunsupportednestedcanary";

    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let first_root = fixture.join("qoder-history");
    write_qoder_message(
        &first_root.join("current/nested/not-a-session.jsonl"),
        "qoder-renamed-nested",
        NESTED_MARKER,
    );
    let definition = |path| ctx_history_capture::ProviderRootDefinition {
        id: ROOT_ID.to_owned(),
        provider: CaptureProvider::Qoder,
        path,
        group: Some("archive".to_owned()),
        kind: None,
    };
    let first_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(first_root.clone())]);
    let nested_only_report =
        ctx_history_capture::discover_provider_sources_for_provider_with_context(
            &first_discovery,
            CaptureProvider::Qoder,
        );
    assert!(nested_only_report.issues.is_empty());
    assert_eq!(nested_only_report.sources.len(), 1);
    assert_eq!(nested_only_report.sources[0].path, first_root);
    assert_eq!(
        nested_only_report.sources[0].status,
        ProviderSourceStatus::Empty
    );

    write_qoder_message(
        &first_root.join("current/direct-session.jsonl"),
        "qoder-renamed-direct",
        DIRECT_MARKER,
    );
    write_qoder_message(
        &first_root.join("legacy/transcript/legacy-session.jsonl"),
        "qoder-renamed-legacy",
        LEGACY_MARKER,
    );
    let first_report = ctx_history_capture::discover_provider_sources_for_provider_with_context(
        &first_discovery,
        CaptureProvider::Qoder,
    );
    assert!(first_report.issues.is_empty(), "{:?}", first_report.issues);
    assert_eq!(first_report.sources.len(), 1);
    assert_eq!(first_report.sources[0].path, first_root);
    assert_eq!(
        first_report.sources[0].status,
        ProviderSourceStatus::Available
    );
    assert!(matches!(
        &first_report.sources[0].route_provenance,
        ProviderSourceRouteProvenance::ConfiguredRoot { root_id, .. }
            if root_id == ROOT_ID
    ));
    run_report(&first_discovery, first_report, &data_root, &index_root).unwrap();

    let first = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        first
            .complete_lexical_search(DIRECT_MARKER, 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        first
            .complete_lexical_search(LEGACY_MARKER, 8)
            .unwrap()
            .len(),
        1
    );
    assert!(first
        .complete_lexical_search(NESTED_MARKER, 8)
        .unwrap()
        .is_empty());
    let first_root_manifest = first.manifest().provider_root(ROOT_ID).unwrap();
    assert_eq!(
        first_root_manifest.source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
    let first_routes = first_root_manifest.routes().to_vec();
    let first_sources = first
        .manifest()
        .sources
        .iter()
        .map(|source| {
            let source = source.observation().source();
            (
                ctx_history_index::source_token(source),
                serde_json::to_vec(source).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(first_sources.len(), 2);
    drop(first);

    let moved_root = fixture.join("qoder-history-moved");
    fs::rename(&first_root, &moved_root).unwrap();
    let moved_discovery = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(moved_root.clone())]);
    let moved_report = ctx_history_capture::discover_provider_sources_for_provider_with_context(
        &moved_discovery,
        CaptureProvider::Qoder,
    );
    assert!(moved_report.issues.is_empty(), "{:?}", moved_report.issues);
    assert_eq!(moved_report.sources.len(), 1);
    assert_eq!(moved_report.sources[0].path, moved_root);
    assert_eq!(
        moved_report.sources[0].status,
        ProviderSourceStatus::Available
    );
    run_report(&moved_discovery, moved_report, &data_root, &index_root).unwrap();

    let moved = VerifiedIndex::open_pinned(&index_root).unwrap();
    let moved_root_manifest = moved.manifest().provider_root(ROOT_ID).unwrap();
    assert_eq!(moved_root_manifest.routes(), first_routes);
    assert_eq!(moved_root_manifest.definition().path, moved_root);
    assert_eq!(
        moved_root_manifest.source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
    let moved_sources = moved
        .manifest()
        .sources
        .iter()
        .map(|source| {
            let source = source.observation().source();
            (
                ctx_history_index::source_token(source),
                serde_json::to_vec(source).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(moved_sources, first_sources);
    assert_eq!(
        moved
            .complete_lexical_search(DIRECT_MARKER, 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        moved
            .complete_lexical_search(LEGACY_MARKER, 8)
            .unwrap()
            .len(),
        1
    );
    assert!(moved
        .complete_lexical_search(NESTED_MARKER, 8)
        .unwrap()
        .is_empty());
}

#[test]
fn failed_same_name_incompatible_replacement_keeps_predecessor_until_success() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let root = |id: &str, provider, path| ctx_history_capture::ProviderRootDefinition {
        id: id.to_owned(),
        provider,
        path,
        group: Some(format!("{id}-group")),
        kind: None,
    };
    let work_home = fixture.join("codex-work");
    let work_sessions = work_home.join("sessions");
    write_codex_rollout(
        &work_sessions,
        "019fb700-0000-7000-8000-000000000730",
        "atomicpredecessorhistory",
    );
    let peer_home = fixture.join("codex-peer");
    let peer_sessions = peer_home.join("sessions");
    write_codex_rollout(
        &peer_sessions,
        "019fb700-0000-7000-8000-000000000731",
        "atomichealthypeer",
    );
    let initial = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            root("work", CaptureProvider::Codex, work_home.clone()),
            root("peer", CaptureProvider::Codex, peer_home.clone()),
        ]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial,
        DiscoveryReport {
            sources: vec![
                configured_provider_source_for_path(
                    CaptureProvider::Codex,
                    work_sessions,
                    "work",
                    work_home.clone(),
                    "codex-sessions",
                ),
                configured_provider_source_for_path(
                    CaptureProvider::Codex,
                    peer_sessions.clone(),
                    "peer",
                    peer_home.clone(),
                    "codex-sessions",
                ),
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
    let initial_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let predecessor_root = initial_index.manifest().provider_root("work").unwrap();
    let predecessor_routes = predecessor_root.routes().to_vec();
    let predecessor_tokens = initial_index
        .manifest()
        .provider_root_source_tokens(&["work".to_owned()], &[])
        .unwrap();
    drop(initial_index);

    let replacement_home = fixture.join("claude-work");
    let replacement_projects = replacement_home.join("projects");
    let replacement_session = replacement_projects.join("project/session.jsonl");
    write_claude_message(
        &replacement_session,
        "session",
        "atomicsuccessorreplacement",
    );
    let replacement = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            root("work", CaptureProvider::Claude, replacement_home.clone()),
            root("peer", CaptureProvider::Codex, peer_home.clone()),
        ]);
    let mut failed_report =
        ctx_history_capture::discover_provider_sources_for_provider_with_context(
            &replacement,
            CaptureProvider::Claude,
        );
    let failed_replacement = failed_report
        .sources
        .iter_mut()
        .find(|source| source.path == replacement_projects)
        .unwrap();
    failed_replacement.status = ProviderSourceStatus::Unsupported;
    failed_replacement.unsupported_reason = Some("injected replacement failure");
    failed_report
        .sources
        .push(configured_provider_source_for_path(
            CaptureProvider::Codex,
            peer_sessions.clone(),
            "peer",
            peer_home.clone(),
            "codex-sessions",
        ));
    let failed = refresh_all_provider_sources(
        &replacement,
        failed_report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert!(failed
        .route_results
        .iter()
        .any(|result| result.outcome.failure_class().is_some()));
    let failed_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        failed_index
            .complete_lexical_search("atomicpredecessorhistory", 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        failed_index
            .complete_lexical_search("atomichealthypeer", 8)
            .unwrap()
            .len(),
        1
    );
    let failed_root = failed_index.manifest().provider_root("work").unwrap();
    assert_eq!(failed_root.definition().provider, CaptureProvider::Codex);
    assert_eq!(failed_root.definition().path, work_home);
    assert_eq!(
        failed_root.source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
    assert_eq!(failed_root.routes(), predecessor_routes);
    assert!(failed_root.exact_source_memberships().is_empty());
    let work_tokens = failed_index
        .manifest()
        .provider_root_source_tokens(&["work".to_owned()], &[])
        .unwrap();
    assert_eq!(work_tokens, predecessor_tokens);
    assert_eq!(
        failed_index
            .complete_filtered_lexical_search(
                "atomicpredecessorhistory",
                &EventSearchFilters {
                    allowed_source_keys: Some(work_tokens),
                    ..EventSearchFilters::default()
                },
                8,
            )
            .unwrap()
            .len(),
        1
    );
    drop(failed_index);

    let mut succeeded_report =
        ctx_history_capture::discover_provider_sources_for_provider_with_context(
            &replacement,
            CaptureProvider::Claude,
        );
    succeeded_report
        .sources
        .push(configured_provider_source_for_path(
            CaptureProvider::Codex,
            peer_sessions,
            "peer",
            peer_home,
            "codex-sessions",
        ));
    let succeeded_publication = refresh_all_provider_sources(
        &replacement,
        succeeded_report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let succeeded = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(
        predecessor_routes
            .iter()
            .all(|route| succeeded.manifest().source_route(route).is_none()),
        "predecessor routes {:?} remain in {:?}; replacement root is {:?}",
        predecessor_routes,
        succeeded
            .manifest()
            .source_routes()
            .iter()
            .map(|route| route.route_identity().clone())
            .collect::<Vec<_>>(),
        succeeded.manifest().provider_root("work")
    );
    assert!(succeeded
        .complete_lexical_search("atomicpredecessorhistory", 8)
        .unwrap()
        .is_empty());
    assert!(succeeded_publication
        .route_results
        .iter()
        .all(|result| result.outcome.is_success()));
    assert_eq!(
        succeeded
            .manifest()
            .provider_root("work")
            .unwrap()
            .definition()
            .provider,
        CaptureProvider::Claude
    );
    let work_tokens = succeeded
        .manifest()
        .provider_root_source_tokens(&["work".to_owned()], &[])
        .unwrap();
    assert_eq!(
        succeeded
            .complete_filtered_lexical_search(
                "atomicpredecessorhistory",
                &EventSearchFilters {
                    allowed_source_keys: Some(work_tokens),
                    ..EventSearchFilters::default()
                },
                8,
            )
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn failed_same_provider_kind_replacement_keeps_predecessor_until_success() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let legacy_home = fixture.join("openhands-legacy");
    write_openhands_legacy_message(&legacy_home, "atomicopenhandslegacy");
    let definition = |path, kind| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::OpenHands,
        path,
        group: Some("work-group".to_owned()),
        kind: Some(kind),
    };
    let initial = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(
            legacy_home.clone(),
            ProviderRootKind::OpenHandsLegacyPersistence,
        )]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial,
        ctx_history_capture::discover_provider_sources_for_provider_with_context(
            &initial,
            CaptureProvider::OpenHands,
        ),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let predecessor = VerifiedIndex::open_pinned(&index_root).unwrap();
    let predecessor_route = predecessor
        .manifest()
        .provider_root("work")
        .unwrap()
        .routes()[0]
        .clone();
    assert_eq!(
        predecessor
            .complete_lexical_search("atomicopenhandslegacy", 8)
            .unwrap()
            .len(),
        1
    );
    drop(predecessor);

    let current_home = fixture.join("openhands-current");
    write_openhands_current_message(&current_home, "atomicopenhandscurrent");
    let replacement = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(
            current_home,
            ProviderRootKind::OpenHandsCurrentConversations,
        )]);
    let mut failed_report =
        ctx_history_capture::discover_provider_sources_for_provider_with_context(
            &replacement,
            CaptureProvider::OpenHands,
        );
    assert_eq!(failed_report.sources.len(), 1);
    failed_report.sources[0].status = ProviderSourceStatus::Unsupported;
    failed_report.sources[0].unsupported_reason = Some("injected kind replacement failure");
    refresh_all_provider_sources(
        &replacement,
        failed_report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let failed = VerifiedIndex::open_pinned(&index_root).unwrap();
    let failed_root = failed.manifest().provider_root("work").unwrap();
    assert_eq!(
        failed_root.definition().kind,
        Some(ProviderRootKind::OpenHandsLegacyPersistence)
    );
    assert_eq!(
        failed_root.routes(),
        std::slice::from_ref(&predecessor_route)
    );
    assert_eq!(
        failed
            .complete_lexical_search("atomicopenhandslegacy", 8)
            .unwrap()
            .len(),
        1
    );
    drop(failed);

    refresh_all_provider_sources(
        &replacement,
        ctx_history_capture::discover_provider_sources_for_provider_with_context(
            &replacement,
            CaptureProvider::OpenHands,
        ),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let succeeded = VerifiedIndex::open_pinned(&index_root).unwrap();
    let succeeded_root = succeeded.manifest().provider_root("work").unwrap();
    assert_eq!(
        succeeded_root.definition().kind,
        Some(ProviderRootKind::OpenHandsCurrentConversations)
    );
    assert!(succeeded
        .manifest()
        .source_route(&predecessor_route)
        .is_none());
    assert!(succeeded
        .complete_lexical_search("atomicopenhandslegacy", 8)
        .unwrap()
        .is_empty());
    assert_eq!(
        succeeded
            .complete_lexical_search("atomicopenhandscurrent", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn compound_same_name_replacement_waits_for_every_successor_in_either_route_order() {
    for reverse_successor_order in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let fixture = fs::canonicalize(temp.path()).unwrap();
        let data_root = fixture.join("data");
        let index_root = source_backed_index_root(&data_root);
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        let (_, _, discovery) = discovery_fixture(&fixture);
        let predecessor_home = fixture.join("claude-predecessor");
        let predecessor_session = predecessor_home.join("projects/work/session.jsonl");
        write_claude_message(&predecessor_session, "session", "compoundnamedpredecessor");
        let peer_home = fixture.join("peer-home");
        let peer_session = peer_home.join("projects/peer/session.jsonl");
        write_claude_message(&peer_session, "session", "compoundpeerinitial");
        let root = |id: &str, provider, path| ctx_history_capture::ProviderRootDefinition {
            id: id.to_owned(),
            provider,
            path,
            group: Some(format!("{id}-group")),
            kind: None,
        };
        let initial = discovery
            .clone()
            .with_automatic_provider_discovery(false)
            .with_configured_provider_roots(vec![
                root("work", CaptureProvider::Claude, predecessor_home.clone()),
                root("peer", CaptureProvider::Claude, peer_home.clone()),
            ]);
        let initial_report =
            ctx_history_capture::discover_provider_sources_for_provider_with_context(
                &initial,
                CaptureProvider::Claude,
            );
        let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
        let initial_receipt = refresh_all_provider_sources(
            &initial,
            initial_report,
            StdDuration::ZERO,
            &data_root,
            &index_root,
            None,
            SourceBackedRefreshScope::All,
            &mut progress,
        )
        .unwrap();
        assert!(
            initial_receipt
                .route_results
                .iter()
                .all(|result| result.rejected_record_total == 0),
            "unexpected initial rejections: {:#?}",
            initial_receipt.route_results
        );
        let initial_index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let predecessor_root = initial_index.manifest().provider_root("work").unwrap();
        assert_eq!(
            predecessor_root.source_identity(),
            ProviderRootSourceIdentity::NamedV1
        );
        assert!(predecessor_root.exact_source_memberships().is_empty());
        let predecessor_routes = predecessor_root.routes().to_vec();
        let predecessor_tokens = initial_index
            .manifest()
            .provider_root_source_tokens(&["work".to_owned()], &[])
            .unwrap();
        assert_eq!(
            initial_index
                .complete_lexical_search("compoundnamedpredecessor", 8)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            initial_index
                .complete_filtered_lexical_search(
                    "compoundnamedpredecessor",
                    &EventSearchFilters {
                        allowed_source_keys: Some(predecessor_tokens.clone()),
                        ..EventSearchFilters::default()
                    },
                    8,
                )
                .unwrap()
                .len(),
            1
        );
        drop(initial_index);

        write_claude_message(&peer_session, "session", "compoundpeeradvanced");
        let successor_home = fixture.join("codex-successor");
        let successor_sessions = successor_home.join("sessions");
        let successor_history = successor_home.join("history.jsonl");
        write_codex_rollout(
            &successor_sessions,
            "019fb700-0000-7000-8000-000000000741",
            "compoundsessionsuccess",
        );
        write_codex_history(
            &successor_history,
            "compound-history-success",
            "compoundhistorysuccess",
        );
        let replacement = discovery
            .with_automatic_provider_discovery(false)
            .with_configured_provider_roots(vec![
                root("work", CaptureProvider::Codex, successor_home.clone()),
                root("peer", CaptureProvider::Claude, peer_home.clone()),
            ]);
        let successor_sources = || {
            let mut sources =
                ctx_history_capture::discover_provider_sources_for_provider_with_context(
                    &replacement,
                    CaptureProvider::Codex,
                )
                .sources;
            sources.retain(|source| {
                source.path == successor_sessions || source.path == successor_history
            });
            assert_eq!(sources.len(), 2);
            if reverse_successor_order {
                sources.reverse();
            }
            sources
        };
        let mut failed_sources = successor_sources();
        let failed_successor = failed_sources
            .iter_mut()
            .find(|source| source.path == successor_history)
            .unwrap();
        failed_successor.status = ProviderSourceStatus::Unsupported;
        failed_successor.unsupported_reason = Some("injected compound successor failure");
        failed_sources.insert(
            1,
            configured_provider_source_for_path(
                CaptureProvider::Claude,
                peer_home.join("projects"),
                "peer",
                peer_home.clone(),
                "claude-projects",
            ),
        );
        let failed = refresh_all_provider_sources(
            &replacement,
            DiscoveryReport {
                sources: failed_sources,
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
        assert!(failed
            .route_results
            .iter()
            .any(|result| result.outcome.is_success()));
        assert!(failed
            .route_results
            .iter()
            .any(|result| result.outcome.failure_class().is_some()));
        let failed_index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let failed_root = failed_index.manifest().provider_root("work").unwrap();
        assert_eq!(failed_root.definition().provider, CaptureProvider::Claude);
        assert_eq!(
            failed_root.source_identity(),
            ProviderRootSourceIdentity::NamedV1
        );
        assert!(predecessor_routes
            .iter()
            .all(|route| failed_root.routes().contains(route)));
        let failed_tokens = failed_index
            .manifest()
            .provider_root_source_tokens(&["work".to_owned()], &[])
            .unwrap();
        assert_eq!(failed_tokens, predecessor_tokens);
        assert_eq!(
            failed_index
                .complete_filtered_lexical_search(
                    "compoundnamedpredecessor",
                    &EventSearchFilters {
                        allowed_source_keys: Some(failed_tokens),
                        ..EventSearchFilters::default()
                    },
                    8,
                )
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            failed_index
                .complete_lexical_search("compoundpeeradvanced", 8)
                .unwrap()
                .len(),
            1
        );
        assert!(failed_index
            .complete_lexical_search("compoundsessionsuccess", 8)
            .unwrap()
            .is_empty());
        assert!(failed_index
            .complete_lexical_search("compoundhistorysuccess", 8)
            .unwrap()
            .is_empty());
        drop(failed_index);

        if !reverse_successor_order {
            let reverted_report =
                ctx_history_capture::discover_provider_sources_for_provider_with_context(
                    &initial,
                    CaptureProvider::Claude,
                );
            refresh_all_provider_sources(
                &initial,
                reverted_report,
                StdDuration::ZERO,
                &data_root,
                &index_root,
                None,
                SourceBackedRefreshScope::All,
                &mut progress,
            )
            .unwrap();
            let reverted = VerifiedIndex::open_pinned(&index_root).unwrap();
            assert!(reverted
                .complete_lexical_search("compoundsessionsuccess", 8)
                .unwrap()
                .is_empty());
            let reverted_tokens = reverted
                .manifest()
                .provider_root_source_tokens(&["work".to_owned()], &[])
                .unwrap();
            assert_eq!(reverted_tokens, predecessor_tokens);
            assert!(reverted
                .complete_filtered_lexical_search(
                    "compoundsessionsuccess",
                    &EventSearchFilters {
                        allowed_source_keys: Some(reverted_tokens),
                        ..EventSearchFilters::default()
                    },
                    8,
                )
                .unwrap()
                .is_empty());
            assert_eq!(
                reverted
                    .complete_lexical_search("compoundnamedpredecessor", 8)
                    .unwrap()
                    .len(),
                1
            );
        }

        let mut succeeded_sources = successor_sources();
        succeeded_sources.insert(
            1,
            configured_provider_source_for_path(
                CaptureProvider::Claude,
                peer_home.join("projects"),
                "peer",
                peer_home.clone(),
                "claude-projects",
            ),
        );
        let succeeded = refresh_all_provider_sources(
            &replacement,
            DiscoveryReport {
                sources: succeeded_sources,
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
        assert!(
            succeeded
                .route_results
                .iter()
                .all(|result| result.outcome.is_success()),
            "unexpected successor outcomes: {:#?}",
            succeeded.route_results
        );
        let succeeded_index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let succeeded_root = succeeded_index.manifest().provider_root("work").unwrap();
        assert_eq!(succeeded_root.definition().provider, CaptureProvider::Codex);
        assert!(succeeded_root.exact_source_memberships().is_empty());
        assert!(predecessor_routes
            .iter()
            .all(|route| !succeeded_root.routes().contains(route)));
        assert!(succeeded_index
            .complete_lexical_search("compoundnamedpredecessor", 8)
            .unwrap()
            .is_empty());
        assert_eq!(
            succeeded_index
                .complete_lexical_search("compoundsessionsuccess", 8)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            succeeded_index
                .complete_lexical_search("compoundhistorysuccess", 8)
                .unwrap()
                .len(),
            1
        );
    }
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
        kind: None,
    };
    let first_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(first_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &first_discovery,
        DiscoveryReport {
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Claude,
                first_projects,
                "work",
                first_home.clone(),
                "claude-projects",
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
    let first = VerifiedIndex::open_pinned(&index_root).unwrap();
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
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Claude,
                second_projects,
                "work",
                second_home.clone(),
                "claude-projects",
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

    let moved = VerifiedIndex::open_pinned(&index_root).unwrap();
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
        kind: None,
    };
    let first_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(first_home.clone())]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &first_discovery,
        DiscoveryReport {
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Codex,
                first_sessions,
                "work",
                first_home.clone(),
                "codex-sessions",
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
    let first = VerifiedIndex::open_pinned(&index_root).unwrap();
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
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Codex,
                second_sessions,
                "work",
                second_home.clone(),
                "codex-sessions",
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

    let moved = VerifiedIndex::open_pinned(&index_root).unwrap();
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
fn same_id_provider_replacement_retires_old_routes_and_publishes_the_new_root() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);

    let claude_home = fixture.join("claude-work");
    let claude_projects = claude_home.join("projects");
    let claude_session = claude_projects.join("project/session.jsonl");
    fs::create_dir_all(claude_session.parent().unwrap()).unwrap();
    fs::write(
        &claude_session,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "provider-replacement-claude-message",
                "sessionId": "019fb700-0000-7000-8000-000000000717",
                "message": {"role": "user", "content": "retiredproviderfixture"}
            })
        ),
    )
    .unwrap();
    let definition = |provider, path| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider,
        path,
        group: Some("work".to_owned()),
        kind: None,
    };
    let initial_discovery = discovery
        .clone()
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(
            CaptureProvider::Claude,
            claude_home.clone(),
        )]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial_discovery,
        DiscoveryReport {
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Claude,
                claude_projects,
                "work",
                claude_home,
                "claude-projects",
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

    let codex_home = fixture.join("codex-work");
    let codex_sessions = codex_home.join("sessions");
    fs::create_dir_all(&codex_sessions).unwrap();
    fs::write(
        codex_sessions.join("rollout.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-24T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000718",
                    "timestamp": "2026-08-24T00:00:00Z",
                    "cwd": "/repo/provider-replacement",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-24T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "replacementproviderfixture"}]
                }
            })
        ),
    )
    .unwrap();
    let replacement_discovery = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(
            CaptureProvider::Codex,
            codex_home.clone(),
        )]);
    refresh_all_provider_sources(
        &replacement_discovery,
        DiscoveryReport {
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Codex,
                codex_sessions,
                "work",
                codex_home,
                "codex-sessions",
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

    let replaced = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(replaced
        .complete_lexical_search("retiredproviderfixture", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        replaced
            .complete_lexical_search("replacementproviderfixture", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(replaced.manifest().source_routes().len(), 1);
    assert_eq!(replaced.manifest().provider_roots().len(), 1);
    assert_eq!(
        replaced.manifest().provider_roots()[0]
            .definition()
            .provider,
        CaptureProvider::Codex
    );
    assert_eq!(replaced.manifest().provider_roots()[0].routes().len(), 1);
}
