use crate::provider::providers::trae::{TRAE_CN_INPUT_HISTORY_KEY, TRAE_STATE_VSCDB_SOURCE_FORMAT};
use crate::tests::support::assertions::{
    assert_event_type_count, assert_events_have_provider_citations, assert_search_hits_provider,
    assert_search_misses,
};
use crate::tests::support::paths::{copy_dir_all, provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::CaptureError;
use crate::{
    discover_provider_sources_for_provider, import_auggie_history, import_firebender_sqlite,
    import_lingma_sqlite, import_openclaw_history, import_rovodev_history, import_trae_history,
    provider_source_for_path, AuggieImportOptions, FirebenderSqliteImportOptions,
    LingmaSqliteImportOptions, OpenClawImportOptions, ProviderImportSupport, ProviderSourceStatus,
    RovoDevImportOptions, TraeImportOptions, AUGGIE_SESSION_JSON_SOURCE_FORMAT,
    LINGMA_SQLITE_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, Fidelity};
use ctx_history_store::Store;
use serde_json::json;
use std::fs;

#[test]
fn native_openclaw_store_import_is_typed_unsupported_schema() {
    let temp = tempdir();
    let root = temp.path().join("openclaw/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("empty.jsonl"), "").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let error = import_openclaw_history(
        temp.path().join("openclaw"),
        &mut store,
        OpenClawImportOptions {
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::UnsupportedSchema(ref reason)
            if reason == "OpenClaw Store ingestion was removed; use source-backed ingestion"
    ));
}

#[test]
fn native_trae_fixture_imports_searches_and_reimports() {
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

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Trae,
        "trae-workspace-1/trae-fixture-session",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Trae);
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-fixture")
    );
    let session_metadata = session.sync.metadata["metadata"].to_string();
    assert!(!session_metadata.contains("\"messages\""));
    assert!(!session_metadata.contains("trae oracle answer from state vscdb"));

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
fn native_trae_chatstore_entries_schema_drift_imports() {
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
fn native_trae_cn_input_history_key_imports_user_messages() {
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
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Trae,
        "cn-workspace/trae-cn-input-history",
    );
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
#[test]
fn native_auggie_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Auggie, fixture.clone());
    assert_eq!(source.source_format, AUGGIE_SESSION_JSON_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Auggie,
        "01K0AUGGIESESSION0000000000",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("auggie session json oracle prompt"));
    assert!(rendered.contains("Auggie session import finished"));
    assert!(rendered.contains("auggie node text oracle prompt"));
    assert!(store
        .search_event_hits("Auggie node response imported", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Auggie)));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Auggie, "01K0AUGGIESESSION0000000000")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["upstream_schema_anchor"]["package"].as_str(),
        Some("@augmentcode/auggie@0.32.0")
    );

    let second = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}

