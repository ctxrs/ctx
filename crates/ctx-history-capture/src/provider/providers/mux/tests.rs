use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::json;

use crate::test_support_paths::tempdir;
use crate::{
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportWorkResult,
};

use super::import_mux_native_path;

struct TestSink {
    fail: bool,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    pages: AtomicUsize,
    behind: AtomicBool,
}

impl TestSink {
    fn recording() -> Self {
        Self {
            fail: false,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
            pages: AtomicUsize::new(0),
            behind: AtomicBool::new(false),
        }
    }

    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::recording()
        }
    }
}

impl ProOutputSink for TestSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "test"
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
        if self.fail {
            return Err(ProOutputSinkError::new("test_failure", "injected"));
        }
        let mut progress = self.progress.lock().unwrap();
        let prior = progress.get(&page.source);
        assert_eq!(
            page.expected_prior_source_epoch,
            prior.map(|prior| prior.source_epoch)
        );
        assert_eq!(
            page.expected_prior_cursor.as_ref(),
            prior.and_then(|prior| prior.cursor.as_ref())
        );
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        self.pages.fetch_add(1, Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        progress.insert(
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

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "mux-nativepath-test".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    }
}

fn options() -> ProviderImportOptions {
    ProviderImportOptions {
        ..ProviderImportOptions::default()
    }
}

fn write_session(root: &Path, messages: usize) -> std::path::PathBuf {
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspaceId": "session-1",
            "createdAt": "2026-07-25T11:00:00Z",
            "projectPath": "/work/mux",
            "model": "mux-test",
        }))
        .unwrap(),
    )
    .unwrap();
    let mut chat = String::new();
    for index in 0..messages {
        chat.push_str(
            &serde_json::to_string(&json!({
                "id": format!("message-{index}"),
                "workspaceId": "session-1",
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "createdAt": "2026-07-25T11:01:00Z",
                "parts": [{"type": "text", "text": format!("message {index}")}],
                "metadata": {"historySequence": index},
            }))
            .unwrap(),
        );
        chat.push('\n');
    }
    fs::write(session.join("chat.jsonl"), chat).unwrap();
    session
}

fn append_success_output(session: &Path, id: &str, secret: &str, sequence: usize) {
    let output = json!({
        "id": id,
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:03:00Z",
        "parts": [{
            "type": "dynamic-tool",
            "toolCallId": format!("call-{sequence}"),
            "toolName": "shell",
            "state": "output-available",
            "success": true,
            "output": secret,
        }],
        "metadata": {"historySequence": sequence},
    });
    let mut chat = OpenOptions::new()
        .append(true)
        .open(session.join("chat.jsonl"))
        .unwrap();
    writeln!(chat, "{}", serde_json::to_string(&output).unwrap()).unwrap();
    chat.sync_all().unwrap();
}

#[test]
fn nativepath_fresh_replay_and_append_are_idempotent() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 65);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(first.imported_events, 65);
    assert_eq!(first.failed, 0);

    let replay = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.imported_events, 0);

    let mut chat = OpenOptions::new()
        .append(true)
        .open(session.join("chat.jsonl"))
        .unwrap();
    writeln!(
        chat,
        "{}",
        serde_json::to_string(&json!({
            "id": "message-65",
            "workspaceId": "session-1",
            "role": "assistant",
            "createdAt": "2026-07-25T11:02:00Z",
            "parts": [{"type": "text", "text": "append"}],
            "metadata": {"historySequence": 65},
        }))
        .unwrap()
    )
    .unwrap();
    chat.sync_all().unwrap();

    let appended = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(appended.imported_events, 1);
    let canonical = store
        .session_by_external_session(CaptureProvider::Mux, "session-1")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(canonical.id).unwrap().len(), 66);
}

#[test]
fn successful_output_body_never_enters_core() {
    const SECRET: &str = "mux-success-output-secret";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 1);
    append_success_output(&session, "tool-output", SECRET, 1);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();

    let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!archive.contains(SECRET));
}

