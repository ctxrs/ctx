use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::*;
use crate::{
    test_support_paths::tempdir, ProOutputMaterializationPage, ProOutputPageResult,
    TabnineCliImportOptions,
};

const MACHINE: &str = "tabnine-nativepath-test-machine";
const SUCCESS_BODY: &str = "TABNINE_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

#[test]
fn production_lifecycle_covers_all_source_changes_and_retires_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("tabnine-life"),
            message("fresh-user", "user", "fresh-user"),
            message("fresh-assistant", "tabnine", "fresh-assistant"),
            tool_call("fresh-call"),
        ],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 4);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Tabnine)
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
        &message("append", "tabnine", "append-assistant"),
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
            header("tabnine-life"),
            message("rewrite-user", "user", &"rewrite-user-content-".repeat(24)),
            message(
                "rewrite-assistant",
                "tabnine",
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
        &[header("tabnine-life"), message("short", "user", "short")],
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
            header("tabnine-life"),
            message("replacement", "user", "replacement-generation"),
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
fn production_is_core_first_with_independent_pro_replay() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("tabnine-core-first"),
            message("core-first", "user", "core-first"),
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
                .find(|session| session.provider == CaptureProvider::Tabnine)
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
fn released_gemini_result_reaches_core_outcome_and_exact_pro_handoff() {
    const LEGACY_SUCCESS: &str = "TABNINE_LEGACY_GEMINI_SUCCESS";
    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("tabnine-legacy-result"),
            message("legacy-user", "user", "legacy-user"),
            tool_call("legacy-call"),
            tool_result_with_type("legacy-success", "gemini", LEGACY_SUCCESS, Some(0)),
            tool_result_with_type("legacy-failure", "gemini", "legacy failure", Some(9)),
        ],
    );
    let store_path = temp.path().join("legacy.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path));

    let summary = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        &[LEGACY_SUCCESS.as_bytes().to_vec()]
    );
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Tabnine)
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == EventType::ToolOutput)
            .count(),
        1
    );
}

#[test]
fn frozen_v025_source_scoped_store_upgrades_without_duplicate_events() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("tabnine-v025-upgrade"),
            message("v025-user", "user", "v025 user"),
            message("v025-assistant", "gemini", "v025 assistant"),
        ],
    );

    // Freeze the released source-scoped row shape independently of the
    // current NativePath cursor and event identity implementations.
    let oracle_path = temp.path().join("oracle.sqlite");
    let mut oracle = Store::open(&oracle_path).unwrap();
    import(&root, &mut oracle, ImportProfile::CoreOnly);
    let source = oracle
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.descriptor.provider == CaptureProvider::Tabnine)
        .unwrap();
    let session = oracle
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("tabnine-v025-upgrade"))
        .unwrap();
    let current_events = oracle.events_for_session(session.id).unwrap();
    assert_eq!(current_events.len(), 3);

    let mut frozen = Store::open(temp.path().join("frozen-v025.sqlite")).unwrap();
    frozen.upsert_capture_source(&source).unwrap();
    frozen.upsert_session(&session).unwrap();
    let mut released_ids = BTreeSet::new();
    for mut event in current_events {
        let raw_ordinal = event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap();
        let event_hash = event.sync.metadata["provider_event_hash"].as_str().unwrap();
        let released = crate::provider::importer::provider_source_event_import_identity(
            source.id,
            raw_ordinal,
            event_hash,
        );
        event.id = released.id;
        event.seq = released.seq;
        event.dedupe_key = Some(released.dedupe_key);
        if let Some(metadata) = event.sync.metadata.as_object_mut() {
            metadata.insert(
                "provider_event_index".to_owned(),
                Value::Number(raw_ordinal.into()),
            );
            metadata.remove("source_record_ordinal");
            metadata.remove("source_record_subrecord_index");
        }
        frozen.upsert_event(&event).unwrap();
        released_ids.insert(event.id);
    }

    let upgraded = import(&root, &mut frozen, ImportProfile::CoreOnly);
    assert_eq!(upgraded.failed, 0, "{:?}", upgraded.failures);
    let upgraded_events = frozen.events_for_session(session.id).unwrap();
    assert_eq!(upgraded_events.len(), released_ids.len());
    assert_eq!(
        upgraded_events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>(),
        released_ids
    );
}

