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
        provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
        CertifiedProviderCursor,
    },
    CaptureWorkLimit, ImportProfile, OutputNativeCursor, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportWorkResult,
    DEEPAGENTS_SQLITE_SOURCE_FORMAT,
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
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', 'checkpoint-a', ?1, ?2, 'messages', 'msgpack', ?3)",
        rusqlite::params![task_id, idx, message_blob(messages)],
    )
    .unwrap();
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
