use super::*;

#[test]
fn equal_automatic_legacy_and_configured_current_roots_publish_each_event_once() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let selected = temp.path().join("shared-openhands-root");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_message(
        &selected,
        "legacy-witness",
        "legacy-event",
        "legacy witness",
    );
    write_current_message_at_root(
        &selected,
        "current-session",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("OH_PERSISTENCE_DIR", selected.as_os_str())
    .with_env("OPENHANDS_CONVERSATIONS_DIR", selected.as_os_str())
    .with_configured_provider_roots(vec![ProviderRootDefinition {
        id: "configured-current".to_owned(),
        provider: CaptureProvider::OpenHands,
        path: selected,
        group: None,
        kind: Some(ProviderRootKind::OpenHandsCurrentConversations),
    }]);
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        &context,
        CaptureProvider::OpenHands,
    );
    let data_root = temp.path().join("ctx-data");
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &crate::test_provider_probes(),
        &context,
        &data_root,
        report,
    );

    assert_eq!(build.executable_route_count(), 1);
    assert!(matches!(
        build.issues.as_slice(),
        [SourceBackedAutomaticRegistryIssue::Discovery(
            DiscoveryIssue {
                kind: ctx_history_source_discovery::DiscoveryIssueKind::ConfiguredRootConflict,
                ..
            }
        )]
    ));
    let index = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index, &build.registry, WriterOptions::default())
            .unwrap();
    let bodies = indexed_bodies(&index, &receipt);
    assert_eq!(
        bodies.iter().filter(|body| *body == "current body").count(),
        1
    );
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 2);
}

