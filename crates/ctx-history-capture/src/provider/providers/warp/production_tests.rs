use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event,
    EventRole, EventType, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{ProviderSourceLocatorObservation, Store};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::tempdir;
use uuid::Uuid;

use super::import_warp_nativepath;
use crate::{
    provider::importer::{
        provider_path_identity, provider_scoped_source_uuid,
        provider_source_cursor_stream_for_path, provider_source_event_import_identity,
        provider_source_identity, provider_source_session_uuid, provider_sync_metadata, timestamps,
        BoundedParserCheckpoint, CertifiedProviderCursor,
    },
    CaptureWorkLimit, ImportProfile, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportWorkResult, WARP_SQLITE_SOURCE_FORMAT,
};

fn field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3 | 2);
    value.extend(varint(payload.len() as u64));
    value.extend_from_slice(payload);
    value
}

fn integer_field(number: u32, integer: u64) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3);
    value.extend(varint(integer));
    value
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            return output;
        }
    }
}

fn text_message(id: &str, body: &str, timestamp_seconds: u64) -> Vec<u8> {
    let mut timestamp = integer_field(1, timestamp_seconds);
    timestamp.extend(integer_field(2, 0));
    let text = field(2, &field(1, body.as_bytes()));
    let mut message = field(1, id.as_bytes());
    message.extend(text);
    message.extend(field(11, b"task-1"));
    message.extend(field(13, b"request-1"));
    message.extend(field(14, &timestamp));
    message
}

fn text_task(messages: &[(&str, &str, u64)]) -> Vec<u8> {
    let mut task = field(1, b"task-1");
    task.extend(field(2, b"Task 1"));
    for (id, body, timestamp_seconds) in messages {
        task.extend(field(5, &text_message(id, body, *timestamp_seconds)));
    }
    task
}

fn task() -> Vec<u8> {
    text_task(&[("message-1", "hello from Warp", 1_782_259_200)])
}

fn task_with_successful_output() -> Vec<u8> {
    let mut timestamp = integer_field(1, 1_782_259_201);
    timestamp.extend(integer_field(2, 0));
    let finished = field(1, b"secret successful output");
    let run_shell = field(5, &finished);
    let mut result = field(1, b"call-1");
    result.extend(field(2, &run_shell));
    let mut message = field(1, b"message-output");
    message.extend(field(5, &result));
    message.extend(field(11, b"task-1"));
    message.extend(field(13, b"request-output"));
    message.extend(field(14, &timestamp));
    let mut task = task();
    task.extend(field(5, &message));
    task
}

fn create_source_with_task(path: &std::path::Path, task: Vec<u8>) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma user_version = 1;
             create table agent_conversations (
                 id integer primary key,
                 conversation_id text not null unique,
                 conversation_data text not null,
                 last_modified_at text not null
             );
             create table agent_tasks (
                 id integer primary key,
                 conversation_id text not null,
                 task_id text not null unique,
                 task blob not null,
                 last_modified_at text not null
             );
             create table ai_queries (
                 id integer primary key,
                 exchange_id text not null unique,
                 conversation_id text not null,
                 start_ts text not null,
                 input text not null,
                 working_directory text,
                 output_status text not null,
                 model_id text not null,
                 planning_model_id text not null default '',
                 coding_model_id text not null default ''
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into agent_conversations
             (conversation_id, conversation_data, last_modified_at)
             values ('conversation-1',
                     '{\"agent_name\":\"Warp\",\"run_id\":\"run-1\"}',
                     '2026-07-24 12:00:00')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "insert into agent_tasks
             (conversation_id, task_id, task, last_modified_at)
             values ('conversation-1', 'task-1', ?1, '2026-07-24 12:00:01')",
            params![task],
        )
        .unwrap();
}

fn create_source(path: &std::path::Path) {
    create_source_with_task(path, task());
}

fn update_task(path: &std::path::Path, task: Vec<u8>, modified_at: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "update agent_tasks
             set task = ?1, last_modified_at = ?2
             where task_id = 'task-1'",
            params![task, modified_at],
        )
        .unwrap();
}

