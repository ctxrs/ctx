#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_shelley_imports_sessions_messages_metadata_and_citations() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 3);
    assert_eq!(summary.imported_events, 4);
    assert_eq!(summary.imported_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Shelley, "shelley-root");
    let child_id = provider_session_uuid(CaptureProvider::Shelley, "shelley-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    assert!(store
        .get_session(parent_id)
        .unwrap()
        .sync
        .metadata
        .to_string()
        .contains("queued oracle"));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        fixture.to_str()
    );
    assert_eq!(source.descriptor.provider, CaptureProvider::Shelley);

    let events = store.events_for_session(parent_id).unwrap();
    assert_eq!(events.len(), 3);
    let agent_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-agent"))
        .expect("Shelley agent event imported");
    let tool_result_event = events
        .iter()
        .find(|event| {
            event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-tool-result")
        })
        .expect("Shelley tool-result event imported");
    assert_eq!(agent_event.event_type, EventType::ToolCall);
    assert_eq!(tool_result_event.event_type, EventType::ToolOutput);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("shelley search oracle"));
    assert!(rendered.contains("thinking through the search"));
    assert!(rendered.contains("tool call: bash"));
    assert!(rendered.contains("tool output oracle"));
    assert!(rendered.contains("claude-opus-4-7"));
    assert!(rendered.contains("https://api.anthropic.com/v1/messages"));
    let user_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .expect("Shelley user event imported");
    assert!(user_event
        .sync
        .metadata
        .to_string()
        .contains("conversation:shelley-root:sequence:1:message:msg-user"));

    let cursor = store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_cursor_stream(CaptureProvider::Shelley, SHELLEY_SQLITE_SOURCE_FORMAT),
        )
        .unwrap()
        .unwrap();
    assert!(cursor
        .cursor
        .contains("conversation:shelley-root:sequence:3:message:msg-tool-result"));
}

#[test]
pub(crate) fn native_shelley_reimport_is_idempotent() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.imported_events, 4);

    let second = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 3);
    assert_eq!(second.skipped_events, 4);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
pub(crate) fn native_shelley_handles_duplicate_sequences_and_nonchat_rows() {
    let temp = tempdir();
    let fixture = write_shelley_adversarial_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);

    let session_id = provider_session_uuid(CaptureProvider::Shelley, "shelley-adversarial");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("duplicate sequence first"));
    assert!(rendered.contains("duplicate sequence second"));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::VcsChange));
    assert!(events
        .iter()
        .any(|event| event.sync.metadata["metadata"]["message_type"].as_str() == Some("warning")));

    let large = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-large"))
        .expect("large Shelley event imported");
    assert_eq!(large.payload["body"]["truncated"].as_bool(), Some(true));
    assert!(
        large.payload["body"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= PROVIDER_MAX_TEXT_CHARS
    );
}

#[test]
pub(crate) fn native_shelley_text_extraction_is_not_duplicate_or_unbounded() {
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": "once"}
        ]
    }))
    .unwrap();
    assert_eq!(text, "once");

    let huge = "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200);
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": huge},
            {"Type": 2, "Text": "after cap"}
        ]
    }))
    .unwrap();
    assert_eq!(text.chars().count(), PROVIDER_MAX_TEXT_CHARS + 1);
    assert!(!text.contains("after cap"));
}

#[test]
pub(crate) fn native_shelley_event_index_uses_stable_message_identity() {
    let message = ShelleyMessageRow {
        rowid: 1,
        message_id: "msg-stable".to_owned(),
        conversation_id: "conv-stable".to_owned(),
        sequence_id: 42,
        entry_type: "user".to_owned(),
        llm_data: None,
        user_data: None,
        usage_data: None,
        created_at: None,
        display_data: None,
        excluded_from_context: false,
        generation: None,
        llm_api_url: None,
        model_name: None,
        forked_from_message_id: None,
    };
    let mut moved_row = message.clone();
    moved_row.rowid = 999;
    let mut duplicate_sequence = message.clone();
    duplicate_sequence.message_id = "msg-stable-other".to_owned();

    assert_eq!(
        shelley_event_index(&message),
        shelley_event_index(&moved_row)
    );
    assert_ne!(
        shelley_event_index(&message),
        shelley_event_index(&duplicate_sequence)
    );
}