#[test]
fn current_cli_automatic_discovery_covers_append_rewrite_and_conversation_deletion() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let profile = home.join(".openhands");
    let conversations = profile.join("conversations");
    let first = write_current_message(
        &profile,
        "conversation-lifecycle",
        1,
        "event-a",
        "one",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("automatic-index");

    let cold_registry = discovered_openhands_registry(&context, &data_root);
    let current_route = cold_registry
        .routes()
        .find(|route| route.source.provider == CaptureProvider::OpenHands)
        .unwrap();
    assert_eq!(current_route.source.path, conversations);
    assert_eq!(current_route.source.status, ProviderSourceStatus::Available);
    let cold =
        refresh_source_backed_generation(&index, &cold_registry, WriterOptions::default()).unwrap();
    assert_eq!(indexed_bodies(&index, &cold), vec!["one"]);

    write_current_message(
        &profile,
        "conversation-lifecycle",
        2,
        "event-b",
        "two",
        "2026-07-28T12:00:01Z",
    );
    let appended_registry = discovered_openhands_registry(&context, &data_root);
    let appended =
        refresh_source_backed_generation(&index, &appended_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(indexed_bodies(&index, &appended), vec!["one", "two"]);

    fs::write(
        &first,
        serde_json::to_vec(&message("event-a", "one rewritten")).unwrap(),
    )
    .unwrap();
    let rewritten_registry = discovered_openhands_registry(&context, &data_root);
    let rewritten =
        refresh_source_backed_generation(&index, &rewritten_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(
        indexed_bodies(&index, &rewritten),
        vec!["one rewritten", "two"]
    );

    fs::remove_dir_all(conversations.join("conversation-lifecycle")).unwrap();
    let deleted_registry = discovered_openhands_registry(&context, &data_root);
    let current_route = deleted_registry
        .routes()
        .find(|route| route.source.provider == CaptureProvider::OpenHands)
        .unwrap();
    assert_eq!(current_route.source.path, conversations);
    assert_eq!(current_route.source.status, ProviderSourceStatus::Empty);
    refresh_source_backed_generation(&index, &deleted_registry, WriterOptions::default()).unwrap();
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 0);
}

#[test]
fn disjoint_legacy_and_current_routes_coexist_and_delete_independently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let legacy_root = temp.path().join("legacy-profile");
    let current_profile = temp.path().join("current-profile");
    let conversations = current_profile.join("conversations");
    write_message(
        &legacy_root,
        "conversation-legacy",
        "legacy-event",
        "legacy body",
    );
    write_current_message(
        &current_profile,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("OH_PERSISTENCE_DIR", legacy_root.as_os_str().to_owned())
    .with_env(
        "OPENHANDS_CONVERSATIONS_DIR",
        conversations.as_os_str().to_owned(),
    );
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("coexistence-index");

    let cold_registry = discovered_openhands_registry(&context, &data_root);
    let cold_routes = openhands_route_facts(&cold_registry);
    assert_eq!(cold_routes.len(), 2);
    assert_eq!(
        cold_routes
            .iter()
            .map(|(_, format, authority)| (*format, *authority))
            .collect::<Vec<_>>(),
        vec![
            (
                OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
                SourceBackedSelectorAuthority::CatalogLineage,
            ),
            (
                "openhands_file_events",
                SourceBackedSelectorAuthority::DiscoveredWinner,
            ),
        ]
    );
    assert_ne!(cold_routes[0].0, cold_routes[1].0);
    let cold =
        refresh_source_backed_generation(&index, &cold_registry, WriterOptions::default()).unwrap();
    assert_eq!(
        indexed_bodies(&index, &cold),
        vec!["current body", "legacy body"]
    );

    fs::remove_dir_all(
        legacy_root
            .join("v1_conversations")
            .join("conversation-legacy"),
    )
    .unwrap();
    let legacy_deleted_registry = discovered_openhands_registry(&context, &data_root);
    assert_eq!(openhands_route_facts(&legacy_deleted_registry), cold_routes);
    let legacy_deleted = refresh_source_backed_generation(
        &index,
        &legacy_deleted_registry,
        WriterOptions::default(),
    )
    .unwrap();
    assert_eq!(
        indexed_bodies(&index, &legacy_deleted),
        vec!["current body"]
    );

    write_message(
        &legacy_root,
        "conversation-legacy",
        "legacy-event",
        "legacy body",
    );
    let restored_registry = discovered_openhands_registry(&context, &data_root);
    let restored =
        refresh_source_backed_generation(&index, &restored_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(
        indexed_bodies(&index, &restored),
        vec!["current body", "legacy body"]
    );

    fs::remove_dir_all(conversations.join("conversation-current")).unwrap();
    let current_deleted_registry = discovered_openhands_registry(&context, &data_root);
    assert_eq!(
        openhands_route_facts(&current_deleted_registry),
        cold_routes
    );
    let current_deleted = refresh_source_backed_generation(
        &index,
        &current_deleted_registry,
        WriterOptions::default(),
    )
    .unwrap();
    assert_eq!(
        indexed_bodies(&index, &current_deleted),
        vec!["legacy body"]
    );
}

#[test]
fn openhands_append_rewrite_delete_and_exact_replay_converge() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    let first = write_message(&selected, "conversation", "event-1", "one");
    let index = temp.path().join("index");
    let registry = registry(&selected);
    let cold =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    let source_id = cold.sources[0].observation().source().identity().digest();

    let second = write_message(&selected, "conversation", "event-2", "two");
    let appended =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(indexed_bodies(&index, &appended), vec!["one", "two"]);

    fs::write(
        &first,
        serde_json::to_vec(&message("event-1", "one rewritten")).unwrap(),
    )
    .unwrap();
    let rewritten =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(
        indexed_bodies(&index, &rewritten),
        vec!["one rewritten", "two"]
    );

    fs::remove_file(second).unwrap();
    let deleted =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(indexed_bodies(&index, &deleted), vec!["one rewritten"]);
    assert_eq!(
        deleted.sources[0]
            .observation()
            .source()
            .identity()
            .digest(),
        source_id
    );

    let replay =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(replay.commit.generation_id, deleted.commit.generation_id);
    assert_eq!(replay.sources, deleted.sources);
    assert_eq!(indexed_bodies(&index, &replay), vec!["one rewritten"]);
}
