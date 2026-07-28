use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use super::*;
use crate::{
    test_support_paths::tempdir, CaptureWorkLimit, ProOutputMaterializationPage,
    ProOutputPageResult,
};

const MACHINE: &str = "factory-droid-nativepath-test-machine";
const SUCCESS_BODY: &str = "FACTORY_DROID_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const SENTINEL: &str = "FACTORY_DROID_SOURCE_BACKED_SENTINEL";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let transcript = transcript_path(&root);
    let source_record = message("source-backed-user", "user", SENTINEL);
    write_transcript(&transcript, &[header("droid-life"), source_record.clone()]);
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        factory_droid_source_backed_adapter(),
        &root,
        "droid-life",
        SENTINEL,
        &expected_record,
    );
}

#[test]
fn production_lifecycle_covers_replay_append_all_rewrites_and_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("droid-life"),
            message("fresh-user", "user", "fresh-user"),
            tool_call("fresh-call"),
            tool_result("fresh-result", SUCCESS_BODY),
        ],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 3);
    let session = provider_session(&store, "droid-life");
    let original_events = store.events_for_session(session.id).unwrap();
    assert_eq!(original_events.len(), 3);
    assert!(original_events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    assert!(!serde_json::to_string(&original_events)
        .unwrap()
        .contains(SUCCESS_BODY));
    let routed_event = original_events[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let previous = checkpoint(&store, &transcript);
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Unchanged
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    let previous = checkpoint(&store, &transcript);
    append_record(
        &transcript,
        &message("append", "assistant", "append-assistant"),
    );
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Append
    );
    let appended = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(appended.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(appended.imported_events, 1);

    let previous = checkpoint(&store, &transcript);
    write_transcript(
        &transcript,
        &[
            header("droid-life"),
            message("rewrite-user", "user", &"rewrite-user-content-".repeat(24)),
            message(
                "rewrite-assistant",
                "assistant",
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
        &[header("droid-life"), message("short", "user", "short")],
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
            header("droid-life"),
            message("replacement", "user", "replacement-generation"),
        ],
    );
    fs::remove_file(&transcript).unwrap();
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Replacement
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_file(&transcript).unwrap();
    let missing_source = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        missing_source.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_transcript(
        &transcript,
        &[
            header("droid-life"),
            message("root-returned", "user", "root-returned"),
        ],
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn production_is_core_first_and_pro_failure_is_independent() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("droid-core-first"),
            message("core-first", "user", "core-first"),
            tool_call("call-with-output"),
            tool_result("result-with-output", SUCCESS_BODY),
        ],
    );

    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let empty_path = temp.path().join("empty.sqlite");
    let mut empty_store = Store::open(&empty_path).unwrap();
    let empty_sink = Arc::new(RecordingSink::new(empty_path, false));
    assert_eq!(
        import(
            &root,
            &mut empty_store,
            ImportProfile::ProReplayOnly(empty_sink.clone()),
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
    );
    assert!(empty_store.list_sessions().unwrap().is_empty());
    assert_eq!(empty_sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(empty_sink.outputs.load(Ordering::SeqCst), 0);

    let sink = Arc::new(RecordingSink::new(store_path.clone(), false));
    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    let core_events = store
        .events_for_session(provider_session(&store, "droid-core-first").id)
        .unwrap();
    assert!(!serde_json::to_string(&core_events)
        .unwrap()
        .contains(SUCCESS_BODY));

    let pages_after_fresh = sink.pages.load(Ordering::SeqCst);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone())).work_result(),
        ProviderImportWorkResult::NoOp
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

    let later_path = temp.path().join("later.sqlite");
    let mut later_store = Store::open(&later_path).unwrap();
    assert_eq!(
        import(&root, &mut later_store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let later_sink = Arc::new(RecordingSink::new(later_path, false));
    let replay = import(
        &root,
        &mut later_store,
        ImportProfile::ProReplayOnly(later_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(later_sink.outputs.load(Ordering::SeqCst), 1);

    let failure_path = temp.path().join("failure.sqlite");
    let mut failure_store = Store::open(&failure_path).unwrap();
    let failing_sink = Arc::new(RecordingSink::new(failure_path, true));
    let core_survives = import(
        &root,
        &mut failure_store,
        ImportProfile::CoreAndPro(failing_sink.clone()),
    );
    assert_eq!(
        core_survives.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(!failure_store.list_sessions().unwrap().is_empty());
    assert!(failing_sink.behind.load(Ordering::SeqCst) > 0);
}

#[test]
fn relationships_corruption_incomplete_tail_and_result_privacy_are_exact() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let parent = root.join("project/a-parent.jsonl");
    let child = root.join("project/b-child.jsonl");
    write_transcript(
        &parent,
        &[
            header("droid-parent"),
            message("parent-user", "user", "parent"),
        ],
    );
    write_transcript(
        &child,
        &[
            child_header("droid-child", "droid-parent"),
            message("child-user", "user", "child"),
        ],
    );
    let mut bytes = fs::read(&child).unwrap();
    bytes.extend_from_slice(b"{malformed-json}\n");
    let incomplete = serde_json::to_vec(&message(
        "incomplete",
        "assistant",
        "complete-only-after-newline",
    ))
    .unwrap();
    bytes.extend_from_slice(&incomplete);
    fs::write(&child, bytes).unwrap();

    let mut store = Store::open(temp.path().join("relationships.sqlite")).unwrap();
    let first = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    let parent_session = provider_session(&store, "droid-parent");
    let child_session = provider_session(&store, "droid-child");
    assert_eq!(child_session.parent_session_id, Some(parent_session.id));
    assert!(store
        .events_for_session(child_session.id)
        .unwrap()
        .iter()
        .all(|event| {
            !serde_json::to_string(event)
                .unwrap()
                .contains("complete-only-after-newline")
        }));

    append_raw(&child, b"\n");
    let resumed = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(resumed.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .events_for_session(child_session.id)
        .unwrap()
        .iter()
        .any(|event| {
            serde_json::to_string(event)
                .unwrap()
                .contains("complete-only-after-newline")
        }));

    let redacted = json!({
        "type": "message",
        "redacted": true,
        "message": {
            "role": "tool",
            "content": [{"type": "tool_result", "content": "secret"}]
        }
    });
    let results = enumerate_factory_droid_results(&redacted).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.is_none());
    assert!(results[0].call_id.is_none());
}

#[test]
fn stable_native_id_rewrite_updates_payload_and_invalid_shape_keeps_rejection_authority() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".factory/sessions");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("droid-stable-rewrite"),
            message("stable-message", "user", "old-stable-payload"),
        ],
    );
    let mut store = Store::open(temp.path().join("stable.sqlite")).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).imported_events,
        2
    );
    let session = provider_session(&store, "droid-stable-rewrite");
    let original = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .find(|event| {
            serde_json::to_string(event)
                .unwrap()
                .contains("old-stable-payload")
        })
        .unwrap();

    write_transcript(
        &transcript,
        &[
            header("droid-stable-rewrite"),
            message("inserted-message", "assistant", "inserted-before-stable"),
            message("stable-message", "user", "new-stable-payload"),
        ],
    );
    let rewritten = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rewritten.work_result(), ProviderImportWorkResult::Changed);
    let stable = store.get_event(original.id).unwrap();
    let rendered = serde_json::to_string(&stable).unwrap();
    assert!(rendered.contains("new-stable-payload"));
    assert!(!rendered.contains("old-stable-payload"));
    assert_eq!(
        stable
            .sync
            .metadata
            .get("provider_event_hash_authority")
            .and_then(Value::as_str),
        Some("normalized_payload_fallback")
    );

    let invalid = json!({
        "type": "message",
        "id": "invalid-result",
        "timestamp": "2026-07-25T12:00:02Z",
        "message": {
            "role": "tool",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-invalid",
                "content": {"unexpected": "object"}
            }]
        }
    });
    write_transcript(
        &transcript,
        &[
            header("droid-stable-rewrite"),
            invalid,
            message(
                "valid-after-invalid",
                "assistant",
                "valid-after-invalid-shape",
            ),
        ],
    );
    let rejected = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rejected.failed, 1);
    assert!(rejected.failures[0].error.contains("invalid shape"));
    assert!(
        serde_json::to_string(&store.events_for_session(session.id).unwrap())
            .unwrap()
            .contains("valid-after-invalid-shape")
    );
    let rejected_checkpoint = checkpoint(&store, &transcript);
    assert!(!rejected_checkpoint.terminal);
    assert_eq!(rejected_checkpoint.next_raw_ordinal, 1);
    let repeated = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(repeated.failed, 1);
    assert_ne!(repeated.work_result(), ProviderImportWorkResult::NoOp);
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    behind: AtomicUsize,
    saw_core_before_page: AtomicBool,
    fail_pages: bool,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail_pages: bool) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            fail_pages,
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "factory-droid-nativepath-test-materializer-v1"
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
        if self.fail_pages {
            return Err(ProOutputSinkError::new(
                "factory_droid_test_failure",
                "injected output materialization failure",
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

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    import_factory_ai_droid_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: root,
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            history_record_id: None,
            capture_work_limit: CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            import_profile,
        },
    )
    .unwrap()
}

