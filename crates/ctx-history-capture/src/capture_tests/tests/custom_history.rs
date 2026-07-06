#[allow(unused_imports)]
use super::*;

pub(crate) fn custom_history_fixture(name: &str) -> PathBuf {
    materialized_fixture("custom-history-jsonl", name)
}

#[test]
pub(crate) fn custom_history_jsonl_imports_full_shape_and_is_idempotent() {
    let temp = tempdir();
    let fixture = custom_history_fixture("basic.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T12:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 2);
    assert_eq!(first.imported_edges, 2);

    let root_provider_session_id =
        custom_history_internal_session_id("demo-agent", "demo-source", "demo-session");
    let child_provider_session_id =
        custom_history_internal_session_id("demo-agent", "demo-source", "demo-session-worker");
    let root_id = provider_session_uuid(CaptureProvider::Custom, &root_provider_session_id);
    let child_id = provider_session_uuid(CaptureProvider::Custom, &child_provider_session_id);
    let root = store.get_session(root_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(root.provider, CaptureProvider::Custom);
    assert_eq!(child.parent_session_id, Some(root_id));
    assert!(root
        .sync
        .metadata
        .to_string()
        .contains("\"provider_key\":\"demo-agent\""));
    let events = store.events_for_session(root_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].payload.to_string().contains("Add a parser test."));

    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let touched: i64 = conn
        .query_row("SELECT COUNT(*) FROM files_touched", [], |row| row.get(0))
        .unwrap();
    assert_eq!(touched, 1);
    let spawned_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_edges WHERE edge_type = 'spawned'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(spawned_edges, 1);
    let cursor_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_cursors WHERE stream LIKE 'provider:custom:demo-agent:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor_count, 1);
    let cursor: String = conn
        .query_row(
            "SELECT cursor FROM sync_cursors WHERE stream LIKE 'provider:custom:demo-agent:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor, "5");
    let raw_cursor_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_cursors WHERE stream = 'demo-agent:demo-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_cursor_count, 0);
    drop(conn);

    let second = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T12:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_events, 2);
    assert_eq!(second.skipped_edges, 2);
}

#[test]
pub(crate) fn custom_history_jsonl_malformed_import_is_atomic_by_default() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T13:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 1);
    assert_eq!(store.capture_source_count().unwrap(), 0);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 0);
    assert_eq!(events, 0);
}

#[test]
pub(crate) fn custom_history_jsonl_rejects_oversized_line() {
    let temp = tempdir();
    let path = temp.path().join("oversized-custom.jsonl");
    write_oversized_jsonl_line(&path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_custom_history_jsonl_v1(
        &path,
        &mut store,
        CustomHistoryJsonlV1ImportOptions::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("provider JSONL line exceeds"));
    assert_eq!(store.capture_source_count().unwrap(), 0);
}

#[test]
pub(crate) fn custom_history_jsonl_preview_overrides_raw_payload_for_searchable_event_payload() {
    let temp = tempdir();
    let fixture = temp.path().join("preview-overrides-payload.jsonl");
    fs::write(
        &fixture,
        [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"preview-agent","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-06-23T14:00:00Z"}"#,
            r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-06-23T14:00:01Z","payload":{"raw":"unindexed-raw-payload-token"},"preview":"bounded searchable preview text"}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    let session_id = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("preview-agent", "src", "run"),
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].payload["body"],
        json!({ "text": "bounded searchable preview text" })
    );
    assert!(!events[0]
        .payload
        .to_string()
        .contains("unindexed-raw-payload-token"));
    assert_eq!(
        events[0].sync.metadata["metadata"]["ctx_history_jsonl_v1"]["raw_payload"]["raw"].as_str(),
        Some("unindexed-raw-payload-token")
    );
}

#[test]
pub(crate) fn custom_history_jsonl_namespaces_provider_keys_to_avoid_collisions() {
    let temp = tempdir();
    let fixture = temp.path().join("same-native-ids.jsonl");
    fs::write(
        &fixture,
        [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"alpha","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"same","started_at":"2026-06-23T14:00:00Z"}"#,
            r#"{"record_type":"event","source_id":"src","session_id":"same","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"alpha text"}}"#,
            r#"{"record_type":"source","source_id":"src-2","provider_key":"beta","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"src-2","session_id":"same","started_at":"2026-06-23T14:01:00Z"}"#,
            r#"{"record_type":"event","source_id":"src-2","session_id":"same","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:01:01Z","payload":{"text":"beta text"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    let alpha_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("alpha", "src", "same"),
    );
    let beta_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("beta", "src-2", "same"),
    );
    assert_ne!(alpha_session, beta_session);
    assert!(store
        .events_for_session(alpha_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("alpha text")));
    assert!(store
        .events_for_session(beta_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("beta text")));
}

#[test]
pub(crate) fn custom_history_jsonl_hashes_delimited_identifiers_without_collisions() {
    let temp = tempdir();
    let fixture = temp.path().join("delimited-identifiers.jsonl");
    fs::write(
        &fixture,
        [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"a:b","provider_key":"delim-agent","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"a:b","session_id":"c","started_at":"2026-06-23T14:00:00Z"}"#,
            r#"{"record_type":"event","source_id":"a:b","session_id":"c","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"left text"}}"#,
            r#"{"record_type":"source","source_id":"a","provider_key":"delim-agent","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"a","session_id":"b:c","started_at":"2026-06-23T14:01:00Z"}"#,
            r#"{"record_type":"event","source_id":"a","session_id":"b:c","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:01:01Z","payload":{"text":"right text"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    let left_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("delim-agent", "a:b", "c"),
    );
    let right_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("delim-agent", "a", "b:c"),
    );
    assert_ne!(left_session, right_session);
    assert!(store
        .events_for_session(left_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("left text")));
    assert!(store
        .events_for_session(right_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("right text")));
}

#[test]
pub(crate) fn custom_history_jsonl_dedupes_explicit_parent_child_edge_from_session_parent() {
    let temp = tempdir();
    let fixture = temp.path().join("duplicate-parent-child.jsonl");
    fs::write(
        &fixture,
        [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"edge-agent","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"root","started_at":"2026-06-23T15:00:00Z"}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"child","parent_session_id":"root","started_at":"2026-06-23T15:00:01Z"}"#,
            r#"{"record_type":"edge","source_id":"src","from_session_id":"root","to_session_id":"child","edge_type":"parent_child","edge_id":"explicit-parent","occurred_at":"2026-06-23T15:00:02Z"}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T15:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_edges, 1);
    assert_eq!(summary.skipped_edges, 1);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let parent_child_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_edges WHERE edge_type = 'parent_child'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parent_child_edges, 1);
}
