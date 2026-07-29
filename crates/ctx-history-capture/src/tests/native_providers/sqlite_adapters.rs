use crate::provider::providers::forgecode::forgecode_text_message_text;
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_crush_sqlite, import_deepagents_sqlite,
    import_goose_sessions_sqlite, import_junie_history, import_kiro_sqlite,
    import_zed_threads_sqlite, provider_source_for_path,
    CrushSqliteImportOptions, DeepAgentsSqliteImportOptions, GooseSessionsSqliteImportOptions,
    JunieImportOptions, KiroSqliteImportOptions, ProviderImportSupport, ProviderSourceStatus,
    ZedThreadsSqliteImportOptions, CRUSH_SQLITE_SOURCE_FORMAT,
    DEEPAGENTS_SQLITE_SOURCE_FORMAT, JUNIE_SESSION_EVENTS_SOURCE_FORMAT, KIRO_SQLITE_SOURCE_FORMAT,
    ZED_THREADS_SQLITE_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, Confidence, EventRole, EventType, FileChangeKind};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]

fn native_crush_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("crush/v1/crush.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_crush_sqlite(
        &fixture,
        &mut store,
        CrushSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..CrushSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.imported_edges, 1);
    let parent_id = stored_provider_session_id(&store, CaptureProvider::Crush, "crush-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Crush, "crush-child");
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
    assert!(!events
        .iter()
        .any(|event| event.event_type == EventType::CommandOutput));
    let child_events = store.events_for_session(child_id).unwrap();
    assert!(!child_events
        .iter()
        .any(|event| event.event_type == EventType::CommandOutput));
    assert!(store
        .search_event_hits("crush oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Crush)));
    let source = provider_source_for_path(CaptureProvider::Crush, fixture.clone());
    assert_eq!(source.source_format, CRUSH_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let second = import_crush_sqlite(
        &fixture,
        &mut store,
        CrushSqliteImportOptions {
            ..CrushSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 3);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
fn native_goose_store_import_is_a_typed_source_backed_rejection() {
    let temp = tempdir();
    let fixture = provider_history_fixture("goose/v14/sessions.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let error = import_goose_sessions_sqlite(
        &fixture,
        &mut store,
        GooseSessionsSqliteImportOptions {
            ..GooseSessionsSqliteImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(
        matches!(
            &error,
            crate::CaptureError::InvalidPayload(message)
                if message
                    == "Goose Store ingestion was removed; use source-backed ingestion"
        ),
        "unexpected Goose Store rejection: {error}"
    );
}

#[test]
fn native_kiro_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("kiro-cli/v2/data.sqlite3");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::KiroCli, fixture.clone());
    assert_eq!(source.source_format, KIRO_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_kiro_sqlite(
        &fixture,
        &mut store,
        KiroSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-25T20:12:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..KiroSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::KiroCli,
        "00000000-0000-4000-8000-000000000001",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::KiroCli);
    let source = store
        .capture_source_by_external_session(
            CaptureProvider::KiroCli,
            "00000000-0000-4000-8000-000000000001",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.cwd.as_deref(),
        Some("/workspace/kiro-fixture")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "/workspace/kiro-fixture"));
    assert!(store
        .search_event_hits("kiro oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::KiroCli)));

    let second = import_kiro_sqlite(
        &fixture,
        &mut store,
        KiroSqliteImportOptions {
            ..KiroSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}
#[test]
fn native_junie_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("junie/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Junie, fixture.clone());
    assert_eq!(source.source_format, JUNIE_SESSION_EVENTS_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_junie_history(
        &fixture,
        &mut store,
        JunieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-06T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Junie, "session-260607-100000-acme");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Junie);
    let source = store
        .capture_source_by_external_session(CaptureProvider::Junie, "session-260607-100000-acme")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.cwd.as_deref(),
        Some("/workspace/junie-fixture")
    );
    assert_eq!(
        source.sync.metadata["source_format"].as_str(),
        Some(JUNIE_SESSION_EVENTS_SOURCE_FORMAT)
    );

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(!events
        .iter()
        .any(|event| event.event_type == EventType::CommandOutput));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("JUNIE_ORACLE_USER_TEXT violet cedar compass"));
    assert!(!rendered.contains("JUNIE_TERMINAL_OUTPUT saffron harbor"));
    assert!(!rendered.contains("JUNIE_FILE_CHANGE_TEXT cobalt lantern"));
    assert!(rendered.contains("JUNIE_RESULT_TEXT copper lantern atlas"));

    assert!(store
        .search_event_hits("JUNIE_RESULT_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));
    assert!(store
        .search_event_hits("JUNIE_TERMINAL_OUTPUT", 10)
        .unwrap()
        .is_empty());

    let archive = store.export_archive().unwrap();
    let touched = archive
        .files_touched
        .iter()
        .find(|file| file.path == "src/junie_theme.rs")
        .expect("missing Junie file touch");
    assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
    assert_eq!(touched.confidence, Confidence::Explicit);
    assert!(touched.event_id.is_some());

    let second = import_junie_history(
        &fixture,
        &mut store,
        JunieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            ..JunieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 0);
}

#[test]
fn native_junie_index_rejects_traversal_session_ids() {
    let temp = tempdir();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(sessions.join("session-safe")).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        "{\"sessionId\":\"../escape\",\"createdAt\":1783339200000}\n\
             {\"sessionId\":\"session-safe\",\"createdAt\":1783339200000,\"taskName\":\"safe\"}\n",
    )
    .unwrap();
    fs::write(
        sessions.join("session-safe").join("events.jsonl"),
        "{\"kind\":\"UserPromptEvent\",\"prompt\":\"JUNIE_SAFE_SESSION_TEXT\"}\n",
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_junie_history(
        &sessions,
        &mut store,
        JunieImportOptions {
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].line, 1);
    assert!(summary.failures[0]
        .error
        .contains("missing or unsafe sessionId"));
    assert_eq!(summary.imported_sessions, 1);
    assert!(store
        .capture_source_by_external_session(CaptureProvider::Junie, "../escape")
        .unwrap()
        .is_none());
    assert!(store
        .search_event_hits("JUNIE_SAFE_SESSION_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));
}
#[test]
fn native_zed_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("zed/v1/threads.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Zed, fixture.clone());
    assert_eq!(source.source_format, ZED_THREADS_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_zed_threads_sqlite(
        &fixture,
        &mut store,
        ZedThreadsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:10:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..ZedThreadsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 5);
    assert_eq!(first.imported_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Zed, "zed-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Zed, "zed-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 3);
    assert_eq!(
        parent_events
            .iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![EventType::Message, EventType::ToolCall, EventType::Summary,]
    );
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(!parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
    let tool_call = parent_events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert_eq!(tool_call.payload["provider_event_index"].as_u64(), Some(2));
    assert_eq!(
        tool_call.sync.metadata["nativepath_publication"].as_u64(),
        Some(2)
    );
    assert!(tool_call.payload["cursor"].as_str().is_some());
    let rendered = serde_json::to_string(&parent_events).unwrap();
    assert!(rendered.contains("zed sqlite oracle prompt"));
    assert!(rendered.contains("zed sqlite oracle answer"));
    assert!(rendered.contains("zed compacted summary oracle"));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/zed_oracle.txt"));
    assert!(store
        .search_event_hits("zed sqlite oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Zed)));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Zed, "zed-root")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.source_format.as_deref(),
        Some(ZED_THREADS_SQLITE_SOURCE_FORMAT)
    );
    assert!(source.descriptor.source_identity.is_some());
    assert_eq!(
        source.sync.metadata["nativepath_publication"].as_u64(),
        Some(2)
    );

    let second = import_zed_threads_sqlite(
        &fixture,
        &mut store,
        ZedThreadsSqliteImportOptions {
            ..ZedThreadsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 5);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
fn native_zed_tool_call_input_is_metadata_only_and_not_searchable() {
    let temp = tempdir();
    let fixture = write_zed_raw_tool_input_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_zed_threads_sqlite(
        &fixture,
        &mut store,
        ZedThreadsSqliteImportOptions {
            source_path: Some(fixture.clone()),
            ..ZedThreadsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Zed, "zed-raw-input");
    let events = store.events_for_session(session_id).unwrap();
    let tool_call = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .expect("tool call event imported");
    let rendered_tool_call = serde_json::to_string(tool_call).unwrap();
    assert!(rendered_tool_call.contains("edit_file"));
    assert!(!rendered_tool_call.contains("ZED_RAW_TOOL_INPUT_NEEDLE"));
    assert!(!rendered_tool_call.contains("ZED_RAW_TOOL_INPUT_KEY_NEEDLE"));
    assert!(!rendered_tool_call.contains("*** Begin Patch"));

    let rendered_events = serde_json::to_string(&events).unwrap();
    assert!(rendered_events.contains("zed raw input prompt oracle"));
    assert!(!rendered_events.contains("ZED_RAW_TOOL_INPUT_NEEDLE"));
    assert!(!rendered_events.contains("ZED_RAW_TOOL_INPUT_KEY_NEEDLE"));
    assert!(store
        .search_event_hits("ZED_RAW_TOOL_INPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("ZED_RAW_TOOL_INPUT_KEY_NEEDLE", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_zed_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = temp.path().join("zed-malformed.db");
    {
        let conn = rusqlite::Connection::open(&malformed).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    summary TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    data_type TEXT NOT NULL
                );",
        )
        .unwrap();
    }
    let corrupt = temp.path().join("zed-corrupt.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_zed_threads_sqlite(
        &malformed,
        &mut store,
        ZedThreadsSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("Zed NativePath threads table missing required column(s): data"),
        "{err}"
    );

    let err = import_zed_threads_sqlite(
        &corrupt,
        &mut store,
        ZedThreadsSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
fn provider_sources_discovers_zed_default_db() {
    let temp = tempdir();
    let data = temp.path().join("platform-data");
    let db = data.join("zed/threads/threads.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();

    let context = crate::DiscoveryContext::new(
        temp.path(),
        temp.path(),
        crate::DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs {
            data: Some(data),
            ..crate::DiscoveryPlatformDirs::default()
        },
    );
    let sources =
        crate::discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Zed)
            .sources;
    let source = sources
        .iter()
        .find(|source| source.source_format == ZED_THREADS_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Zed source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Zed);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}

fn write_zed_raw_tool_input_db(temp: &TempDir) -> PathBuf {
    let db = temp.path().join("zed-raw-input.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "create table threads (
            id text primary key,
            parent_id text,
            folder_paths text,
            folder_paths_order text,
            summary text not null,
            updated_at text not null,
            data_type text not null,
            data blob not null,
            created_at text
        );",
    )
    .unwrap();
    let thread = json!({
        "title": "Zed raw input fixture",
        "version": "0.3.0",
        "messages": [
            {
                "User": {
                    "content": [
                        {"Text": "zed raw input prompt oracle"}
                    ]
                }
            },
            {
                "Agent": {
                    "content": [
                        {
                            "ToolUse": {
                                "id": "tool-raw-input",
                                "name": "edit_file",
                                "input": {
                                    "path": "src/zed_raw_input.rs",
                                    "patch": "*** Begin Patch\nZED_RAW_TOOL_INPUT_NEEDLE\n*** End Patch",
                                    "secret": "ZED_RAW_TOOL_INPUT_NEEDLE",
                                    "ZED_RAW_TOOL_INPUT_KEY_NEEDLE": "x"
                                }
                            }
                        }
                    ]
                }
            }
        ]
    });
    conn.execute(
        "insert into threads (
            id, parent_id, folder_paths, folder_paths_order, summary, updated_at, data_type, data, created_at
        ) values (?1, NULL, ?2, NULL, ?3, ?4, 'json', ?5, ?6)",
        rusqlite::params![
            "zed-raw-input",
            "/workspace/zed",
            "Zed raw input",
            "2026-07-04T12:00:00Z",
            serde_json::to_vec(&thread).unwrap(),
            "2026-07-04T11:59:00Z",
        ],
    )
    .unwrap();
    db
}

#[test]
fn native_forgecode_empty_text_message_does_not_fabricate_search_text() {
    let text = forgecode_text_message_text(&json!({"role": "assistant"}), EventType::Message);
    assert!(text.is_empty());
}

#[test]
fn native_deepagents_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("deepagents/v1/sessions.db");
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let source = provider_source_for_path(CaptureProvider::DeepAgents, fixture.clone());
    assert_eq!(source.source_format, DEEPAGENTS_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_deepagents_sqlite(
        &fixture,
        &mut store,
        DeepAgentsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T19:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::DeepAgents,
        "deepagents-fixture-thread",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert!(!events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(events.iter().all(|event| {
        event
            .sync
            .metadata
            .to_string()
            .contains("decoded from writes.messages only")
    }));
    assert!(store
        .search_event_hits("deepagents fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::DeepAgents)));

    let source_metadata: String = Connection::open(&store_path)
        .unwrap()
        .query_row(
            "SELECT metadata_json FROM capture_sources WHERE provider = 'deepagents'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(source_metadata.contains("checkpoint state blobs are not indexed"));

    let second = import_deepagents_sqlite(
        &fixture,
        &mut store,
        DeepAgentsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            ..DeepAgentsSqliteImportOptions::default()
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
fn native_deepagents_reports_malformed_writes_and_corrupt_db() {
    let temp = tempdir();
    let fixture = provider_history_fixture("deepagents/v1/sessions.db");
    let malformed = temp.path().join("malformed-deepagents.db");
    fs::copy(&fixture, &malformed).unwrap();
    Connection::open(&malformed)
        .unwrap()
        .execute("UPDATE writes SET value = x'd9'", [])
        .unwrap();
    let corrupt = temp.path().join("corrupt-deepagents.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_deepagents_sqlite(
        &malformed,
        &mut store,
        DeepAgentsSqliteImportOptions {
            source_path: Some(malformed.clone()),
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("invalid Deep Agents msgpack payload"));
    // The terminal checkpoint row still contributes the source-scoped session
    // marker even when its only message row is rejected.
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);

    let err = import_deepagents_sqlite(
        &corrupt,
        &mut store,
        DeepAgentsSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}
