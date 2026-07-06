#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn provider_fixture_replay_supports_opencode_fixture() {
    let temp = tempdir();
    let fixture = provider_fixture("opencode.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-session-1");
    let child_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-session-1-scout");
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(parent.provider, CaptureProvider::OpenCode);
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
    assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
}

#[test]
pub(crate) fn native_opencode_imports_read_only_sqlite() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-root");
    let child_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
}

#[cfg(unix)]
#[test]
pub(crate) fn native_opencode_normalizer_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let link = temp.path().join("linked-opencode.db");
    symlink(&fixture, &link).unwrap();

    let err = normalize_opencode_sqlite(
        &link,
        &ProviderAdapterContext::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-opencode.db")
                && reason == "symlinked provider transcript files are rejected"
    ));
}

#[test]
pub(crate) fn native_opencode_synthesizes_session_message_seq_when_missing() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_without_seq_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-no-seq");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].payload["body"]["session_message_seq"].as_i64(),
        Some(1)
    );
    assert_eq!(
        events[1].payload["body"]["session_message_seq"].as_i64(),
        Some(2)
    );
    assert_ne!(events[0].id, events[1].id);
}

#[test]
pub(crate) fn native_opencode_rejects_negative_session_message_seq() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update session_message set seq = -1 where id = 'msg-user'",
        [],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_events, 2);
    let session_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-root");
    let events = store.events_for_session(session_id).unwrap();
    assert!(events.iter().all(|event| {
        event.payload["body"]["session_message_seq"]
            .as_i64()
            .is_some_and(|seq| seq >= 0)
    }));
}

#[test]
pub(crate) fn native_opencode_rejects_out_of_range_message_timestamp() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let data_without_payload_time = json!({"text": "bad timestamp fallback"}).to_string();
    conn.execute(
        "update session_message set time_created = ?1, data = ?2 where id = 'msg-user'",
        rusqlite::params![i64::MAX, data_without_payload_time],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message time_created"));
    assert_eq!(summary.imported_events, 2);
}

#[test]
pub(crate) fn native_opencode_rejects_oversized_sqlite_text_value() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let oversized_data = format!(
        "{{\"time\":{{\"created\":1782259200000}},\"text\":\"{}\"}}",
        "x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1)
    );
    conn.execute(
        "update session_message set data = ?1 where id = 'msg-user'",
        [&oversized_data],
    )
    .unwrap();
    drop(conn);

    let err = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("too big"),
        "unexpected error: {err}"
    );
}

#[test]
pub(crate) fn native_opencode_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_opencode_smoke_db(&temp, true);
    let corrupt = temp.path().join("corrupt-opencode.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &malformed,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0].error.contains("invalid JSON"));

    let err = import_opencode_sqlite(&corrupt, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
pub(crate) fn native_opencode_accepts_schema_without_model_column() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
}

#[test]
pub(crate) fn native_opencode_imports_legacy_message_table_when_session_message_is_absent() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);

    let session_id = provider_session_uuid(CaptureProvider::OpenCode, "current-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
}

#[test]
pub(crate) fn native_opencode_rejects_changed_message_schema_before_querying() {
    let temp = tempdir();
    let fixture = write_opencode_future_incomplete_schema_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("OpenCode SQLite message table missing required column(s): data"));
}