#[test]
fn native_auggie_tool_only_nodes_import_metadata_only_session() {
    let temp = tempdir();
    let root = temp.path().join("auggie/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("auggie-tool-only.json"),
        json!({
            "sessionId": "auggie-tool-only",
            "created": "2026-07-04T20:00:00Z",
            "chatHistory": [
                {
                    "exchange": {
                        "request_id": "req-tool-only",
                        "request_nodes": [
                            {
                                "type": "tool_call",
                                "name": "read_file",
                                "args": {
                                    "path": "src/auggie_tool_only.rs"
                                }
                            }
                        ],
                        "response_nodes": [
                            {
                                "type": "tool_result",
                                "content": "AUGGIE_RAW_TOOL_OUTPUT_NEEDLE"
                            },
                            {
                                "type": "tool_result",
                                "text_node": {
                                    "content": "AUGGIE_RAW_TEXT_NODE_TOOL_OUTPUT_NEEDLE"
                                }
                            }
                        ]
                    },
                    "finishedAt": "2026-07-04T20:00:01Z"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_auggie_history(
        &root,
        &mut store,
        AuggieImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T20:05:00Z".parse().unwrap(),
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);
    assert!(store.search_event_hits("read_file", 10).unwrap().is_empty());
    assert!(store
        .search_event_hits("AUGGIE_RAW_TOOL_OUTPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("AUGGIE_RAW_TEXT_NODE_TOOL_OUTPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Auggie, "auggie-tool-only");
    assert!(store.events_for_session(session_id).unwrap().is_empty());
}

#[test]
fn native_auggie_mixed_tool_nodes_do_not_store_raw_tool_output() {
    let temp = tempdir();
    let root = temp.path().join("auggie/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("auggie-mixed-tool.json"),
        json!({
            "sessionId": "auggie-mixed-tool",
            "created": "2026-07-04T20:10:00Z",
            "chatHistory": [
                {
                    "exchange": {
                        "request_id": "req-mixed-tool",
                        "request_message": "Auggie mixed request oracle",
                        "response_nodes": [
                            {
                                "text_node": {
                                    "content": "Auggie mixed response oracle"
                                }
                            },
                            {
                                "type": "tool_result",
                                "content": "AUGGIE_MIXED_RAW_TOOL_OUTPUT_NEEDLE"
                            }
                        ]
                    },
                    "finishedAt": "2026-07-04T20:10:01Z"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_auggie_history(
        &root,
        &mut store,
        AuggieImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T20:15:00Z".parse().unwrap(),
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Auggie, "auggie-mixed-tool");
    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("Auggie mixed request oracle"));
    assert!(rendered.contains("Auggie mixed response oracle"));
    assert!(rendered.contains("tool_node_count"));
    assert!(!rendered.contains("AUGGIE_MIXED_RAW_TOOL_OUTPUT_NEEDLE"));
    assert!(store
        .search_event_hits("AUGGIE_MIXED_RAW_TOOL_OUTPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_rovodev_non_array_message_history_rejects_no_real_message() {
    let temp = tempdir();
    let session_dir = temp.path().join("rovodev/sessions/rovodev-non-array");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("session_context.json"),
        json!({
            "session_id": "rovodev-non-array",
            "message_history": {
                "role": "user",
                "content": "rovodev non-array history should not import"
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_rovodev_history(
        temp.path().join("rovodev/sessions"),
        &mut store,
        RovoDevImportOptions {
            source_path: Some(temp.path().join("rovodev/sessions")),
            imported_at: "2026-07-04T20:10:00Z".parse().unwrap(),
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("missing message_history array"));
    assert!(store
        .search_event_hits("rovodev non-array history should not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_firebender_store_import_rejects_after_source_backed_cutover() {
    let temp = tempdir();
    let project_root = temp.path().join("firebender-project");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let error = import_firebender_sqlite(
        &project_root,
        &mut store,
        FirebenderSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(project_root.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:10:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..FirebenderSqliteImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        crate::CaptureError::UnsupportedSchema(reason)
            if reason == "Firebender Store ingestion was removed; use source-backed ingestion"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}
#[test]
fn provider_sources_discovers_auggie_default_sessions() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let sessions = temp.path().join(".augment").join("sessions");
    copy_dir_all(&fixture, &sessions);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Auggie);
    let source = sources
        .iter()
        .find(|source| source.source_format == AUGGIE_SESSION_JSON_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Auggie source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Auggie);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, sessions);
}

#[test]
fn native_lingma_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Lingma, fixture.clone());
    assert_eq!(source.source_format, LINGMA_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);

    let alpha = stored_provider_session_id(&store, CaptureProvider::Lingma, "lingma-session-1");
    let events = store.events_for_session(alpha).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[1].sync.fidelity, Fidelity::SummaryOnly);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("lingma oracle prompt update"));
    assert!(rendered.contains("src/lingma_fixture.rs"));
    assert!(rendered.contains("Lingma summary oracle answer"));
    assert!(rendered.contains("summary_only"));
    assert!(rendered.contains("assistant_content_caveat"));
    assert!(store
        .search_event_hits("Lingma summary oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));
    assert!(store
        .search_event_hits("lingma oracle prompt update", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));

    let error_session =
        stored_provider_session_id(&store, CaptureProvider::Lingma, "lingma-session-2");
    let error_events = store.events_for_session(error_session).unwrap();
    assert_eq!(error_events.len(), 2);
    assert_eq!(error_events[1].event_type, EventType::Notice);
    assert_eq!(error_events[1].sync.fidelity, Fidelity::SummaryOnly);
    let error_rendered = serde_json::to_string(&error_events).unwrap();
    assert!(error_rendered.contains("\"body_kind\":\"error_result\""));
    assert!(error_rendered.contains("assistant_content_caveat"));
    assert!(!error_rendered.contains("sanitized Lingma error"));
    assert!(store
        .search_event_hits("sanitized Lingma error", 10)
        .unwrap()
        .iter()
        .all(|hit| !hit.preview.contains("sanitized Lingma error")));

    let second = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_lingma_import_reports_corrupt_sqlite() {
    let temp = tempdir();
    let db = temp.path().join("corrupt-lingma.db");
    fs::write(&db, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_lingma_sqlite(&db, &mut store, LingmaSqliteImportOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not a database") || err.contains("sqlite"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn native_lingma_import_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let link = temp.path().join("linked-lingma.db");
    symlink(&fixture, &link).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err =
        import_lingma_sqlite(&link, &mut store, LingmaSqliteImportOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-lingma.db")
                && reason == "Lingma SQLite source must be a regular non-symlink file"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}
