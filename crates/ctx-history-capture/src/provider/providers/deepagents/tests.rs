use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{CaptureProvider, EntityTimestamps, SyncCursor};
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{
    common::time::parse_rfc3339_utc,
    native_source::NativePosition,
    provider::importer::{
        provider_path_identity, provider_source_cursor_stream_for_path,
        provider_source_event_import_identity, BoundedParserCheckpoint, CertifiedProviderCursor,
    },
    CaptureError, CaptureWorkLimit, ImportProfile, OutputNativeCursor,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderAdapterContext, ProviderImportOptions,
    ProviderImportTerminalOutcome, ProviderImportWorkResult, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
};

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "deepagents-nativepath-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: parse_rfc3339_utc("2026-07-25T20:00:00Z").unwrap(),
    }
}

fn create_database(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table checkpoints (
            thread_id text not null,
            checkpoint_ns text not null default '',
            checkpoint_id text not null,
            parent_checkpoint_id text,
            type text,
            checkpoint blob,
            metadata blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id)
        );
        create table writes (
            thread_id text not null,
            checkpoint_ns text not null default '',
            checkpoint_id text not null,
            task_id text not null,
            idx integer not null,
            channel text not null,
            type text,
            value blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id, task_id, idx)
        );",
    )
    .unwrap();
    conn
}

fn insert_checkpoint(conn: &Connection, thread_id: &str, checkpoint_id: &str) {
    let metadata = serde_json::to_vec(&json!({
        "updated_at": "2026-07-25T20:00:00Z",
        "agent_name": "deepagents-test",
        "git_branch": "test/nativepath",
        "cwd": "/workspace/deepagents",
    }))
    .unwrap();
    conn.execute(
        "insert into checkpoints
         (thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata)
         values (?1, '', ?2, x'00', ?3)",
        rusqlite::params![thread_id, checkpoint_id, metadata],
    )
    .unwrap();
}

fn message(role: &str, text: &str, id: &str) -> MsgpackValue {
    MsgpackValue::Map(vec![
        (
            MsgpackValue::String("type".into()),
            MsgpackValue::String(role.into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String(text.into()),
        ),
        (
            MsgpackValue::String("id".into()),
            MsgpackValue::String(id.into()),
        ),
    ])
}

fn tool_message(text: &str, id: &str, status: &str) -> MsgpackValue {
    let MsgpackValue::Map(mut fields) = message("tool", text, id) else {
        unreachable!();
    };
    fields.push((
        MsgpackValue::String("status".into()),
        MsgpackValue::String(status.into()),
    ));
    MsgpackValue::Map(fields)
}

fn message_blob(messages: Vec<MsgpackValue>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_msgpack_value(&mut bytes, &MsgpackValue::Array(messages)).unwrap();
    bytes
}

fn insert_write(conn: &Connection, messages: Vec<MsgpackValue>) {
    insert_write_at(conn, "task-a", 0, messages);
}

fn insert_write_at(conn: &Connection, task_id: &str, idx: i64, messages: Vec<MsgpackValue>) {
    insert_write_blob_at(conn, task_id, idx, message_blob(messages));
}

fn insert_write_blob_at(conn: &Connection, task_id: &str, idx: i64, value: Vec<u8>) {
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', 'checkpoint-a', ?1, ?2, 'messages', 'msgpack', ?3)",
        rusqlite::params![task_id, idx, value],
    )
    .unwrap();
}

fn replace_write_messages(conn: &Connection, messages: Vec<MsgpackValue>) {
    conn.execute(
        "update writes set value = ?1 where thread_id = 'thread-a' and task_id = 'task-a' and idx = 0",
        [message_blob(messages)],
    )
    .unwrap();
}

