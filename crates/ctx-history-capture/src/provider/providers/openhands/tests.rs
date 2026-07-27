use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::{json, Value};

use super::import_openhands_nativepath;
use crate::{
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
};

const MACHINE: &str = "openhands-nativepath-test-machine";
const SUCCESS_BODY: &str = "OPENHANDS_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

#[test]
fn production_nativepath_covers_restart_rewrite_corruption_and_disappearance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let event_path = event_path(&root, "conversation-life", "0001-message.json");
    write_event(&event_path, message_event("message-v1", "first body"));
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 1);
    let event_id = provider_events(&store)[0].id;
    assert!(store.authorized_source_route_for_event(event_id).is_ok());

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

    write_event(&event_path, message_event("message-v2", "rewritten body"));
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(provider_events(&store).len(), 2);

    fs::write(&event_path, b"{incomplete").unwrap();
    let corrupt = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(corrupt.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(corrupt.failed, 1);
    assert_eq!(provider_events(&store).len(), 2);

    write_event(&event_path, message_event("message-v3", "repaired body"));
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store.authorized_source_route_for_event(event_id).is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_event(&event_path, message_event("message-v4", "returned body"));
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(provider_events(&store).len(), 4);
}

#[test]
fn core_commits_before_independent_output_replay_and_later_activation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let output_path = event_path(&root, "conversation-pro", "0001-output.json");
    write_event(&output_path, successful_output_event(SUCCESS_BODY));
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone(), false));

    let imported = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(imported.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert!(provider_events(&store).is_empty());
    assert!(
        !serde_json::to_string(&store.list_capture_sources().unwrap())
            .unwrap()
            .contains(SUCCESS_BODY)
    );

    let later_root = temp.path().join("later-profile");
    let later_event = event_path(&later_root, "conversation-later", "0001-output.json");
    write_event(
        &later_event,
        successful_output_event("later activation body"),
    );
    let later_store_path = temp.path().join("later.sqlite");
    let mut later_store = Store::open(&later_store_path).unwrap();
    assert_eq!(
        import(&later_root, &mut later_store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let later_sink = Arc::new(RecordingSink::new(later_store_path, false));
    let replay = import(
        &later_root,
        &mut later_store,
        ImportProfile::ProReplayOnly(later_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(later_sink.outputs.load(Ordering::SeqCst), 1);
}

#[test]
fn output_failure_marks_only_pro_behind_after_core_commit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let event_path = event_path(&root, "conversation-failure", "0001-output.json");
    write_event(&event_path, successful_output_event("transient output"));
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path, true));

    let summary = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store.list_sessions().unwrap().is_empty());
    assert!(sink.behind.load(Ordering::SeqCst));
}

fn import(root: &Path, store: &mut Store, profile: ImportProfile) -> ProviderImportSummary {
    import_openhands_nativepath(
        root,
        store,
        ProviderAdapterContext {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        },
        ProviderImportOptions {
            import_profile: profile,
            ..ProviderImportOptions::default()
        },
    )
    .unwrap()
}

fn event_path(root: &Path, conversation: &str, file: &str) -> PathBuf {
    root.join("user")
        .join("v1_conversations")
        .join(conversation)
        .join("events")
        .join(file)
}

fn write_event(path: &Path, value: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn message_event(id: &str, content: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-25T12:00:00Z",
        "kind": "MessageEvent",
        "source": "user",
        "llm_message": {"role": "user", "content": content},
    })
}

fn successful_output_event(content: &str) -> Value {
    json!({
        "id": "output-id",
        "timestamp": "2026-07-25T12:00:00Z",
        "kind": "ObservationEvent",
        "source": "environment",
        "observation": {
            "kind": "ExecuteBashObservation",
            "content": content,
            "exit_code": 0,
        },
    })
}

fn provider_events(store: &Store) -> Vec<ctx_history_core::Event> {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::OpenHands)
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect()
}

struct RecordingSink {
    fail: bool,
    progress: Mutex<BTreeMap<String, ProOutputProgress>>,
    outputs: AtomicUsize,
    saw_core_before_page: AtomicBool,
    behind: AtomicBool,
}

impl RecordingSink {
    fn new(_store_path: PathBuf, fail: bool) -> Self {
        Self {
            fail,
            progress: Mutex::new(BTreeMap::new()),
            outputs: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            behind: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "openhands-test-materializer-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self
            .progress
            .lock()
            .unwrap()
            .get(&source.source_id)
            .cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        // OpenHands emits a Pro page only after reading back and verifying the
        // exact terminal Core cursor for this physical source.
        self.saw_core_before_page.store(true, Ordering::SeqCst);
        if self.fail {
            return Err(ProOutputSinkError::new("test_failure", "injected failure"));
        }
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().unwrap().insert(
            page.source.source_id.clone(),
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
