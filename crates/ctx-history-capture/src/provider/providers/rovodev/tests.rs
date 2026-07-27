use std::{
    collections::HashMap,
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

use crate::{
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderImportSummary,
    ProviderImportWorkResult,
};

use super::{
    import_rovodev_native_path, native_path::ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS,
    rovodev_result_content,
};
use crate::{CaptureWorkLimit, ProviderAdapterContext, ProviderImportOptions};

const MACHINE: &str = "rovodev-nativepath-test";
const OUTPUT_SENTINEL: &str = "rovodev-success-output-must-never-enter-core";

#[test]
fn result_profile_selects_only_explicit_tool_result_parts() {
    let message = json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "not a result"},
            {"type": "tool_result", "content": [
                {"text": "first"},
                {"output": "second"}
            ]}
        ]
    });
    assert_eq!(
        rovodev_result_content(&message).as_deref(),
        Some("first\nsecond")
    );
}

#[test]
fn nativepath_lifecycle_covers_append_rewrite_truncation_replacement_and_disappearance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let session = write_session(
        &root,
        "lifecycle",
        &[
            message("user", "initial-user"),
            message("assistant", "initial-assistant"),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    let original_event = first_rovodev_event(&store);
    let original_source_id = store
        .authorized_source_route_for_event(original_event)
        .expect("initial source route")
        .capture_source_id();
    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    let restart = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

    let context_bytes =
        fs::read(session.join("session_context.json")).expect("same-content context");
    let metadata_bytes = fs::read(session.join("metadata.json")).expect("same-content metadata");
    fs::remove_dir_all(&session).expect("remove original physical source");
    fs::create_dir_all(&session).expect("recreate source directory");
    fs::write(session.join("session_context.json"), context_bytes).expect("restore context");
    fs::write(session.join("metadata.json"), metadata_bytes).expect("restore metadata");
    let same_content_replacement = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        same_content_replacement.work_result(),
        ProviderImportWorkResult::Changed
    );
    let replacement_source_id = store
        .authorized_source_route_for_event(first_authorized_rovodev_event(&store))
        .expect("replacement source route")
        .capture_source_id();
    assert_ne!(replacement_source_id, original_source_id);

    write_context(
        &session,
        "lifecycle",
        &[
            message("user", "initial-user"),
            message("assistant", "initial-assistant"),
            message("assistant", "append-suffix"),
        ],
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "append-suffix");

    write_context(
        &session,
        "lifecycle",
        &[
            message("user", "rewrite-user"),
            message("assistant", "rewrite-assistant"),
        ],
    );
    let rewrite = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "rewrite-assistant");
    assert!(store
        .authorized_source_route_for_event(original_event)
        .is_err());

    write_context(&session, "lifecycle", &[message("user", "truncated")]);
    let truncation = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(truncation.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "truncated");

    fs::remove_dir_all(&session).expect("remove old physical source");
    let replacement = write_session(
        &root,
        "lifecycle",
        &[
            message("user", "replacement-user"),
            message("assistant", "replacement-assistant"),
        ],
    );
    let replaced = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "replacement-assistant");

    fs::remove_dir_all(replacement).expect("remove source");
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    let retired_noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(retired_noop.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn core_commit_output_failure_and_later_output_replay_are_independent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    write_session(
        &root,
        "outputs",
        &[
            message("user", "core-first-user"),
            successful_output(OUTPUT_SENTINEL),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let failing = Arc::new(RecordingSink::new(store_path.clone(), true));

    let core = import(
        &root,
        &mut store,
        ImportProfile::CoreAndPro(failing.clone()),
    );
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert!(core.failed > 0);
    assert_core_excludes_output(&store);
    assert!(failing.saw_committed_core.load(Ordering::SeqCst));

    let replay_sink = Arc::new(RecordingSink::new(store_path, false));
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
fn output_progress_appends_without_replaying_committed_output_prefixes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let session = write_session(
        &root,
        "output-append",
        &[
            message("user", "first-core"),
            successful_output("first-output"),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let sink = Arc::new(RecordingSink::new(store_path, false));

    let first = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);

    write_context(
        &session,
        "output-append",
        &[
            message("user", "first-core"),
            successful_output("first-output"),
            message("assistant", "second-core"),
            successful_output("second-output"),
        ],
    );
    let append = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    assert_eq!(
        sink.contents.lock().expect("contents").as_slice(),
        [b"first-output".as_slice(), b"second-output".as_slice()]
    );
}

#[test]
fn corrupt_and_over_budget_sources_commit_only_diagnostics_then_recover() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let corrupt = root.join("corrupt");
    fs::create_dir_all(&corrupt).expect("session directory");
    fs::write(corrupt.join("session_context.json"), b"{not-json").expect("corrupt source");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let rejected = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rejected.failed, 1);
    assert!(store.list_sessions().expect("sessions").is_empty());
    let replay = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replay.failed, 1);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);

    write_context(&corrupt, "corrupt", &[message("user", "recovered-source")]);
    let recovered = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(recovered.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "recovered-source");
    let recovered_event = first_authorized_rovodev_event(&store);

    fs::write(corrupt.join("session_context.json"), b"{broken-again")
        .expect("break a formerly valid source");
    let invalidated = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(invalidated.work_result(), ProviderImportWorkResult::Changed);
    assert!(invalidated.failed > 0);
    assert!(store
        .authorized_source_route_for_event(recovered_event)
        .is_err());
    write_context(
        &corrupt,
        "corrupt",
        &[message("user", "recovered-source-again")],
    );
    let recovered_again = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        recovered_again.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_search(&store, "recovered-source-again");

    let over_budget = root.join("over-budget");
    fs::create_dir_all(&over_budget).expect("over-budget directory");
    fs::write(
        over_budget.join("session_context.json"),
        serde_json::to_vec(&json!({
            "session_id": "over-budget",
            "message_history": [message("user", "must-not-project")],
            "adversarial": vec![Value::Null; ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS]
        }))
        .expect("over-budget JSON"),
    )
    .expect("over-budget source");
    let bounded = import(&root, &mut store, ImportProfile::CoreOnly);
    assert!(bounded.failed > 0);
    assert!(store
        .search_event_hits("must-not-project", 10)
        .expect("search")
        .is_empty());
}

