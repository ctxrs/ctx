use super::support::*;

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
            allow_partial_failures: true,
            ..CrushSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 4);
    assert_eq!(first.imported_edges, 1);
    let parent_id = provider_session_uuid(CaptureProvider::Crush, "crush-root");
    let child_id = provider_session_uuid(CaptureProvider::Crush, "crush-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
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
            allow_partial_failures: true,
            ..CrushSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 4);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
fn native_goose_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("goose/v14/sessions.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_goose_sessions_sqlite(
        &fixture,
        &mut store,
        GooseSessionsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..GooseSessionsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = provider_session_uuid(CaptureProvider::Goose, "goose-root");
    store.get_session(session_id).unwrap();
    let source = store
        .capture_source_by_external_session(CaptureProvider::Goose, "goose-root")
        .unwrap()
        .unwrap();
    assert_eq!(source.descriptor.cwd.as_deref(), Some("/workspace/goose"));
    assert!(source
        .sync
        .metadata
        .to_string()
        .contains("\"goose_schema_version\":14"));
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(store
        .search_event_hits("goose oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Goose)));

    let second = import_goose_sessions_sqlite(
        &fixture,
        &mut store,
        GooseSessionsSqliteImportOptions {
            allow_partial_failures: true,
            ..GooseSessionsSqliteImportOptions::default()
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
            allow_partial_failures: true,
            ..KiroSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = provider_session_uuid(
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
            allow_partial_failures: true,
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
fn native_astrbot_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("astrbot/v1/data/data_v4.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::AstrBot, fixture.clone());
    assert_eq!(source.source_format, ASTRBOT_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_astrbot_sqlite(
        &fixture,
        &mut store,
        AstrBotSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-06T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..AstrBotSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);

    let session_id = provider_session_uuid(CaptureProvider::AstrBot, "umo-astrbot-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::AstrBot);
    let source = store
        .capture_source_by_external_session(CaptureProvider::AstrBot, "umo-astrbot-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_format"].as_str(),
        Some(ASTRBOT_SQLITE_SOURCE_FORMAT)
    );

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("ASTRBOT_ORACLE_USER_TEXT violet jasper harbor"));
    assert!(rendered.contains("ASTRBOT_ORACLE_ASSISTANT_TEXT copper lantern atlas"));
    assert!(rendered.contains("ASTRBOT_PLATFORM_HISTORY_TEXT saffron comet"));

    assert!(store
        .search_event_hits("ASTRBOT_ORACLE_ASSISTANT_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::AstrBot)));
    assert!(store
        .search_event_hits("ASTRBOT_PLATFORM_HISTORY_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::AstrBot)));

    let second = import_astrbot_sqlite(
        &fixture,
        &mut store,
        AstrBotSqliteImportOptions {
            allow_partial_failures: true,
            ..AstrBotSqliteImportOptions::default()
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
            allow_partial_failures: true,
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 5);

    let session_id = provider_session_uuid(CaptureProvider::Junie, "session-260607-100000-acme");
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
    assert_eq!(events.len(), 5);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::CommandOutput));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("JUNIE_ORACLE_USER_TEXT violet cedar compass"));
    assert!(rendered.contains("JUNIE_TERMINAL_OUTPUT saffron harbor"));
    assert!(rendered.contains("JUNIE_FILE_CHANGE_TEXT cobalt lantern"));
    assert!(rendered.contains("JUNIE_RESULT_TEXT copper lantern atlas"));

    assert!(store
        .search_event_hits("JUNIE_RESULT_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));
    assert!(store
        .search_event_hits("JUNIE_TERMINAL_OUTPUT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));

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
            allow_partial_failures: true,
            ..JunieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 5);
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
            allow_partial_failures: true,
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
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
            allow_partial_failures: true,
            ..ZedThreadsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 5);
    assert_eq!(first.imported_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Zed, "zed-root");
    let child_id = provider_session_uuid(CaptureProvider::Zed, "zed-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 3);
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
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
        source.sync.metadata["source_metadata"]["upstream_schema_anchor"]["commit"].as_str(),
        Some("e3b73c6b30cdc09e820823fe44542b89850d4be1")
    );

    let second = import_zed_threads_sqlite(
        &fixture,
        &mut store,
        ZedThreadsSqliteImportOptions {
            allow_partial_failures: true,
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
    assert!(err
        .to_string()
        .contains("Zed threads table missing required column(s): data"));

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
    let db = temp.path().join(".local/share/zed/threads/threads.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Zed);
    let source = sources
        .iter()
        .find(|source| source.source_format == ZED_THREADS_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Zed source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Zed);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}

#[test]
fn native_forgecode_fixture_imports_searches_reimports_and_file_metrics() {
    let temp = tempdir();
    let fixture = provider_history_fixture("forgecode/v1/forge.db");
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let source = provider_source_for_path(CaptureProvider::ForgeCode, fixture.clone());
    assert_eq!(source.source_format, FORGECODE_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_forgecode_sqlite(
        &fixture,
        &mut store,
        ForgeCodeSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ForgeCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = provider_session_uuid(CaptureProvider::ForgeCode, "forge-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(store
        .search_event_hits("forgecode oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::ForgeCode)));
    let file_touch_count: i64 = Connection::open(&store_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ctx_files_touched WHERE provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file_touch_count, 4);

    let second = import_forgecode_sqlite(
        &fixture,
        &mut store,
        ForgeCodeSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..ForgeCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
    let file_touch_count_after: i64 = Connection::open(&store_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ctx_files_touched WHERE provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file_touch_count_after, file_touch_count);
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
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id =
        provider_session_uuid(CaptureProvider::DeepAgents, "deepagents-fixture-thread");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Tool)));
    assert!(events
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
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
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
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("invalid Deep Agents msgpack payload"));
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

#[test]
fn native_mistral_vibe_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mistral-vibe/v1/logs/session");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::MistralVibe, fixture.clone());
    assert_eq!(source.source_format, "mistral_vibe_session_jsonl_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_mistral_vibe_history(
        &fixture,
        &mut store,
        MistralVibeImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T19:05:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..MistralVibeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);
    let session_id = provider_session_uuid(CaptureProvider::MistralVibe, "mistral-vibe-native");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(store
        .search_event_hits("mistral vibe oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::MistralVibe)));

    let second = import_mistral_vibe_history(
        &fixture,
        &mut store,
        MistralVibeImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..MistralVibeImportOptions::default()
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
fn native_mux_fixture_imports_searches_reimports_and_subagents() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/v0.27.0/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Mux, fixture.clone());
    assert_eq!(source.source_format, "mux_session_jsonl_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T19:20:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);
    assert_eq!(first.imported_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Mux, "mux-parent-session");
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 4);
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let parent_rendered = serde_json::to_string(&parent_events).unwrap();
    assert!(parent_rendered.contains("mux jsonl oracle prompt"));
    assert!(parent_rendered.contains("mux partial response still searchable"));
    assert!(parent_rendered.contains("src/mux_oracle.txt"));

    let child_id = provider_session_uuid(CaptureProvider::Mux, "mux-child-session");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    let child_events = store.events_for_session(child_id).unwrap();
    assert_eq!(child_events.len(), 2);
    assert!(serde_json::to_string(&child_events)
        .unwrap()
        .contains("src/mux_child_oracle.txt"));

    assert!(store
        .search_event_hits("mux jsonl oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/mux_oracle.txt"));

    let second = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..MuxImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_mux_reports_malformed_jsonl_partially() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/malformed/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            allow_partial_failures: true,
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert!(store
        .search_event_hits("mux after malformed oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
}
#[test]
fn native_rovodev_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("rovodev/v1/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::RovoDev, fixture.clone());
    assert_eq!(source.source_format, "rovodev_session_json_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_rovodev_history(
        &fixture,
        &mut store,
        RovoDevImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    assert!(store
        .search_event_hits("rovodev fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/rovodev_oracle.rs"));

    let second = import_rovodev_history(
        &fixture,
        &mut store,
        RovoDevImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..RovoDevImportOptions::default()
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
fn native_forgecode_reports_missing_table_and_corrupt_db() {
    let temp = tempdir();
    let missing_table = temp.path().join("missing-forge.db");
    let conn = Connection::open(&missing_table).unwrap();
    conn.execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(conn);
    let corrupt = temp.path().join("corrupt-forge.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_forgecode_sqlite(
        &missing_table,
        &mut store,
        ForgeCodeSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("ForgeCode .forge.db is missing required conversations table"));

    let err = import_forgecode_sqlite(
        &corrupt,
        &mut store,
        ForgeCodeSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]

fn native_jsonl_tree_imports_gemini_droid_and_copilot_smokes() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let gemini = write_gemini_smoke_fixture(&temp);
    let gemini_summary = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_summary.failed, 0);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_edges, 1);

    let tabnine = provider_history_fixture("tabnine-cli/.tabnine/agent");
    let tabnine_summary = import_tabnine_cli_history(
        &tabnine,
        &mut store,
        TabnineCliImportOptions {
            allow_partial_failures: true,
            ..TabnineCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(tabnine_summary.failed, 0, "{:?}", tabnine_summary.failures);
    assert_eq!(tabnine_summary.imported_sessions, 2);
    assert_eq!(tabnine_summary.imported_events, 6);
    assert_eq!(tabnine_summary.imported_edges, 1);

    let tabnine_events = store
        .events_for_session(provider_session_uuid(
            CaptureProvider::Tabnine,
            "tabnine-root",
        ))
        .unwrap();
    assert!(tabnine_events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert!(tabnine_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    let tabnine_rendered = serde_json::to_string(&tabnine_events).unwrap();
    assert!(tabnine_rendered.contains("tabnine jsonl oracle prompt"));
    assert!(tabnine_rendered.contains("tabnine jsonl oracle answer"));
    assert!(tabnine_rendered.contains("src/tabnine_oracle.txt"));

    let tabnine_child = provider_session_uuid(CaptureProvider::Tabnine, "tabnine-child");
    let tabnine_parent = provider_session_uuid(CaptureProvider::Tabnine, "tabnine-root");
    assert_eq!(
        store.get_session(tabnine_child).unwrap().parent_session_id,
        Some(tabnine_parent)
    );

    let droid = write_droid_smoke_fixture(&temp);
    let droid_summary = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_summary.failed, 0);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_edges, 1);

    let copilot = write_copilot_smoke_fixture(&temp);
    let copilot_summary = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            allow_partial_failures: true,
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_summary.failed, 0);
    assert_eq!(copilot_summary.imported_sessions, 1);
    assert_eq!(copilot_summary.imported_events, 5);
}

#[test]
fn native_jsonl_tree_imports_qwen_and_kimi_smokes_are_idempotent() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let qwen = write_qwen_smoke_fixture(&temp);
    let qwen_summary = import_qwen_code_history(
        &qwen,
        &mut store,
        QwenCodeImportOptions {
            allow_partial_failures: true,
            ..QwenCodeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(qwen_summary.failed, 0, "{:?}", qwen_summary.failures);
    assert_eq!(qwen_summary.imported_sessions, 1);
    assert_eq!(qwen_summary.imported_events, 3);

    let qwen_events = store
        .events_for_session(provider_session_uuid(
            CaptureProvider::QwenCode,
            "qwen-smoke",
        ))
        .unwrap();
    assert_eq!(qwen_events.len(), 3);
    assert!(qwen_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(qwen_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let qwen_rendered = serde_json::to_string(&qwen_events).unwrap();
    assert!(qwen_rendered.contains("qwen jsonl oracle prompt"));
    assert!(qwen_rendered.contains("src/qwen_oracle.txt"));

    let qwen_second = import_qwen_code_history(
        &qwen,
        &mut store,
        QwenCodeImportOptions {
            allow_partial_failures: true,
            ..QwenCodeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(qwen_second.failed, 0, "{:?}", qwen_second.failures);
    assert_eq!(qwen_second.imported_sessions, 0);
    assert_eq!(qwen_second.imported_events, 0);

    let kimi = write_kimi_smoke_fixture(&temp);
    let kimi_summary = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            allow_partial_failures: true,
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kimi_summary.failed, 0, "{:?}", kimi_summary.failures);
    assert_eq!(kimi_summary.imported_sessions, 2);
    assert_eq!(kimi_summary.imported_events, 7);
    assert_eq!(kimi_summary.imported_edges, 1);

    let kimi_events = store
        .events_for_session(provider_session_uuid(
            CaptureProvider::KimiCodeCli,
            "kimi-smoke",
        ))
        .unwrap();
    assert_eq!(kimi_events.len(), 5);
    assert!(kimi_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(kimi_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let kimi_rendered = serde_json::to_string(&kimi_events).unwrap();
    assert!(kimi_rendered.contains("kimi jsonl oracle prompt"));
    assert!(kimi_rendered.contains("src/kimi_oracle.txt"));

    let kimi_child =
        provider_session_uuid(CaptureProvider::KimiCodeCli, "kimi-smoke/agents/agent-1");
    let kimi_parent = provider_session_uuid(CaptureProvider::KimiCodeCli, "kimi-smoke");
    assert_eq!(
        store.get_session(kimi_child).unwrap().parent_session_id,
        Some(kimi_parent)
    );

    let kimi_second = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            allow_partial_failures: true,
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kimi_second.failed, 0, "{:?}", kimi_second.failures);
    assert_eq!(kimi_second.imported_sessions, 0);
    assert_eq!(kimi_second.imported_events, 0);
    assert_eq!(kimi_second.imported_edges, 0);
}
#[test]
fn native_jsonl_tree_skips_headerless_native_files() {
    let temp = tempdir();
    let root = temp.path().join("gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("headerless.jsonl"),
        "{\"type\":\"user\",\"content\":\"missing session header\"}\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_gemini_cli_history(
        temp.path().join("gemini/.gemini"),
        &mut store,
        GeminiCliImportOptions {
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no importable native JSONL session header"));
}

#[test]
fn native_jsonl_tree_tolerates_unimportable_siblings_for_shared_providers() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let gemini = write_gemini_smoke_fixture(&temp);
    write_unimportable_jsonl_siblings(
        &temp.path().join("gemini/.gemini/tmp/project/chats"),
        "gemini",
    );
    let gemini_summary = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_summary.failed, 2, "{:?}", gemini_summary.failures);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_events, 5);
    assert_provider_failures_include_headerless_and_malformed(&gemini_summary);

    let droid = write_droid_smoke_fixture(&temp);
    write_unimportable_jsonl_siblings(&temp.path().join("droid/sessions/project"), "droid");
    let droid_summary = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_summary.failed, 2, "{:?}", droid_summary.failures);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_events, 5);
    assert_provider_failures_include_headerless_and_malformed(&droid_summary);

    let copilot = write_copilot_smoke_fixture(&temp);
    write_unimportable_copilot_siblings(&temp.path().join("copilot/session-state"));
    let copilot_summary = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            allow_partial_failures: true,
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_summary.failed, 2, "{:?}", copilot_summary.failures);
    assert_eq!(copilot_summary.imported_sessions, 1);
    assert_eq!(copilot_summary.imported_events, 5);
    assert_provider_failures_include_headerless_and_malformed(&copilot_summary);
}
