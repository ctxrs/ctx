use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::*;
use crate::{
    test_support_paths::tempdir, CopilotCliImportOptions, ProOutputMaterializationPage,
    ProOutputPageResult,
};

const MACHINE: &str = "copilot-nativepath-test-machine";
const SUCCESS_BODY: &str = "COPILOT_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

#[test]
fn production_lifecycle_covers_all_source_changes_and_retires_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("copilot-life"),
            message("fresh-user", "user.message", "fresh-user"),
            message("fresh-assistant", "assistant.message", "fresh-assistant"),
            tool_call("fresh-call"),
        ],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let source_before_import = fs::read(&transcript).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fs::read(&transcript).unwrap(), source_before_import);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 4);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::CopilotCli)
        .unwrap();
    let original_events = store.events_for_session(session.id).unwrap();
    assert_eq!(original_events.len(), 4);
    assert!(original_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
    let routed_event = original_events[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let previous = checkpoint(&store, &transcript);
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Unchanged
    );
    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    let restart = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

    let previous = checkpoint(&store, &transcript);
    append_record(
        &transcript,
        &message("append", "assistant.message", "append-assistant"),
    );
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Append
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(append.imported_events, 1);

    let previous = checkpoint(&store, &transcript);
    write_transcript(
        &transcript,
        &[
            header("copilot-life"),
            message(
                "fresh-user",
                "user.message",
                &"rewrite-user-content-".repeat(24),
            ),
            message(
                "fresh-assistant",
                "assistant.message",
                &"rewrite-assistant-content-".repeat(24),
            ),
        ],
    );
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Rewrite
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    let previous = checkpoint(&store, &transcript);
    write_transcript(
        &transcript,
        &[
            header("copilot-life"),
            message("fresh-user", "user.message", "short"),
        ],
    );
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Truncation
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    let previous = checkpoint(&store, &transcript);
    let replacement = transcript.with_extension("replacement");
    write_transcript(
        &replacement,
        &[
            header("copilot-life"),
            message("fresh-user", "user.message", "replacement-generation"),
        ],
    );
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Replacement
    );
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
    let repeated = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn production_retires_deleted_source_before_missing_root() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let first = transcript_path(&root);
    let second = root.join("copilot-sibling/events.jsonl");
    write_transcript(
        &first,
        &[
            header("copilot-life"),
            message("first", "user.message", "first-source"),
        ],
    );
    write_transcript(
        &second,
        &[
            header("copilot-sibling"),
            message("second", "user.message", "second-source"),
        ],
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.imported_sessions, 2);
    let routed_event = |session_id: &str| {
        let session = store
            .session_by_external_session(CaptureProvider::CopilotCli, session_id)
            .unwrap()
            .unwrap();
        store.events_for_session(session.id).unwrap()[0].id
    };
    let first_event = routed_event("copilot-life");
    let second_event = routed_event("copilot-sibling");

    fs::remove_dir_all(first.parent().unwrap()).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(first_event)
        .is_err());
    assert!(store
        .authorized_source_route_for_event(second_event)
        .is_ok());

    fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(second_event)
        .is_err());
}

#[test]
fn malformed_record_and_incomplete_tail_retry_without_losing_valid_core() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let mut bytes = serde_json::to_vec(&header("copilot-recovery")).unwrap();
    bytes.extend_from_slice(b"\n{\"broken\":\n");
    bytes.extend_from_slice(
        serde_json::to_string(&message(
            "valid-after-corruption",
            "user.message",
            "valid-after-corruption",
        ))
        .unwrap()
        .as_bytes(),
    );
    bytes.extend_from_slice(
        b"\n{\"id\":\"incomplete\",\"type\":\"assistant.message\",\"data\":{\"content\":\"later\"}",
    );
    fs::write(&transcript, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(first.failed, 1);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("malformed JSONL")));
    let first_checkpoint = checkpoint(&store, &transcript);
    assert!(!first_checkpoint.terminal);

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    file.write_all(b"}\n").unwrap();
    drop(file);
    let retry = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(retry.imported_events, 1);
    assert_eq!(retry.failed, 1);
    assert!(!checkpoint(&store, &transcript).terminal);
}

#[test]
fn released_copilot_cursor_is_reset_at_the_migration_boundary() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    write_transcript(&transcript, &[header("copilot-cursor-reset")]);
    let observation = super::super::reader::observe_file(&transcript).unwrap();
    let decoded = super::super::decode_direct_jsonl_cursor(
        "{}",
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        &transcript,
        &observation,
    )
    .unwrap();
    assert!(matches!(
        decoded,
        super::super::DirectJsonlCursorDecode::Reset
    ));
}