fn seed_released_warp_store(
    store: &mut Store,
    source_path: &std::path::Path,
    context: &ProviderAdapterContext,
) -> (Uuid, Uuid, Uuid) {
    let raw_source_path = source_path.canonicalize().unwrap().display().to_string();
    let source_root = context
        .source_root
        .as_deref()
        .unwrap()
        .display()
        .to_string();
    let canonical_source_identity = provider_source_identity(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .unwrap();
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::Warp,
        "conversation-1",
        WARP_SQLITE_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let source_revision =
        "warp-sqlite-snapshot-v1:capture=5;policy=7;schema=released-fixture".to_owned();
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Warp,
                machine_id: context.machine_id.clone(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(raw_source_path.clone()),
                source_format: Some(WARP_SQLITE_SOURCE_FORMAT.to_owned()),
                source_root: Some(source_root.clone()),
                source_identity: Some(canonical_source_identity.clone()),
                external_session_id: Some("conversation-1".to_owned()),
            },
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: None,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({"source_revision": source_revision}),
            ),
        })
        .unwrap();
    let path_identity = provider_path_identity(source_path).unwrap();
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let route = store
        .reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Warp,
            source_format: WARP_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: format!("warp-sqlite:{path_identity}"),
            cursor_stream: cursor_stream.clone(),
            proposed_source_identity: canonical_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.clone(),
            observed_at_ms: 0,
        })
        .unwrap();
    store
        .bind_capture_source_provider_route(source_id, &route.route_binding())
        .unwrap();

    let session_id = provider_source_session_uuid(&canonical_source_identity, "conversation-1");
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Warp,
            external_session_id: Some("conversation-1".to_owned()),
            external_agent_id: Some("warp-agent".to_owned()),
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: None,
            timestamps: timestamps(DateTime::<Utc>::UNIX_EPOCH),
            sync: provider_sync_metadata(Fidelity::Imported, json!({})),
        })
        .unwrap();
    let identity = provider_source_event_import_identity(source_id, 0, "message-1");
    let event_id = identity.id;
    store
        .upsert_event(&Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": "warp",
                "provider_session_id": "conversation-1",
                "provider_event_index": 0,
                "provider_event_hash": "message-1",
                "text": "hello from Warp",
                "body": {"text": "hello from Warp", "message_index": 0},
                "artifacts": [],
            }),
            payload_blob_id: None,
            dedupe_key: Some(identity.dedupe_key),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": "conversation-1",
                    "provider_event_index": 0,
                    "provider_event_hash": "message-1",
                    "provider_event_hash_authority": "provider_supplied",
                    "metadata": {"event_path": raw_source_path},
                }),
            ),
        })
        .unwrap();
    let released_cursor = CertifiedProviderCursor::new(
        source_revision,
        5,
        7,
        crate::native_source::NativePosition::new("warp-conversation-task-keyset-v4", vec![0])
            .unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: cursor_stream,
            cursor: released_cursor,
            last_synced_at: None,
            timestamps: timestamps(DateTime::<Utc>::UNIX_EPOCH),
        })
        .unwrap();
    (source_id, session_id, event_id)
}

