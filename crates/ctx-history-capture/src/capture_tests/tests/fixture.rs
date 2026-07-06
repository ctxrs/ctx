#[allow(unused_imports)]
use super::*;

pub(crate) fn fixture_options(dedupe_key: &str, title: &str) -> FixtureOptions {
    FixtureOptions {
        title: title.to_owned(),
        body: "captured body".to_owned(),
        tags: vec!["capture-test".to_owned()],
        dedupe_key: Some(dedupe_key.to_owned()),
        machine_id: Some("test-machine".to_owned()),
        cwd: Some(PathBuf::from("/tmp/work")),
        occurred_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    }
}

pub(crate) fn provider_fixture(name: &str) -> PathBuf {
    materialized_fixture("provider", name)
}

pub(crate) fn provider_history_fixture(name: &str) -> PathBuf {
    materialized_fixture("provider-history", name)
}

pub(crate) fn materialized_fixture(category: &str, name: &str) -> PathBuf {
    let source = match category {
        "provider" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider")
            .join(name),
        "provider-history" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history")
            .join(name),
        "custom-history-jsonl" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/custom-history-jsonl")
            .join(name),
        _ => panic!("unknown fixture category {category}"),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-data/materialized-fixtures");
    fs::create_dir_all(&root).unwrap();
    let unique = format!(
        "{}-{}-{}-{}",
        category,
        name.replace(['/', '\\', '.'], "_"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = root.join(unique);
    if source.is_dir() {
        copy_dir_all(&source, &target);
    } else {
        fs::copy(&source, &target).unwrap();
    }
    target
}

pub(crate) fn fixed_import_options(path: PathBuf) -> ProviderFixtureImportOptions {
    ProviderFixtureImportOptions {
        machine_id: "test-machine".into(),
        source_path: Some(path),
        imported_at: DateTime::parse_from_rfc3339("2026-06-23T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        history_record_id: None,
        expected_provider: None,
        allow_partial_failures: false,
        ..ProviderFixtureImportOptions::default()
    }
}

pub(crate) fn write_minimal_provider_fixture(
    temp: &TempDir,
    provider: CaptureProvider,
    external_session_id: &str,
) -> PathBuf {
    let provider_name = provider.as_str();
    let path = temp.path().join(format!("{provider_name}.jsonl"));
    let line = json!({
        "provider": provider_name,
        "session": {
            "provider_session_id": external_session_id,
            "agent_type": "primary",
            "role_hint": "primary",
            "is_primary": true,
            "status": "imported",
            "started_at": "2026-06-23T17:00:00Z",
            "cwd": "/workspace/example",
            "metadata": {"source": "temp-fixture", "provider": provider_name}
        },
        "event": {
            "provider_event_index": 0,
            "cursor": format!("{provider_name}-cursor-0"),
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-06-23T17:00:01Z",
            "payload": {"text": format!("{provider_name} provider fixture smoke")},
            "metadata": {"source": "temp-fixture"}
        }
    });
    fs::write(&path, format!("{line}\n")).unwrap();
    path
}

#[test]
pub(crate) fn spool_writer_closes_tmp_file_atomically_to_jsonl() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    let envelope = fixture_envelope(fixture_options("atomic", "Atomic capture")).unwrap();
    let mut writer = SpoolWriter::create(&inbox, "test-machine").unwrap();
    let tmp_path = writer.tmp_path().to_path_buf();
    let final_path = writer.final_path().to_path_buf();

    writer.write_envelope(&envelope).unwrap();
    assert!(tmp_path.exists());
    assert!(!final_path.exists());

    let closed_path = writer.finish().unwrap();
    assert_eq!(closed_path, final_path);
    assert!(!tmp_path.exists());
    assert!(final_path.exists());
    assert_eq!(read_jsonl(&final_path).unwrap(), vec![envelope]);
}

#[test]
pub(crate) fn import_is_idempotent_by_dedupe_key() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    let envelope = fixture_envelope(fixture_options("same-dedupe", "First title")).unwrap();
    let mut first = SpoolWriter::create(&inbox, "test-machine").unwrap();
    first.write_envelope(&envelope).unwrap();
    first.finish().unwrap();
    let mut second = SpoolWriter::create(&inbox, "test-machine").unwrap();
    second.write_envelope(&envelope).unwrap();
    second.finish().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_spool(&inbox, &mut store).unwrap();

    assert_eq!(summary.failed_files, 0);
    assert_eq!(summary.processed_files, 2);
    let records = store.list_records(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, stable_capture_uuid("same-dedupe", "record"));
    assert_eq!(records[0].id.get_version_num(), 7);
    assert_eq!(records[0].title, "First title");
    assert_eq!(spool_counts(&inbox).unwrap().done, 2);
}

#[test]
pub(crate) fn provider_fixture_replay_defers_child_edges_until_parent_is_known() {
    let temp = tempdir();
    let fixture = provider_fixture("out-of-order-subagent.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.imported_edges, 1);
    assert_eq!(summary.skipped_edges, 0);

    let parent_id = provider_session_uuid(CaptureProvider::Codex, "out-of-order-root");
    let child_id = provider_session_uuid(CaptureProvider::Codex, "out-of-order-child");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 1);
}

pub(crate) fn write_droid_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("droid-root.jsonl"),
        concat!(
            "{\"type\":\"session_start\",\"sessionId\":\"droid-root\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
            "{\"type\":\"message\",\"id\":\"droid-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"delegate\"}]}\n",
            "{\"type\":\"message\",\"id\":\"droid-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}\n",
        ),
    )
    .unwrap();
    fs::write(
        root.join("droid-child.jsonl"),
        concat!(
            "{\"type\":\"session_start\",\"sessionId\":\"droid-child\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\",\"parent\":\"droid-root\",\"decompSessionType\":\"worker\"}\n",
            "{\"type\":\"message\",\"id\":\"droid-child-user\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"inspect\"}]}\n",
        ),
    )
    .unwrap();
    temp.path().join("droid/sessions")
}