#[test]
fn pro_replay_only_requires_exact_committed_core() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(&root, 1);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut replay_options = options();
    replay_options.import_profile = ImportProfile::ProReplayOnly(Arc::new(TestSink::recording()));

    let error =
        import_mux_native_path(&root, &mut store, context(&root), replay_options).unwrap_err();

    assert!(error
        .to_string()
        .contains("requires committed NativePath Core"));
}

#[test]
fn later_pro_activation_replays_outputs_independently() {
    const SECRET: &str = "mux-later-pro-output-secret";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 10);
    append_success_output(&session, "tool-output", SECRET, 10);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    let sink = Arc::new(TestSink::recording());
    let mut replay_options = options();
    replay_options.import_profile = ImportProfile::ProReplayOnly(sink.clone());
    let replay = import_mux_native_path(&root, &mut store, context(&root), replay_options).unwrap();

    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.pages.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [SECRET.as_bytes()]
    );
    let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!archive.contains(SECRET));
}

#[test]
fn pro_failure_never_blocks_or_rolls_back_core() {
    const SECRET: &str = "mux-failed-pro-output-secret";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 1);
    append_success_output(&session, "tool-output", SECRET, 1);
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(TestSink::failing());
    let mut import_options = options();
    import_options.import_profile = ImportProfile::CoreAndPro(sink.clone());

    let summary =
        import_mux_native_path(&root, &mut store, context(&root), import_options).unwrap();

    assert_eq!(summary.imported_events, 1);
    assert!(summary.failed >= 1);
    assert!(sink.behind.load(Ordering::SeqCst));
    drop(store);
    let committed = Store::open_read_only(store_path).unwrap();
    let canonical = committed
        .session_by_external_session(CaptureProvider::Mux, "session-1")
        .unwrap()
        .unwrap();
    assert_eq!(committed.events_for_session(canonical.id).unwrap().len(), 1);
}

#[test]
fn incomplete_tail_resumes_and_root_disappearance_retires_routes() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 1);
    let chat_path = session.join("chat.jsonl");
    let mut chat = OpenOptions::new().append(true).open(&chat_path).unwrap();
    write!(
        chat,
        "{{\"id\":\"message-1\",\"workspaceId\":\"session-1\",\"role\":\"assistant\""
    )
    .unwrap();
    chat.sync_all().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let incomplete = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(incomplete.imported_events, 1);
    assert_eq!(incomplete.failed, 0);
    assert!(incomplete.work_remaining);

    let mut chat = OpenOptions::new().append(true).open(&chat_path).unwrap();
    writeln!(
        chat,
        ",\"createdAt\":\"2026-07-25T11:04:00Z\",\"parts\":[{{\"type\":\"text\",\"text\":\"complete\"}}],\"metadata\":{{\"historySequence\":1}}}}"
    )
    .unwrap();
    chat.sync_all().unwrap();
    let resumed = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(resumed.imported_events, 1);
    assert_eq!(resumed.failed, 0);

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    let replay = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn replacement_and_truncation_reset_source_authority() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 3);
    let chat_path = session.join("chat.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();

    let mut replacement = fs::read_to_string(&chat_path).unwrap();
    replacement.push_str(
        &serde_json::to_string(&json!({
            "id": "message-3",
            "workspaceId": "session-1",
            "role": "assistant",
            "createdAt": "2026-07-25T11:05:00Z",
            "parts": [{"type": "text", "text": "replacement append"}],
            "metadata": {"historySequence": 3},
        }))
        .unwrap(),
    );
    replacement.push('\n');
    let replacement_path = session.join("chat.replacement");
    fs::write(&replacement_path, replacement).unwrap();
    fs::rename(&replacement_path, &chat_path).unwrap();
    let replaced = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(replaced.imported_events, 4);

    let truncated = format!(
        "{}\n",
        serde_json::to_string(&json!({
            "id": "message-after-truncation",
            "workspaceId": "session-1",
            "role": "user",
            "createdAt": "2026-07-25T11:06:00Z",
            "parts": [{"type": "text", "text": "rewritten"}],
            "metadata": {"historySequence": 0},
        }))
        .unwrap()
    );
    fs::write(&chat_path, truncated).unwrap();
    let rewritten = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(rewritten.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(rewritten.failed, 0);

    let replay = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}
