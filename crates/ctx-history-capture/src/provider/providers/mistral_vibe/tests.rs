use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_store::{Store, StoreError};
use serde_json::json;
use tempfile::TempDir;

use crate::{
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportWorkResult,
};

use super::{import_mistral_vibe_nativepath, native_path::source_cursor_stream};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    messages: PathBuf,
    database: PathBuf,
}

fn fixture(lines: &[serde_json::Value]) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vibe");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        json!({
            "session_id": "mistral-nativepath-session",
            "start_time": "2026-07-25T12:00:00Z",
            "environment": {"working_directory": "/workspace"},
        })
        .to_string(),
    )
    .unwrap();
    let mut transcript = lines
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    transcript.push('\n');
    let messages = session.join("messages.jsonl");
    fs::write(&messages, transcript).unwrap();
    let database = temp.path().join("history.sqlite");
    Fixture {
        _temp: temp,
        root,
        messages,
        database,
    }
}

fn context(root: &std::path::Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "mistral-nativepath-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-25T13:00:00Z".parse::<DateTime<Utc>>().unwrap(),
    }
}

fn import(fixture: &Fixture, store: &mut Store) -> crate::Result<crate::ProviderImportSummary> {
    import_with_profile(fixture, store, ImportProfile::CoreOnly)
}

fn import_with_profile(
    fixture: &Fixture,
    store: &mut Store,
    import_profile: ImportProfile,
) -> crate::Result<crate::ProviderImportSummary> {
    import_mistral_vibe_nativepath(
        &fixture.root,
        store,
        context(&fixture.root),
        ProviderImportOptions {
            import_profile,
            ..ProviderImportOptions::default()
        },
    )
}

#[test]
fn nativepath_core_is_idempotent_resumes_append_and_elides_success_output() {
    let fixture = fixture(&[
        json!({
            "message_id": "message-1",
            "role": "user",
            "content": "hello",
            "timestamp": "2026-07-25T12:00:01Z",
        }),
        json!({
            "message_id": "call-1",
            "role": "assistant",
            "tool_calls": [{"id": "call-1", "function": {"name": "shell"}}],
            "timestamp": "2026-07-25T12:00:02Z",
        }),
        json!({
            "message_id": "result-success",
            "role": "tool",
            "tool_call_id": "call-1",
            "name": "shell",
            "status": "success",
            "content": "SUCCESS_BODY_MUST_NOT_ENTER_CORE",
            "timestamp": "2026-07-25T12:00:03Z",
        }),
        json!({
            "message_id": "result-failure",
            "role": "tool",
            "tool_call_id": "call-2",
            "name": "shell",
            "status": "error",
            "is_error": true,
            "content": "bounded failure evidence",
            "timestamp": "2026-07-25T12:00:04Z",
        }),
    ]);
    let mut store = Store::open(&fixture.database).unwrap();

    let initial = import(&fixture, &mut store).unwrap();
    assert_eq!(initial.imported_sessions, 1, "{:?}", initial.failures);
    let session = store.list_sessions().unwrap().pop().unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let event_types = events
        .iter()
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(
        initial.imported_events, 3,
        "types={event_types:?} failures={:?}",
        initial.failures
    );
    assert_eq!(events.len(), 3, "types={event_types:?}");
    let persisted = serde_json::to_string(&events).unwrap();
    assert!(!persisted.contains("SUCCESS_BODY_MUST_NOT_ENTER_CORE"));
    assert!(persisted.contains("bounded failure evidence"));

    let stream = source_cursor_stream(&fixture.messages).unwrap();
    let first_cursor = store
        .get_sync_cursor(None, "mistral-nativepath-machine", &stream)
        .unwrap()
        .unwrap()
        .cursor;
    let replay = import(&fixture, &mut store).unwrap();
    assert_eq!(replay.imported_events, 0, "{:?}", replay.failures);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        store
            .get_sync_cursor(None, "mistral-nativepath-machine", &stream)
            .unwrap()
            .unwrap()
            .cursor,
        first_cursor
    );

    let appended = json!({
        "message_id": "message-2",
        "role": "assistant",
        "content": "append",
        "timestamp": "2026-07-25T12:00:05Z",
    });
    let mut transcript = fs::read_to_string(&fixture.messages).unwrap();
    transcript.push_str(&appended.to_string());
    transcript.push('\n');
    fs::write(&fixture.messages, &transcript).unwrap();
    let append = import(&fixture, &mut store).unwrap();
    assert_eq!(append.imported_events, 1, "{:?}", append.failures);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 4);
}

