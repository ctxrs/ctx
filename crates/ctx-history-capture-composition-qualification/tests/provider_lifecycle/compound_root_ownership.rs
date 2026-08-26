use std::path::Path;

use ctx_history_capture_model::ProviderRootSourceIdentity;
use ctx_history_index::{source_token, EventSearchFilters, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::json;

use super::*;

const MARKERS: [&str; 3] = [
    "compoundownership compoundalpha",
    "compoundownership compoundbeta",
    "compoundownership compoundautomatic",
];
const UNIQUE_MARKERS: [&str; 3] = ["compoundalpha", "compoundbeta", "compoundautomatic"];

struct Fixture {
    context: DiscoveryContext,
    roots: Vec<ProviderRootDefinition>,
}

#[derive(Debug, PartialEq, Eq)]
struct StableSourceBytes {
    route: Vec<u8>,
    source: Vec<u8>,
    sessions: Vec<Vec<u8>>,
    events: Vec<Vec<u8>>,
    records: Vec<Vec<u8>>,
}

fn write_crush(path: &Path, index: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                id text primary key, parent_session_id text, title text,
                prompt_tokens integer, completion_tokens integer, cost real,
                created_at integer, updated_at integer, summary_message_id text
             );
             create table messages (
                id text primary key, session_id text not null, role text not null,
                parts text not null, created_at integer, updated_at integer,
                provider text, model text, is_summary_message integer not null default 0
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into sessions values (?1, null, 'fixture', 1, 1, 0, 1000, 1000, null)",
            [format!("compound-session-{index}")],
        )
        .unwrap();
    connection
        .execute(
            "insert into messages values (?1, ?2, 'assistant', ?3, 1001, 1001, 'fixture', 'model', 0)",
            params![
                format!("compound-message-{index}"),
                format!("compound-session-{index}"),
                json!([{"type":"text","data":{"text":MARKERS[index]}}]).to_string(),
            ],
        )
        .unwrap();
}

fn write_lingma(path: &Path, index: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table chat_record (
                session_id text not null, request_id text, chat_prompt text not null,
                summary text, error_result text, gmt_create integer, extra text
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into chat_record values (?1, ?2, ?3, null, null, 1780000000, '{}')",
            params![
                format!("compound-session-{index}"),
                format!("compound-request-{index}"),
                MARKERS[index],
            ],
        )
        .unwrap();
}

fn fixture(root: &Path, provider: CaptureProvider, reverse_roots: bool) -> Fixture {
    let home = root.join("home");
    let cwd = root.join("cwd");
    let config = root.join("config");
    fs::create_dir_all(&cwd).unwrap();
    let paths = match provider {
        CaptureProvider::Crush => {
            let projects = (0..3)
                .map(|index| {
                    let project = root.join(format!("project-{index}"));
                    let data = root.join(format!("crush-data-{index}"));
                    fs::create_dir_all(&project).unwrap();
                    write_crush(&data.join("crush.db"), index);
                    json!({"path": project, "data_dir": data})
                })
                .collect::<Vec<_>>();
            let registry = home.join(".local/share/crush/projects.json");
            fs::create_dir_all(registry.parent().unwrap()).unwrap();
            fs::write(
                registry,
                serde_json::to_vec(&json!({"projects": projects})).unwrap(),
            )
            .unwrap();
            (0..3)
                .map(|index| root.join(format!("crush-data-{index}/crush.db")))
                .collect::<Vec<_>>()
        }
        CaptureProvider::Lingma => {
            let storage = (0..3)
                .map(|index| root.join(format!("lingma-storage-{index}")))
                .collect::<Vec<_>>();
            for (index, storage) in storage.iter().enumerate() {
                write_lingma(&storage.join("sharedClientCache/cache/db/local.db"), index);
                let settings = if index == 0 {
                    config.join("Code/User/settings.json")
                } else {
                    config.join(format!("Code/User/profiles/profile-{index}/settings.json"))
                };
                fs::create_dir_all(settings.parent().unwrap()).unwrap();
                fs::write(
                    settings,
                    serde_json::to_vec(&json!({"QoderCN.LocalMachineStoragePath": storage}))
                        .unwrap(),
                )
                .unwrap();
            }
            storage
                .into_iter()
                .map(|path| path.join("sharedClientCache/cache/db/local.db"))
                .collect()
        }
        _ => unreachable!(),
    };
    let mut roots = paths[..2]
        .iter()
        .enumerate()
        .map(|(index, path)| ProviderRootDefinition {
            id: ["alpha", "beta"][index].to_owned(),
            provider,
            path: path.clone(),
            group: Some(format!("{}-group", ["alpha", "beta"][index])),
            kind: None,
        })
        .collect::<Vec<_>>();
    if reverse_roots {
        roots.reverse();
    }
    Fixture {
        context: DiscoveryContext::new(
            &home,
            &cwd,
            DiscoveryPlatform::Linux,
            crate::DiscoveryPlatformDirs {
                config: Some(config),
                ..crate::DiscoveryPlatformDirs::default()
            },
        ),
        roots,
    }
}

fn build(
    fixture: &Fixture,
    provider: CaptureProvider,
    data_root: &Path,
    automatic: bool,
    root_count: usize,
) -> SourceBackedAutomaticRegistryBuild {
    let context = fixture
        .context
        .clone()
        .with_automatic_provider_discovery(automatic)
        .with_configured_provider_roots(fixture.roots[..root_count].to_vec());
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        &context,
        provider,
    );
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &crate::test_provider_probes(),
        &context,
        data_root,
        report,
    );
    assert!(build.issues.is_empty(), "{provider}: {:?}", build.issues);
    assert_eq!(build.executable_route_count(), 1, "{provider}");
    build
}

