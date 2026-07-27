use crate::tests::native_sqlite::opencode_fixtures::{
    write_opencode_all_metadata_db, write_opencode_current_schema_db,
    write_opencode_future_incomplete_schema_db,
    write_opencode_session_entry_metadata_with_legacy_message_db,
    write_opencode_session_message_malformed_with_legacy_message_db,
    write_opencode_session_message_metadata_bad_seq_with_legacy_message_db,
    write_opencode_session_message_metadata_with_legacy_message_db,
    write_opencode_session_message_without_seq_db, write_opencode_tool_only_db,
};
use crate::tests::support::fixtures::sqlite::{
    write_opencode_message_part_db, write_opencode_smoke_db,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
#[cfg(unix)]
use crate::CaptureError;
use crate::{
    import_kilo_sqlite, import_mimocode_sqlite, import_opencode_sqlite, KiloSqliteImportOptions,
    MiMoCodeSqliteImportOptions, OpenCodeSqliteImportOptions, KILO_SQLITE_SOURCE_FORMAT,
    MAX_PROVIDER_SQLITE_VALUE_BYTES, MIMOCODE_SQLITE_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use std::fs;

#[test]

fn native_opencode_imports_read_only_sqlite() {
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
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-child");
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
#[test]
fn native_kilo_imports_opencode_derived_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("kilo/kilo.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Kilo, "kilo-root");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Kilo);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(KILO_SQLITE_SOURCE_FORMAT)
    );
    let first_seq = events[0].payload["body"]["session_message_seq"]
        .as_i64()
        .expect("Kilo synthesized sequence");
    let second_seq = events[1].payload["body"]["session_message_seq"]
        .as_i64()
        .expect("Kilo synthesized sequence");
    assert!(first_seq > 0);
    assert!(second_seq > 0);
    assert_ne!(first_seq, second_seq);

    let second = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            source_path: Some(fixture.clone()),
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}
#[cfg(unix)]
#[test]
fn native_opencode_import_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let link = temp.path().join("linked-opencode.db");
    symlink(&fixture, &link).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_opencode_sqlite(&link, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-opencode.db")
                && reason
                    == "OpenCode-family SQLite source component must be a regular non-symlink file"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_synthesizes_stable_session_message_identity_when_seq_is_missing() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_without_seq_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-no-seq");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let first_seq = events[0].payload["body"]["session_message_seq"]
        .as_i64()
        .expect("OpenCode synthesized sequence");
    let second_seq = events[1].payload["body"]["session_message_seq"]
        .as_i64()
        .expect("OpenCode synthesized sequence");
    assert!(first_seq > 0);
    assert!(second_seq > 0);
    assert_ne!(first_seq, second_seq);
    assert_ne!(events[0].id, events[1].id);
}

#[test]
fn native_opencode_rejects_negative_session_message_seq() {
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
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_events, 2);
    let events = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), summary.imported_events);
    assert!(events.iter().all(|event| {
        event.payload["body"]["session_message_seq"]
            .as_i64()
            .is_some_and(|seq| seq >= 0)
    }));
}

#[test]
fn native_opencode_rejects_out_of_range_message_timestamp() {
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

fn oversized_opencode_text_payload() -> String {
    format!(
        "{{\"time\":{{\"created\":1782259200000}},\"text\":\"{}\"}}",
        "x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1)
    )
}

#[test]
fn native_opencode_rejects_oversized_sqlite_text_value_and_imports_other_rows() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let oversized_data = oversized_opencode_text_payload();
    conn.execute(
        "update session_message set data = ?1 where id = 'msg-user'",
        [&oversized_data],
    )
    .unwrap();
    let other_conversational: i64 = conn
        .query_row(
            "select count(*) from session_message where id != 'msg-user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        other_conversational > 0,
        "test fixture must contain at least one non-oversized conversational row"
    );
    drop(conn);

    let summary = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .expect("oversized rows should be rejected without aborting the whole import");

    assert_eq!(summary.failed, 1, "unexpected summary: {summary:?}");
    assert!(summary.failures[0].error.contains("exceeds the"));
    assert_eq!(summary.skipped, 0, "unexpected summary: {summary:?}");
    assert_eq!(summary.skipped_events, 0, "unexpected summary: {summary:?}");
    assert_eq!(
        summary.skipped_sessions, 0,
        "unexpected summary: {summary:?}"
    );
    assert_eq!(
        summary.imported_events, 2,
        "unexpected summary: {summary:?}"
    );
}