fn event_search_row(store_path: &Path, event_id: Uuid) -> (i64, String) {
    Connection::open(store_path)
        .unwrap()
        .query_row(
            "select rowid, preview_text from event_search where event_id = ?1",
            [event_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn import(path: &Path, store: &mut Store, options: ProviderImportOptions) -> ProviderImportSummary {
    import_deepagents_nativepath(path, store, context(path), options).unwrap()
}

#[test]
fn nativepath_fresh_noop_and_success_output_privacy() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        vec![
            message("human", "hello from Deep Agents", "message-a"),
            tool_message("SUCCESS_OUTPUT_SECRET", "tool-a", "success"),
        ],
    );
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let first = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("SUCCESS_OUTPUT_SECRET"));

    let second = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(second.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 1);
}

#[test]
fn msgpack_requires_eof_and_rejects_trailing_bytes() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    let mut payload = message_blob(vec![message("human", "valid prefix", "message-a")]);
    payload.push(0xc0);
    insert_write_blob_at(&conn, "task-a", 0, payload);
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let summary = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0].error.contains("trailing bytes"));
    assert_eq!(
        summary.terminal_outcome(),
        ProviderImportTerminalOutcome::CoreCursorCommitted
    );
}

#[test]
fn unsupported_non_system_entries_reject_without_dropping_valid_siblings() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        vec![
            message("human", "valid first sibling", "message-a"),
            message(
                "future_message",
                "unsupported sibling",
                "message-unsupported",
            ),
            MsgpackValue::Integer(7.into()),
            message("ai", "valid second sibling", "message-b"),
            message(
                "system",
                "intentionally ignored system entry",
                "message-system",
            ),
        ],
    );
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let first = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(first.imported_events, 2);
    assert_eq!(first.failed, 2);
    assert!(first.has_accepted_content());
    assert_eq!(
        first.terminal_outcome(),
        ProviderImportTerminalOutcome::CoreCursorCommitted
    );
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("entry 1")));
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("entry 2")));
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("valid first sibling"));
    assert!(rendered.contains("valid second sibling"));
    assert!(!rendered.contains("unsupported sibling"));

    let replay = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.skipped_events, 2);
    assert_eq!(replay.failed, 2);
    assert!(replay.has_accepted_content());
    assert_eq!(replay.failures, first.failures);
    assert_eq!(
        replay.terminal_outcome(),
        ProviderImportTerminalOutcome::CoreCursorCommitted
    );
}

#[test]
fn replay_preserves_exact_rejection_count_with_bounded_evidence() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    let mut messages = vec![message(
        "human",
        "accepted beside many rejects",
        "message-a",
    )];
    messages.extend((0..70).map(|value| MsgpackValue::Integer(value.into())));
    insert_write(&conn, messages);
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let first = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 70);
    assert_eq!(first.failures.len(), 64);

    let replay = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(replay.skipped_events, 1);
    assert_eq!(replay.failed, 70);
    assert_eq!(replay.failures, first.failures);
    assert!(replay.has_accepted_content());
}

#[test]
fn replacement_retires_omitted_event_and_rewrite_restores_it() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        vec![
            message("human", "first", "message-a"),
            message("ai", "second", "message-b"),
        ],
    );
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    import(&source_path, &mut store, ProviderImportOptions::default());
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    let initial = store.events_for_session(session.id).unwrap();
    assert_eq!(initial.len(), 2);
    let omitted = initial
        .iter()
        .find(|event| event.payload.to_string().contains("second"))
        .unwrap()
        .id;

    std::fs::remove_file(&source_path).unwrap();
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(&conn, vec![message("human", "first", "message-a")]);
    drop(conn);
    import(&source_path, &mut store, ProviderImportOptions::default());
    assert!(store.get_event(omitted).unwrap().sync.deleted_at.is_some());

    let conn = Connection::open(&source_path).unwrap();
    conn.execute("delete from writes", []).unwrap();
    insert_write(
        &conn,
        vec![
            message("human", "first", "message-a"),
            message("ai", "second", "message-b"),
        ],
    );
    drop(conn);
    import(&source_path, &mut store, ProviderImportOptions::default());
    assert!(store.get_event(omitted).unwrap().sync.deleted_at.is_none());
}