#[test]
fn released_warp_unchanged_source_migrates_in_place() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp-released.sqlite");
    create_source(&source_path);
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "warp-released-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let (_, released_session_id, released_event_id) =
        seed_released_warp_store(&mut store, &source_path, &context);

    let first = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert!(first.work_remaining);
    assert_eq!(
        store.events_for_session(released_session_id).unwrap().len(),
        1
    );
    let summary = import_warp_nativepath(
        &source_path,
        &mut store,
        context,
        ProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let events = store.events_for_session(released_session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, released_event_id);
    assert_eq!(events[0].payload["text"], "hello from Warp");
    assert_eq!(events[0].payload["text_retention"]["mode"], "bounded");
    assert_eq!(
        events[0].sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
    assert!(store
        .authorized_source_route_for_event(released_event_id)
        .is_ok());

    update_task(
        &source_path,
        text_task(&[(
            "message-1",
            "rewritten after released cutover",
            1_782_259_200,
        )]),
        "2026-07-24 12:00:02",
    );
    let context = ProviderAdapterContext {
        machine_id: "warp-released-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    import_warp_nativepath(
        &source_path,
        &mut store,
        context,
        ProviderImportOptions::default(),
    )
    .unwrap();
    let events = store.events_for_session(released_session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, released_event_id);
    assert_eq!(
        events[0].payload["text"],
        "rewritten after released cutover"
    );
}

#[test]
fn released_warp_missing_source_retires_before_cutover() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp-released-missing.sqlite");
    create_source(&source_path);
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "warp-released-missing-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let (_, released_session_id, released_event_id) =
        seed_released_warp_store(&mut store, &source_path, &context);
    fs::remove_file(&source_path).unwrap();

    let summary = import_warp_nativepath(
        &source_path,
        &mut store,
        context,
        ProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(
        store.events_for_session(released_session_id).unwrap().len(),
        1
    );
    assert!(store
        .authorized_source_route_for_event(released_event_id)
        .is_err());
}

#[test]
fn nativepath_store_cutover_is_idempotent_and_retires_a_missing_source() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp.sqlite");
    create_source(&source_path);
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "warp-production-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };

    let first = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("conversation-1"))
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]
            .payload
            .get("body")
            .and_then(serde_json::Value::as_str),
        Some("hello from Warp")
    );

    let replay = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    let observed = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions {
            inventory_observation_token: Some("warp-observation-2".to_owned()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(observed.work_result(), ProviderImportWorkResult::Changed);
    let observed_replay = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions {
            inventory_observation_token: Some("warp-observation-2".to_owned()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        observed_replay.work_result(),
        ProviderImportWorkResult::NoOp
    );

    let replacement_path = directory.path().join("replacement.sqlite");
    create_source(&replacement_path);
    fs::rename(&replacement_path, &source_path).unwrap();
    let replaced = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(replaced.skipped_sessions, 1);
    assert_eq!(replaced.imported_events, 1);
    assert_eq!(
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .filter(|session| { session.external_session_id.as_deref() == Some("conversation-1") })
            .count(),
        1
    );
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);

    fs::remove_file(&source_path).unwrap();
    let retired = import_warp_nativepath(
        &source_path,
        &mut store,
        context,
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
}

#[test]
fn nativepath_append_rewrite_and_truncation_preserve_native_identity() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp-mutation.sqlite");
    create_source(&source_path);
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "warp-mutation-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };

    import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("conversation-1"))
        .unwrap();
    let first_id = store.events_for_session(session.id).unwrap()[0].id;

    update_task(
        &source_path,
        text_task(&[
            ("message-1", "hello from Warp", 1_782_259_200),
            ("message-2", "appended from Warp", 1_782_259_201),
        ]),
        "2026-07-24 12:00:02",
    );
    let appended = import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(appended.imported_events, 1);
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .find(|event| event.payload["native_record_id"] == "message-1")
            .unwrap()
            .id,
        first_id
    );

    update_task(
        &source_path,
        text_task(&[
            (
                "message-1",
                "rewritten content from the same Warp message",
                1_782_259_200,
            ),
            ("message-2", "appended from Warp", 1_782_259_201),
        ]),
        "2026-07-24 12:00:03",
    );
    import_warp_nativepath(
        &source_path,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let rewritten = events
        .iter()
        .find(|event| event.payload["native_record_id"] == "message-1")
        .unwrap();
    assert_eq!(rewritten.id, first_id);
    assert_eq!(
        rewritten.payload["body"],
        "rewritten content from the same Warp message"
    );

    update_task(
        &source_path,
        text_task(&[(
            "message-1",
            "rewritten content from the same Warp message",
            1_782_259_200,
        )]),
        "2026-07-24 12:00:04",
    );
    import_warp_nativepath(
        &source_path,
        &mut store,
        context,
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        store.events_for_session(session.id).unwrap().len(),
        2,
        "truncation preserves historical events without an explicit tombstone policy"
    );
}

struct TestOutputSink {
    fail: bool,
    progress: Mutex<Option<ProOutputProgress>>,
    observations: AtomicUsize,
    behind: AtomicUsize,
}

impl TestOutputSink {
    fn new(fail: bool) -> Self {
        Self {
            fail,
            progress: Mutex::new(None),
            observations: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
        }
    }
}

impl ProOutputSink for TestOutputSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &crate::OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "intentional output failure",
            ));
        }
        self.observations
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(page.next_safe_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor,
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
fn output_failure_never_rolls_back_core_and_later_pro_activation_replays() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("warp-output.sqlite");
    create_source_with_task(&source_path, task_with_successful_output());
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "warp-output-test".to_owned(),
        source_path: Some(source_path.clone()),
        source_root: Some(directory.path().to_path_buf()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let failing = Arc::new(TestOutputSink::new(true));
    let options = ProviderImportOptions {
        import_profile: ImportProfile::CoreAndPro(failing.clone()),
        ..ProviderImportOptions::default()
    };
    let summary =
        import_warp_nativepath(&source_path, &mut store, context.clone(), options).unwrap();
    assert_eq!(summary.imported_events, 1);
    assert!(failing.behind.load(Ordering::SeqCst) > 0);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("conversation-1"))
        .unwrap();
    let core = serde_json::to_string(&store.events_for_session(session.id).unwrap()).unwrap();
    assert!(!core.contains("secret successful output"));

    let replay = Arc::new(TestOutputSink::new(false));
    let options = ProviderImportOptions {
        import_profile: ImportProfile::ProReplayOnly(replay.clone()),
        ..ProviderImportOptions::default()
    };
    import_warp_nativepath(&source_path, &mut store, context, options).unwrap();
    assert_eq!(replay.observations.load(Ordering::SeqCst), 1);
    assert!(replay
        .progress
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|progress| progress.terminal));
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}
