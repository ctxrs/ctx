use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{CaptureProvider, EventType, RunStatus};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{
    import_roo_task_json_history, ImportProfile, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult, RooTaskJsonImportOptions,
};

const MACHINE: &str = "roo-nativepath-production-test";
const OUTPUT_SENTINEL: &str = "roo-success-output-body-must-stay-out-of-core";

#[test]
fn roo_production_lifecycle_is_nativepath_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-task");
    write_task(
        &task,
        &[
            message("user", "fresh-user"),
            message("assistant", "fresh-assistant"),
            successful_output(OUTPUT_SENTINEL),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_core_has_no_successful_output_body(&store);
    let routed_event = first_roo_event(&store);

    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    let restart = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

    write_api(
        &task,
        &[
            message("user", "fresh-user"),
            message("assistant", "fresh-assistant"),
            successful_output(OUTPUT_SENTINEL),
            message("assistant", "append-suffix"),
        ],
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .search_event_hits("append-suffix", 10)
        .expect("append search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RooCode)));

    write_api(
        &task,
        &[
            message("user", "rewrite-user"),
            message("assistant", "rewrite-assistant"),
        ],
    );
    let rewrite = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);

    write_api(&task, &[message("user", "truncated")]);
    let truncation = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(truncation.work_result(), ProviderImportWorkResult::Changed);

    fs::remove_dir_all(&task).expect("remove prior task");
    write_task(
        &task,
        &[
            message("user", "replacement-user"),
            message("assistant", "replacement-assistant"),
        ],
    );
    let replacement = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replacement.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .search_event_hits("replacement-user", 10)
        .expect("replacement search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RooCode)));

    fs::remove_dir_all(&task).expect("remove task");
    let disappearance = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        disappearance.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    let retired_noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(retired_noop.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn roo_core_commit_and_pro_output_replay_are_independent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-output-task");
    write_task(
        &task,
        &[
            message("user", "core-first-user"),
            successful_output(OUTPUT_SENTINEL),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let core = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert_core_has_no_successful_output_body(&store);

    let replay_sink = Arc::new(RecordingSink::new(store_path.clone()));
    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(replay_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(replay_sink.saw_committed_core.load(Ordering::SeqCst));
    assert!(replay_sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        replay_sink.contents.lock().expect("contents").as_slice(),
        [OUTPUT_SENTINEL.as_bytes()]
    );
}

#[test]
fn roo_command_failures_publish_canonical_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-command-task");
    write_task(&task, &[message("user", "run-link-user")]);
    fs::write(
        task.join("ui_messages.json"),
        serde_json::to_vec(&json!([{
            "id": "roo-command-failure",
            "type": "command_output",
            "text": "bounded failure diagnostic",
            "exitCode": 7
        }]))
        .expect("command output fixture"),
    )
    .expect("write command output fixture");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let summary = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    let session = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::RooCode)
        .expect("Roo session");
    let runs = store.runs_for_session(session.id).expect("runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert_eq!(runs[0].exit_code, Some(7));
    let events = store.events_for_session(session.id).expect("events");
    assert!(
        events.iter().any(|event| {
            event.event_type == EventType::CommandOutput && event.run_id == Some(runs[0].id)
        }),
        "command output is not linked to the retained run: {events:?}"
    );
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
        "roo-nativepath-production-test-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().expect("progress").get(source).cloned())
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
        self.contents.lock().expect("contents").extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().expect("progress").insert(
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
            accepted_outputs: u32::try_from(page.observations.len()).expect("bounded outputs"),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    import_roo_task_json_history(
        root,
        store,
        RooTaskJsonImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
            import_profile,
            ..RooTaskJsonImportOptions::default()
        },
    )
    .expect("Roo NativePath import")
}

fn write_task(task: &Path, messages: &[Value]) {
    fs::create_dir_all(task).expect("task directory");
    fs::write(
        task.join("history_item.json"),
        json!({
            "id": "roo-output-task",
            "task": "Roo NativePath production lifecycle",
            "ts": "2026-07-25T11:00:00Z",
            "cwd": "/workspace/roo-nativepath",
            "tokensIn": 9,
            "tokensOut": 4
        })
        .to_string(),
    )
    .expect("history item");
    fs::write(
        task.join("_index.json"),
        json!({
            "id": "roo-output-task",
            "lastModified": "2026-07-25T11:01:00Z",
            "model": "roo-model"
        })
        .to_string(),
    )
    .expect("task index");
    write_api(task, messages);
}

fn write_api(task: &Path, messages: &[Value]) {
    fs::write(
        task.join("api_conversation_history.json"),
        serde_json::to_vec(messages).expect("messages"),
    )
    .expect("api history");
}

fn message(role: &str, text: &str) -> Value {
    json!({"role": role, "content": text})
}

fn successful_output(content: &str) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": "roo-call",
            "content": content,
            "status": "success"
        }]
    })
}

fn first_roo_event(store: &Store) -> uuid::Uuid {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::RooCode)
        .and_then(|session| {
            store
                .events_for_session(session.id)
                .expect("events")
                .into_iter()
                .next()
        })
        .map(|event| event.id)
        .expect("Roo event")
}

fn assert_core_has_no_successful_output_body(store: &Store) {
    let events = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::RooCode)
        .flat_map(|session| store.events_for_session(session.id).expect("events"))
        .collect::<Vec<_>>();
    let outputs = events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type,
                EventType::ToolOutput | EventType::CommandOutput
            )
        })
        .collect::<Vec<_>>();
    assert!(outputs
        .iter()
        .all(|event| event.payload.get("body").is_some_and(Value::is_null)));
    assert!(!serde_json::to_string(&events)
        .expect("events JSON")
        .contains(OUTPUT_SENTINEL));
}
