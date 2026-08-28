use super::*;
use ctx_history_capture_model::ProviderRootSourceIdentity;

#[test]
fn partial_codex_home_adopts_released_identity_without_duplicate_records() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let codex_home = fixture.join("partial-codex-home");
    let sessions = codex_home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000098";
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("partial Codex released identity marker")],
    );
    let context = DiscoveryContext::new(
        &fixture,
        &fixture,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("CODEX_HOME", codex_home.as_os_str());

    let automatic = build_discovered_codex_registry(&context, &fixture.join("automatic-data"));
    let automatic_index = fixture.join("automatic-index");
    refresh_source_backed_generation(&automatic_index, &automatic.registry, writer_options())
        .unwrap();
    let automatic_records = serde_json::to_vec(&records_for(
        &VerifiedIndex::open_pinned(&automatic_index).unwrap(),
        native_session_id,
    ))
    .unwrap();

    for automatic_enabled in [true, false] {
        let configured = build_discovered_codex_registry(
            &context
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![ProviderRootDefinition {
                    id: "personal".to_owned(),
                    provider: CaptureProvider::Codex,
                    path: codex_home.clone(),
                    group: Some("personal".to_owned()),
                    kind: None,
                }]),
            &fixture.join(format!("configured-data-{automatic_enabled}")),
        );
        let (_, _, roots) = configured.registry.applied_provider_roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0].source_identity(),
            ProviderRootSourceIdentity::Released
        );

        let index_root = fixture.join(format!("configured-index-{automatic_enabled}"));
        refresh_source_backed_generation(&index_root, &configured.registry, writer_options())
            .unwrap();
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(index.manifest().sources.len(), 1);
        assert_eq!(
            serde_json::to_vec(&records_for(&index, native_session_id)).unwrap(),
            automatic_records
        );
    }
}

#[test]
fn configured_codex_homes_with_the_same_native_session_publish_independent_sources() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let personal = fixture.join("personal/sessions");
    let personal_archive = fixture.join("personal/archived_sessions");
    let work = fixture.join("work/sessions");
    fs::create_dir_all(&personal).unwrap();
    fs::create_dir_all(&personal_archive).unwrap();
    fs::create_dir_all(&work).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000099";
    write_session(
        &personal,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("personal pineapple marker")],
    );
    write_session(
        &work,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("work kumquat marker")],
    );
    write_session(
        &personal_archive,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("personal archived duplicate should coalesce")],
    );
    let personal_home = fixture.join("personal");
    let work_home = fixture.join("work");
    let context = DiscoveryContext::new(
        &fixture,
        &fixture,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![
        ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: personal_home.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: work_home.clone(),
            group: Some("work".to_owned()),
            kind: None,
        },
    ]);
    let mut sources = Vec::new();
    for (root, root_id, root_path, route_role) in [
        (&personal, "personal", &personal_home, "codex-sessions"),
        (
            &personal_archive,
            "personal",
            &personal_home,
            "codex-archived-sessions",
        ),
        (&work, "work", &work_home, "codex-sessions"),
    ] {
        let mut source = fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            root,
        );
        source.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
            root_id: root_id.to_owned(),
            root_path: root_path.clone(),
            route_role: ProviderRouteRole::from_static(route_role),
            automatic_route_role: None,
        };
        sources.push(source);
    }
    let probes = test_provider_probes();
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &probes,
        &context,
        &fixture.join("data"),
        DiscoveryReport {
            sources,
            issues: Vec::new(),
        },
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    let registry = build.registry;

    let index_root = fixture.join("index");
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(
        receipt.failed_routes.is_empty(),
        "{:?}",
        receipt.failed_routes
    );
    assert_eq!(receipt.successful_route_ids.len(), 3);

    let archive_refresh = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [route_identity(&registry, &personal_archive)],
    )
    .unwrap();
    assert!(
        archive_refresh.failed_routes.is_empty(),
        "{:?}",
        archive_refresh.failed_routes
    );

    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let codex_sources = index
        .manifest()
        .sources
        .iter()
        .filter(|source| source.observation().source().provider() == "codex")
        .collect::<Vec<_>>();
    assert_eq!(codex_sources.len(), 2);
    let mut bodies = codex_sources
        .into_iter()
        .flat_map(|source| {
            index
                .core_source_event_page(source.observation().source(), None, 16)
                .unwrap()
                .items
                .into_iter()
                .filter_map(|item| item.core_record.content.normalized_body)
        })
        .collect::<Vec<_>>();
    bodies.sort();
    assert!(bodies
        .iter()
        .any(|body| body == "personal pineapple marker"));
    assert!(bodies.iter().any(|body| body == "work kumquat marker"));
    assert!(!bodies
        .iter()
        .any(|body| body == "personal archived duplicate should coalesce"));
}

