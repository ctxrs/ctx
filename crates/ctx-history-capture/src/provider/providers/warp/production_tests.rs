use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_store::Store;
use rusqlite::{params, Connection};
use tempfile::tempdir;

use super::import_warp_nativepath;
use crate::{
    ImportProfile, ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProviderAdapterContext, ProviderImportOptions,
    ProviderImportWorkResult,
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
