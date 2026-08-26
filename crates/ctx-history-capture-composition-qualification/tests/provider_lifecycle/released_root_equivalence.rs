use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRootKind, ProviderRootSourceIdentity,
};
use ctx_history_index::{
    policy::AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS, source_token, EventSearchFilters,
};
use rusqlite::Connection;

use super::*;

struct ProviderFixture {
    context: DiscoveryContext,
    root: ProviderRootDefinition,
    marker: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
struct RouteBytes {
    route: String,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    provider: CaptureProvider,
    source_format: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
struct PublicationBytes {
    sources: Vec<Vec<u8>>,
    aggregates: Vec<u8>,
    source_routes: Vec<u8>,
    route_controls: Vec<u8>,
    records: Vec<Vec<u8>>,
}

fn copy_fixture(relative: &str, destination: &Path) {
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::copy(
        crate::test_support_paths::capture_repo_root()
            .join("tests/fixtures/provider-history")
            .join(relative),
        destination,
    )
    .unwrap();
}

fn create_hermes_fixture(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 ended_at real,
                 message_count integer default 0,
                 cwd text,
                 git_branch text,
                 git_repo_root text
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null,
                 active integer not null default 1,
                 compacted integer not null default 0
             );
             insert into sessions
                 (id, source, parent_session_id, started_at, message_count, cwd)
                 values ('hermes-equivalence', 'acp', null, 1782259200.0, 1, '/repo');
             insert into messages (id, session_id, role, content, timestamp)
                 values (1, 'hermes-equivalence', 'user',
                         'hermes released equivalence oracle', 1782259201.0);",
        )
        .unwrap();
}

fn write_openhands_legacy_message(root: &Path, conversation: &str, event: &str, body: &str) {
    let path = root
        .join("v1_conversations")
        .join(conversation)
        .join(format!("{event}.json"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "id": event,
            "source": "user",
            "message": body,
            "timestamp": "2026-08-25T12:00:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_openhands_current_message(root: &Path, conversation: &str, event: &str, body: &str) {
    let path = root
        .join(conversation)
        .join("events")
        .join(format!("event-00001-{event}.json"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "id": event,
            "source": "user",
            "message": body,
            "timestamp": "2026-08-25T12:00:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
}

fn provider_fixture(root: &Path, provider: CaptureProvider) -> ProviderFixture {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let mut context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let (path, marker) = match provider {
        CaptureProvider::OpenClaw => {
            let state = root.join("openclaw-state");
            let sessions = state.join("agents/alpha/sessions");
            fs::create_dir_all(&sessions).unwrap();
            fs::write(
                state.join("openclaw.json"),
                b"{agents:{list:[{id:'alpha'}]}}",
            )
            .unwrap();
            fs::write(
                sessions.join("equivalence.jsonl"),
                concat!(
                    "{\"type\":\"message\",\"id\":\"openclaw-equivalence\",",
                    "\"timestamp\":\"2026-06-24T12:00:00Z\",",
                    "\"message\":{\"role\":\"user\",",
                    "\"content\":\"openclaw released equivalence oracle\"}}\n"
                ),
            )
            .unwrap();
            context = context.with_env("OPENCLAW_STATE_DIR", state.as_os_str());
            (state, "openclaw released equivalence oracle")
        }
        CaptureProvider::Hermes => {
            let database = home.join(".hermes/state.db");
            create_hermes_fixture(&database);
            (database, "hermes released equivalence oracle")
        }
        CaptureProvider::Crush => {
            let database = cwd.join(".crush/crush.db");
            fs::create_dir_all(cwd.join(".git")).unwrap();
            copy_fixture("crush/v1/crush.db", &database);
            (database, "crush sqlite search oracle request")
        }
        CaptureProvider::Goose => {
            let goose_root = root.join("goose-root");
            let database = goose_root.join("data/sessions/sessions.db");
            copy_fixture("goose/v15/sessions.db", &database);
            context = context.with_env("GOOSE_PATH_ROOT", goose_root.as_os_str());
            (database, "goose sqlite search oracle request")
        }
        CaptureProvider::AstrBot => {
            let database = home.join(".astrbot/data/data_v4.db");
            copy_fixture("astrbot/v1/data/data_v4.db", &database);
            (database, "ASTRBOT_ORACLE_USER_TEXT")
        }
        CaptureProvider::Lingma => {
            let database = home.join(".lingma/vscode/sharedClientCache/cache/db/local.db");
            copy_fixture("lingma/v1/local.db", &database);
            (database, "lingma oracle prompt")
        }
        CaptureProvider::Warp => {
            let state = root.join("xdg-state");
            let database = state.join("warp-terminal/warp.sqlite");
            copy_fixture("warp/v1/warp.sqlite", &database);
            context = context.with_env("XDG_STATE_HOME", state.as_os_str());
            (database, "warp sqlite oracle prompt")
        }
        _ => unreachable!("released-root equivalence fixture is provider-scoped"),
    };
    ProviderFixture {
        context,
        root: ProviderRootDefinition {
            id: format!("released-{}", provider.as_str()),
            provider,
            path,
            group: Some("released".to_owned()),
            kind: None,
        },
        marker,
    }
}

fn build_provider_registry(
    context: &DiscoveryContext,
    data_root: &Path,
    provider: CaptureProvider,
) -> SourceBackedAutomaticRegistryBuild {
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        context,
        provider,
    );
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &crate::test_provider_probes(),
        context,
        data_root,
        report,
    );
    assert!(build.issues.is_empty(), "{provider}: {:?}", build.issues);
    build
}

fn build_provider_registry_with_retained(
    context: &DiscoveryContext,
    data_root: &Path,
    provider: CaptureProvider,
    retained: &BTreeMap<String, RetainedProviderRootAuthority>,
) -> SourceBackedAutomaticRegistryBuild {
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        context,
        provider,
    );
    let build = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &crate::test_provider_probes(),
        context,
        data_root,
        report,
        retained,
    );
    assert!(build.issues.is_empty(), "{provider}: {:?}", build.issues);
    build
}