#[test]
fn unavailable_configured_codex_home_carries_only_itself_while_peer_refreshes() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let personal_home = fixture.join("personal-codex-home");
    let work_home = fixture.join("work-codex-home");
    let personal_sessions = personal_home.join("sessions");
    let work_sessions = work_home.join("sessions");
    fs::create_dir_all(&personal_sessions).unwrap();
    fs::create_dir_all(&work_sessions).unwrap();
    let personal_session_id = "019fb000-0000-7000-8000-000000000081";
    let work_session_id = "019fb000-0000-7000-8000-000000000082";
    write_session(
        &personal_sessions,
        personal_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("configured Codex personal initial")],
    );
    write_session(
        &work_sessions,
        work_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("configured Codex work retained")],
    );
    let context = DiscoveryContext::new(
        &fixture,
        &fixture,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: personal_home.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: work_home.clone(),
            group: Some("work".to_owned()),
            kind: None,
        },
    ]);
    let data_root = fixture.join("data");
    let initial = build_discovered_codex_registry(&context, &data_root);
    assert_eq!(
        initial
            .issues
            .iter()
            .filter(|issue| matches!(
                issue,
                SourceBackedAutomaticRegistryIssue::Unavailable {
                    reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                        ProviderSourceStatus::Missing
                    ),
                    ..
                }
            ))
            .count(),
        4,
        "each healthy configured home reports its absent archive and prompt-history routes"
    );
    let index_root = fixture.join("index");
    let initial_receipt =
        refresh_source_backed_generation(&index_root, &initial.registry, writer_options()).unwrap();
    assert_eq!(initial_receipt.failed_routes.len(), 4);
    assert!(initial_receipt.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::Unavailable && !failure.carried_forward
    }));

    append_event(
        &session_path(&personal_sessions, personal_session_id),
        message("configured Codex personal refreshed"),
    );
    let displaced_work_home = fixture.join("work-codex-displaced");
    fs::rename(&work_home, &displaced_work_home).unwrap();
    fs::write(&work_home, b"temporarily not a directory").unwrap();
    let mut current = build_discovered_codex_registry(&context, &data_root);
    assert_eq!(
        current
            .issues
            .iter()
            .filter(|issue| matches!(issue, SourceBackedAutomaticRegistryIssue::Discovery(_)))
            .count(),
        1,
        "equivalent physical selector issues are deduplicated while all routes are retained"
    );
    assert_eq!(
        current
            .issues
            .iter()
            .filter(|issue| matches!(
                issue,
                SourceBackedAutomaticRegistryIssue::Unavailable {
                    reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                        ProviderSourceStatus::Missing
                    ),
                    ..
                }
            ))
            .count(),
        2,
        "the healthy peer retains typed absence for its optional routes"
    );
    let retained = VerifiedIndex::open_pinned(&index_root).unwrap();
    current
        .registry
        .retain_unavailable_provider_root_routes(retained.manifest().provider_roots())
        .unwrap();
    fs::remove_file(&work_home).unwrap();
    fs::rename(&displaced_work_home, &work_home).unwrap();

    let receipt =
        refresh_source_backed_generation(&index_root, &current.registry, writer_options()).unwrap();
    // A root-level wrong-kind discovery failure is route-less; the retained
    // manifest restores the exact prior Codex membership instead of inventing
    // child failures for a root that cannot be safely expanded.
    assert_eq!(receipt.failed_routes.len(), 2);
    assert!(receipt.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::Unavailable && !failure.carried_forward
    }));
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(source_records_contain(
        &index,
        personal_session_id,
        "configured Codex personal refreshed"
    ));
    assert!(source_records_contain(
        &index,
        work_session_id,
        "configured Codex work retained"
    ));
    let work_root = index
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "work")
        .unwrap();
    assert_eq!(
        work_root.routes().len(),
        1,
        "retention restores the exact previously published session route"
    );
}