pub(crate) fn write_opencode_smoke_db(temp: &TempDir, malformed: bool) -> PathBuf {
    let path = temp.path().join(if malformed {
        "opencode-malformed.db"
    } else {
        "opencode.db"
    });
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key, parent_id text, title text not null, directory text not null,
            model text, agent text, time_created integer not null, time_updated integer not null,
            tokens_input integer not null, tokens_output integer not null,
            tokens_reasoning integer not null, tokens_cache_read integer not null,
            tokens_cache_write integer not null
        );
        create table session_message (
            id text primary key, session_id text not null, type text not null, seq integer not null,
            time_created integer not null, time_updated integer not null, data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session values (?1, null, 'root', '/workspace', '{\"id\":\"test\"}', 'build', 1782259200000, 1782259200000, 1, 1, 0, 0, 0)",
        ["opencode-root"],
    )
    .unwrap();
    conn.execute(
        "insert into session values (?1, ?2, 'child', '/workspace', '{\"id\":\"test\"}', 'scout', 1782259201000, 1782259201000, 1, 1, 0, 0, 0)",
        ["opencode-child", "opencode-root"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1, 1782259200000, 1782259200000, ?3)",
        [
            "msg-user",
            "opencode-root",
            "{\"time\":{\"created\":1782259200000},\"text\":\"inspect\"}",
        ],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'assistant', 2, 1782259201000, 1782259201000, ?3)",
        ["msg-assistant", "opencode-root", "{\"time\":{\"created\":1782259201000},\"content\":[{\"type\":\"tool\",\"name\":\"bash\"}]}"],
    )
    .unwrap();
    let child_data = if malformed {
        "{\"time\":{\"created\":1782259202000},\"text\":"
    } else {
        "{\"time\":{\"created\":1782259202000},\"text\":\"child done\"}"
    };
    conn.execute(
        "insert into session_message values (?1, ?2, 'assistant', 1, 1782259202000, 1782259202000, ?3)",
        ["msg-child", "opencode-child", child_data],
    )
    .unwrap();
    path
}

pub(crate) fn write_opencode_session_message_without_seq_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("opencode-no-seq.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key, title text not null, directory text not null,
            time_created integer not null, time_updated integer not null
        );
        create table session_message (
            id text primary key, session_id text not null, type text not null,
            time_created integer not null, time_updated integer not null, data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session values (?1, 'no seq', '/workspace', 1782259200000, 1782259200000)",
        ["opencode-no-seq"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
        [
            "msg-no-seq-user",
            "opencode-no-seq",
            "{\"time\":{\"created\":1782259200000},\"text\":\"first no seq\"}",
        ],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'assistant', 1782259201000, 1782259201000, ?3)",
        [
            "msg-no-seq-assistant",
            "opencode-no-seq",
            "{\"time\":{\"created\":1782259201000},\"text\":\"second no seq\"}",
        ],
    )
    .unwrap();
    path
}

pub(crate) fn write_opencode_current_schema_db(temp: &TempDir, with_message: bool) -> PathBuf {
    let path = temp.path().join(if with_message {
        "opencode-current-message.db"
    } else {
        "opencode-current-empty.db"
    });
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key,
            project_id text not null,
            parent_id text,
            slug text not null,
            directory text not null,
            title text not null,
            version text not null,
            share_url text,
            summary_additions integer,
            summary_deletions integer,
            summary_files integer,
            summary_diffs text,
            revert text,
            permission text,
            time_created integer not null,
            time_updated integer not null,
            time_compacting integer,
            time_archived integer,
            workspace_id text
        );
        create table session_entry (
            id text primary key,
            session_id text not null,
            type text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table message (
            id text primary key,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table part (
            id text primary key,
            message_id text not null,
            session_id text not null,
            type text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );",
    )
    .unwrap();

    if with_message {
        conn.execute(
            "insert into session (
                id, project_id, parent_id, slug, directory, title, version, permission,
                time_created, time_updated
            ) values (?1, 'project-1', null, 'current-root', '/workspace', 'current root',
                '0.8.0', 'default', 1782259200000, 1782259200000)",
            ["current-root"],
        )
        .unwrap();
        conn.execute(
            "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
            [
                "current-message-1",
                "current-root",
                "{\"role\":\"user\",\"time\":{\"created\":1782259200000},\"text\":\"legacy hello\"}",
            ],
        )
        .unwrap();
    }

    path
}

pub(crate) fn write_opencode_future_incomplete_schema_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("opencode-future-incomplete.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key,
            project_id text not null,
            slug text not null,
            directory text not null,
            title text not null,
            version text not null,
            time_created integer not null,
            time_updated integer not null
        );
        create table message (
            id text primary key,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
            id, project_id, slug, directory, title, version, time_created, time_updated
        ) values ('future-root', 'project-1', 'future-root', '/workspace', 'future root',
            '0.9.0', 1782259200000, 1782259200000)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into message values ('future-message-1', 'future-root', 1782259200000,
            1782259200000)",
        [],
    )
    .unwrap();
    path
}