#[test]
fn production_is_core_first_with_independent_pro_replay() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("copilot-core-first"),
            message("core-first", "user.message", "core-first"),
            tool_call("call-with-output"),
            tool_result("result-with-output", SUCCESS_BODY),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    let core_events = store
        .events_for_session(
            store
                .list_sessions()
                .unwrap()
                .into_iter()
                .find(|session| session.provider == CaptureProvider::CopilotCli)
                .unwrap()
                .id,
        )
        .unwrap();
    assert!(core_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
    assert!(!serde_json::to_string(&core_events)
        .unwrap()
        .contains(SUCCESS_BODY));
    let pages_after_fresh = sink.pages.load(Ordering::SeqCst);

    let noop = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

    let pro_only_path = temp.path().join("pro-only.sqlite");
    let mut pro_only_store = Store::open(&pro_only_path).unwrap();
    let pro_only_sink = Arc::new(RecordingSink::new(pro_only_path));
    let replay = import(
        &root,
        &mut pro_only_store,
        ImportProfile::ProReplayOnly(pro_only_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(pro_only_store.list_sessions().unwrap().is_empty());
    assert!(!pro_only_sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(pro_only_sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(pro_only_sink.outputs.load(Ordering::SeqCst), 0);
}

#[test]
fn output_failure_never_blocks_core_and_later_replay_catches_up() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("copilot-output-retry"),
            message("core-first", "user.message", "core-survives"),
            tool_call("call-with-output"),
            tool_result("result-with-output", SUCCESS_BODY),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path));
    sink.fail_pages.store(true, Ordering::SeqCst);

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    let session = store
        .session_by_external_session(CaptureProvider::CopilotCli, "copilot-output-retry")
        .unwrap()
        .unwrap();
    let core_events = store.events_for_session(session.id).unwrap();
    assert_eq!(core_events.len(), 3);
    assert!(!serde_json::to_string(&core_events)
        .unwrap()
        .contains(SUCCESS_BODY));
    assert!(sink.progress.lock().unwrap().is_none());

    sink.fail_pages.store(false, Ordering::SeqCst);
    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert!(sink.progress.lock().unwrap().as_ref().unwrap().terminal);
}

#[test]
fn pro_replay_waits_for_append_rewrite_and_replacement_core_commits() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".copilot/session-state");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("copilot-authority"),
            message("initial", "user.message", "initial"),
            tool_call("initial-call"),
            tool_result("initial-result", "initial-output"),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let sink = Arc::new(RecordingSink::new(store_path));

    append_record(
        &transcript,
        &tool_result("appended-result", "appended-output"),
    );
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::ProReplayOnly(sink.clone()),
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
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
    write_transcript(
        &transcript,
        &[
            header("copilot-authority"),
            message("rewrite", "user.message", "rewrite"),
            tool_call("rewrite-call"),
            tool_result("rewrite-result", "rewrite-output"),
        ],
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
    let replacement = transcript.with_extension("replacement");
    write_transcript(
        &replacement,
        &[
            header("copilot-authority"),
            message("replacement", "user.message", "replacement"),
            tool_call("replacement-call"),
            tool_result("replacement-result", "replacement-output"),
        ],
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

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_core_before_page: AtomicBool,
    fail_pages: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            fail_pages: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "copilot-cli-nativepath-test-materializer-v1"
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
        if self.fail_pages.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "test_output_failure",
                "injected output failure",
            ));
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

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    crate::import_copilot_cli_session_events(
        root,
        store,
        CopilotCliImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("copilot-life/events.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "id": format!("{session_id}-start"),
        "timestamp": "2026-07-25T12:00:00Z",
        "type": "session.start",
        "data": {
            "sessionId": session_id,
            "startTime": "2026-07-25T12:00:00Z",
            "selectedModel": "gpt-5-mini",
            "context": { "cwd": "/workspace/copilot" },
        },
    })
}

fn message(id: &str, kind: &str, content: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": kind,
        "data": { "content": content },
    })
}

fn tool_call(id: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:02Z",
        "type": "tool.execution_start",
        "data": {
            "toolCallId": "call-1",
            "toolName": "read_file",
            "arguments": {"file_path": "README.md"},
        },
    })
}

fn tool_result(id: &str, result: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:03Z",
        "type": "tool.execution_complete",
        "data": {
            "toolCallId": "call-1",
            "toolName": "read_file",
            "success": true,
            "result": { "content": result },
        },
    })
}

fn write_transcript(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}

fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
    let canonical = fs::canonicalize(path).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(
        &cursor.cursor,
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
    )
    .unwrap()
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
