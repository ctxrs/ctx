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
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.imported_edges, 0);
    let parent_id = stored_provider_session_id(&store, CaptureProvider::Crush, "crush-root");
    assert!(store
        .sessions_by_external_session_limited(CaptureProvider::Crush, "crush-child", 10)
        .unwrap()
        .is_empty());
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
    assert_eq!(second.skipped_edges, 0);
}

#[test]
fn native_parent_child_edges_import_for_claimed_provider_shapes() {
    let temp = tempdir();

    let kilo = write_opencode_smoke_db(&temp, false);
    assert_imports_parent_child_edge(
        "Kilo",
        CaptureProvider::Kilo,
        "opencode-root",
        "opencode-child",
        |store| {
            import_kilo_sqlite(
                &kilo,
                store,
                KiloSqliteImportOptions {
                    source_path: Some(kilo.clone()),
                    allow_partial_failures: true,
                    ..KiloSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let mimocode = temp.path().join("mimocode-edge.db");
    fs::copy(&kilo, &mimocode).unwrap();
    assert_imports_parent_child_edge(
        "MiMo Code",
        CaptureProvider::MiMoCode,
        "opencode-root",
        "opencode-child",
        |store| {
            import_mimocode_sqlite(
                &mimocode,
                store,
                MiMoCodeSqliteImportOptions {
                    source_path: Some(mimocode.clone()),
                    allow_partial_failures: true,
                    ..MiMoCodeSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let crush = write_crush_edge_db(&temp);
    assert_imports_parent_child_edge(
        "Crush",
        CaptureProvider::Crush,
        "crush-edge-root",
        "crush-edge-child",
        |store| {
            import_crush_sqlite(
                &crush,
                store,
                CrushSqliteImportOptions {
                    source_path: Some(crush.clone()),
                    allow_partial_failures: true,
                    ..CrushSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let hermes = write_hermes_edge_db(&temp);
    assert_imports_parent_child_edge(
        "Hermes",
        CaptureProvider::Hermes,
        "hermes-edge-root",
        "hermes-edge-child",
        |store| {
            import_hermes_sqlite(
                &hermes,
                store,
                HermesSqliteImportOptions {
                    source_path: Some(hermes.clone()),
                    allow_partial_failures: true,
                    ..HermesSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let warp = write_warp_edge_db(&temp);
    assert_imports_parent_child_edge(
        "Warp",
        CaptureProvider::Warp,
        "warp-conversation-1",
        "warp-child-conversation",
        |store| {
            import_warp_sqlite(
                &warp,
                store,
                WarpSqliteImportOptions {
                    source_path: Some(warp.clone()),
                    allow_partial_failures: true,
                    ..WarpSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let mistral = write_mistral_vibe_edge_fixture(&temp);
    assert_imports_parent_child_edge(
        "Mistral Vibe",
        CaptureProvider::MistralVibe,
        "mistral-edge-root",
        "mistral-edge-child",
        |store| {
            import_mistral_vibe_history(
                &mistral,
                store,
                MistralVibeImportOptions {
                    source_path: Some(mistral.clone()),
                    allow_partial_failures: true,
                    ..MistralVibeImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let rovodev = write_rovodev_edge_fixture(&temp);
    assert_imports_parent_child_edge(
        "Rovo Dev",
        CaptureProvider::RovoDev,
        "rovodev-edge-root",
        "rovodev-edge-child",
        |store| {
            import_rovodev_history(
                &rovodev,
                store,
                RovoDevImportOptions {
                    source_path: Some(rovodev.clone()),
                    allow_partial_failures: true,
                    ..RovoDevImportOptions::default()
                },
            )
            .unwrap()
        },
    );
}

#[test]
fn native_tool_outputs_are_metadata_only_for_sqlite_provider_shapes() {
    let temp = tempdir();

    let crush = write_crush_tool_output_db(&temp);
    assert_imports_metadata_only_tool_output(
        "Crush",
        CaptureProvider::Crush,
        "crush-tool-output",
        "crush tool output policy oracle",
        "CRUSH_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH",
        Some("CRUSH_RAW_COMMAND_OUTPUT_SHOULD_NOT_SEARCH"),
        |store| {
            import_crush_sqlite(
                &crush,
                store,
                CrushSqliteImportOptions {
                    source_path: Some(crush.clone()),
                    allow_partial_failures: true,
                    ..CrushSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let hermes = write_hermes_tool_output_db(&temp);
    assert_imports_metadata_only_tool_output(
        "Hermes",
        CaptureProvider::Hermes,
        "hermes-tool-output",
        "hermes tool output policy oracle",
        "HERMES_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH",
        None,
        |store| {
            import_hermes_sqlite(
                &hermes,
                store,
                HermesSqliteImportOptions {
                    source_path: Some(hermes.clone()),
                    allow_partial_failures: true,
                    ..HermesSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );
}

fn assert_imports_parent_child_edge(
    label: &str,
    provider: CaptureProvider,
    parent_external_id: &str,
    child_external_id: &str,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = run_import(&mut store);
    assert_eq!(summary.failed, 0, "{label}: {:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2, "{label}: {summary:?}");
    assert_eq!(summary.imported_edges, 1, "{label}: {summary:?}");
    let parent_id = stored_provider_session_id(&store, provider, parent_external_id);
    let child_id = stored_provider_session_id(&store, provider, child_external_id);
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id),
        "{label}: child session did not point at parent"
    );
}

fn assert_imports_metadata_only_tool_output(
    label: &str,
    provider: CaptureProvider,
    external_session_id: &str,
    searchable: &str,
    raw_output: &str,
    raw_command_output: Option<&str>,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = run_import(&mut store);
    assert_eq!(summary.failed, 0, "{label}: {:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1, "{label}: {summary:?}");
    let session_id = stored_provider_session_id(&store, provider, external_session_id);
    let events = store.events_for_session(session_id).unwrap();
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    if let Some(raw_command_output) = raw_command_output {
        assert_event_type_count(&events, EventType::CommandOutput, 1);
        assert_search_misses(&store, raw_command_output);
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains(raw_command_output),
            "{label}: raw command output leaked into stored event payload"
        );
    }
    assert_events_have_provider_citations(&events);
    assert_search_hits_provider(&store, searchable, provider);
    assert_search_misses(&store, raw_output);
    assert!(
        !serde_json::to_string(&events).unwrap().contains(raw_output),
        "{label}: raw tool output leaked into stored event payload"
    );
}

fn write_crush_tool_output_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("crush-tool-output.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            parent_session_id text,
            title text,
            prompt_tokens integer,
            completion_tokens integer,
            cost real,
            created_at integer not null,
            updated_at integer not null,
            summary_message_id text
        );
        create table messages (
            id text primary key,
            session_id text not null,
            role text not null,
            parts text not null default '[]',
            created_at integer not null,
            updated_at integer not null,
            provider text,
            model text,
            is_summary_message integer not null default 0
        );
        create table files (
            id text primary key,
            session_id text not null,
            path text not null,
            version text,
            created_at integer not null,
            updated_at integer not null
        );
        create table read_files (
            session_id text not null,
            path text not null,
            read_at integer not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, null, 'tool output', 1, 1, 0.0, 1782259200000, 1782259203000, null)",
        ["crush-tool-output"],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'user', ?3, 1782259200000, 1782259200000, null, null, 0)",
        rusqlite::params![
            "crush-tool-user",
            "crush-tool-output",
            json!([{"type": "text", "text": "crush tool output policy oracle"}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 1782259201000, 1782259201000, null, null, 0)",
        rusqlite::params![
            "crush-tool-call",
            "crush-tool-output",
            json!([{"type": "tool_call", "data": {"name": "read_file", "input": {"path": "src/crush.rs"}}}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'tool', ?3, 1782259202000, 1782259202000, null, null, 0)",
        rusqlite::params![
            "crush-tool-result",
            "crush-tool-output",
            json!([{"type": "tool_result", "data": {"name": "read_file", "content": "CRUSH_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"}}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 1782259203000, 1782259203000, null, null, 0)",
        rusqlite::params![
            "crush-command-output",
            "crush-tool-output",
            json!([{"type": "shell_command", "data": {"command": "cargo test", "output": "CRUSH_RAW_COMMAND_OUTPUT_SHOULD_NOT_SEARCH"}}]).to_string(),
        ],
    )
    .unwrap();
    path
}

fn write_hermes_tool_output_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("hermes-tool-output.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            parent_session_id text,
            started_at real not null,
            cwd text
        );
        create table messages (
            id integer primary key autoincrement,
            session_id text not null,
            role text not null,
            content text,
            tool_calls text,
            tool_call_id text,
            tool_name text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', null, 1782259200.0, '/workspace/hermes')",
        ["hermes-tool-output"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'hermes tool output policy oracle', 1782259201.0)",
        ["hermes-tool-output"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, tool_calls, tool_name, timestamp)
         values (?1, 'assistant', 'calling read_file', ?2, 'read_file', 1782259202.0)",
        [
            "hermes-tool-output",
            r#"[{"id":"call-hermes-1","name":"read_file"}]"#,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, tool_call_id, tool_name, timestamp)
         values (?1, 'tool', 'HERMES_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH', 'call-hermes-1', 'read_file', 1782259203.0)",
        ["hermes-tool-output"],
    )
    .unwrap();
    path
}

fn write_crush_edge_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("crush-edge.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            parent_session_id text,
            title text,
            prompt_tokens integer,
            completion_tokens integer,
            cost real,
            created_at integer not null,
            updated_at integer not null,
            summary_message_id text
        );
        create table messages (
            id text primary key,
            session_id text not null,
            role text not null,
            parts text not null default '[]',
            created_at integer not null,
            updated_at integer not null,
            provider text,
            model text,
            is_summary_message integer not null default 0
        );
        create table files (
            id text primary key,
            session_id text not null,
            path text not null,
            version text,
            created_at integer not null,
            updated_at integer not null
        );
        create table read_files (
            session_id text not null,
            path text not null,
            read_at integer not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, null, 'root', 1, 1, 0.0, 1782259200000, 1782259201000, null)",
        ["crush-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, ?2, 'child', 1, 1, 0.0, 1782259202000, 1782259203000, null)",
        ["crush-edge-child", "crush-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'user', ?3, 1782259200000, 1782259200000, null, null, 0)",
        rusqlite::params![
            "crush-edge-root-msg",
            "crush-edge-root",
            json!([{"type": "text", "text": "crush edge root oracle"}]).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages values (?1, ?2, 'assistant', ?3, 1782259202000, 1782259202000, null, null, 0)",
        rusqlite::params![
            "crush-edge-child-msg",
            "crush-edge-child",
            json!([{"type": "text", "text": "crush edge child oracle"}]).to_string(),
        ],
    )
    .unwrap();
    path
}

fn write_hermes_edge_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("hermes-edge.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            parent_session_id text,
            started_at real not null,
            cwd text
        );
        create table messages (
            id integer primary key autoincrement,
            session_id text not null,
            role text not null,
            content text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', null, 1782259200.0, '/workspace/hermes')",
        ["hermes-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', ?2, 1782259202.0, '/workspace/hermes')",
        ["hermes-edge-child", "hermes-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'hermes edge root oracle', 1782259201.0)",
        ["hermes-edge-root"],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'hermes edge child oracle', 1782259203.0)",
        ["hermes-edge-child"],
    )
    .unwrap();
    path
}

fn write_warp_edge_db(temp: &TempDir) -> PathBuf {
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let path = temp.path().join("warp-edge.sqlite");
    fs::copy(&fixture, &path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let task: Vec<u8> = conn
        .query_row(
            "select task from agent_tasks where conversation_id = 'warp-conversation-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "insert into agent_conversations (conversation_id, conversation_data, last_modified_at)
         values (?1, ?2, '2026-06-24 12:00:05')",
        [
            "warp-child-conversation",
            r#"{"agent_name":"Warp child","parent_conversation_id":"warp-conversation-1"}"#,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into agent_tasks (conversation_id, task_id, task, last_modified_at)
         values (?1, ?2, ?3, '2026-06-24 12:00:06')",
        rusqlite::params!["warp-child-conversation", "warp-child-task", task],
    )
    .unwrap();
    path
}

fn write_mistral_vibe_edge_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("mistral-edge/logs/session");
    write_mistral_vibe_session(&root, "root", "mistral-edge-root", None);
    write_mistral_vibe_session(
        &root,
        "child",
        "mistral-edge-child",
        Some("mistral-edge-root"),
    );
    root
}

fn write_mistral_vibe_session(
    root: &Path,
    dir_name: &str,
    session_id: &str,
    parent_session_id: Option<&str>,
) {
    let session = root.join(dir_name);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        json!({
            "session_id": session_id,
            "parent_session_id": parent_session_id,
            "start_time": "2026-07-04T19:05:00Z",
            "environment": {"working_directory": "/workspace/mistral-edge"}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("messages.jsonl"),
        format!(
            "{}\n",
            json!({
                "role": "user",
                "content": format!("{session_id} oracle"),
                "message_id": format!("{session_id}-msg")
            })
        ),
    )
    .unwrap();
}

fn write_rovodev_edge_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("rovodev-edge/sessions");
    write_rovodev_session(&root, "rovodev-edge-root", None);
    write_rovodev_session(&root, "rovodev-edge-child", Some("rovodev-edge-root"));
    root
}

fn write_rovodev_session(root: &Path, session_id: &str, parent_session_id: Option<&str>) {
    let session = root.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        json!({
            "session_id": session_id,
            "parent_session_id": parent_session_id,
            "workspace_path": "/workspace/rovodev-edge",
            "created_at": "2026-07-04T18:20:00Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("session_context.json"),
        json!({
            "message_history": [{
                "id": format!("{session_id}-msg"),
                "role": "user",
                "created_at": "2026-07-04T18:20:00Z",
                "parts": [{"kind": "text", "text": format!("{session_id} oracle")}]
            }]
        })
        .to_string(),
    )
    .unwrap();
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
    let session_id = stored_provider_session_id(&store, CaptureProvider::Goose, "goose-root");
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

    let session_id = stored_provider_session_id(&store, CaptureProvider::AstrBot, "umo-astrbot-1");
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
    assert_eq!(first.imported_events, 6);
    assert_eq!(first.imported_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Zed, "zed-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Zed, "zed-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 4);
    assert_eq!(
        parent_events
            .iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![
            EventType::Message,
            EventType::ToolCall,
            EventType::ToolOutput,
            EventType::Summary,
        ]
    );
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
    let tool_call = parent_events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    let tool_output = parent_events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    assert_eq!(
        tool_call.sync.metadata["metadata"]["provider_event_identity_index"].as_u64(),
        Some(1)
    );
    assert_eq!(
        tool_output.sync.metadata["metadata"]["provider_event_identity_index"].as_u64(),
        Some(1_000_001)
    );
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
    assert_eq!(second.skipped_events, 6);
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
            allow_partial_failures: true,
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
    assert!(rendered_tool_call.contains("input_present"));
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
        "version": "test",
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
    let session_id = stored_provider_session_id(&store, CaptureProvider::ForgeCode, "forge-root");
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
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::DeepAgents,
        "deepagents-fixture-thread",
    );
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
    assert_eq!(summary.imported_sessions, 0);
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
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::MistralVibe, "mistral-vibe-native");
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

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Mux, "mux-parent-session");
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 4);
    assert_event_type_count(&parent_events, EventType::Message, 2);
    assert_event_type_count(&parent_events, EventType::ToolCall, 1);
    assert_event_type_count(&parent_events, EventType::ToolOutput, 1);
    assert_event_with_role(&parent_events, EventType::ToolOutput, EventRole::Assistant);
    assert_events_have_provider_citations(&parent_events);
    let parent_rendered = serde_json::to_string(&parent_events).unwrap();
    assert!(parent_rendered.contains("mux jsonl oracle prompt"));
    assert!(parent_rendered.contains("mux partial response still searchable"));
    assert!(parent_rendered.contains("src/mux_oracle.txt"));

    let child_id = stored_provider_session_id(&store, CaptureProvider::Mux, "mux-child-session");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    let child_events = store.events_for_session(child_id).unwrap();
    assert_eq!(child_events.len(), 2);
    assert_event_type_count(&child_events, EventType::Message, 1);
    assert_event_type_count(&child_events, EventType::ToolOutput, 1);
    assert_events_have_provider_citations(&child_events);
    assert!(serde_json::to_string(&child_events)
        .unwrap()
        .contains("src/mux_child_oracle.txt"));

    assert_search_hits_provider(&store, "mux jsonl oracle", CaptureProvider::Mux);
    assert_search_hits_provider(
        &store,
        "mux partial response still searchable",
        CaptureProvider::Mux,
    );
    assert_search_misses(&store, "child proof");
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
fn native_mux_skips_oversized_chat_record() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/v0.27.0/sessions");
    let chat_path = fixture.join("mux-parent-session/chat.jsonl");
    let original = fs::read(&chat_path).unwrap();
    let first_line_end = original
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&original[..first_line_end]);
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(&original[first_line_end..]);
    fs::write(&chat_path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-04T19:30:00Z".parse().unwrap(),
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 6);
    assert!(store
        .search_event_hits("mux jsonl oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
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
    let file_source = provider_source_for_path(
        CaptureProvider::RovoDev,
        fixture
            .join("rovodev-fixture-session")
            .join("session_context.json"),
    );
    assert_eq!(file_source.source_format, "rovodev_session_json_tree");
    assert_eq!(file_source.status, ProviderSourceStatus::Available);

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
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::RovoDev, "rovodev-fixture-session");
    let events = store.events_for_session(session_id).unwrap();
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some("rovodev_session_json_tree")
    );
    assert!(store
        .search_event_hits("rovodev fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
    assert_search_misses(&store, "wrote src/rovodev_oracle.rs");
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("wrote src/rovodev_oracle.rs"));
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
    assert_eq!(gemini_summary.imported_events, 6);
    assert_eq!(gemini_summary.imported_edges, 1);
    let gemini_parent = stored_provider_session_id(&store, CaptureProvider::Gemini, "gemini-root");
    let gemini_events = store.events_for_session(gemini_parent).unwrap();
    assert_eq!(gemini_events.len(), 4);
    assert_event_type_count(&gemini_events, EventType::Message, 1);
    assert_event_type_count(&gemini_events, EventType::ToolCall, 1);
    assert_event_type_count(&gemini_events, EventType::ToolOutput, 1);
    assert_event_type_count(&gemini_events, EventType::Notice, 1);
    assert_event_with_role(&gemini_events, EventType::ToolOutput, EventRole::Assistant);
    assert_events_have_provider_citations(&gemini_events);
    assert_search_hits_provider(
        &store,
        "gemini jsonl oracle prompt",
        CaptureProvider::Gemini,
    );
    assert_search_misses(&store, "GEMINI_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    let gemini_child = stored_provider_session_id(&store, CaptureProvider::Gemini, "gemini-child");
    assert_eq!(
        store.get_session(gemini_child).unwrap().parent_session_id,
        Some(gemini_parent)
    );
    let gemini_second = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_second.failed, 0, "{:?}", gemini_second.failures);
    assert_eq!(gemini_second.imported_sessions, 0);
    assert_eq!(gemini_second.imported_events, 0);
    assert_eq!(gemini_second.imported_edges, 0);
    assert_eq!(gemini_second.skipped_sessions, 2);
    assert_eq!(gemini_second.skipped_events, 6);
    assert_eq!(gemini_second.skipped_edges, 1);

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
    assert_eq!(tabnine_summary.imported_events, 7);
    assert_eq!(tabnine_summary.imported_edges, 1);

    let tabnine_events = store
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::Tabnine,
            "tabnine-root",
        ))
        .unwrap();
    assert_eq!(tabnine_events.len(), 5);
    assert_event_type_count(&tabnine_events, EventType::Message, 2);
    assert_event_type_count(&tabnine_events, EventType::ToolCall, 1);
    assert_event_type_count(&tabnine_events, EventType::ToolOutput, 1);
    assert_event_type_count(&tabnine_events, EventType::Notice, 1);
    assert_event_with_role(&tabnine_events, EventType::ToolCall, EventRole::Assistant);
    assert_event_with_role(&tabnine_events, EventType::ToolOutput, EventRole::Assistant);
    assert_events_have_provider_citations(&tabnine_events);
    let tabnine_rendered = serde_json::to_string(&tabnine_events).unwrap();
    assert!(tabnine_rendered.contains("tabnine jsonl oracle prompt"));
    assert!(tabnine_rendered.contains("tabnine jsonl oracle answer"));
    assert!(tabnine_rendered.contains("src/tabnine_oracle.txt"));
    assert_search_hits_provider(
        &store,
        "tabnine jsonl oracle prompt",
        CaptureProvider::Tabnine,
    );
    assert_search_misses(&store, "TABNINE_RAW_TOOL_RESULT_SHOULD_NOT_SEARCH");

    let tabnine_child =
        stored_provider_session_id(&store, CaptureProvider::Tabnine, "tabnine-child");
    let tabnine_parent =
        stored_provider_session_id(&store, CaptureProvider::Tabnine, "tabnine-root");
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
    assert_eq!(droid_summary.imported_events, 6);
    assert_eq!(droid_summary.imported_edges, 1);
    let droid_parent =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-root");
    let droid_events = store.events_for_session(droid_parent).unwrap();
    assert_eq!(droid_events.len(), 4);
    assert_event_type_count(&droid_events, EventType::Message, 1);
    assert_event_type_count(&droid_events, EventType::ToolCall, 1);
    assert_event_type_count(&droid_events, EventType::ToolOutput, 1);
    assert_event_type_count(&droid_events, EventType::Notice, 1);
    assert_event_with_role(&droid_events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&droid_events);
    assert_search_hits_provider(
        &store,
        "droid jsonl oracle prompt",
        CaptureProvider::FactoryAiDroid,
    );
    assert_search_misses(&store, "DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    let droid_child =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-child");
    assert_eq!(
        store.get_session(droid_child).unwrap().parent_session_id,
        Some(droid_parent)
    );
    let droid_second = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_second.failed, 0, "{:?}", droid_second.failures);
    assert_eq!(droid_second.imported_sessions, 0);
    assert_eq!(droid_second.imported_events, 0);
    assert_eq!(droid_second.imported_edges, 0);
    assert_eq!(droid_second.skipped_sessions, 2);
    assert_eq!(droid_second.skipped_events, 6);
    assert_eq!(droid_second.skipped_edges, 1);

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
    assert_eq!(copilot_summary.imported_sessions, 2);
    assert_eq!(copilot_summary.imported_events, 7);
    let copilot_events = store
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::CopilotCli,
            "copilot-root",
        ))
        .unwrap();
    assert_eq!(copilot_events.len(), 5);
    assert_event_type_count(&copilot_events, EventType::Message, 2);
    assert_event_type_count(&copilot_events, EventType::ToolCall, 1);
    assert_event_type_count(&copilot_events, EventType::ToolOutput, 1);
    assert_event_type_count(&copilot_events, EventType::Notice, 1);
    assert_event_with_role(&copilot_events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&copilot_events);
    assert_search_hits_provider(&store, "running", CaptureProvider::CopilotCli);
    assert_search_misses(&store, "COPILOT_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    stored_provider_session_id(&store, CaptureProvider::CopilotCli, "copilot-child");
    assert_search_hits_provider(
        &store,
        "copilot child oracle prompt",
        CaptureProvider::CopilotCli,
    );

    let copilot_second = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            allow_partial_failures: true,
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_second.failed, 0, "{:?}", copilot_second.failures);
    assert_eq!(copilot_second.imported_sessions, 0);
    assert_eq!(copilot_second.imported_events, 0);
    assert_eq!(copilot_second.skipped_sessions, 2);
    assert_eq!(copilot_second.skipped_events, 7);
}

#[test]
fn native_jsonl_tree_skips_oversized_record_and_continues_session() {
    let temp = tempdir();
    let chats = temp.path().join("gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    let path = chats.join("oversized-gemini.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "sessionId": "gemini-oversized",
            "startTime": "2026-07-04T15:00:00Z",
            "directories": ["/workspace"]
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "id": "gemini-after-oversized",
            "timestamp": "2026-07-04T15:00:01Z",
            "type": "user",
            "content": "after oversized gemini"
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_gemini_cli_history(
        temp.path().join("gemini/.gemini"),
        &mut store,
        GeminiCliImportOptions {
            source_path: Some(temp.path().join("gemini/.gemini")),
            imported_at: "2026-07-04T15:30:00Z".parse().unwrap(),
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Gemini, "gemini-oversized");
    let rendered = serde_json::to_string(&store.events_for_session(session_id).unwrap()).unwrap();
    assert!(rendered.contains("after oversized gemini"));
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
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::QwenCode,
            "qwen-smoke",
        ))
        .unwrap();
    assert_eq!(qwen_events.len(), 3);
    assert_event_type_count(&qwen_events, EventType::Message, 1);
    assert_event_type_count(&qwen_events, EventType::ToolCall, 1);
    assert_event_type_count(&qwen_events, EventType::ToolOutput, 1);
    assert_event_with_role(&qwen_events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&qwen_events);
    let qwen_rendered = serde_json::to_string(&qwen_events).unwrap();
    assert!(qwen_rendered.contains("qwen jsonl oracle prompt"));
    assert!(qwen_rendered.contains("src/qwen_oracle.txt"));
    assert_search_hits_provider(
        &store,
        "qwen jsonl oracle prompt",
        CaptureProvider::QwenCode,
    );
    assert_search_misses(&store, "QWEN_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");

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
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::KimiCodeCli,
            "kimi-smoke",
        ))
        .unwrap();
    assert_eq!(kimi_events.len(), 5);
    assert_event_type_count(&kimi_events, EventType::Message, 2);
    assert_event_type_count(&kimi_events, EventType::ToolCall, 1);
    assert_event_type_count(&kimi_events, EventType::ToolOutput, 1);
    assert_event_type_count(&kimi_events, EventType::Notice, 1);
    assert_event_with_role(&kimi_events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&kimi_events);
    let kimi_rendered = serde_json::to_string(&kimi_events).unwrap();
    assert!(kimi_rendered.contains("kimi jsonl oracle prompt"));
    assert!(kimi_rendered.contains("src/kimi_oracle.txt"));
    assert!(!kimi_rendered.contains("usage record"));
    assert_search_hits_provider(
        &store,
        "kimi jsonl oracle prompt",
        CaptureProvider::KimiCodeCli,
    );
    assert_search_misses(&store, "usage record");
    assert_search_misses(&store, "KIMI_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");

    let kimi_child = stored_provider_session_id(
        &store,
        CaptureProvider::KimiCodeCli,
        "kimi-smoke/agents/agent-1",
    );
    let kimi_parent =
        stored_provider_session_id(&store, CaptureProvider::KimiCodeCli, "kimi-smoke");
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
fn native_kimi_skips_oversized_index_and_wire_records() {
    let temp = tempdir();
    let kimi = write_kimi_smoke_fixture(&temp);
    let index_path = kimi.join("session_index.jsonl");
    let original_index = fs::read(&index_path).unwrap();
    let mut index_bytes = oversized_jsonl_line();
    index_bytes.extend_from_slice(&original_index);
    fs::write(&index_path, index_bytes).unwrap();

    let wire_path = kimi.join("sessions/wd_demo_abc123/kimi-smoke/agents/main/wire.jsonl");
    let original_wire = fs::read(&wire_path).unwrap();
    let first_line_end = original_wire
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap();
    let mut wire_bytes = Vec::new();
    wire_bytes.extend_from_slice(&original_wire[..first_line_end]);
    wire_bytes.extend_from_slice(&oversized_jsonl_line());
    wire_bytes.extend_from_slice(&original_wire[first_line_end..]);
    fs::write(&wire_path, wire_bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            allow_partial_failures: true,
            imported_at: "2026-07-04T15:30:00Z".parse().unwrap(),
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 7);
    let session_id = stored_provider_session_id(&store, CaptureProvider::KimiCodeCli, "kimi-smoke");
    let source = store
        .capture_source_by_external_session(CaptureProvider::KimiCodeCli, "kimi-smoke")
        .unwrap()
        .unwrap();
    assert_eq!(source.descriptor.cwd.as_deref(), Some("/workspace/kimi"));
    assert_eq!(
        store.events_for_session(session_id).unwrap().len(),
        5,
        "main wire events should resume after the oversized record"
    );
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
fn native_jsonl_tree_rejects_empty_native_files() {
    let temp = tempdir();
    let root = temp.path().join("gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("empty.jsonl"), "").unwrap();
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
    assert!(summary.failures[0].error.contains("transcripts found"));
    assert!(store.list_sessions().unwrap().is_empty());
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
    assert_eq!(gemini_summary.failed, 3, "{:?}", gemini_summary.failures);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_events, 6);
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
    assert_eq!(droid_summary.failed, 3, "{:?}", droid_summary.failures);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_events, 6);
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
    assert_eq!(copilot_summary.failed, 3, "{:?}", copilot_summary.failures);
    assert_eq!(copilot_summary.imported_sessions, 2);
    assert_eq!(copilot_summary.imported_events, 7);
    assert_provider_failures_include_headerless_and_malformed(&copilot_summary);
}
