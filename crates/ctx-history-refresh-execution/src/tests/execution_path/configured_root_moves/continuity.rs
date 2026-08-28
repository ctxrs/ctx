use super::*;

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
            let automatic_source_key = VerifiedIndex::open_pinned(&index_root)
                .unwrap()
                .manifest()
                .sources[0]
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
                path: configured_home.clone(),
                group: Some("work".to_owned()),
                kind: None,
            };
            let configured_discovery = automatic_discovery
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![definition]);
            let configured_source = configured_provider_source_for_path(
                CaptureProvider::Claude,
                configured_projects,
                "work",
                configured_home,
                "claude-projects",
            );
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

            let published = VerifiedIndex::open_pinned(&index_root).unwrap();
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
fn watch_catalog_preserves_released_identity_across_path_and_group_replacement() {
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
    let definition = |path, group: &str| ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Claude,
        path,
        group: Some(group.to_owned()),
        kind: None,
    };
    let initial_discovery = discovery
        .with_env("CLAUDE_CONFIG_DIR", &released_home)
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![definition(released_home.clone(), "work")]);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial_discovery,
        DiscoveryReport {
            sources: vec![configured_provider_source_for_path(
                CaptureProvider::Claude,
                released_projects,
                "work",
                released_home.clone(),
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
    let published = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        published.manifest().provider_roots()[0].source_identity(),
        ProviderRootSourceIdentity::Released
    );
    let published_route = published.manifest().provider_roots()[0].routes()[0].clone();
    drop(published);

    let moved_home = fixture.join("claude-moved");
    fs::rename(&released_home, &moved_home).unwrap();
    let moved_discovery = initial_discovery
        .with_configured_provider_roots(vec![definition(moved_home, "renamed-group")]);
    ctx_history_index_format::reset_verification_activity();
    let catalog = source_backed_watch_catalog(&data_root, &moved_discovery).unwrap();
    let (_, logical_passes) = ctx_history_index_format::verification_activity();

    assert_eq!(logical_passes, 0);
    assert_eq!(
        catalog.route_ids().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([published_route])
    );
}

#[test]
fn released_overlap_root_move_remove_and_readd_preserves_exact_identity() {
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
            kind: None,
        };
        let initial_discovery = discovery
            .with_env("CLAUDE_CONFIG_DIR", &released_home)
            .with_automatic_provider_discovery(false)
            .with_configured_provider_roots(vec![definition(released_home.clone())]);
        let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
        refresh_all_provider_sources(
            &initial_discovery,
            DiscoveryReport {
                sources: vec![configured_provider_source_for_path(
                    CaptureProvider::Claude,
                    released_projects.clone(),
                    "work",
                    released_home.clone(),
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
            provider_source_for_path(CaptureProvider::Claude, recreated_projects.clone());
        let configured_source = configured_provider_source_for_path(
            CaptureProvider::Claude,
            moved_projects.clone(),
            "work",
            moved_home.clone(),
            "claude-projects",
        );
        let sources = if automatic_first {
            vec![automatic_source, configured_source]
        } else {
            vec![configured_source, automatic_source]
        };
        let moved_discovery = initial_discovery
            .with_automatic_provider_discovery(true)
            .with_configured_provider_roots(vec![definition(moved_home.clone())]);
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

        let published = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(published.manifest().sources.len(), 3);
        assert_eq!(published.manifest().source_routes().len(), 2);
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
                .complete_filtered_lexical_search("releasedfirstcanary", &work_filter, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            published
                .complete_filtered_lexical_search("releasedsecondcanary", &work_filter, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(published
            .complete_filtered_lexical_search("oldautomaticcanary", &work_filter, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            published
                .complete_lexical_search("oldautomaticcanary", 10)
                .unwrap()
                .len(),
            1
        );
        let published_identity = ["releasedfirstcanary", "releasedsecondcanary"].map(|marker| {
            let hits = published.complete_lexical_search(marker, 10).unwrap();
            assert_eq!(hits.len(), 1, "{marker}");
            let event = exact_event(&published, &hits[0]);
            serde_json::to_vec(&(event.source, event.session_id, event.event_id)).unwrap()
        });
        drop(published);

        let removed_discovery = moved_discovery
            .clone()
            .with_configured_provider_roots(Vec::new());
        refresh_all_provider_sources(
            &removed_discovery,
            DiscoveryReport {
                sources: vec![provider_source_for_path(
                    CaptureProvider::Claude,
                    recreated_projects.clone(),
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
        let removed = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert!(removed.manifest().provider_roots().is_empty());
        assert_eq!(removed.manifest().source_routes().len(), 1);
        assert_eq!(
            removed
                .complete_lexical_search("oldautomaticcanary", 10)
                .unwrap()
                .len(),
            1
        );
        assert!(removed
            .complete_lexical_search("releasedfirstcanary", 10)
            .unwrap()
            .is_empty());
        drop(removed);

        let rejoined_discovery =
            removed_discovery.with_configured_provider_roots(vec![definition(moved_home.clone())]);
        let automatic_source =
            provider_source_for_path(CaptureProvider::Claude, recreated_projects.clone());
        let configured_source = configured_provider_source_for_path(
            CaptureProvider::Claude,
            moved_projects.clone(),
            "work",
            moved_home,
            "claude-projects",
        );
        let sources = if automatic_first {
            vec![automatic_source, configured_source]
        } else {
            vec![configured_source, automatic_source]
        };
        refresh_all_provider_sources(
            &rejoined_discovery,
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
        let rejoined = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(rejoined.manifest().source_routes().len(), 2);
        assert_eq!(rejoined.manifest().provider_roots().len(), 1);
        assert_eq!(
            rejoined.manifest().provider_roots()[0].source_identity(),
            ProviderRootSourceIdentity::Released
        );
        let allowed_source_keys = rejoined
            .manifest()
            .provider_root_source_tokens(&["work".to_owned()], &[])
            .unwrap();
        let work_filter = EventSearchFilters {
            allowed_source_keys: Some(allowed_source_keys),
            ..EventSearchFilters::default()
        };
        assert_eq!(
            rejoined
                .complete_filtered_lexical_search("releasedfirstcanary", &work_filter, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(rejoined
            .complete_filtered_lexical_search("oldautomaticcanary", &work_filter, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            rejoined
                .complete_lexical_search("oldautomaticcanary", 10)
                .unwrap()
                .len(),
            1
        );
        for (marker, expected) in ["releasedfirstcanary", "releasedsecondcanary"]
            .into_iter()
            .zip(published_identity)
        {
            let hits = rejoined.complete_lexical_search(marker, 10).unwrap();
            assert_eq!(hits.len(), 1, "{marker}");
            let event = exact_event(&rejoined, &hits[0]);
            assert_eq!(
                serde_json::to_vec(&(event.source, event.session_id, event.event_id)).unwrap(),
                expected,
                "{marker} source/session/event identity"
            );
        }
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
        kind: None,
    };
    let peer_definition = definition("peer", peer_home.clone());
    let initial_discovery = discovery
        .with_env("CLAUDE_CONFIG_DIR", &automatic_home)
        .with_configured_provider_roots(vec![peer_definition.clone()]);
    let automatic_source =
        provider_source_for_path(CaptureProvider::Claude, automatic_projects.clone());
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &initial_discovery,
        DiscoveryReport {
            sources: vec![
                automatic_source,
                configured_provider_source_for_path(
                    CaptureProvider::Claude,
                    peer_projects.clone(),
                    "peer",
                    peer_home.clone(),
                    "claude-projects",
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
                configured_provider_source_for_path(
                    CaptureProvider::Claude,
                    automatic_projects,
                    "automatic",
                    automatic_home,
                    "claude-projects",
                ),
                configured_provider_source_for_path(
                    CaptureProvider::Claude,
                    peer_projects,
                    "peer",
                    peer_home,
                    "claude-projects",
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

    assert!(!publication.route_results.is_empty());
    let published = VerifiedIndex::open_pinned(&index_root).unwrap();
    let automatic_root = published
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "automatic")
        .unwrap();
    assert_eq!(
        automatic_root.source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
    assert!(automatic_root.routes().is_empty());
    assert_eq!(
        published
            .complete_lexical_search("retainedautomaticfixture", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        published
            .complete_lexical_search("advancedpeerfixture", 10)
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn unchanged_symlinked_configured_root_retains_history_while_peer_advances() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let retained_home = fixture.join("claude-retained");
    let peer_home = fixture.join("claude-peer");
    let retained_session =
        retained_home.join("projects/project/019fb700-0000-7000-8000-000000000717.jsonl");
    let peer_session =
        peer_home.join("projects/project/019fb700-0000-7000-8000-000000000718.jsonl");
    fs::create_dir_all(retained_session.parent().unwrap()).unwrap();
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
        &retained_session,
        claude_message(
            "019fb710-0000-7000-8000-000000000717",
            "019fb700-0000-7000-8000-000000000717",
            "retainedsymlinkfixture",
        ),
    )
    .unwrap();
    fs::write(
        &peer_session,
        claude_message(
            "019fb710-0000-7000-8000-000000000718",
            "019fb700-0000-7000-8000-000000000718",
            "peer initial",
        ),
    )
    .unwrap();
    let definition = |id: &str, path: PathBuf| ctx_history_capture::ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Claude,
        path,
        group: Some(id.to_owned()),
        kind: None,
    };
    let definitions = vec![
        definition("retained", retained_home.clone()),
        definition("peer", peer_home.clone()),
    ];
    let discovery = discovery
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(definitions);
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &discovery,
        ctx_history_capture::discover_provider_sources_with_context(&discovery),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let first = VerifiedIndex::open_pinned(&index_root).unwrap();
    let retained_routes = first
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "retained")
        .unwrap()
        .routes()
        .to_vec();
    drop(first);

    let displaced_home = fixture.join("claude-retained-displaced");
    fs::rename(&retained_home, &displaced_home).unwrap();
    symlink(&displaced_home, &retained_home).unwrap();
    fs::write(
        &peer_session,
        format!(
            "{}{}",
            claude_message(
                "019fb710-0000-7000-8000-000000000718",
                "019fb700-0000-7000-8000-000000000718",
                "peer initial",
            ),
            claude_message(
                "019fb710-0000-7000-8000-000000000719",
                "019fb700-0000-7000-8000-000000000718",
                "advancedsymlinkpeerfixture",
            )
        ),
    )
    .unwrap();
    let report = ctx_history_capture::discover_provider_sources_with_context(&discovery);
    assert!(report.issues.iter().any(|issue| {
        issue.provider == CaptureProvider::Claude
            && issue.kind == DiscoveryIssueKind::SelectorUnreconstructible
    }));
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
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

    let published = VerifiedIndex::open_pinned(&index_root).unwrap();
    let retained_root = published
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "retained")
        .unwrap();
    assert_eq!(retained_root.routes(), retained_routes);
    assert_eq!(
        published
            .complete_lexical_search("retainedsymlinkfixture", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        published
            .complete_lexical_search("advancedsymlinkpeerfixture", 10)
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
            path: home.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        }]);
    let configured_source = configured_provider_source_for_path(
        CaptureProvider::Claude,
        source.path.clone(),
        "personal",
        home,
        "claude-projects",
    );
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    refresh_all_provider_sources(
        &configured_discovery,
        DiscoveryReport {
            sources: vec![configured_source],
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
    let configured_source_key = VerifiedIndex::open_pinned(&index_root)
        .unwrap()
        .manifest()
        .sources[0]
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

    let published = VerifiedIndex::open_pinned(&index_root).unwrap();
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