fn discover_provider_registry_with_retained(
    context: &DiscoveryContext,
    data_root: &Path,
    provider: CaptureProvider,
    retained: &BTreeMap<String, RetainedProviderRootAuthority>,
) -> SourceBackedAutomaticRegistryBuild {
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        context,
        provider,
    );
    build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &crate::test_provider_probes(),
        context,
        data_root,
        report,
        retained,
    )
}

fn move_provider_root(path: &Path, destination_root: &Path, step: usize) -> std::path::PathBuf {
    let destination = destination_root.join(format!("move-{step}")).join(
        path.file_name()
            .expect("provider fixture root has a final component"),
    );
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::rename(path, &destination).unwrap();
    destination
}

fn route_bytes(registry: &SourceBackedProviderRegistry) -> Vec<RouteBytes> {
    let mut routes = registry
        .routes()
        .filter_map(|metadata| {
            Some(RouteBytes {
                route: metadata.route_identity.as_ref()?.as_str().to_owned(),
                selection: metadata.selection?,
                selector_authority: metadata.selector_authority,
                provider: metadata.source.provider,
                source_format: metadata.source.source_format,
            })
        })
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| left.route.cmp(&right.route));
    routes
}

fn publication_bytes(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    marker: &str,
) -> PublicationBytes {
    let receipt = refresh_source_backed_generation(
        index_root,
        registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(
        receipt.failed_routes.is_empty(),
        "{:?}",
        receipt.failed_routes
    );
    assert!(!receipt.sources.is_empty());
    let index = VerifiedIndex::open(index_root).unwrap();
    let manifest = index.manifest();
    let mut sources = manifest
        .sources
        .iter()
        .map(|source| serde_json::to_vec(source.observation().source()).unwrap())
        .collect::<Vec<_>>();
    sources.sort();
    let aggregates = serde_json::to_vec(&manifest.core_record_aggregates).unwrap();
    let source_routes = serde_json::to_vec(manifest.source_routes()).unwrap();
    let route_controls = serde_json::to_vec(&receipt.route_controls).unwrap();
    let mut records = index
        .search_event_candidates(marker, 32)
        .unwrap()
        .into_iter()
        .filter_map(|candidate| {
            index
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .transpose()
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .map(|record| serde_json::to_vec(&record).unwrap())
        .collect::<Vec<_>>();
    records.sort();
    assert!(!records.is_empty(), "no records matched {marker:?}");
    PublicationBytes {
        sources,
        aggregates,
        source_routes,
        route_controls,
        records,
    }
}

fn assert_publication_bytes_eq(
    actual: &PublicationBytes,
    expected: &PublicationBytes,
    context: &str,
) {
    assert_eq!(actual.sources, expected.sources, "{context} sources");
    assert_eq!(
        actual.aggregates, expected.aggregates,
        "{context} aggregates"
    );
    assert_eq!(
        actual.source_routes, expected.source_routes,
        "{context} source routes"
    );
    assert_eq!(
        actual.route_controls, expected.route_controls,
        "{context} route controls"
    );
    assert_eq!(actual.records, expected.records, "{context} records");
}

fn records_matching(index_root: &Path, marker: &str) -> Vec<CoreRecord> {
    VerifiedIndex::open(index_root)
        .unwrap()
        .search_event_candidates(marker, 32)
        .unwrap()
        .into_iter()
        .map(|candidate| {
            VerifiedIndex::open(index_root)
                .unwrap()
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .unwrap()
                .expect("search candidate has a Core record")
        })
        .filter(|record| serde_json::to_string(record).unwrap().contains(marker))
        .collect()
}

fn only_record_matching(index_root: &Path, marker: &str) -> CoreRecord {
    let records = records_matching(index_root, marker);
    assert_eq!(records.len(), 1, "expected one record matching {marker:?}");
    records.into_iter().next().unwrap()
}

fn auggie_record_owned_by_root(index_root: &Path, root_id: &str, marker: &str) -> CoreRecord {
    let index = VerifiedIndex::open(index_root).unwrap();
    let allowed = index
        .manifest()
        .provider_root_source_tokens(&[root_id.to_owned()], &[])
        .unwrap();
    assert_eq!(allowed.len(), 1, "{root_id} must own one Auggie source");
    let matches = index
        .search_event_candidates_with_filters(
            marker,
            &EventSearchFilters {
                allowed_source_keys: Some(allowed.clone()),
                ..EventSearchFilters::default()
            },
            16,
        )
        .unwrap();
    assert!(
        !matches.is_empty(),
        "{root_id} must own matching Auggie events"
    );
    assert!(matches
        .iter()
        .all(|candidate| source_token(&candidate.event.source) == allowed[0]));
    index
        .core_record_by_id(matches[0].event.event_id.as_uuid())
        .unwrap()
        .unwrap()
}

fn openhands_root(id: &str, path: &Path, kind: ProviderRootKind) -> ProviderRootDefinition {
    ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::OpenHands,
        path: path.to_path_buf(),
        group: Some("released".to_owned()),
        kind: Some(kind),
    }
}

#[test]
fn exact_source_catalog_lineage_preserves_released_v1_identity() {
    let lineage = explicit_source_catalog_lineage(
        CaptureProvider::NanoClaw,
        "nanoclaw_project",
        Path::new("/fixture/nanoclaw"),
    );
    assert_eq!(
        lineage
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "5213b19342d779063b64336dd7fff3a678de719fadb60240a1e1061798687e56"
    );
    assert_ne!(
        lineage,
        explicit_source_catalog_lineage(
            CaptureProvider::NanoClaw,
            "nanoclaw_project",
            Path::new("/fixture/nanoclaw-other"),
        )
    );
}

#[test]
fn configured_auggie_parent_root_publishes_its_authentic_sessions_child() {
    const MARKER: &str = "auggie session json oracle prompt";

    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let selected = temp.path().join("configured-auggie");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    copy_fixture(
        "auggie/v0.32.0/sessions/01K0AUGGIESESSION0000000000.json",
        &selected.join("sessions/01K0AUGGIESESSION0000000000.json"),
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![ProviderRootDefinition {
        id: "configured-auggie".to_owned(),
        provider: CaptureProvider::Auggie,
        path: selected.clone(),
        group: Some("work".to_owned()),
        kind: None,
    }]);

    let build = build_provider_registry(
        &context,
        &temp.path().join("ctx-data"),
        CaptureProvider::Auggie,
    );
    assert_eq!(build.executable_route_count(), 1);
    let route = build.registry.routes().next().unwrap();
    assert_eq!(route.source.path, selected);
    assert_eq!(route.source.source_format, "auggie_session_json");
    assert!(route.source.route_provenance.configured_root().is_some());
    assert_eq!(
        build.registry.applied_provider_roots().unwrap().2[0].source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );

    let publication = publication_bytes(
        &temp.path().join("configured-index"),
        &build.registry,
        MARKER,
    );
    assert!(!publication.records.is_empty());
}

#[test]
fn named_auggie_roots_partition_shared_native_sessions_across_move_lifecycle() {
    const MARKER: &str = "auggie session json oracle prompt";
    const FIXTURE: &str = "auggie/v0.32.0/sessions/01K0AUGGIESESSION0000000000.json";

    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let first = temp.path().join("first-auggie");
    let second = temp.path().join("second-auggie");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    copy_fixture(FIXTURE, &first.join("sessions/session.json"));
    copy_fixture(FIXTURE, &second.join("sessions/session.json"));

    let definitions = |first: &Path| {
        vec![
            ProviderRootDefinition {
                id: "first-auggie".to_owned(),
                provider: CaptureProvider::Auggie,
                path: first.to_path_buf(),
                group: Some("work".to_owned()),
                kind: None,
            },
            ProviderRootDefinition {
                id: "second-auggie".to_owned(),
                provider: CaptureProvider::Auggie,
                path: second.clone(),
                group: Some("work".to_owned()),
                kind: None,
            },
        ]
    };
    let context = |first: &Path| {
        DiscoveryContext::new(
            &home,
            &cwd,
            DiscoveryPlatform::Linux,
            crate::DiscoveryPlatformDirs::default(),
        )
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(definitions(first))
    };
    let index_root = temp.path().join("index");

    let initial = build_provider_registry(
        &context(&first),
        &temp.path().join("initial-data"),
        CaptureProvider::Auggie,
    );
    let initial_roots = &initial.registry.applied_provider_roots().unwrap().2;
    assert_eq!(initial_roots.len(), 2);
    assert!(initial_roots
        .iter()
        .all(|root| root.source_identity() == ProviderRootSourceIdentity::NamedV1));
    assert_ne!(initial_roots[0].routes(), initial_roots[1].routes());
    refresh_source_backed_generation(
        &index_root,
        &initial.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    let initial_first = auggie_record_owned_by_root(&index_root, "first-auggie", MARKER);
    let initial_second = auggie_record_owned_by_root(&index_root, "second-auggie", MARKER);
    assert_ne!(initial_first.source, initial_second.source);
    assert_ne!(initial_first.session_id, initial_second.session_id);
    assert_ne!(initial_first.event_id, initial_second.event_id);

    let moved = temp.path().join("moved-first-auggie");
    fs::rename(&first, &moved).unwrap();
    let moved_build = build_provider_registry(
        &context(&moved),
        &temp.path().join("moved-data"),
        CaptureProvider::Auggie,
    );
    refresh_source_backed_generation(
        &index_root,
        &moved_build.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    let moved_first = auggie_record_owned_by_root(&index_root, "first-auggie", MARKER);
    let moved_second = auggie_record_owned_by_root(&index_root, "second-auggie", MARKER);
    assert_eq!(moved_first.source, initial_first.source);
    assert_eq!(moved_first.session_id, initial_first.session_id);
    assert_eq!(moved_first.event_id, initial_first.event_id);
    assert_eq!(moved_second, initial_second);
}

#[test]
fn disjoint_openhands_automatic_routes_each_adopt_released_identity() {
    const MARKER: &str = "openhands disjoint released adoption";

    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let legacy = temp.path().join("legacy-persistence");
    let current = temp.path().join("current-conversations");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_openhands_legacy_message(
        &legacy,
        "legacy-conversation",
        "legacy-event",
        &format!("{MARKER} legacy"),
    );
    write_openhands_current_message(
        &current,
        "current-conversation",
        "current-event",
        &format!("{MARKER} current"),
    );
    let automatic_context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("OH_PERSISTENCE_DIR", legacy.as_os_str())
    .with_env("OPENHANDS_CONVERSATIONS_DIR", current.as_os_str());
    let automatic = build_provider_registry(
        &automatic_context,
        &temp.path().join("automatic-data"),
        CaptureProvider::OpenHands,
    );
    assert_eq!(automatic.executable_route_count(), 2);
    let automatic_routes = route_bytes(&automatic.registry);
    let automatic_publication = publication_bytes(
        &temp.path().join("automatic-index"),
        &automatic.registry,
        MARKER,
    );
    let roots = vec![
        openhands_root(
            "current",
            &current,
            ProviderRootKind::OpenHandsCurrentConversations,
        ),
        openhands_root(
            "legacy",
            &legacy,
            ProviderRootKind::OpenHandsLegacyPersistence,
        ),
    ];

    for automatic_enabled in [true, false] {
        let context = automatic_context
            .clone()
            .with_automatic_provider_discovery(automatic_enabled)
            .with_configured_provider_roots(roots.clone());
        let configured = build_provider_registry(
            &context,
            &temp
                .path()
                .join(format!("configured-data-{automatic_enabled}")),
            CaptureProvider::OpenHands,
        );
        let applied = &configured.registry.applied_provider_roots().unwrap().2;
        assert_eq!(applied.len(), 2);
        assert!(applied.iter().all(|root| {
            root.source_identity() == ProviderRootSourceIdentity::Released
                && root.routes().len() == 1
        }));
        assert_ne!(applied[0].routes(), applied[1].routes());
        assert_eq!(route_bytes(&configured.registry), automatic_routes);
        assert_publication_bytes_eq(
            &publication_bytes(
                &temp
                    .path()
                    .join(format!("configured-index-{automatic_enabled}")),
                &configured.registry,
                MARKER,
            ),
            &automatic_publication,
            &format!("OpenHands automatic={automatic_enabled} disjoint released routes"),
        );
    }
}

#[test]
fn moved_openhands_current_root_retains_released_authority_through_full_lifecycle() {
    const MOVED_MARKER: &str = "openhands moved released authority marker";
    const REAPPEARED_MARKER: &str = "openhands old path reappearance marker";

    for automatic_enabled in [true, false] {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("cwd");
        let legacy = temp.path().join("never-present-legacy");
        let original = temp.path().join("original-current-conversations");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        write_openhands_current_message(
            &original,
            "moved-conversation",
            "moved-event",
            MOVED_MARKER,
        );
        let automatic_context = DiscoveryContext::new(
            &home,
            &cwd,
            DiscoveryPlatform::Linux,
            crate::DiscoveryPlatformDirs::default(),
        )
        .with_env("OH_PERSISTENCE_DIR", legacy.as_os_str())
        .with_env("OPENHANDS_CONVERSATIONS_DIR", original.as_os_str());
        let automatic = build_provider_registry(
            &automatic_context,
            &temp.path().join("automatic-data"),
            CaptureProvider::OpenHands,
        );
        assert_eq!(automatic.executable_route_count(), 1);
        let automatic_route = automatic
            .registry
            .routes()
            .find(|route| route.source.source_format == "openhands_cli_file_events")
            .and_then(|route| route.route_identity.clone())
            .unwrap();
        let automatic_index = temp.path().join("automatic-index");
        refresh_source_backed_generation(
            &automatic_index,
            &automatic.registry,
            source_backed_refresh_writer_options(),
        )
        .unwrap();
        let automatic_record = only_record_matching(&automatic_index, MOVED_MARKER);

        let mut definition = openhands_root(
            "current",
            &original,
            ProviderRootKind::OpenHandsCurrentConversations,
        );
        let initial_context = automatic_context
            .clone()
            .with_automatic_provider_discovery(automatic_enabled)
            .with_configured_provider_roots(vec![definition.clone()]);
        let initial = build_provider_registry(
            &initial_context,
            &temp.path().join("initial-data"),
            CaptureProvider::OpenHands,
        );
        let initial_applied = initial.registry.applied_provider_roots().unwrap().2[0].clone();
        assert_eq!(
            initial_applied.source_identity(),
            ProviderRootSourceIdentity::Released
        );
        assert_eq!(
            initial_applied.routes(),
            std::slice::from_ref(&automatic_route)
        );
        let index_root = temp.path().join("lifecycle-index");
        refresh_source_backed_generation(
            &index_root,
            &initial.registry,
            source_backed_refresh_writer_options(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&only_record_matching(&index_root, MOVED_MARKER)).unwrap(),
            serde_json::to_vec(&automatic_record).unwrap()
        );

        let moved = temp.path().join("moved-current-conversations");
        fs::rename(&original, &moved).unwrap();
        definition.path = moved.clone();
        let moved_context = automatic_context
            .clone()
            .with_automatic_provider_discovery(automatic_enabled)
            .with_configured_provider_roots(vec![definition.clone()]);
        let retained = BTreeMap::from([(
            definition.id.clone(),
            initial_applied.retained_authority().unwrap(),
        )]);
        let moved_build = build_provider_registry_with_retained(
            &moved_context,
            &temp.path().join("moved-data"),
            CaptureProvider::OpenHands,
            &retained,
        );
        let moved_applied = moved_build.registry.applied_provider_roots().unwrap().2[0].clone();
        assert_eq!(
            moved_applied.routes(),
            std::slice::from_ref(&automatic_route)
        );
        refresh_source_backed_generation(
            &index_root,
            &moved_build.registry,
            source_backed_refresh_writer_options(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&only_record_matching(&index_root, MOVED_MARKER)).unwrap(),
            serde_json::to_vec(&automatic_record).unwrap()
        );

        let persisted = serde_json::to_vec(&moved_applied).unwrap();
        let restarted: AppliedProviderRoot = serde_json::from_slice(&persisted).unwrap();
        assert_eq!(
            restarted
                .connector_binding()
                .and_then(|binding| binding.identity_root()),
            Some(original.as_path())
        );

        let parked = temp
            .path()
            .join("temporarily-missing-current-conversations");
        fs::rename(&moved, &parked).unwrap();
        let mut restarted = restarted;
        // Repeat beyond the index-owned Missing grace. A detached Released
        // binding must retain the moved route rather than letting the old
        // automatic path age it out while the replacement is unavailable.
        for observation in 0..=AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS {
            let retained = BTreeMap::from([(
                definition.id.clone(),
                restarted.retained_authority().unwrap(),
            )]);
            let mut missing = discover_provider_registry_with_retained(
                &moved_context,
                &temp.path().join(format!("missing-data-{observation}")),
                CaptureProvider::OpenHands,
                &retained,
            );
            assert_eq!(missing.executable_route_count(), 0);
            assert!(missing.issues.iter().any(|issue| matches!(
                issue,
                SourceBackedAutomaticRegistryIssue::Unavailable {
                    source,
                    reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                        ProviderSourceStatus::Missing
                    ),
                } if source.path == moved
            )));
            assert!(missing.registry.routes().all(|route| {
                route.source.path != original
                    || route.source.status != ProviderSourceStatus::Missing
            }));
            missing
                .registry
                .retain_unavailable_provider_root_routes(std::slice::from_ref(&restarted))
                .unwrap();
            refresh_source_backed_generation(
                &index_root,
                &missing.registry,
                source_backed_refresh_writer_options(),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_vec(&only_record_matching(&index_root, MOVED_MARKER)).unwrap(),
                serde_json::to_vec(&automatic_record).unwrap()
            );
            let missing_index = VerifiedIndex::open(&index_root).unwrap();
            assert!(missing_index
                .manifest()
                .source_route(&automatic_route)
                .unwrap()
                .missing_state()
                .is_none());
            restarted = serde_json::from_slice(
                &serde_json::to_vec(&missing_index.manifest().provider_roots()[0]).unwrap(),
            )
            .unwrap();
        }

        fs::rename(&parked, &moved).unwrap();
        let retained = BTreeMap::from([(
            definition.id.clone(),
            restarted.retained_authority().unwrap(),
        )]);
        let restored = build_provider_registry_with_retained(
            &moved_context,
            &temp.path().join("restored-data"),
            CaptureProvider::OpenHands,
            &retained,
        );
        assert_eq!(
            restored.registry.applied_provider_roots().unwrap().2[0].routes(),
            std::slice::from_ref(&automatic_route)
        );
        refresh_source_backed_generation(
            &index_root,
            &restored.registry,
            source_backed_refresh_writer_options(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&only_record_matching(&index_root, MOVED_MARKER)).unwrap(),
            serde_json::to_vec(&automatic_record).unwrap()
        );

        write_openhands_current_message(
            &original,
            "reappeared-conversation",
            "reappeared-event",
            REAPPEARED_MARKER,
        );
        let restored_applied = restored.registry.applied_provider_roots().unwrap().2[0].clone();
        let retained = BTreeMap::from([(
            definition.id.clone(),
            restored_applied.retained_authority().unwrap(),
        )]);
        let reappeared = build_provider_registry_with_retained(
            &moved_context,
            &temp.path().join("reappeared-data"),
            CaptureProvider::OpenHands,
            &retained,
        );
        let executable = reappeared.registry.executable_route_identities();
        let expected_route_count = if automatic_enabled { 2 } else { 1 };
        assert_eq!(executable.len(), expected_route_count);
        assert_eq!(
            executable
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            expected_route_count
        );
        assert!(executable.contains(&automatic_route));
        refresh_source_backed_generation(
            &index_root,
            &reappeared.registry,
            source_backed_refresh_writer_options(),
        )
        .unwrap();
        assert_eq!(records_matching(&index_root, MOVED_MARKER).len(), 1);
        assert_eq!(
            serde_json::to_vec(&only_record_matching(&index_root, MOVED_MARKER)).unwrap(),
            serde_json::to_vec(&automatic_record).unwrap()
        );
        assert_eq!(
            records_matching(&index_root, REAPPEARED_MARKER).len(),
            usize::from(automatic_enabled)
        );
        assert_eq!(
            VerifiedIndex::open(&index_root).unwrap().document_count(),
            expected_route_count as u64
        );
    }
}

#[test]
fn matching_released_roots_reproduce_automatic_authority_and_record_bytes() {
    for provider in [
        CaptureProvider::OpenClaw,
        CaptureProvider::Hermes,
        CaptureProvider::Crush,
        CaptureProvider::Goose,
        CaptureProvider::AstrBot,
        CaptureProvider::Lingma,
        CaptureProvider::Warp,
    ] {
        let temp = tempdir().unwrap();
        let fixture = provider_fixture(temp.path(), provider);
        let automatic = build_provider_registry(
            &fixture.context,
            &temp.path().join("automatic-data"),
            provider,
        );
        assert_eq!(automatic.executable_route_count(), 1, "{provider}");
        let automatic_routes = route_bytes(&automatic.registry);
        let automatic_publication = publication_bytes(
            &temp.path().join("automatic-index"),
            &automatic.registry,
            fixture.marker,
        );

        for automatic_enabled in [true, false] {
            let context = fixture
                .context
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![fixture.root.clone()]);
            let configured = build_provider_registry(
                &context,
                &temp
                    .path()
                    .join(format!("configured-data-{automatic_enabled}")),
                provider,
            );
            assert_eq!(configured.executable_route_count(), 1, "{provider}");
            let (_, _, applied) = configured.registry.applied_provider_roots().unwrap();
            assert_eq!(applied.len(), 1, "{provider}");
            assert_eq!(
                applied[0].source_identity(),
                ProviderRootSourceIdentity::Released,
                "{provider} automatic={automatic_enabled}"
            );
            assert_eq!(
                serde_json::to_vec(&applied[0].source_identity()).unwrap(),
                b"\"released\"",
                "{provider} automatic={automatic_enabled}"
            );
            assert_eq!(
                route_bytes(&configured.registry),
                automatic_routes,
                "{provider} automatic={automatic_enabled} route authority"
            );
            assert_eq!(
                publication_bytes(
                    &temp
                        .path()
                        .join(format!("configured-index-{automatic_enabled}")),
                    &configured.registry,
                    fixture.marker,
                ),
                automatic_publication,
                "{provider} automatic={automatic_enabled} source/session/event bytes"
            );
        }
    }
}

#[test]
fn moved_released_roots_survive_restart_and_second_move_without_rotating_bytes() {
    for provider in [
        CaptureProvider::OpenClaw,
        CaptureProvider::Hermes,
        CaptureProvider::Crush,
        CaptureProvider::Goose,
        CaptureProvider::AstrBot,
        CaptureProvider::Lingma,
        CaptureProvider::Warp,
    ] {
        for automatic_enabled in [true, false] {
            let temp = tempdir().unwrap();
            let mut fixture = provider_fixture(temp.path(), provider);
            let identity_root = fixture.root.path.clone();
            let automatic = build_provider_registry(
                &fixture.context,
                &temp.path().join("automatic-data"),
                provider,
            );
            let automatic_routes = route_bytes(&automatic.registry);
            let automatic_publication = publication_bytes(
                &temp.path().join("automatic-index"),
                &automatic.registry,
                fixture.marker,
            );

            let initial_context = fixture
                .context
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![fixture.root.clone()]);
            let initial = build_provider_registry(
                &initial_context,
                &temp.path().join("initial-configured-data"),
                provider,
            );
            let initial_applied = initial.registry.applied_provider_roots().unwrap().2[0].clone();
            assert_eq!(
                initial_applied
                    .connector_binding()
                    .expect("released root has a connector binding")
                    .identity_root(),
                Some(identity_root.as_path()),
                "{provider} automatic={automatic_enabled} initial binding"
            );

            fixture.root.path =
                move_provider_root(&fixture.root.path, &temp.path().join("moved"), 1);
            let first_context = fixture
                .context
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![fixture.root.clone()]);
            let first_retained = BTreeMap::from([(
                fixture.root.id.clone(),
                initial_applied.retained_authority().unwrap(),
            )]);
            let first = build_provider_registry_with_retained(
                &first_context,
                &temp.path().join("first-move-data"),
                provider,
                &first_retained,
            );
            assert_eq!(
                route_bytes(&first.registry),
                automatic_routes,
                "{provider} automatic={automatic_enabled} first move route authority"
            );
            assert_publication_bytes_eq(
                &publication_bytes(
                    &temp.path().join("first-move-index"),
                    &first.registry,
                    fixture.marker,
                ),
                &automatic_publication,
                &format!("{provider} automatic={automatic_enabled} first move"),
            );

            let first_applied = first.registry.applied_provider_roots().unwrap().2[0].clone();
            let persisted = serde_json::to_vec(&first_applied).unwrap();
            let restarted: AppliedProviderRoot = serde_json::from_slice(&persisted).unwrap();
            assert_eq!(
                restarted
                    .connector_binding()
                    .expect("restarted released root has a connector binding")
                    .identity_root(),
                Some(identity_root.as_path()),
                "{provider} automatic={automatic_enabled} restarted binding"
            );

            fixture.root.path =
                move_provider_root(&fixture.root.path, &temp.path().join("moved"), 2);
            let second_context = fixture
                .context
                .clone()
                .with_automatic_provider_discovery(automatic_enabled)
                .with_configured_provider_roots(vec![fixture.root.clone()]);
            let second_retained = BTreeMap::from([(
                fixture.root.id.clone(),
                restarted.retained_authority().unwrap(),
            )]);
            let second = build_provider_registry_with_retained(
                &second_context,
                &temp.path().join("second-move-data"),
                provider,
                &second_retained,
            );
            assert_eq!(
                route_bytes(&second.registry),
                automatic_routes,
                "{provider} automatic={automatic_enabled} second move route authority"
            );
            assert_publication_bytes_eq(
                &publication_bytes(
                    &temp.path().join("second-move-index"),
                    &second.registry,
                    fixture.marker,
                ),
                &automatic_publication,
                &format!("{provider} automatic={automatic_enabled} second move"),
            );
        }
    }
}

