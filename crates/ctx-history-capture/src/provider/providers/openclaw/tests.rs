use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{
    import_openclaw_history, ImportProfile, OpenClawImportOptions, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult,
};

const MACHINE: &str = "openclaw-nativepath-test-machine";
const SUCCESS_BODY: &str = "OPENCLAW_SUCCESS_BODY_MUST_NOT_ENTER_CORE";
const FAILURE_BODY: &str = "OPENCLAW_FAILURE_BODY";

#[test]
fn nativepath_lifecycle_covers_restart_mutations_and_disappearance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("fresh", "user", "fresh OpenClaw prompt"),
        ],
        "fresh label",
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 1);
    let session = openclaw_session(&store);
    let routed_event = store.events_for_session(session.id).unwrap()[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    append_record(
        &transcript,
        &message("append", "assistant", "appended OpenClaw answer"),
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(append.imported_events, 1);

    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("rewrite", "user", &"rewritten OpenClaw content ".repeat(32)),
        ],
        "rewrite label",
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    write_fixture(
        &transcript,
        &[header("session-1"), message("short", "user", "short")],
        "short label",
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = transcript.with_extension("replacement");
    write_fixture(
        &replacement,
        &[
            header("session-1"),
            message("replacement", "assistant", "replacement generation"),
        ],
        "replacement label",
    );
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn nativepath_is_core_first_and_replays_outputs_independently() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-output"),
            message("prompt", "user", "run the command"),
            tool_result("success", 0, SUCCESS_BODY),
            tool_result("failure", 13, FAILURE_BODY),
        ],
        "output label",
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    let core_events = store
        .events_for_session(openclaw_session(&store).id)
        .unwrap();
    assert_eq!(core_events.len(), 2);
    assert!(core_events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput
            || event.payload.to_string().contains("failure")));
    let encoded = serde_json::to_string(&core_events).unwrap();
    assert!(!encoded.contains(SUCCESS_BODY));
    assert!(encoded.contains(FAILURE_BODY));
    let pages_after_fresh = sink.pages.load(Ordering::SeqCst);

    let noop = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

    let replay_store_path = temp.path().join("replay.sqlite");
    let mut replay_store = Store::open(&replay_store_path).unwrap();
    let replay_sink = Arc::new(RecordingSink::new(replay_store_path));
    let replay = import(
        &root,
        &mut replay_store,
        ImportProfile::ProReplayOnly(replay_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(replay_store.list_sessions().unwrap().is_empty());
    assert_eq!(replay_sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 0);

    let failing_store_path = temp.path().join("failing.sqlite");
    let mut failing_store = Store::open(&failing_store_path).unwrap();
    let failing_sink = Arc::new(FailingSink::default());
    let core_survives = import(
        &root,
        &mut failing_store,
        ImportProfile::CoreAndPro(failing_sink.clone()),
    );
    assert_eq!(
        core_survives.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(failing_store.list_sessions().unwrap().len(), 1);
    assert!(failing_sink.behind.load(Ordering::SeqCst));
}

#[test]
fn pro_replay_waits_for_openclaw_append_rewrite_and_replacement_core() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-authority"),
            message("initial", "user", "initial"),
            tool_result("initial", 0, "initial-output"),
        ],
        "initial label",
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let sink = Arc::new(RecordingSink::new(store_path));

    append_record(&transcript, &tool_result("append", 0, "append-output"));
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);

    let pages_after_append = sink.pages.load(Ordering::SeqCst);
    write_fixture(
        &transcript,
        &[
            header("session-authority"),
            message("rewrite", "user", "rewrite"),
            tool_result("rewrite", 0, "rewrite-output"),
        ],
        "rewrite label",
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_append);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert!(sink.pages.load(Ordering::SeqCst) > pages_after_append);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 3);

    let pages_after_rewrite = sink.pages.load(Ordering::SeqCst);
    let replacement = transcript.with_file_name("replacement.jsonl");
    write_fixture(
        &replacement,
        &[
            header("session-authority"),
            message("replacement", "user", "replacement"),
            tool_result("replacement", 0, "replacement-output"),
        ],
        "replacement label",
    );
    fs::remove_file(&transcript).unwrap();
    fs::rename(&replacement, &transcript).unwrap();
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_rewrite);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert!(sink.pages.load(Ordering::SeqCst) > pages_after_rewrite);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 4);
}

#[test]
fn nativepath_retries_incomplete_tail_and_reports_corrupt_records() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(&transcript, &[header("session-tail")], "tail label");
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    write!(
        file,
        "{{\"type\":\"message\",\"id\":\"tail\",\"timestamp\":\"2026-07-25T12:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"tail"
    )
    .unwrap();
    drop(file);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let incomplete = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(incomplete.failed, 0);
    assert!(store
        .events_for_session(openclaw_session(&store).id)
        .unwrap()
        .is_empty());

    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(file, " completed\"}}}}").unwrap();
    writeln!(file, "{{malformed-openclaw-record").unwrap();
    drop(file);
    let completed = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(completed.imported_events, 1);
    assert_eq!(completed.failed, 1);
    assert_eq!(
        store
            .events_for_session(openclaw_session(&store).id)
            .unwrap()
            .len(),
        1
    );
    let replay = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.failed, 1);
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_core_before_page: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "openclaw-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
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
            self.saw_core_before_page.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

#[derive(Default)]
struct FailingSink {
    behind: AtomicBool,
}

impl ProOutputSink for FailingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "openclaw-nativepath-failing-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        _page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        Err(ProOutputSinkError::new(
            "intentional_test_failure",
            "output sink failure",
        ))
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    import_openclaw_history(
        root,
        store,
        OpenClawImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:30:00Z".parse().unwrap(),
            import_profile,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap()
}

fn openclaw_session(store: &Store) -> ctx_history_core::Session {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| {
            session.provider == CaptureProvider::OpenClaw
                && session.role_hint.as_deref() != Some("relationship_placeholder")
        })
        .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("agents/personal-agent/sessions/session-1.jsonl")
}

fn header(id: &str) -> Value {
    json!({
        "type": "session",
        "id": id,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/openclaw",
    })
}

fn message(id: &str, role: &str, content: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "message": {
            "role": role,
            "content": content,
        }
    })
}

fn tool_result(id: &str, exit_code: i32, content: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:02Z",
        "message": {
            "role": "tool",
            "name": "bash",
            "tool_call_id": format!("call-{id}"),
            "exit_code": exit_code,
            "duration_ms": 17,
            "content": content,
            "input": {"command": format!("command-{id}")},
        }
    })
}

fn write_fixture(path: &Path, records: &[Value], label: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
    fs::write(
        path.parent().unwrap().join("sessions.json"),
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": label,
            },
            "session-output": {
                "sessionId": "session-output",
                "label": label,
            },
            "session-tail": {
                "sessionId": "session-tail",
                "label": label,
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}