#[test]
fn native_opencode_reports_all_oversized_sqlite_text_values_without_scaffolding() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute("delete from session_message where id != 'msg-user'", [])
        .unwrap();
    let oversized_data = oversized_opencode_text_payload();
    conn.execute(
        "update session_message set data = ?1 where id = 'msg-user'",
        [&oversized_data],
    )
    .unwrap();
    drop(conn);

    let summary = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .expect("oversized rows should be rejected without aborting the import");

    assert_eq!(summary.failed, 1, "unexpected summary: {summary:?}");
    assert!(summary.failures[0].error.contains("exceeds the"));
    assert_eq!(summary.skipped, 0, "unexpected summary: {summary:?}");
    assert_eq!(summary.skipped_events, 0, "unexpected summary: {summary:?}");
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
}

#[test]
fn native_opencode_rejects_oversized_legacy_message_value_without_scaffolding() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, true);
    let conn = Connection::open(&fixture).unwrap();
    let oversized_data = oversized_opencode_text_payload();
    conn.execute("update message set data = ?1", [&oversized_data])
        .unwrap();
    drop(conn);

    let summary = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .expect("oversized legacy message rows should be rejected without aborting the import");

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert!(summary.failures[0].error.contains("exceeds the"));
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert_eq!(summary.skipped_events, 0, "{summary:?}");
    assert_eq!(summary.imported_events, 0, "{summary:?}");
}

#[test]
fn native_opencode_imports_message_part_text_and_metadata() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "opencode-message-part.db",
        "opencode-part-root",
        "opencode message part oracle",
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_message_part_import(
        &store,
        CaptureProvider::OpenCode,
        OPENCODE_SQLITE_SOURCE_FORMAT,
        "opencode-part-root",
        "opencode message part oracle",
    );
}

#[test]
fn native_kilo_imports_message_part_text_and_metadata() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "kilo-message-part.db",
        "kilo-part-root",
        "kilo message part oracle",
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_message_part_import(
        &store,
        CaptureProvider::Kilo,
        KILO_SQLITE_SOURCE_FORMAT,
        "kilo-part-root",
        "kilo message part oracle",
    );
}

#[test]
fn native_mimocode_imports_message_part_text_and_metadata_idempotently() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "mimocode-message-part.db",
        "mimocode-part-root",
        "mimocode message part oracle",
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_mimocode_sqlite(
        &fixture,
        &mut store,
        MiMoCodeSqliteImportOptions {
            source_path: Some(fixture.clone()),
            ..MiMoCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);
    assert_message_part_import(
        &store,
        CaptureProvider::MiMoCode,
        MIMOCODE_SQLITE_SOURCE_FORMAT,
        "mimocode-part-root",
        "mimocode message part oracle",
    );

    let second = import_mimocode_sqlite(
        &fixture,
        &mut store,
        MiMoCodeSqliteImportOptions {
            source_path: Some(fixture.clone()),
            ..MiMoCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 0);
    assert_eq!(second.skipped_events, 0);
}

#[test]
fn native_opencode_message_part_invalid_json_reports_failure() {
    let temp = tempdir();
    let fixture = write_opencode_message_part_db(
        &temp,
        "opencode-message-part-invalid-json.db",
        "opencode-invalid-part-root",
        "opencode invalid part oracle",
    );
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update part set data = '{invalid json' where id = 'part-text'",
        [],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
            .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("invalid JSON in message part part-text"));
}