#[test]
fn one_safe_group_resumes_without_republishing_committed_prefixes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let messages = (0..140)
        .map(|index| message("assistant", &format!("bounded-message-{index}")))
        .collect::<Vec<_>>();
    write_session(&root, "bounded", &messages);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    let mut calls = 0_usize;
    loop {
        calls = calls.saturating_add(1);
        let summary = import_with_limit(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            CaptureWorkLimit::OneSafeGroup,
        );
        if !summary.work_remaining {
            break;
        }
        assert!(calls < 10, "bounded import did not converge");
    }
    assert!(calls >= 3);
    let session = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::RovoDev)
        .expect("RovoDev session");
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        messages.len()
    );
}

#[test]
fn one_safe_group_persists_disappearance_authority_with_the_first_core_page() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let messages = (0..140)
        .map(|index| message("assistant", &format!("interrupted-message-{index}")))
        .collect::<Vec<_>>();
    write_session(&root, "interrupted", &messages);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let first = import_with_limit(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        CaptureWorkLimit::OneSafeGroup,
    );
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert!(first.work_remaining);
    let event = first_rovodev_event(&store);

    fs::remove_dir_all(&root).expect("remove root between bounded calls");
    let retired = import_with_limit(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        CaptureWorkLimit::OneSafeGroup,
    );
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(!retired.work_remaining);
    assert!(store.authorized_source_route_for_event(event).is_err());
    let stable = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(stable.work_result(), ProviderImportWorkResult::NoOp);
}

struct RecordingSink {
    store_path: PathBuf,
    fail: bool,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail: bool) -> Self {
        Self {
            store_path,
            fail,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
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
        "rovodev-nativepath-test-v1"
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
        if self.fail {
            return Err(ProOutputSinkError::new(
                "injected",
                "injected output failure",
            ));
        }
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

fn import(root: &Path, store: &mut Store, profile: ImportProfile) -> ProviderImportSummary {
    import_with_limit(root, store, profile, CaptureWorkLimit::Drain)
}

fn import_with_limit(
    root: &Path,
    store: &mut Store,
    profile: ImportProfile,
    capture_work_limit: CaptureWorkLimit,
) -> ProviderImportSummary {
    import_rovodev_native_path(
        root,
        store,
        ProviderAdapterContext {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
        },
        ProviderImportOptions {
            capture_work_limit,
            import_profile: profile,
            ..ProviderImportOptions::default()
        },
    )
    .expect("RovoDev NativePath import")
}

fn write_session(root: &Path, id: &str, messages: &[Value]) -> PathBuf {
    let session = root.join(id);
    fs::create_dir_all(&session).expect("session directory");
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "session_id": id,
            "created_at": "2026-07-25T11:00:00Z",
            "workspace_path": "/workspace/rovodev"
        }))
        .expect("metadata"),
    )
    .expect("metadata file");
    write_context(&session, id, messages);
    session
}

fn write_context(session: &Path, id: &str, messages: &[Value]) {
    fs::write(
        session.join("session_context.json"),
        serde_json::to_vec(&json!({
            "session_id": id,
            "message_history": messages
        }))
        .expect("context"),
    )
    .expect("context file");
}

fn message(role: &str, content: &str) -> Value {
    json!({"role": role, "content": content})
}

fn successful_output(content: &str) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": "rovodev-call",
            "content": content,
            "status": "success"
        }]
    })
}

fn first_rovodev_event(store: &Store) -> uuid::Uuid {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::RovoDev)
        .and_then(|session| {
            store
                .events_for_session(session.id)
                .expect("events")
                .into_iter()
                .next()
        })
        .map(|event| event.id)
        .expect("RovoDev event")
}

fn first_authorized_rovodev_event(store: &Store) -> uuid::Uuid {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::RovoDev)
        .flat_map(|session| store.events_for_session(session.id).expect("events"))
        .find(|event| store.authorized_source_route_for_event(event.id).is_ok())
        .map(|event| event.id)
        .expect("authorized RovoDev event")
}

fn assert_search(store: &Store, needle: &str) {
    assert!(store
        .search_event_hits(needle, 10)
        .expect("search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
}

fn assert_core_excludes_output(store: &Store) {
    let events = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::RovoDev)
        .flat_map(|session| store.events_for_session(session.id).expect("events"))
        .collect::<Vec<_>>();
    assert!(!serde_json::to_string(&events)
        .expect("event JSON")
        .contains(OUTPUT_SENTINEL));
    assert!(store
        .search_event_hits(OUTPUT_SENTINEL, 10)
        .expect("output search")
        .is_empty());
}