#[test]
fn nativepath_holds_incomplete_tail_and_retires_a_disappeared_root() {
    let fixture = fixture(&[json!({
        "message_id": "message-1",
        "role": "user",
        "content": "complete",
    })]);
    let mut store = Store::open(&fixture.database).unwrap();
    import(&fixture, &mut store).unwrap();
    let session = store.list_sessions().unwrap().pop().unwrap();
    let event_id = store.events_for_session(session.id).unwrap()[0].id;

    let partial = json!({
        "message_id": "message-2",
        "role": "assistant",
        "content": "complete after newline",
    })
    .to_string();
    let mut transcript = fs::read_to_string(&fixture.messages).unwrap();
    transcript.push_str(&partial);
    fs::write(&fixture.messages, &transcript).unwrap();
    let incomplete = import(&fixture, &mut store).unwrap();
    assert_eq!(incomplete.imported_events, 0, "{:?}", incomplete.failures);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    transcript.push('\n');
    fs::write(&fixture.messages, transcript).unwrap();
    let completed = import(&fixture, &mut store).unwrap();
    assert_eq!(completed.imported_events, 1, "{:?}", completed.failures);
    store
        .authorized_source_route_for_event(event_id)
        .expect("live Mistral source route must remain authorized");

    fs::remove_dir_all(&fixture.root).unwrap();
    let retired = import(&fixture, &mut store).unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(matches!(
        store.authorized_source_route_for_event(event_id),
        Err(StoreError::AuthorizedSourceRouteUnavailable { .. })
    ));
    let retry = import(&fixture, &mut store).unwrap();
    assert_eq!(retry.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn nativepath_reconciles_rewrite_truncation_and_live_replacement() {
    let fixture = fixture(&[json!({
        "message_id": "original-1",
        "role": "user",
        "content": "original",
    })]);
    let mut store = Store::open(&fixture.database).unwrap();
    import(&fixture, &mut store).unwrap();

    fs::write(
        &fixture.messages,
        format!(
            "{}\n{}\n",
            json!({
                "message_id": "rewrite-1",
                "role": "user",
                "content": "rewrite first",
            }),
            json!({
                "message_id": "rewrite-2",
                "role": "assistant",
                "content": "rewrite second",
            }),
        ),
    )
    .unwrap();
    let rewrite = import(&fixture, &mut store).unwrap();
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store
        .search_event_hits("rewrite second", 10)
        .unwrap()
        .is_empty());

    fs::write(
        &fixture.messages,
        format!(
            "{}\n",
            json!({
                "message_id": "truncate-1",
                "role": "user",
                "content": "truncated source",
            })
        ),
    )
    .unwrap();
    let truncation = import(&fixture, &mut store).unwrap();
    assert_eq!(truncation.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store
        .search_event_hits("truncated source", 10)
        .unwrap()
        .is_empty());

    fs::remove_file(&fixture.messages).unwrap();
    fs::write(
        &fixture.messages,
        format!(
            "{}\n",
            json!({
                "message_id": "replacement-1",
                "role": "assistant",
                "content": "replacement source",
            })
        ),
    )
    .unwrap();
    let replacement = import(&fixture, &mut store).unwrap();
    assert_eq!(replacement.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store
        .search_event_hits("replacement source", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn nativepath_replays_output_after_later_pro_activation_without_touching_core() {
    const OUTPUT: &str = "MISTRAL_SUCCESS_OUTPUT_ONLY_IN_PRO";
    let fixture = fixture(&[
        json!({
            "message_id": "message-1",
            "role": "user",
            "content": "core first",
        }),
        json!({
            "message_id": "result-success",
            "role": "tool",
            "tool_call_id": "call-1",
            "name": "read_file",
            "status": "success",
            "content": OUTPUT,
        }),
    ]);
    let mut store = Store::open(&fixture.database).unwrap();
    let core = import(&fixture, &mut store).unwrap();
    assert_eq!(core.imported_events, 1, "{:?}", core.failures);
    let session = store.list_sessions().unwrap().pop().unwrap();
    assert!(
        !serde_json::to_string(&store.events_for_session(session.id).unwrap())
            .unwrap()
            .contains(OUTPUT)
    );

    let sink = Arc::new(RecordingSink::new(fixture.database.clone()));
    let replay = import_with_profile(
        &fixture,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [OUTPUT.as_bytes()]
    );
    let pages = sink.pages.load(Ordering::SeqCst);
    import_with_profile(
        &fixture,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages);
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            saw_committed_core: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "mistral-nativepath-test-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().get(source).cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().unwrap().insert(
            page.source.clone(),
            ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            },
        );
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }
}
