use std::{
    collections::HashMap,
    fs, io,
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
    import_roo_task_json_history, CaptureError, ImportProfile, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult, RooTaskJsonImportOptions,
};

use super::{clear_cline_io_failure, inject_cline_io_failure, ClineInjectedIoOperation};

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
fn roo_output_failure_never_fails_core_and_later_replay_catches_up() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-output-failure-task");
    write_task(
        &task,
        &[
            message("user", "core-survives-output-failure"),
            successful_output(OUTPUT_SENTINEL),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let failing_sink = Arc::new(RecordingSink::failing(store_path.clone()));

    let core = import(
        &root,
        &mut store,
        ImportProfile::CoreAndPro(failing_sink.clone()),
    );
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert!(failing_sink.saw_committed_core.load(Ordering::SeqCst));
    assert!(failing_sink.behind.load(Ordering::SeqCst));
    assert_core_has_no_successful_output_body(&store);

    let replay_sink = Arc::new(RecordingSink::new(store_path));
    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(replay_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        replay_sink.contents.lock().expect("contents").as_slice(),
        [OUTPUT_SENTINEL.as_bytes()]
    );
}

#[test]
fn roo_observe_failure_happens_after_core_and_only_marks_pro_behind() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-observe-failure-task");
    write_task(
        &task,
        &[
            identified_message("observe-core", "user", "core-before-observe"),
            successful_output(OUTPUT_SENTINEL),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let sink = Arc::new(RecordingSink::failing_observe(store_path));

    let summary = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));
    assert!(sink.behind.load(Ordering::SeqCst));
    assert!(store
        .search_event_hits("core-before-observe", 10)
        .expect("Core search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RooCode)));
}

#[test]
fn roo_native_ids_survive_reorder_rewrite_and_relocation_without_stale_search() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let old_task = root.join("tasks").join("roo-stable-old");
    write_task(
        &old_task,
        &[
            identified_tool_touch("touch-native"),
            identified_message("stable-native", "user", "stale-search-old"),
        ],
    );
    fs::write(
        old_task.join("ui_messages.json"),
        serde_json::to_vec(&json!([{
            "id": "command-native",
            "type": "command_output",
            "text": "old-command-diagnostic",
            "exitCode": 9
        }]))
        .expect("command fixture"),
    )
    .expect("write command fixture");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    import(&root, &mut store, ImportProfile::CoreOnly);
    let baseline_session = roo_session(&store);
    let baseline_source = baseline_session.capture_source_id.expect("capture source");
    let stable_event = event_with_body(&store, baseline_session.id, "stale-search-old");
    let baseline_run = store
        .runs_for_session(baseline_session.id)
        .expect("runs")
        .into_iter()
        .next()
        .expect("command run");
    let baseline_touches = file_touch_ids(&store);
    assert_eq!(baseline_touches.len(), 1);

    write_api(
        &old_task,
        &[
            identified_message("inserted-native", "assistant", "inserted-before"),
            identified_message("stable-native", "user", "stale-search-new"),
            identified_tool_touch("touch-native"),
        ],
    );
    fs::write(
        old_task.join("ui_messages.json"),
        serde_json::to_vec(&json!([{
            "id": "command-native",
            "type": "command_output",
            "text": "new-command-diagnostic",
            "exitCode": 9
        }]))
        .expect("command rewrite fixture"),
    )
    .expect("write command rewrite fixture");
    let rewrite = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    let rewritten_session = roo_session(&store);
    assert_eq!(rewritten_session.id, baseline_session.id);
    assert_eq!(rewritten_session.capture_source_id, Some(baseline_source));
    assert_eq!(
        event_with_body(&store, rewritten_session.id, "stale-search-new").id,
        stable_event.id
    );
    assert!(store
        .search_event_hits("stale-search-old", 10)
        .expect("old search")
        .is_empty());
    assert!(store
        .search_event_hits("stale-search-new", 10)
        .expect("new search")
        .iter()
        .any(|hit| hit.event_id == stable_event.id));
    assert_eq!(
        store
            .runs_for_session(rewritten_session.id)
            .expect("rewritten runs")[0]
            .id,
        baseline_run.id
    );
    assert_eq!(file_touch_ids(&store)[0], baseline_touches[0]);

    let before_relocation_events = store
        .events_for_session(rewritten_session.id)
        .expect("events before relocation")
        .into_iter()
        .map(|event| event.id)
        .collect::<std::collections::BTreeSet<_>>();
    let new_task = root.join("tasks").join("roo-stable-new");
    fs::rename(&old_task, &new_task).expect("relocate task");
    let relocation = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(relocation.work_result(), ProviderImportWorkResult::Changed);
    let relocated_session = roo_session(&store);
    assert_eq!(relocated_session.id, baseline_session.id);
    assert_eq!(relocated_session.capture_source_id, Some(baseline_source));
    assert_eq!(
        store
            .events_for_session(relocated_session.id)
            .expect("events after relocation")
            .into_iter()
            .map(|event| event.id)
            .collect::<std::collections::BTreeSet<_>>(),
        before_relocation_events
    );
    assert!(store
        .authorized_source_route_for_event(stable_event.id)
        .is_ok());
}

#[test]
fn roo_released_v025_event_migrates_exact_hash_and_ordinal_in_place() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-v025-task");
    write_task(
        &task,
        &[identified_message(
            "released-native",
            "user",
            "released-stale-body",
        )],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    import(&root, &mut store, ImportProfile::CoreOnly);
    let session = roo_session(&store);
    let source_id = session.capture_source_id.expect("capture source");
    let current = event_with_body(&store, session.id, "released-stale-body");
    let provider_session_id = session
        .external_session_id
        .as_deref()
        .expect("provider session")
        .to_owned();
    drop(store);

    let connection = rusqlite::Connection::open(&store_path).expect("open Store database");
    connection
        .execute("DELETE FROM events WHERE id = ?1", [current.id.to_string()])
        .expect("remove post-v0.25 event");
    drop(connection);

    let mut store = Store::open(&store_path).expect("reopen store");
    let legacy_hash = "roo-output-task:api_conversation_history:released-native".to_owned();
    let legacy_identity =
        crate::provider::importer::provider_event_import_identity_with_exact_legacy_source(
            &store,
            CaptureProvider::RooCode,
            &provider_session_id,
            source_id,
            0,
            0,
            &legacy_hash,
            None,
            Some(0),
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::RooCode,
                    &provider_session_id,
                ),
        )
        .expect("released event identity");
    let mut legacy = current;
    legacy.id = legacy_identity.id;
    legacy.seq = legacy_identity.seq;
    legacy.payload["provider_event_index"] = json!(0);
    legacy.payload["provider_event_hash"] = json!(legacy_hash);
    legacy.sync.metadata["provider_event_index"] = json!(0);
    legacy.sync.metadata["provider_event_hash"] = json!(legacy_hash);
    legacy.sync.metadata["provider_event_hash_authority"] = json!("provider_supplied");
    legacy.dedupe_key = Some(
        Store::provider_event_dedupe_key_with_payload_hash(
            &legacy_identity.dedupe_key,
            &legacy_hash,
        )
        .unwrap_or(legacy_identity.dedupe_key),
    );
    store
        .upsert_event(&legacy)
        .expect("seed released v0.25 event");

    write_api(
        &task,
        &[identified_message(
            "released-native",
            "user",
            "released-current-body",
        )],
    );
    let migrated = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(migrated.work_result(), ProviderImportWorkResult::Changed);
    let current = event_with_body(&store, session.id, "released-current-body");
    assert_eq!(current.id, legacy.id);
    assert_eq!(
        current.sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
    assert!(store
        .search_event_hits("released-stale-body", 10)
        .expect("stale search")
        .is_empty());
    assert!(store
        .search_event_hits("released-current-body", 10)
        .expect("current search")
        .iter()
        .any(|hit| hit.event_id == legacy.id));
}

#[test]
fn roo_degraded_metadata_promotes_in_place_with_certified_aliases() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("degraded-directory-id");
    fs::create_dir_all(&task).expect("task directory");
    write_api(
        &task,
        &[identified_message(
            "promotion-native",
            "user",
            "promotion-body",
        )],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    import(&root, &mut store, ImportProfile::CoreOnly);
    let degraded_session = roo_session(&store);
    let degraded_source = degraded_session.capture_source_id.expect("capture source");
    let degraded_event = event_with_body(&store, degraded_session.id, "promotion-body");
    assert_eq!(
        degraded_session.external_session_id.as_deref(),
        Some("degraded-directory-id")
    );

    fs::write(
        task.join("history_item.json"),
        json!({
            "id": "authoritative-task-id",
            "task": "authoritative promotion",
            "ts": "2026-07-25T11:00:00Z"
        })
        .to_string(),
    )
    .expect("history metadata");
    let promoted = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(promoted.work_result(), ProviderImportWorkResult::Changed);
    let authoritative = roo_session(&store);
    assert_eq!(authoritative.id, degraded_session.id);
    assert_eq!(authoritative.capture_source_id, Some(degraded_source));
    assert_eq!(
        authoritative.external_session_id.as_deref(),
        Some("authoritative-task-id")
    );
    assert_eq!(
        event_with_body(&store, authoritative.id, "promotion-body").id,
        degraded_event.id
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn roo_resource_oom_remains_a_typed_system_io_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-storage");
    let task = root.join("tasks").join("roo-oom-task");
    write_task(
        &task,
        &[identified_message("oom-native", "user", "oom-body")],
    );
    let api = task.join("api_conversation_history.json");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    inject_cline_io_failure(
        ClineInjectedIoOperation::ComponentOpen,
        api,
        io::Error::new(io::ErrorKind::OutOfMemory, "resource exhausted"),
        1,
    );
    let error = import_roo_task_json_history(
        &root,
        &mut store,
        RooTaskJsonImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.clone()),
            imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
            import_profile: ImportProfile::CoreOnly,
            ..RooTaskJsonImportOptions::default()
        },
    )
    .expect_err("OOM must abort the source");
    clear_cline_io_failure();
    assert!(matches!(
        error,
        CaptureError::SystemIo { source, .. } if source.kind() == io::ErrorKind::OutOfMemory
    ));
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
    fail_pages: bool,
    fail_observe: bool,
    behind: AtomicBool,
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
            fail_pages: false,
            fail_observe: false,
            behind: AtomicBool::new(false),
        }
    }

    fn failing(store_path: PathBuf) -> Self {
        Self {
            fail_pages: true,
            ..Self::new(store_path)
        }
    }

    fn failing_observe(store_path: PathBuf) -> Self {
        Self {
            fail_observe: true,
            ..Self::new(store_path)
        }
    }

    fn record_committed_core(&self) -> std::result::Result<(), ProOutputSinkError> {
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        Ok(())
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
        self.record_committed_core()?;
        if self.fail_observe {
            return Err(ProOutputSinkError::new(
                "roo_test_observe_failure",
                "injected observe_source failure",
            ));
        }
        Ok(self.progress.lock().expect("progress").get(source).cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        self.record_committed_core()?;
        if self.fail_pages {
            return Err(ProOutputSinkError::new(
                "roo_test_output_failure",
                "injected output failure",
            ));
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

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
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

fn identified_message(id: &str, role: &str, text: &str) -> Value {
    json!({"id": id, "role": role, "content": text})
}

fn identified_tool_touch(id: &str) -> Value {
    json!({
        "id": id,
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "tool_use_id": "touch-call",
            "name": "apply_patch",
            "input": {
                "patch": "*** Begin Patch\n*** Add File: src/stable.rs\n+stable\n*** End Patch"
            }
        }]
    })
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

fn roo_session(store: &Store) -> ctx_history_core::Session {
    let sessions = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::RooCode)
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 1, "unexpected Roo sessions: {sessions:?}");
    sessions.into_iter().next().expect("Roo session")
}

fn event_with_body(store: &Store, session_id: uuid::Uuid, body: &str) -> ctx_history_core::Event {
    store
        .events_for_session(session_id)
        .expect("events")
        .into_iter()
        .find(|event| event.payload.get("body").and_then(Value::as_str) == Some(body))
        .unwrap_or_else(|| panic!("missing event body `{body}`"))
}

fn file_touch_ids(store: &Store) -> Vec<uuid::Uuid> {
    let connection = rusqlite::Connection::open(store.path()).expect("open Store database");
    let mut statement = connection
        .prepare("SELECT id FROM files_touched ORDER BY id")
        .expect("prepare file-touch IDs");
    statement
        .query_map([], |row| {
            let id: String = row.get(0)?;
            Ok(id)
        })
        .expect("query file-touch IDs")
        .map(|id| {
            id.expect("file-touch ID")
                .parse()
                .expect("UUID file-touch ID")
        })
        .collect()
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
