#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_trae_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("trae/User/workspaceStorage");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_trae_history(
        &fixture,
        &mut store,
        TraeImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T21:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let source = provider_source_for_path(CaptureProvider::Trae, fixture.clone());
    assert_eq!(source.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let session_id = provider_session_uuid(
        CaptureProvider::Trae,
        "trae-workspace-1/trae-fixture-session",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Trae);
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-fixture")
    );

    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("trae oracle prompt from state vscdb"));
    assert!(rendered.contains("trae oracle answer from state vscdb"));
    assert!(store
        .search_event_hits("trae oracle answer", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));

    let second = import_trae_history(
        &fixture,
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}

#[test]
pub(crate) fn native_trae_chatstore_entries_schema_drift_imports() {
    let temp = tempdir();
    let workspace = temp.path().join("User/workspaceStorage/schema-drift");
    fs::create_dir_all(&workspace).unwrap();
    let db_path = workspace.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    let value = json!({
        "entries": {
            "drift-session": {
                "id": "drift-session",
                "name": "Drift session",
                "messages": [
                    {
                        "id": "drift-user",
                        "role": "user",
                        "content": [{"type": "text", "text": "trae drift prompt"}],
                        "createdAt": "2026-07-05T12:00:00Z"
                    },
                    {
                        "id": "drift-assistant",
                        "role": "assistant",
                        "content": {"summary": "trae drift answer"},
                        "createdAt": "2026-07-05T12:01:00Z"
                    }
                ]
            }
        }
    })
    .to_string();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES ('ChatStore', ?1)",
        [value],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_trae_history(
        temp.path().join("User/workspaceStorage"),
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(store
        .search_event_hits("trae drift answer", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}

#[test]
pub(crate) fn native_trae_cn_input_history_key_imports_user_messages() {
    let temp = tempdir();
    let workspace = temp
        .path()
        .join("Trae CN/User/workspaceStorage/cn-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-cn-fixture"}"#,
    )
    .unwrap();
    let db_path = workspace.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        rusqlite::params![
            TRAE_CN_INPUT_HISTORY_KEY,
            json!([
                {
                    "id": "cn-input-1",
                    "inputText": "TRAE_CN_INPUT_HISTORY_ORACLE alpha",
                    "createdAt": "2026-07-05T13:00:00Z"
                },
                {
                    "id": "cn-input-2",
                    "text": "TRAE_CN_INPUT_HISTORY_ORACLE beta",
                    "createdAt": "2026-07-05T13:01:00Z"
                }
            ])
            .to_string()
        ],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_trae_history(
        temp.path().join("Trae CN/User/workspaceStorage"),
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        provider_session_uuid(CaptureProvider::Trae, "cn-workspace/trae-cn-input-history");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-cn-fixture")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .all(|event| event.role == Some(EventRole::User)));
    assert!(store
        .search_event_hits("TRAE_CN_INPUT_HISTORY_ORACLE", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}