#[test]
pub(crate) fn provider_fixture_replay_supports_search_only_temp_fixtures() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    for (
        fixture_name,
        provider,
        external_session_id,
        fixture_sessions,
        fixture_events,
        fixture_edges,
    ) in [
        (
            "copilot_cli.jsonl",
            CaptureProvider::CopilotCli,
            "copilot-cli-session-1",
            1,
            2,
            0,
        ),
        (
            "factory_ai_droid.jsonl",
            CaptureProvider::FactoryAiDroid,
            "factory-ai-droid-session-1",
            2,
            3,
            1,
        ),
    ] {
        let fixture = provider_fixture(fixture_name);
        let (fixture, sessions, events, edges) = if fixture.exists() {
            (fixture, fixture_sessions, fixture_events, fixture_edges)
        } else {
            (
                write_minimal_provider_fixture(&temp, provider, external_session_id),
                1,
                1,
                0,
            )
        };
        let mut options = fixed_import_options(fixture.clone());
        options.expected_provider = Some(provider);

        let first = import_provider_fixture_jsonl(&fixture, &mut store, options.clone()).unwrap();
        assert_eq!(first.failed, 0, "{provider}: {:?}", first.failures);
        assert_eq!(first.imported_sessions, sessions, "{provider}");
        assert_eq!(first.imported_events, events, "{provider}");
        assert_eq!(first.imported_edges, edges, "{provider}");

        let second = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();
        assert_eq!(second.failed, 0, "{provider}: {:?}", second.failures);
        assert_eq!(second.imported_sessions, 0, "{provider}");
        assert_eq!(second.imported_events, 0, "{provider}");
        assert_eq!(second.imported_edges, 0, "{provider}");
        assert_eq!(second.skipped_sessions, sessions, "{provider}");
        assert_eq!(second.skipped_events, events, "{provider}");
        assert_eq!(second.skipped_edges, edges, "{provider}");

        let session_id = provider_session_uuid(provider, external_session_id);
        let session = store.get_session(session_id).unwrap();
        assert_eq!(session.provider, provider);
        assert!(!store.events_for_session(session_id).unwrap().is_empty());
    }
}

#[test]
pub(crate) fn provider_fixture_replay_rejects_malformed_lines_without_partial_import_by_default() {
    let temp = tempdir();
    let fixture = provider_fixture("malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 1);
    let session_id = provider_session_uuid(CaptureProvider::Codex, "malformed-partial-session");
    assert!(store.events_for_session(session_id).unwrap().is_empty());
}

#[test]
pub(crate) fn provider_fixture_replay_allows_explicit_partial_import() {
    let temp = tempdir();
    let fixture = provider_fixture("malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut options = fixed_import_options(fixture.clone());
    options.allow_partial_failures = true;

    let summary = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();

    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].line, 3);
    let session_id = provider_session_uuid(CaptureProvider::Codex, "malformed-partial-session");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0]
        .payload
        .to_string()
        .contains("Valid event before malformed line."));
    assert!(events[1]
        .payload
        .to_string()
        .contains("Valid event after malformed line."));
}