#[test]
fn bounded_restart_and_append_resume_from_committed_native_cursor() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let store_path = directory.path().join("store.sqlite");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        (0..55)
            .map(|index| {
                message(
                    if index % 2 == 0 { "human" } else { "ai" },
                    &format!("bounded message {index}"),
                    &format!("message-{index}"),
                )
            })
            .collect(),
    );
    drop(conn);

    let mut groups = 0;
    loop {
        groups += 1;
        let mut store = Store::open(&store_path).unwrap();
        let options = ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            ..Default::default()
        };
        let summary = import(&source_path, &mut store, options);
        if !summary.work_remaining {
            break;
        }
        assert!(groups < 12);
    }
    assert!(groups > 2);
    let store = Store::open(&store_path).unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 55);
    drop(store);

    let conn = Connection::open(&source_path).unwrap();
    insert_write_at(
        &conn,
        "task-b",
        0,
        vec![message("ai", "appended after restart", "message-appended")],
    );
    drop(conn);
    let mut attempts = 0;
    loop {
        attempts += 1;
        let mut store = Store::open(&store_path).unwrap();
        let options = ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            ..Default::default()
        };
        if !import(&source_path, &mut store, options).work_remaining {
            break;
        }
        assert!(attempts < 12);
    }
    let store = Store::open(&store_path).unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 56);
    assert!(events
        .iter()
        .any(|event| event.payload.to_string().contains("appended after restart")));
}

#[test]
fn one_safe_group_restart_never_claims_write_for_unpublished_thread() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let store_path = directory.path().join("store.sqlite");
    let conn = create_database(&source_path);
    conn.execute(
        "insert into checkpoints
         (thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata)
         values ('thread-a', '', 'checkpoint-a', x'00', 'unsupported text metadata')",
        [],
    )
    .unwrap();
    insert_write(
        &conn,
        vec![message(
            "human",
            "must never be published",
            "message-unpublished",
        )],
    );
    drop(conn);

    let mut attempts = 0;
    loop {
        attempts += 1;
        let mut store = Store::open(&store_path).unwrap();
        let summary = import_deepagents_nativepath(
            &source_path,
            &mut store,
            context(&source_path),
            ProviderImportOptions {
                capture_work_limit: CaptureWorkLimit::OneSafeGroup,
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("restart attempt {attempts} failed: {error:?}"));
        assert_eq!(store.list_capture_sources().unwrap().len(), 0);
        if !summary.work_remaining {
            assert_eq!(summary.imported_events, 0);
            assert_eq!(summary.failed, 2);
            assert_eq!(
                summary.terminal_outcome(),
                ProviderImportTerminalOutcome::CoreCursorCommitted
            );
            break;
        }
        assert!(attempts < 12);
    }
    assert!(attempts > 1);

    let canonical_path = std::fs::canonicalize(&source_path).unwrap();
    let route_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let mut store = Store::open(&store_path).unwrap();
    let stored = store
        .get_sync_cursor(None, "deepagents-nativepath-test", &stream)
        .unwrap()
        .unwrap();
    let committed = ctx_history_store::decode_native_path_committed_cursor(&stored.cursor).unwrap();
    let cursor: serde_json::Value = serde_json::from_str(committed.provider_cursor()).unwrap();
    assert_eq!(cursor["accepted_sessions"], 0);
    assert_eq!(cursor["accepted_events"], 0);
    assert_eq!(cursor["rejected_records"], 2);
    let cursor_rejections: Vec<crate::ProviderImportFailure> =
        serde_json::from_value(cursor["rejections"].clone()).unwrap();
    assert_eq!(cursor_rejections.len(), 2);
    assert!(cursor_rejections
        .iter()
        .any(|failure| failure.error.contains("no valid bounded checkpoint")));
    assert!(cursor_rejections
        .iter()
        .any(|failure| failure.error.contains("uncommitted thread")));

    let replay = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.skipped_events, 0);
    assert_eq!(replay.failed, 2);
    assert!(!replay.has_accepted_content());
    assert_eq!(replay.failures, cursor_rejections);
}

