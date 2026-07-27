use std::{collections::BTreeSet, path::Path};

use chrono::{TimeZone, Utc};
use ctx_history_capture::{
    import_opencode_sqlite, CaptureWorkLimit, OpenCodeSqliteImportOptions, ProviderImportWorkResult,
};
use ctx_history_core::SyncCursor;
use ctx_history_store::{
    decode_native_path_committed_cursor, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, Store,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const MACHINE_ID: &str = "opencode-nativepath-release-blockers";

fn options(capture_work_limit: CaptureWorkLimit) -> OpenCodeSqliteImportOptions {
    OpenCodeSqliteImportOptions {
        machine_id: MACHINE_ID.to_owned(),
        imported_at: Utc.timestamp_millis_opt(1_785_024_000_000).unwrap(),
        capture_work_limit,
        ..OpenCodeSqliteImportOptions::default()
    }
}

fn create_session_schema(path: &Path, message_part: bool) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma journal_mode = wal;
             create table session (
                 id text primary key,
                 parent_id text,
                 title text,
                 directory text,
                 time_created integer not null,
                 time_updated integer not null
             );",
        )
        .unwrap();
    if message_part {
        connection
            .execute_batch(
                "create table message (
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
                     type text,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap();
    } else {
        connection
            .execute_batch(
                "create table session_message (
                     id text primary key,
                     session_id text not null,
                     type text not null,
                     seq integer not null,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap();
    }
    connection.pragma_update(None, "user_version", 8).unwrap();
    connection
}

fn insert_session(connection: &Connection, id: &str, created: i64) {
    connection
        .execute(
            "insert into session
             (id, parent_id, title, directory, time_created, time_updated)
             values (?1, null, ?2, ?3, ?4, ?5)",
            params![
                id,
                format!("title-{id}"),
                format!("/workspace/{id}"),
                created,
                created + 10,
            ],
        )
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_part(
    connection: &Connection,
    message_id: &str,
    part_id: &str,
    session_id: &str,
    role: &str,
    part_type: &str,
    created: i64,
    data: Value,
) {
    connection
        .execute(
            "insert or ignore into message
             (id, session_id, time_created, time_updated, data)
             values (?1, ?2, ?3, ?4, ?5)",
            params![
                message_id,
                session_id,
                created,
                created,
                json!({"role": role, "time": {"created": created}}).to_string(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "insert into part
             (id, message_id, session_id, type, time_created, time_updated, data)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                part_id,
                message_id,
                session_id,
                part_type,
                created,
                created,
                data.to_string(),
            ],
        )
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    connection: &Connection,
    id: &str,
    session_id: &str,
    role: &str,
    sequence: i64,
    created: i64,
    data: &str,
) {
    connection
        .execute(
            "insert into session_message
             (id, session_id, type, seq, time_created, time_updated, data)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, session_id, role, sequence, created, created, data],
        )
        .unwrap();
}

fn active_file_touches(store: &Store) -> Vec<ctx_history_core::FileTouched> {
    store
        .export_archive()
        .unwrap()
        .files_touched
        .into_iter()
        .filter(|touch| touch.sync.deleted_at.is_none())
        .collect()
}

fn opencode_cursor(store: &Store) -> SyncCursor {
    let stream = Connection::open(store.path())
        .unwrap()
        .query_row(
            "select stream from sync_cursors
             where device_id = ?1
               and stream like 'provider:opencode:opencode_sqlite:%'",
            [MACHINE_ID],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    store
        .get_sync_cursor(None, MACHINE_ID, &stream)
        .unwrap()
        .unwrap()
}

fn provider_cursor_value(store: &Store) -> Value {
    let stored = opencode_cursor(store);
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    serde_json::from_str(committed.provider_cursor()).unwrap()
}

#[test]
fn tool_touch_import_and_reimport_do_not_pack_the_sha_event_index() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let connection = create_session_schema(&source_path, true);
    insert_session(&connection, "session-a", 1);
    insert_part(
        &connection,
        "message-tool",
        "part-tool",
        "session-a",
        "assistant",
        "tool",
        2,
        json!({
            "type": "tool",
            "tool": "write_file",
            "state": {
                "status": "pending",
                "input": {"path": "src/overflow-safe.rs"}
            }
        }),
    );
    drop(connection);

    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    let first =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    let touches = active_file_touches(&store);
    assert_eq!(touches.len(), 1);
    assert_eq!(touches[0].path, "src/overflow-safe.rs");
    let stable_event_index = touches[0].sync.metadata["stable_provider_event_index"]
        .as_u64()
        .unwrap();
    assert!(stable_event_index > u64::MAX / (u64::from(u16::MAX) + 1));
    assert_eq!(touches[0].sync.metadata["source_event_touch_index"], 0);
    assert_eq!(
        touches[0].sync.metadata["provider_touch_index"],
        u64::from(u16::MAX) + 1
    );
    let touch_id = touches[0].id;

    let repeated =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(active_file_touches(&store)[0].id, touch_id);
}

#[test]
fn rewrite_retires_omitted_core_fts_and_touch_rows_after_restart() {
    const STALE: &str = "opencodestalealpha";
    const REWRITTEN: &str = "opencoderewrittenbravo";
    const UNRELATED: &str = "opencodeunrelatedcharlie";

    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let unrelated_path = temp.path().join("unrelated.db");
    let connection = create_session_schema(&source_path, true);
    insert_session(&connection, "session-a", 1);
    insert_part(
        &connection,
        "message-a",
        "part-text-a",
        "session-a",
        "user",
        "text",
        2,
        json!({"type": "text", "text": STALE}),
    );
    insert_part(
        &connection,
        "message-tool-a",
        "part-tool-a",
        "session-a",
        "assistant",
        "tool",
        3,
        json!({
            "type": "tool",
            "tool": "write_file",
            "state": {
                "status": "pending",
                "input": {"path": "src/obsolete.rs"}
            }
        }),
    );
    drop(connection);
    let connection = create_session_schema(&unrelated_path, true);
    insert_session(&connection, "session-b", 10);
    insert_part(
        &connection,
        "message-b",
        "part-text-b",
        "session-b",
        "user",
        "text",
        11,
        json!({"type": "text", "text": UNRELATED}),
    );
    insert_part(
        &connection,
        "message-tool-b",
        "part-tool-b",
        "session-b",
        "assistant",
        "tool",
        12,
        json!({
            "type": "tool",
            "tool": "write_file",
            "state": {
                "status": "pending",
                "input": {"path": "src/unrelated.rs"}
            }
        }),
    );
    drop(connection);

    let store_path = temp.path().join("store.db");
    let mut store = Store::open(&store_path).unwrap();
    for source in [&source_path, &unrelated_path] {
        import_opencode_sqlite(source, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    }
    assert_eq!(store.search_event_hits(STALE, 10).unwrap().len(), 1);
    assert_eq!(store.search_event_hits(UNRELATED, 10).unwrap().len(), 1);
    assert_eq!(active_file_touches(&store).len(), 2);

    let connection = Connection::open(&source_path).unwrap();
    connection
        .execute(
            "update part set data = ?1, time_updated = 20 where id = 'part-text-a'",
            [json!({"type": "text", "text": REWRITTEN}).to_string()],
        )
        .unwrap();
    connection
        .execute("delete from part where id = 'part-tool-a'", [])
        .unwrap();
    drop(connection);
    let interrupted = import_opencode_sqlite(
        &source_path,
        &mut store,
        options(CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert!(interrupted.work_remaining);
    assert_eq!(store.search_event_hits(STALE, 10).unwrap().len(), 1);
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    let resumed =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(resumed.work_result(), ProviderImportWorkResult::Changed);
    assert!(!resumed.work_remaining);
    assert!(store.search_event_hits(STALE, 10).unwrap().is_empty());
    assert_eq!(store.search_event_hits(REWRITTEN, 10).unwrap().len(), 1);
    assert_eq!(store.search_event_hits(UNRELATED, 10).unwrap().len(), 1);
    let active_paths = active_file_touches(&store)
        .into_iter()
        .map(|touch| touch.path)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        active_paths,
        BTreeSet::from(["src/unrelated.rs".to_owned()])
    );
    let archive = store.export_archive().unwrap();
    assert_eq!(
        archive
            .events
            .iter()
            .filter(|event| event.sync.deleted_at.is_none())
            .count(),
        3
    );
    assert_eq!(
        archive
            .events
            .iter()
            .filter(|event| event.sync.deleted_at.is_some())
            .count(),
        1
    );
    assert_eq!(store.list_sessions().unwrap().len(), 2);

    let repeated =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn rejection_ledger_holds_frontier_across_restart_and_clears_after_correction() {
    const VALID: &str = "opencodevalidsiblingdelta";
    const CORRECTED: &str = "opencodecorrectedecho";

    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let connection = create_session_schema(&source_path, false);
    insert_session(&connection, "session-a", 1);
    insert_row(
        &connection,
        "message-malformed",
        "session-a",
        "user",
        1,
        2,
        "{not-json",
    );
    insert_row(
        &connection,
        "message-valid",
        "session-a",
        "assistant",
        2,
        3,
        &json!({"role": "assistant", "text": VALID}).to_string(),
    );
    drop(connection);

    let store_path = temp.path().join("store.db");
    let mut store = Store::open(&store_path).unwrap();
    let first = import_opencode_sqlite(
        &source_path,
        &mut store,
        options(CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert!(first.work_remaining);
    let first_frontier = provider_cursor_value(&store)["frontier"].clone();
    let rejected = import_opencode_sqlite(
        &source_path,
        &mut store,
        options(CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert_eq!(rejected.failed, 1);
    assert!(rejected.work_remaining);
    let valid_event_id = store.search_event_hits(VALID, 10).unwrap()[0].event_id;
    let rejected_cursor = provider_cursor_value(&store);
    assert_eq!(rejected_cursor["frontier"], first_frontier);
    assert_eq!(rejected_cursor["rejected_records"], 1);
    assert_eq!(rejected_cursor["rejections"].as_array().unwrap().len(), 1);
    assert!(rejected_cursor["completed_state"].is_null());
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    let replayed =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(replayed.failed, 1);
    assert!(replayed.work_remaining);
    assert_eq!(
        store.search_event_hits(VALID, 10).unwrap()[0].event_id,
        valid_event_id
    );
    let replayed_cursor = provider_cursor_value(&store);
    assert_eq!(
        replayed_cursor["rejections"], rejected_cursor["rejections"],
        "replayed diagnostics must be durably deduplicated"
    );

    let connection = Connection::open(&source_path).unwrap();
    connection
        .execute(
            "update session_message
             set data = ?1, time_updated = 30
             where id = 'message-malformed'",
            [json!({"role": "user", "text": CORRECTED}).to_string()],
        )
        .unwrap();
    drop(connection);
    let corrected =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(corrected.failed, 0);
    assert!(!corrected.work_remaining);
    assert_eq!(store.search_event_hits(CORRECTED, 10).unwrap().len(), 1);
    assert_eq!(
        store.search_event_hits(VALID, 10).unwrap()[0].event_id,
        valid_event_id
    );
    let corrected_cursor = provider_cursor_value(&store);
    assert_eq!(corrected_cursor["rejected_records"], 0);
    assert!(corrected_cursor["rejections"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(corrected_cursor["generation_phase"]["phase"], "complete");
    assert!(corrected_cursor["completed_state"].is_object());

    let repeated =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn prior_real_terminal_policy_cursor_republishes_current_identity_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("opencode.db");
    let connection = create_session_schema(&source_path, false);
    insert_session(&connection, "session-a", 1);
    insert_row(
        &connection,
        "message-a",
        "session-a",
        "user",
        1,
        2,
        &json!({"role": "user", "text": "terminal cursor migration"}).to_string(),
    );
    drop(connection);

    let mut store = Store::open(temp.path().join("store.db")).unwrap();
    import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    let stored = opencode_cursor(&store);
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    let mut prior: Value = serde_json::from_str(committed.provider_cursor()).unwrap();
    assert_eq!(prior["generation_phase"]["phase"], "complete");
    let prior_generation = prior["generation"].as_u64().unwrap();
    prior["version"] = json!(1);
    prior["pending_state"]["policy_revision"] = json!(2);
    prior["completed_state"]["policy_revision"] = json!(2);
    prior.as_object_mut().unwrap().remove("generation_phase");
    prior.as_object_mut().unwrap().remove("rejections");
    let mut next = stored.clone();
    next.cursor = serde_json::to_string(&prior).unwrap();
    let transition = NativePathCursorTransition::new(Some(stored.cursor.clone()), next);
    let bulk_guard = store.begin_event_search_bulk_mode().unwrap();
    let admission = store.admit_event_search_bulk_group(&bulk_guard).unwrap();
    let mut group = store
        .begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(
                0,
                1,
                transition.next().cursor.len().saturating_add(4096),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        group
            .classify_cursor_set(
                "opencode-nativepath-integration-prior-terminal-v1",
                std::slice::from_ref(&transition),
            )
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    ));
    group.prepare_journal_checkpoint().unwrap();
    group.publish_cursor_set().unwrap();
    group.commit().unwrap();
    store.finish_event_search_bulk_mode(&bulk_guard).unwrap();
    drop(bulk_guard);

    let migrated =
        import_opencode_sqlite(&source_path, &mut store, options(CaptureWorkLimit::Drain)).unwrap();
    assert_eq!(migrated.work_result(), ProviderImportWorkResult::Changed);
    let current = provider_cursor_value(&store);
    assert_eq!(current["generation"], prior_generation + 1);
    assert_eq!(current["pending_state"]["parser_revision"], 2);
    assert_eq!(current["pending_state"]["policy_revision"], 3);
    assert_eq!(current["completed_state"]["policy_revision"], 3);
    assert_eq!(current["generation_phase"]["phase"], "complete");
    assert_eq!(
        store
            .export_archive()
            .unwrap()
            .events
            .into_iter()
            .filter(|event| event.sync.deleted_at.is_none())
            .count(),
        1
    );
}
