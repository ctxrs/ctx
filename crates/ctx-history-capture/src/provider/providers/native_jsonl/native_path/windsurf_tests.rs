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
    test_support_paths::tempdir, ProOutputMaterializationPage, ProOutputPageResult,
    WindsurfCascadeHookImportOptions,
};

const MACHINE: &str = "windsurf-nativepath-test-machine";

#[test]
fn source_backed_cold_projection_and_exact_locator() {
    const SENTINEL: &str = "WINDSURF_SOURCE_BACKED_SENTINEL";

    let temp = tempdir().unwrap();
    let root = temp.path().join("transcripts");
    let transcript = transcript_path(&root);
    let source_record = user_input(0, SENTINEL);
    write_transcript(&transcript, std::slice::from_ref(&source_record));
    let mut expected_record = serde_json::to_vec(&source_record).unwrap();
    expected_record.push(b'\n');

    super::super::source_backed::assert_source_backed_fixture(
        windsurf_source_backed_adapter(),
        &root,
        "windsurf-hook-trajectory",
        SENTINEL,
        &expected_record,
    );
}

#[test]
fn production_lifecycle_covers_all_source_changes_and_retires_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("transcripts");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            user_input(0, "fresh-user"),
            planner_response(1, "fresh-assistant"),
            code_action(
                2,
                "README.md",
                "fresh-successful-output-body-must-not-enter-core",
            ),
        ],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 3);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Windsurf)
        .unwrap();
    let original_events = store.events_for_session(session.id).unwrap();
    assert_eq!(original_events.len(), 3);
    assert!(original_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
    assert!(!original_events.iter().any(|event| event
        .payload
        .to_string()
        .contains("fresh-successful-output-body")));
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
    append_record(&transcript, &planner_response(3, "append"));
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
            user_input(0, &"rewrite-user-content-".repeat(24)),
            planner_response(1, &"rewrite-assistant-content-".repeat(24)),
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
    write_transcript(&transcript, &[user_input(0, "short")]);
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
    write_transcript(&replacement, &[user_input(0, "replacement-generation")]);
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
    let root = temp.path().join("transcripts");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            user_input(0, "core-first"),
            code_action(
                1,
                "src/generated.rs",
                "successful-output-body-must-not-enter-core",
            ),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Windsurf)
        .unwrap();
    let core_events = store.events_for_session(session.id).unwrap();
    assert!(core_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(!core_events
        .iter()
        .any(|event| event.payload.to_string().contains("successful-output-body")));
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
        "windsurf-nativepath-test-materializer-v1"
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

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    crate::import_windsurf_cascade_hook_transcripts(
        root,
        store,
        WindsurfCascadeHookImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("windsurf-hook-trajectory.jsonl")
}

fn user_input(step: u64, content: &str) -> Value {
    json!({
        "status": "done",
        "type": "user_input",
        "timestamp": format!("2026-07-25T12:00:{step:02}Z"),
        "user_input": {"user_response": content},
    })
}

fn planner_response(step: u64, content: &str) -> Value {
    json!({
        "planner_response": {"response": content},
        "status": "done",
        "timestamp": format!("2026-07-25T12:00:{step:02}Z"),
        "type": "planner_response",
    })
}

fn code_action(step: u64, path: &str, successful_output_body: &str) -> Value {
    json!({
        "code_action": {
            "new_content": successful_output_body,
            "path": path,
        },
        "status": "done",
        "timestamp": format!("2026-07-25T12:00:{step:02}Z"),
        "type": "code_action",
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
        CaptureProvider::Windsurf,
        WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(
        &cursor.cursor,
        CaptureProvider::Windsurf,
        WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
    )
    .unwrap()
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::Windsurf,
        WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