#[test]
pub(crate) fn native_shelley_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_shelley_malformed_db(&temp);
    let corrupt = temp.path().join("corrupt-shelley.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_shelley_sqlite(
        &malformed,
        &mut store,
        ShelleySqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("Shelley messages table missing required column(s): type"));

    let err = import_shelley_sqlite(&corrupt, &mut store, ShelleySqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
pub(crate) fn provider_sources_discovers_shelley_default_db() {
    let temp = tempdir();
    let db = temp.path().join(".config/shelley/shelley.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Shelley);
    let source = sources
        .iter()
        .find(|source| source.source_format == SHELLEY_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Shelley);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}

pub(crate) fn write_shelley_smoke_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            conversation_id text primary key,
            slug text,
            user_initiated boolean not null default true,
            created_at datetime not null default current_timestamp,
            updated_at datetime not null default current_timestamp,
            cwd text,
            archived boolean not null default false,
            parent_conversation_id text,
            model text,
            conversation_options text not null default '{}',
            current_generation integer not null default 1,
            agent_working boolean not null default false,
            tags text not null default '[]',
            is_draft boolean not null default false,
            draft text not null default '',
            queued_messages text not null default '[]'
        );
        create table messages (
            message_id text primary key,
            conversation_id text not null,
            sequence_id integer not null,
            type text not null,
            llm_data text,
            user_data text,
            usage_data text,
            created_at datetime not null default current_timestamp,
            display_data text,
            excluded_from_context boolean not null default false,
            generation integer not null default 1,
            llm_api_url text,
            model_name text,
            forked_from_message_id text
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            'shelley-root', 'root-slug', 1, '2026-06-24 12:00:00',
            '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
            'claude-opus-4-7', ?1, 2, 0, ?2, 0, '', ?3
        )",
        [
            r#"{"thinking_level":"high","subagent_backend":"shelley"}"#,
            r#"["native","ctx"]"#,
            r#"[{"id":"queued-1","llm":{"Content":[{"Type":2,"Text":"queued oracle"}]},"created_at":"2026-06-24T12:00:04Z","model":"claude-opus-4-7"}]"#,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            'shelley-child', 'child-slug', 0, '2026-06-24 12:01:00',
            '2026-06-24 12:02:00', '/workspace/shelley', 0, 'shelley-root',
            'claude-sonnet-4-5', '{}', 1, 0, '[]', 0, '', '[]'
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            'shelley-draft', 'old-draft', 1, '2026-06-24 11:00:00',
            '2026-06-24 11:01:00', '/workspace/archive', 1, null,
            null, '{}', 1, 0, '[]', 1, 'draft body', '[]'
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
        ) values ('msg-user', 'shelley-root', 1, 'user', ?1, '2026-06-24 12:00:01')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "please run shelley search oracle"}
            ]
        })
        .to_string()],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, llm_data, usage_data,
            created_at, generation, llm_api_url, model_name
        ) values (
            'msg-agent', 'shelley-root', 2, 'agent', ?1, ?2,
            '2026-06-24 12:00:02', 2, 'https://api.anthropic.com/v1/messages',
            'claude-opus-4-7'
        )",
        [
            json!({
                "Role": 1,
                "Content": [
                    {"Type": 3, "Thinking": "thinking through the search"},
                    {"Type": 2, "Text": "I will inspect the source."},
                    {"Type": 5, "ID": "toolu_1", "ToolName": "bash", "ToolInput": {"command": "rg shelley"}}
                ],
                "EndOfTurn": false
            })
            .to_string(),
            json!({
                "input_tokens": 100,
                "cache_read_input_tokens": 25,
                "output_tokens": 40,
                "cost_usd": 0.0123,
                "model": "claude-opus-4-7",
                "url": "https://api.anthropic.com/v1/messages"
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, display_data,
            created_at, forked_from_message_id
        ) values (
            'msg-tool-result', 'shelley-root', 3, 'user', ?1, ?2,
            '2026-06-24 12:00:03', 'source-msg-tool-result'
        )",
        [
            json!({
                "Role": 0,
                "Content": [
                    {"Type": 6, "ToolUseID": "toolu_1", "ToolResult": [{"Type": 2, "Text": "tool output oracle"}]}
                ]
            })
            .to_string(),
            json!({"stdout": "tool output oracle", "exit_code": 0}).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, llm_data, created_at
        ) values ('msg-child', 'shelley-child', 1, 'agent', ?1, '2026-06-24 12:01:01')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "subagent result from Shelley"}
            ]
        })
        .to_string()],
    )
    .unwrap();
    path
}

pub(crate) fn write_shelley_adversarial_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley-adversarial.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            conversation_id text primary key,
            slug text,
            user_initiated boolean not null default true,
            created_at datetime not null default current_timestamp,
            updated_at datetime not null default current_timestamp,
            cwd text,
            archived boolean not null default false,
            parent_conversation_id text,
            model text,
            conversation_options text not null default '{}',
            current_generation integer not null default 1,
            agent_working boolean not null default false,
            tags text not null default '[]',
            is_draft boolean not null default false,
            draft text not null default '',
            queued_messages text not null default '[]'
        );
        create table messages (
            message_id text primary key,
            conversation_id text not null,
            sequence_id integer not null,
            type text not null,
            llm_data text,
            user_data text,
            usage_data text,
            created_at datetime not null default current_timestamp,
            display_data text,
            excluded_from_context boolean not null default false,
            generation integer not null default 1,
            llm_api_url text,
            model_name text,
            forked_from_message_id text
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            'shelley-adversarial', 'adversarial', 1, '2026-06-24 12:00:00',
            '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
            'claude-opus-4-7', '{}', 1, 0, '[]', 0, '', '[]'
        )",
        [],
    )
    .unwrap();
    for (message_id, sequence_id, message_type, text) in [
        ("msg-dup-a", 1, "user", "duplicate sequence first"),
        ("msg-dup-b", 1, "user", "duplicate sequence second"),
        ("msg-git", 2, "gitinfo", "commit abc touched shelley.rs"),
        ("msg-warning", 3, "warning", "warning message for Shelley"),
    ] {
        conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, created_at
            ) values (?1, 'shelley-adversarial', ?2, ?3, ?4, '2026-06-24 12:00:01')",
            rusqlite::params![
                message_id,
                sequence_id,
                message_type,
                json!({"Content": [{"Type": 2, "Text": text}]}).to_string(),
            ],
        )
        .unwrap();
    }
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, llm_data, created_at
        ) values ('msg-large', 'shelley-adversarial', 4, 'agent', ?1, '2026-06-24 12:00:04')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200)}
            ]
        })
        .to_string()],
    )
    .unwrap();
    path
}

pub(crate) fn write_shelley_malformed_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley-malformed.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (conversation_id text primary key);
         create table messages (
            message_id text primary key,
            conversation_id text not null
         );",
    )
    .unwrap();
    path
}