fn publish(path: &Path, registry: &SourceBackedProviderRegistry) -> VerifiedIndex {
    let receipt = refresh_source_backed_generation(
        path,
        registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert!(
        receipt.failed_routes.is_empty(),
        "{:?}",
        receipt.failed_routes
    );
    VerifiedIndex::open(path).unwrap()
}

fn source_bytes(index: &VerifiedIndex, marker: &str) -> StableSourceBytes {
    let hits = index.search_event_candidates(marker, 8).unwrap();
    assert_eq!(hits.len(), 1, "{marker}");
    let source = &hits[0].event.source;
    let route = index
        .manifest()
        .source_routes()
        .iter()
        .find(|route| route.sources().iter().any(|member| member == source))
        .unwrap();
    let mut records = hits
        .iter()
        .map(|hit| {
            index
                .core_record_by_id(hit.event.event_id.as_uuid())
                .unwrap()
                .map(|record| serde_json::to_vec(&record).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    records.sort();
    StableSourceBytes {
        route: serde_json::to_vec(route.route_identity()).unwrap(),
        source: serde_json::to_vec(source).unwrap(),
        sessions: hits
            .iter()
            .map(|hit| serde_json::to_vec(&hit.event.session_id).unwrap())
            .collect(),
        events: hits
            .iter()
            .map(|hit| serde_json::to_vec(&hit.event.event_id).unwrap())
            .collect(),
        records,
    }
}

fn assert_filters(index: &VerifiedIndex, root_count: usize, expected_sources: usize) {
    let hits = index
        .search_event_candidates("compoundownership", 16)
        .unwrap();
    assert_eq!(hits.len(), expected_sources);
    assert_eq!(
        index.manifest().source_routes()[0].sources().len(),
        expected_sources
    );
    for root in index.manifest().provider_roots().iter().take(root_count) {
        let allowed = index
            .manifest()
            .provider_root_source_tokens(&[root.definition().id.clone()], &[])
            .unwrap();
        assert_eq!(allowed.len(), 1);
        let filtered = index
            .search_event_candidates_with_filters(
                "compoundownership",
                &EventSearchFilters {
                    allowed_source_keys: Some(allowed.clone()),
                    ..EventSearchFilters::default()
                },
                16,
            )
            .unwrap();
        assert_eq!(filtered.len(), 1, "{}", root.definition().id);
        assert_eq!(source_token(&filtered[0].event.source), allowed[0]);
    }
}

#[test]
fn released_compound_roots_filter_exactly_and_preserve_automatic_peers_and_bytes() {
    for provider in [CaptureProvider::Crush, CaptureProvider::Lingma] {
        for automatic in [true, false] {
            let temp = tempdir().unwrap();
            let fixture = fixture(temp.path(), provider, false);
            let automatic_build = build(
                &fixture,
                provider,
                &temp.path().join("automatic-data"),
                true,
                0,
            );
            let automatic_index = publish(
                &temp.path().join("automatic-index"),
                &automatic_build.registry,
            );
            for root_count in [1, 2] {
                let configured = build(
                    &fixture,
                    provider,
                    &temp.path().join(format!("data-{automatic}-{root_count}")),
                    automatic,
                    root_count,
                );
                let roots = &configured.registry.applied_provider_roots().unwrap().2;
                assert_eq!(roots.len(), root_count);
                assert!(roots.iter().all(|root| {
                    root.source_identity() == ProviderRootSourceIdentity::Released
                        && root.exact_source_memberships().len() == 1
                }));
                if root_count == 2 {
                    assert_eq!(roots[0].routes(), roots[1].routes());
                    assert_ne!(
                        roots[0].exact_source_memberships()[0].source_tokens(),
                        roots[1].exact_source_memberships()[0].source_tokens()
                    );
                }
                let configured_index = publish(
                    &temp.path().join(format!("index-{automatic}-{root_count}")),
                    &configured.registry,
                );
                assert_filters(
                    &configured_index,
                    root_count,
                    if automatic { 3 } else { root_count },
                );
                for marker in &UNIQUE_MARKERS[..root_count] {
                    assert_eq!(
                        source_bytes(&configured_index, marker),
                        source_bytes(&automatic_index, marker),
                        "{provider} automatic={automatic} roots={root_count} marker={marker}"
                    );
                }
            }
        }
    }
}

#[test]
fn released_shared_route_build_is_independent_of_root_order() {
    for provider in [CaptureProvider::Crush, CaptureProvider::Lingma] {
        let temp = tempdir().unwrap();
        let mut fixture = fixture(temp.path(), provider, false);
        let forward = build(
            &fixture,
            provider,
            &temp.path().join("forward-data"),
            true,
            2,
        );
        let forward = publish(&temp.path().join("forward-index"), &forward.registry);
        assert_filters(&forward, 2, 3);

        fixture.roots.reverse();
        let reverse = build(
            &fixture,
            provider,
            &temp.path().join("reverse-data"),
            true,
            2,
        );
        let reverse = publish(&temp.path().join("reverse-index"), &reverse.registry);
        assert_filters(&reverse, 2, 3);

        assert_eq!(
            forward.manifest().source_routes(),
            reverse.manifest().source_routes()
        );
        assert_eq!(
            serde_json::to_vec(forward.manifest().provider_roots()).unwrap(),
            serde_json::to_vec(reverse.manifest().provider_roots()).unwrap()
        );
        for marker in UNIQUE_MARKERS {
            assert_eq!(
                source_bytes(&forward, marker),
                source_bytes(&reverse, marker)
            );
        }
    }
}