#[derive(Default)]
struct RecordingOutputSink {
    fail_once: AtomicBool,
    behind: AtomicUsize,
    bodies: Mutex<Vec<Vec<u8>>>,
}

impl ProOutputSink for RecordingOutputSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "deepagents-nativepath-test-materializer"
    }

    fn observe_source(
        &self,
        _source: &crate::OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new("injected", "retry output replay"));
        }
        self.bodies.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: OutputNativeCursor {
                version: page.next_safe_cursor.version,
                payload: page.next_safe_cursor.payload.clone(),
            },
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn output_failure_does_not_roll_back_core_and_replay_is_independent() {
    const SECRET: &str = "DEEPAGENTS_PRO_OUTPUT_SECRET";

    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        vec![
            message("human", "core message", "message-a"),
            tool_message(SECRET, "tool-a", "success"),
        ],
    );
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let sink = Arc::new(RecordingOutputSink::default());
    sink.fail_once.store(true, Ordering::SeqCst);
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(sink.clone()),
        ..Default::default()
    };
    let core = import(&source_path, &mut store, options);
    assert_eq!(core.imported_sessions, 1);
    assert_eq!(core.imported_events, 1);
    assert_eq!(core.failed, 1);
    assert_eq!(
        core.failures[0].error,
        "Deep Agents Pro output is behind committed Core"
    );
    assert!(core.work_remaining);
    assert_eq!(
        core.terminal_outcome(),
        ProviderImportTerminalOutcome::CoreCursorCommitted
    );
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    assert!(
        !serde_json::to_string(&store.events_for_session(session.id).unwrap())
            .unwrap()
            .contains(SECRET)
    );

    let replay_options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(sink.clone()),
        ..Default::default()
    };
    let replay = import(&source_path, &mut store, replay_options);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.failed, 0);
    assert!(!replay.work_remaining);
    assert_eq!(
        replay.terminal_outcome(),
        ProviderImportTerminalOutcome::CoreCursorCommitted
    );
    assert!(sink
        .bodies
        .lock()
        .unwrap()
        .iter()
        .any(|body| body == SECRET.as_bytes()));
}

#[test]
fn released_cursor_is_consumed_only_as_a_readable_source_migration() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        vec![message("human", "migrated", "message-migrated")],
    );
    drop(conn);

    let canonical_path = std::fs::canonicalize(&source_path).unwrap();
    let route_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let released = CertifiedProviderCursor::new(
        "released-deepagents-source",
        4,
        7,
        NativePosition::new("deepagents-logical-rowid-v2", vec![0]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap();
    let imported_at = context(&source_path).imported_at;
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "deepagents-nativepath-test".to_owned(),
            stream: stream.clone(),
            cursor: released.encode().unwrap(),
            last_synced_at: Some(imported_at),
            timestamps: EntityTimestamps {
                created_at: imported_at,
                updated_at: imported_at,
            },
        })
        .unwrap();

    let migrated = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(migrated.imported_sessions, 1);
    assert_eq!(migrated.imported_events, 1);
    let published = store
        .get_sync_cursor(None, "deepagents-nativepath-test", &stream)
        .unwrap()
        .unwrap();
    assert!(ctx_history_store::decode_native_path_committed_cursor(&published.cursor).is_ok());
    assert!(CertifiedProviderCursor::decode_if_certified(&published.cursor).is_err());
}