fn assert_message_part_import(
    store: &Store,
    provider: CaptureProvider,
    source_format: &str,
    provider_session_id: &str,
    oracle_text: &str,
) {
    let session_id = stored_provider_session_id(store, provider, provider_session_id);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(!events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let event = events
        .iter()
        .find(|event| event.event_type == EventType::Message)
        .expect("message part event imported");
    assert_eq!(event.event_type, EventType::Message);
    assert_eq!(event.payload["body"]["text"].as_str(), Some(oracle_text));
    assert_eq!(
        event.payload["body"]["message_id"].as_str(),
        Some("part-message")
    );
    assert_eq!(event.payload["body"]["part_id"].as_str(), Some("part-text"));
    assert_eq!(
        event.sync.metadata["source_format"].as_str(),
        Some(source_format)
    );
    let rendered = serde_json::to_string(event).unwrap();
    assert!(rendered.contains("message:part-message:part:part-text"));
    assert!(!rendered.contains("session_message:"));
    assert!(!rendered.contains("part-tool"));
    assert!(!rendered.contains("write_file"));
    assert!(!rendered.contains("outputPath"));
    assert!(!rendered.contains("part-patch"));
    assert!(!rendered.contains("opencode_part_from_files"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("raw-opencode-patch-needle"));

    assert!(store
        .search_event_hits(oracle_text, 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(provider)));
    assert!(store
        .search_event_hits("Begin Patch", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("raw-opencode-patch-needle", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("tool_arg_should_not_touch", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("opencode_part_from_files", 10)
        .unwrap()
        .is_empty());

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/opencode_part.txt"));
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/opencode_part_from_files.txt"));
    assert!(!archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/tool_arg_should_not_touch.txt"));
}

#[test]
fn native_opencode_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_opencode_smoke_db(&temp, true);
    let corrupt = temp.path().join("corrupt-opencode.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &malformed,
        &mut store,
        OpenCodeSqliteImportOptions {
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
fn native_opencode_accepts_empty_current_schema_without_fabricating_units() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_imports_legacy_message_table_when_session_message_is_absent() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);

    let session_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "current-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
    assert!(events[0].payload.to_string().contains("legacy hello"));
}

#[test]
fn native_opencode_keeps_metadata_only_session_message_authoritative() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_metadata_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = store
        .session_by_external_session(CaptureProvider::OpenCode, "strict-root")
        .unwrap()
        .unwrap()
        .id;
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].payload.to_string().contains("metadata-only"));
    assert!(!events[0]
        .payload
        .to_string()
        .contains("legacy fallback prompt"));
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["legacy_projection"]["selected_message_table"].as_str(),
        Some("session_message")
    );
}

#[test]
fn native_opencode_rejects_malformed_authoritative_rows_without_legacy_fallback() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_malformed_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0].error.contains("invalid JSON"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_malformed_metadata_authoritative_rows_without_legacy_fallback() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_metadata_bad_seq_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_imports_tool_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_tool_only_db(&temp, "opencode-tool-only.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "strict-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::ToolCall);
}

#[test]
fn native_opencode_keeps_metadata_only_session_entry_authoritative() {
    let temp = tempdir();
    let fixture = write_opencode_session_entry_metadata_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = store
        .session_by_external_session(CaptureProvider::OpenCode, "strict-root")
        .unwrap()
        .unwrap()
        .id;
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].payload.to_string().contains("metadata-only"));
    assert!(!events[0]
        .payload
        .to_string()
        .contains("legacy fallback prompt"));
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["legacy_projection"]["selected_message_table"].as_str(),
        Some("session_entry")
    );
}

#[test]
fn native_kilo_imports_metadata_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_all_metadata_db(&temp, "kilo-all-metadata.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Kilo, "strict-root");
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 1);
}

#[test]
fn native_kilo_imports_tool_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_tool_only_db(&temp, "kilo-tool-only.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Kilo, "strict-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::ToolCall);
}

#[test]
fn native_opencode_rejects_changed_message_schema_before_querying() {
    let temp = tempdir();
    let fixture = write_opencode_future_incomplete_schema_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("OpenCode SQLite message table missing required column(s): data"));
}