#[cfg(unix)]
#[test]
fn selected_source_failure_does_not_discard_healthy_core_sibling() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let healthy = root.join("tmp/project/chats/a-healthy.jsonl");
    let rejected = root.join("tmp/project/chats/b-rejected.jsonl");
    write_transcript(
        &healthy,
        &[
            header("tabnine-healthy-core"),
            message("healthy-user", "user", "healthy-core"),
        ],
    );
    symlink(&healthy, &rejected).unwrap();
    let mut store = Store::open(temp.path().join("core-isolation.sqlite")).unwrap();

    let summary = import(&root, &mut store, ImportProfile::CoreOnly);

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0].error.contains("b-rejected.jsonl"));
    assert_eq!(summary.imported_sessions, 1);
    assert!(store
        .list_sessions()
        .unwrap()
        .iter()
        .any(|session| session.external_session_id.as_deref() == Some("tabnine-healthy-core")));
}

#[cfg(unix)]
#[test]
fn selected_source_failure_marks_pro_behind_and_replays_healthy_sibling() {
    const HEALTHY_OUTPUT: &str = "TABNINE_HEALTHY_PRO_OUTPUT";
    let temp = tempdir().unwrap();
    let root = temp.path().join(".tabnine/agent");
    let healthy = root.join("tmp/project/chats/a-healthy.jsonl");
    let rejected = root.join("tmp/project/chats/b-rejected.jsonl");
    write_transcript(
        &healthy,
        &[
            header("tabnine-healthy-pro"),
            message("healthy-user", "user", "healthy-pro"),
            tool_call("healthy-call"),
            tool_result_with_type("healthy-result", "tabnine", HEALTHY_OUTPUT, Some(0)),
        ],
    );
    symlink(&healthy, &rejected).unwrap();
    let store_path = temp.path().join("pro-isolation.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path));

    let summary = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        &[HEALTHY_OUTPUT.as_bytes().to_vec()]
    );
}

#[test]
fn only_selected_file_access_errors_are_contained() {
    assert!(selected_file_source_error(&CaptureError::Io(
        io::Error::new(io::ErrorKind::PermissionDenied, "selected source")
    )));
    assert!(!selected_file_source_error(&CaptureError::Store(
        ctx_history_store::StoreError::BulkSearchImportBusy
    )));
    assert!(!selected_file_source_error(&CaptureError::SystemInvariant(
        "test systemic failure"
    )));
    assert!(!selected_file_source_error(&CaptureError::SystemIo {
        operation: "test",
        source: io::Error::other("system I/O"),
    }));
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    behind: AtomicUsize,
    contents: Mutex<Vec<Vec<u8>>>,
    saw_core_before_page: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            contents: Mutex::new(Vec::new()),
            saw_core_before_page: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "tabnine-nativepath-test-materializer-v1"
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
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
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
    crate::import_tabnine_cli_history(
        root,
        store,
        TabnineCliImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            ..TabnineCliImportOptions::default()
        },
    )
    .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("tmp/project/chats/session-tabnine-life.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "sessionId": session_id,
        "projectHash": "tabnine-nativepath-project",
        "startTime": "2026-07-25T12:00:00Z",
        "lastUpdated": "2026-07-25T12:00:59Z",
        "kind": "main",
        "directories": ["/workspace/tabnine"],
    })
}

fn message(id: &str, kind: &str, content: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": kind,
        "content": content,
        "model": "tabnine-agent",
    })
}

fn tool_call(id: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:02Z",
        "type": "tabnine",
        "toolCalls": [{
            "id": "call-1",
            "name": "read_file",
            "args": {"file_path": "README.md"},
        }],
        "model": "tabnine-agent",
    })
}

fn tool_result(id: &str, result: &str) -> Value {
    tool_result_with_type(id, "tabnine", result, None)
}

fn tool_result_with_type(
    id: &str,
    record_type: &str,
    result: &str,
    exit_code: Option<i32>,
) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:03Z",
        "type": record_type,
        "toolCalls": [{
            "id": "call-1",
            "name": "read_file",
            "result": result,
            "exitCode": exit_code,
        }],
        "model": "tabnine-agent",
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
        CaptureProvider::Tabnine,
        TABNINE_CLI_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(
        &cursor.cursor,
        CaptureProvider::Tabnine,
        TABNINE_CLI_SOURCE_FORMAT,
    )
    .unwrap()
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::Tabnine,
        TABNINE_CLI_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