#[test]
fn released_v025_store_upgrade_preserves_ids_updates_and_unrelated_search_rows() {
    const FIRST_BEFORE: &str = "V025_STABLE_FIRST_BEFORE";
    const FIRST_CHANGED: &str = "V025_STABLE_FIRST_CHANGED";
    const SECOND: &str = "V025_STABLE_APPENDED_SECOND";
    const UNRELATED: &str = "V025_UNRELATED_SEARCH_SENTINEL";

    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(&conn, vec![message("human", FIRST_BEFORE, "message-a")]);
    drop(conn);

    // Materialize the current source/session envelopes once, then seed only the released
    // v0.25-shaped Core rows into a fresh Store.
    let envelope_store_path = directory.path().join("envelopes.sqlite");
    let mut envelope_store = Store::open(&envelope_store_path).unwrap();
    import(
        &source_path,
        &mut envelope_store,
        ProviderImportOptions::default(),
    );
    let source = envelope_store.list_capture_sources().unwrap().remove(0);
    let session = envelope_store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    let current_event = envelope_store
        .events_for_session(session.id)
        .unwrap()
        .remove(0);
    drop(envelope_store);

    let released_cursor = "thread:thread-a:checkpoint:checkpoint-a:task:task-a:write:0:message:0";
    let released_identity = provider_source_event_import_identity(source.id, 1, released_cursor);
    let mut released_event = current_event;
    released_event.id = released_identity.id;
    released_event.seq = released_identity.seq;
    released_event.dedupe_key = Some(released_identity.dedupe_key);
    released_event.payload["provider_event_index"] = json!(1);
    released_event.payload["provider_event_hash"] = json!(released_cursor);
    released_event.payload["cursor"] = json!(released_cursor);
    released_event.sync.metadata["provider_event_index"] = json!(1);
    released_event.sync.metadata["provider_event_hash"] = json!(released_cursor);
    released_event.sync.metadata["cursor"] = json!(released_cursor);

    let unrelated_id = Uuid::new_v4();
    let mut unrelated_event = released_event.clone();
    unrelated_event.id = unrelated_id;
    unrelated_event.seq = 4_000_000_000_000_000_000;
    unrelated_event.session_id = None;
    unrelated_event.capture_source_id = None;
    unrelated_event.dedupe_key = Some(format!("unrelated-search-{unrelated_id}"));
    unrelated_event.payload = json!({"body": {"text": UNRELATED}});
    unrelated_event.sync.metadata = json!({});

    let store_path = directory.path().join("v025-shaped-store.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    store.upsert_capture_source(&source).unwrap();
    store.upsert_session(&session).unwrap();
    store.upsert_event(&released_event).unwrap();
    store.upsert_event(&unrelated_event).unwrap();
    store.refresh_search_index().unwrap();

    let canonical_path = std::fs::canonicalize(&source_path).unwrap();
    let route_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let imported_at = context(&source_path).imported_at;
    let old_cursor = CertifiedProviderCursor::new(
        "released-v025-deepagents",
        1,
        1,
        NativePosition::new("deepagents-v025", vec![1]).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "deepagents-nativepath-test".to_owned(),
            stream,
            cursor: old_cursor.encode().unwrap(),
            last_synced_at: Some(imported_at),
            timestamps: EntityTimestamps {
                created_at: imported_at,
                updated_at: imported_at,
            },
        })
        .unwrap();

    let unrelated_projection = event_search_row(&store_path, unrelated_id);
    assert!(store
        .search_event_hits(UNRELATED, 10)
        .unwrap()
        .iter()
        .any(|hit| hit.event_id == unrelated_id));

    let upgraded = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(upgraded.skipped_events, 1);
    let upgraded_session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    assert_eq!(upgraded_session.id, session.id);
    let upgraded_events = store.events_for_session(session.id).unwrap();
    assert_eq!(upgraded_events.len(), 1);
    assert_eq!(upgraded_events[0].id, released_event.id);
    assert_eq!(
        event_search_row(&store_path, unrelated_id),
        unrelated_projection
    );

    let unchanged = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(unchanged.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(unchanged.skipped_events, 1);
    assert_eq!(
        store.events_for_session(session.id).unwrap()[0].id,
        released_event.id
    );
    assert_eq!(
        event_search_row(&store_path, unrelated_id),
        unrelated_projection
    );

    let conn = Connection::open(&source_path).unwrap();
    replace_write_messages(
        &conn,
        vec![
            message("human", FIRST_BEFORE, "message-a"),
            message("ai", SECOND, "message-b"),
        ],
    );
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    let appended = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(appended.imported_events, 1);
    let appended_events = store.events_for_session(session.id).unwrap();
    assert_eq!(appended_events.len(), 2);
    assert!(appended_events
        .iter()
        .any(|event| event.id == released_event.id));
    let appended_id = appended_events
        .iter()
        .find(|event| event.payload.to_string().contains(SECOND))
        .unwrap()
        .id;
    assert_eq!(
        event_search_row(&store_path, unrelated_id),
        unrelated_projection
    );

    let conn = Connection::open(&source_path).unwrap();
    replace_write_messages(
        &conn,
        vec![
            message("human", FIRST_CHANGED, "message-a"),
            message("ai", SECOND, "message-b"),
        ],
    );
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    import(&source_path, &mut store, ProviderImportOptions::default());
    let changed_events = store.events_for_session(session.id).unwrap();
    assert_eq!(changed_events.len(), 2);
    assert!(changed_events
        .iter()
        .any(|event| event.id == released_event.id
            && event.payload.to_string().contains(FIRST_CHANGED)));
    assert!(changed_events
        .iter()
        .any(|event| event.id == appended_id && event.payload.to_string().contains(SECOND)));
    assert!(!store
        .search_event_hits(FIRST_BEFORE, 10)
        .unwrap()
        .iter()
        .any(|hit| hit.event_id == released_event.id));
    assert!(store
        .search_event_hits(FIRST_CHANGED, 10)
        .unwrap()
        .iter()
        .any(|hit| hit.event_id == released_event.id));
    assert_eq!(
        event_search_row(&store_path, unrelated_id),
        unrelated_projection
    );
    assert!(store
        .search_event_hits(UNRELATED, 10)
        .unwrap()
        .iter()
        .any(|hit| hit.event_id == unrelated_id));
}

#[test]
fn corrupt_write_advances_and_inventory_disappearance_retires_core() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let conn = create_database(&source_path);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', 'checkpoint-a', 'task-a', 0, 'messages', 'msgpack', x'd9')",
        [],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let first = import(&source_path, &mut store, ProviderImportOptions::default());
    assert_eq!(first.failed, 1);
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();

    std::fs::remove_file(&source_path).unwrap();
    let options = ProviderImportOptions {
        inventory_observation_token: Some("inventory-generation-2".to_owned()),
        ..Default::default()
    };
    let retired = import(&source_path, &mut store, options.clone());
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .get_session(session.id)
        .unwrap()
        .sync
        .deleted_at
        .is_some());
    let replay = import(&source_path, &mut store, options);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn missing_required_tables_and_columns_are_typed_unsupported_schema() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let missing_table_path = directory.path().join("missing-table.db");
    let conn = Connection::open(&missing_table_path).unwrap();
    conn.execute("create table unrelated (id integer)", [])
        .unwrap();
    drop(conn);
    let mut store = Store::open(directory.path().join("missing-table-store.sqlite")).unwrap();
    let error = import_deepagents_nativepath(
        &missing_table_path,
        &mut store,
        context(&missing_table_path),
        ProviderImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::UnsupportedSchema(ref reason)
            if reason.contains("checkpoints table")
    ));

    let missing_column_path = directory.path().join("missing-column.db");
    let conn = Connection::open(&missing_column_path).unwrap();
    conn.execute_batch(
        "create table checkpoints (
            thread_id text,
            checkpoint_ns text,
            checkpoint_id text,
            checkpoint blob,
            metadata blob
        );
        create table writes (
            thread_id text,
            checkpoint_ns text,
            checkpoint_id text,
            task_id text,
            idx integer,
            channel text,
            type text
        );",
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(directory.path().join("missing-column-store.sqlite")).unwrap();
    let error = import_deepagents_nativepath(
        &missing_column_path,
        &mut store,
        context(&missing_column_path),
        ProviderImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::UnsupportedSchema(ref reason)
            if reason.contains("value")
    ));
}