#[test]
fn cold_unavailable_configured_codex_home_does_not_block_healthy_peer() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let personal_home = fixture.join("personal-codex-cold");
    let work_home = fixture.join("work-codex-cold");
    let personal_sessions = personal_home.join("sessions");
    fs::create_dir_all(&personal_sessions).unwrap();
    fs::write(&work_home, b"temporarily not a directory").unwrap();
    let personal_session_id = "019fb000-0000-7000-8000-000000000083";
    write_session(
        &personal_sessions,
        personal_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("configured Codex personal cold")],
    );
    let context = DiscoveryContext::new(
        &fixture,
        &fixture,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: personal_home,
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: work_home,
            group: Some("work".to_owned()),
            kind: None,
        },
    ]);
    let build = build_discovered_codex_registry(&context, &fixture.join("data"));
    assert_eq!(
        build
            .issues
            .iter()
            .filter(|issue| matches!(issue, SourceBackedAutomaticRegistryIssue::Discovery(_)))
            .count(),
        1,
        "wrong-kind configured roots surface one root-level discovery issue"
    );
    assert_eq!(
        build
            .issues
            .iter()
            .filter(|issue| matches!(
                issue,
                SourceBackedAutomaticRegistryIssue::Unavailable {
                    reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                        ProviderSourceStatus::Missing
                    ),
                    ..
                }
            ))
            .count(),
        2,
        "the healthy home reports only its absent optional routes"
    );
    let index_root = fixture.join("index");
    let receipt =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options()).unwrap();
    assert_eq!(receipt.failed_routes.len(), 2);
    assert!(receipt.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::Unavailable && !failure.carried_forward
    }));
    assert_eq!(
        receipt.successful_route_ids.len(),
        3,
        "the inferred missing defaults and healthy named home remain independent"
    );
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(source_records_contain(
        &index,
        personal_session_id,
        "configured Codex personal cold"
    ));
    let work_root = index
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "work")
        .unwrap();
    assert!(work_root.routes().is_empty());
}

#[test]
fn codex_subagent_preserves_provider_root_session_in_core_records() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root_native_session_id = "019fb000-0000-7000-8000-000000000081";
    let child_native_session_id = "019fb000-0000-7000-8000-000000000082";
    let metadata = serde_json::json!({
        "timestamp": "2026-08-09T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": child_native_session_id,
            "session_id": root_native_session_id,
            "parent_thread_id": root_native_session_id,
            "timestamp": "2026-08-09T12:00:00Z",
            "cwd": "/tmp/codex-child-independence",
            "source": {
                "subagent": {
                    "thread_spawn": {
                        "depth": 1,
                        "parent_thread_id": root_native_session_id
                    }
                }
            }
        }
    });
    fs::write(
        session_path(&sessions, child_native_session_id),
        jsonl_bytes([metadata, message("providerrootsessionmarker")]),
    )
    .unwrap();

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let records = records_for(&index, child_native_session_id);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(
        record.agent_scope,
        Some(ctx_history_core::AgentScope::Subagent)
    );
    assert!(record.parent_session_id.is_some());
    assert_eq!(record.root_session_id, record.parent_session_id);
    assert_eq!(record.parser_revision, CURRENT_PARSER_REVISION);
}
