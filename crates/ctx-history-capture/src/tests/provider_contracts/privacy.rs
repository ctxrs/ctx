use crate::tests::support::assertions::{
    assert_event_type_count, assert_events_have_provider_citations, assert_search_hits_provider,
    assert_search_misses,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_crush_sqlite, import_forgecode_sqlite, import_goose_sessions_sqlite,
    import_hermes_sqlite, CrushSqliteImportOptions, ForgeCodeSqliteImportOptions,
    GooseSessionsSqliteImportOptions, HermesSqliteImportOptions, ProviderImportSummary,
    FORGECODE_SQLITE_SOURCE_FORMAT, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn native_successful_outputs_are_absent_for_sqlite_provider_shapes() {
    let temp = tempdir();

    let crush = write_crush_tool_output_db(&temp);
    assert_imports_without_success_output(
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
                    ..CrushSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let hermes = write_hermes_tool_output_db(&temp);
    assert_imports_without_success_output(
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
                    ..HermesSqliteImportOptions::default()
                },
            )
            .unwrap()
        },
    );
}

fn assert_imports_without_success_output(
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
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    if let Some(raw_command_output) = raw_command_output {
        assert_event_type_count(&events, EventType::CommandOutput, 0);
        assert_search_misses(&store, raw_command_output);
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains(raw_command_output),
            "{label}: raw command output leaked into stored event payload"
        );
    }
    assert_events_have_provider_citations(&store, &events);
    assert_search_hits_provider(&store, searchable, provider);
    assert_search_misses(&store, raw_output);
    assert!(
        !serde_json::to_string(&events).unwrap().contains(raw_output),
        "{label}: raw tool output leaked into stored event payload"
    );
    assert!(
        store.runs_for_session(session_id).unwrap().is_empty(),
        "{label}: successful outputs must not create Core runs"
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

#[test]
fn native_sqlite_successful_tool_outputs_are_absent_and_not_searchable() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let goose = provider_history_fixture("goose/v14/sessions.db");
    Connection::open(&goose)
        .unwrap()
        .execute(
            "UPDATE messages SET content_json = ?1 WHERE id = 3",
            [json!([{
                "type": "toolResponse",
                "toolResult": {
                    "content": [{"type": "text", "text": "goose-output-sentinel"}]
                }
            }])
            .to_string()],
        )
        .unwrap();
    let goose_summary = import_goose_sessions_sqlite(
        &goose,
        &mut store,
        GooseSessionsSqliteImportOptions {
            source_path: Some(goose.clone()),
            ..GooseSessionsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(goose_summary.failed, 0, "{:?}", goose_summary.failures);
    assert_successful_output_absent(
        &store,
        CaptureProvider::Goose,
        "goose-root",
        "goose-output-sentinel",
    );
    assert_search_hit_cites_source(
        &store,
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        "goose sqlite search oracle request",
    );

    let forgecode = provider_history_fixture("forgecode/v1/forge.db");
    let forge_connection = Connection::open(&forgecode).unwrap();
    let context: String = forge_connection
        .query_row(
            "SELECT context FROM conversations WHERE conversation_id = 'forge-root'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut context: Value = serde_json::from_str(&context).unwrap();
    context["messages"][2]["message"]["tool"]["output"]["values"][0]["text"] =
        Value::String("forge-output-sentinel".into());
    forge_connection
        .execute(
            "UPDATE conversations SET context = ?1 WHERE conversation_id = 'forge-root'",
            [serde_json::to_string(&context).unwrap()],
        )
        .unwrap();
    drop(forge_connection);
    let forge_summary = import_forgecode_sqlite(
        &forgecode,
        &mut store,
        ForgeCodeSqliteImportOptions {
            source_path: Some(forgecode.clone()),
            ..ForgeCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(forge_summary.failed, 0, "{:?}", forge_summary.failures);
    assert_successful_output_absent(
        &store,
        CaptureProvider::ForgeCode,
        "forge-root",
        "forge-output-sentinel",
    );
    assert_search_hit_cites_source(
        &store,
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        "forgecode oracle prompt",
    );
}

fn assert_successful_output_absent(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    forbidden_output: &str,
) {
    let session_id = stored_provider_session_id(store, provider, provider_session_id);
    let events = store.events_for_session(session_id).unwrap();
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(
        !rendered.contains(forbidden_output),
        "{provider:?} leaked raw output into sanitized event payload"
    );
    assert!(
        store
            .search_event_hits(forbidden_output, 10)
            .unwrap()
            .is_empty(),
        "{provider:?} raw output should not be indexed"
    );
    assert!(
        store.runs_for_session(session_id).unwrap().is_empty(),
        "{provider:?} successful output must not create a Core run"
    );
}

fn assert_search_hit_cites_source(
    store: &Store,
    provider: CaptureProvider,
    source_format: &str,
    query: &str,
) {
    let hits = store.search_event_hits(query, 10).unwrap();
    let hit = hits
        .iter()
        .find(|hit| hit.provider == Some(provider))
        .unwrap_or_else(|| panic!("missing {provider:?} search hit for {query:?}"));
    assert_eq!(hit.source_format.as_deref(), Some(source_format));
    assert!(hit.raw_source_path.is_some());
    let event = store.get_event(hit.event_id).unwrap();
    let source = store
        .get_capture_source(
            event
                .capture_source_id
                .expect("search hit has capture source"),
        )
        .unwrap();
    assert_eq!(source.descriptor.provider, provider);
    assert_eq!(
        source.descriptor.source_format.as_deref(),
        Some(source_format)
    );
    assert!(source.descriptor.source_identity.is_some());
}
