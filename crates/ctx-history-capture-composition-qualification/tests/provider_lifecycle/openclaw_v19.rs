use super::*;
use ctx_history_index::WriterOptions;

#[test]
fn native_openclaw_v19_discovery_import_repeat_and_readonly() {
    let temp = tempdir().unwrap();
    let database = temp
        .path()
        .join("home/.openclaw/agents/main/agent/openclaw-agent.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let fixture = test_support_paths::capture_repo_root().join(
        "tests/fixtures/provider-history/openclaw-v19/agents/main/agent/openclaw-agent.sqlite",
    );
    fs::copy(fixture, &database).unwrap();
    let before = fs::read(&database).unwrap();
    let source =
        provider_sources::provider_source_for_path(CaptureProvider::OpenClaw, database.clone());
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "openclaw_agent_sqlite");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let legacy = home.join(".openclaw/agents/main/sessions");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("old.jsonl"), "{\"id\":\"suppressed\"}\n").unwrap();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_data_root(temp.path().join("data"));
    let probes = test_provider_probes();
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &probes,
        &context,
        CaptureProvider::OpenClaw,
    );
    assert_eq!(report.sources.len(), 1, "{report:?}");
    assert_eq!(report.sources[0].path, database);
    assert_eq!(report.sources[0].source_format, "openclaw_agent_sqlite");
    let automatic = build_automatic_source_backed_registry_from_report_with_probes(
        &probes,
        &context,
        &temp.path().join("data"),
        report,
    );
    assert!(automatic.issues.is_empty(), "{:?}", automatic.issues);
    let registry = automatic.registry;
    let index_root = temp.path().join("index");
    let options = || WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let cold = refresh_source_backed_generation(&index_root, &registry, options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(index.manifest().sources.len(), 1);
    let source = index.manifest().sources[0].observation().source();
    let items = index
        .core_source_event_page(source, None, 128)
        .unwrap()
        .items;
    assert_eq!(items.len(), 2);
    let bodies: Vec<_> = items
        .iter()
        .map(|i| i.core_record.content.meaningful_text())
        .collect();
    assert!(bodies.contains(&"OpenClaw nineteen native capture question"));
    assert!(bodies.contains(&"OpenClaw nineteen native capture answer"));
    assert_eq!(search_event_candidates(&index, "nineteen", 10).len(), 2);
    drop(index);
    let replay = refresh_source_backed_generation(&index_root, &registry, options()).unwrap();
    assert!(replay.failed_routes.is_empty());
    assert_eq!(cold.commit.generation_id, replay.commit.generation_id);
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read_dir(database.parent().unwrap()).unwrap().count(), 1);
}