#[test]
fn moved_released_roo_root_keeps_its_dynamic_automatic_role() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let original = home.join(".vscode-mock/global-storage");
    let task = original.join("tasks/roo-move");
    fs::create_dir_all(&task).unwrap();
    fs::write(task.join("history_item.json"), "{}").unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let definition = |path| ProviderRootDefinition {
        id: "roo-work".to_owned(),
        provider: CaptureProvider::RooCode,
        path,
        group: Some("roo".to_owned()),
        kind: None,
    };
    let initial_context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![definition(original.clone())]);
    let initial_report =
        ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
            &crate::test_provider_probes(),
            &initial_context,
            CaptureProvider::RooCode,
        );
    let initial = build_automatic_source_backed_registry_from_report_with_probes(
        &crate::test_provider_probes(),
        &initial_context,
        &temp.path().join("initial-data"),
        initial_report,
    );
    assert!(initial.issues.is_empty(), "{:?}", initial.issues);
    let initial_root = initial.registry.applied_provider_roots().unwrap().2[0].clone();
    assert_eq!(
        initial_root.source_identity(),
        ProviderRootSourceIdentity::Released
    );
    let initial_route = initial_root.routes()[0].clone();
    let initial_role = initial
        .registry
        .routes()
        .find(|route| route.route_identity.as_ref() == Some(&initial_route))
        .and_then(|route| route.source.route_provenance.automatic_route_role())
        .cloned()
        .unwrap();

    let moved = temp.path().join("roo-moved");
    let moved_context =
        initial_context.with_configured_provider_roots(vec![definition(moved.clone())]);
    let missing_report =
        ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
            &crate::test_provider_probes(),
            &moved_context,
            CaptureProvider::RooCode,
        );
    let retained = BTreeMap::from([(
        "roo-work".to_owned(),
        initial_root.retained_authority().unwrap(),
    )]);
    for force_unknown in [false, true] {
        let mut unavailable_report = missing_report.clone();
        if force_unknown {
            for source in &mut unavailable_report.sources {
                source.status = ProviderSourceStatus::Unknown;
                source.unsupported_reason = Some("fixture selector is temporarily unreadable");
            }
        }
        let unavailable =
            build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
                &crate::test_provider_probes(),
                &moved_context,
                &temp.path().join(if force_unknown {
                    "unknown-data"
                } else {
                    "missing-data"
                }),
                unavailable_report,
                &retained,
            );
        assert_eq!(unavailable.issues.len(), 1, "{:?}", unavailable.issues);
        let unavailable_root = &unavailable.registry.applied_provider_roots().unwrap().2[0];
        assert_eq!(
            unavailable_root.source_identity(),
            ProviderRootSourceIdentity::Released
        );
        assert_eq!(
            unavailable_root.routes(),
            std::slice::from_ref(&initial_route),
            "force_unknown={force_unknown}"
        );
    }

    fs::rename(&original, &moved).unwrap();
    let moved_report =
        ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
            &crate::test_provider_probes(),
            &moved_context,
            CaptureProvider::RooCode,
        );
    let moved = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &crate::test_provider_probes(),
        &moved_context,
        &temp.path().join("moved-data"),
        moved_report,
        &retained,
    );
    assert!(moved.issues.is_empty(), "{:?}", moved.issues);
    let moved_root = &moved.registry.applied_provider_roots().unwrap().2[0];
    assert_eq!(
        moved_root.source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(moved_root.routes(), std::slice::from_ref(&initial_route));
    let moved_role = moved
        .registry
        .routes()
        .find(|route| route.route_identity.as_ref() == Some(&initial_route))
        .and_then(|route| route.source.route_provenance.automatic_route_role())
        .cloned()
        .unwrap();
    assert_eq!(moved_role, initial_role);
}