fn provider_session(store: &Store, provider_session_id: &str) -> ctx_history_core::Session {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| {
            session.provider == CaptureProvider::FactoryAiDroid
                && session.external_session_id.as_deref() == Some(provider_session_id)
        })
        .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("project/droid-life.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "type": "session_start",
        "id": session_id,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/factory",
        "model": "factory/droid",
    })
}

fn child_header(session_id: &str, parent: &str) -> Value {
    json!({
        "type": "session_start",
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/factory",
        "model": "factory/droid",
        "callingSessionId": parent,
        "decompSessionType": "worker",
        "decompMissionId": "mission-1",
    })
}

fn message(id: &str, role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "message": {
            "role": role,
            "content": [{"type": "text", "text": text}],
        },
        "model": "factory/droid",
    })
}

fn tool_call(id: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:02Z",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call-1",
                "name": "read_file",
                "input": {"file_path": "README.md"},
            }],
        },
        "model": "factory/droid",
    })
}

fn tool_result(id: &str, result: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-25T12:00:03Z",
        "message": {
            "role": "tool",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "name": "read_file",
                "content": result,
                "is_error": false,
            }],
        },
        "model": "factory/droid",
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

fn append_raw(path: &Path, bytes: &[u8]) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
    let canonical = fs::canonicalize(path).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(
        &cursor.cursor,
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
    )
    .unwrap()
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
