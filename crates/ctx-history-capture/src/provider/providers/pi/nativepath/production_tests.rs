use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{CaptureProvider, RunStatus, SyncCursor};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use crate::{
    import_pi_session_jsonl, CaptureError, CaptureWorkLimit, ImportProfile, OutputSourceIdentity,
    PiSessionImportOptions, ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult,
};

use super::{
    super::PI_SOURCE_FORMAT,
    source::PiNativePathError,
    vertical::{map_native_error, released_cursor_for_test, source_cursor_stream},
};

const MACHINE: &str = "pi-nativepath-production-test";
const OUTPUT_SENTINEL: &str = "pi-success-output-body-must-stay-out-of-core";

#[test]
fn pi_production_lifecycle_and_route_retirement_are_nativepath_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    write_session(&source, "pi-lifecycle", &["fresh-user"]);
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    let event_id = store
        .events_for_session(store.list_sessions().expect("sessions")[0].id)
        .expect("events")[0]
        .id;

    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    let restart = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

    append_message(&source, "append", "append-user");
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    write_session(&source, "pi-lifecycle", &["rewrite-user", "rewrite-second"]);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    write_session(&source, "pi-lifecycle", &["truncated-user"]);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_file(&source).expect("remove prior source");
    write_session(&source, "pi-lifecycle", &["replacement-user"]);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).expect("remove root");
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store.authorized_source_route_for_event(event_id).is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn pi_core_commit_survives_pro_failure_and_later_replay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    write_output_session(&source);
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let sink = Arc::new(RecordingSink::new(store_path.clone(), true));

    let first = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(store.list_sessions().expect("sessions").len(), 1);
    assert!(sink.behind.load(Ordering::SeqCst));
    assert_core_has_no_output_body(&store);

    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().expect("contents").as_slice(),
        [OUTPUT_SENTINEL.as_bytes()]
    );

    let retry = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(retry.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
}

#[test]
fn pi_failed_command_publishes_a_direct_core_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    write_failed_command_session(&source);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let summary = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);

    let session = store.list_sessions().expect("sessions").remove(0);
    let run = store.runs_for_session(session.id).expect("runs").remove(0);
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.command_preview.as_deref(), Some("false"));
    assert_eq!(run.exit_code, Some(1));
    assert_eq!(
        run.ended_at
            .expect("run end")
            .signed_duration_since(run.started_at)
            .num_milliseconds(),
        250
    );

    let event = store
        .events_for_session(session.id)
        .expect("events")
        .remove(0);
    assert_eq!(event.run_id, Some(run.id));
}

#[test]
fn pi_failed_patch_publishes_a_provider_owned_touch_row() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    write_failed_patch_session(&source);
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let summary = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);

    let conn = Connection::open(&store_path).expect("read store");
    let (path, change_kind, event_id, metadata_json): (
        String,
        Option<String>,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "select path, change_kind, event_id, metadata_json from files_touched",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("published file touch");
    assert_eq!(path, "src/pi-patch.rs");
    assert_eq!(change_kind.as_deref(), Some("modified"));
    assert!(event_id.is_some());
    let metadata: serde_json::Value = serde_json::from_str(&metadata_json).expect("touch metadata");
    let provider_event_index = metadata["provider_event_index"]
        .as_u64()
        .expect("provider event index");
    assert_eq!(
        metadata["provider_touch_index"].as_u64(),
        Some(provider_event_index << 16)
    );
    assert_eq!(metadata["provider_session_id"], "pi-patch");
    assert_eq!(metadata["source_format"], PI_SOURCE_FORMAT);
    assert_eq!(metadata["metadata"]["source"], "apply_patch_update");
}

#[test]
fn pi_only_exact_released_cursor_resets_then_safe_group_resume_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    let messages = (0..70)
        .map(|index| format!("bounded-{index}"))
        .collect::<Vec<_>>();
    write_session(
        &source,
        "pi-released-cursor",
        &messages.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let imported_at = "2026-07-25T12:00:00Z".parse().expect("timestamp");
    let mut cursor = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: MACHINE.to_owned(),
        stream: source_cursor_stream(&source).expect("source stream"),
        cursor: "released-pi-captured-batch-cursor".to_owned(),
        last_synced_at: Some(imported_at),
        timestamps: crate::provider::importer::timestamps(imported_at),
    };
    store
        .upsert_sync_cursor(&cursor)
        .expect("seed unsupported cursor");
    assert!(import_pi_session_jsonl(
        &root,
        &mut store,
        PiSessionImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.clone()),
            imported_at,
            ..PiSessionImportOptions::default()
        },
    )
    .is_err());

    cursor.cursor = released_cursor_for_test();
    store
        .upsert_sync_cursor(&cursor)
        .expect("seed released cursor");

    let partial = import_pi_session_jsonl(
        &root,
        &mut store,
        PiSessionImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.clone()),
            imported_at,
            capture_work_limit: crate::CaptureWorkLimit::OneSafeGroup,
            ..PiSessionImportOptions::default()
        },
    )
    .expect("bounded import");
    assert_eq!(partial.work_result(), ProviderImportWorkResult::Changed);
    assert!(partial.work_remaining);

    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    let resumed = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(resumed.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(
        store
            .events_for_session(store.list_sessions().expect("sessions")[0].id)
            .expect("events")
            .len(),
        70
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn pi_append_then_delete_retires_the_latest_locator_revision() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    write_session(&source, "pi-append-delete", &["initial"]);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let event_id = first_event_id(&store);
    let initial_revision = store
        .authorized_source_route_for_event(event_id)
        .expect("initial route")
        .source_revision()
        .to_owned();

    append_message(&source, "append", "append before delete");
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let appended_revision = store
        .authorized_source_route_for_event(event_id)
        .expect("appended route")
        .source_revision()
        .to_owned();
    assert_ne!(appended_revision, initial_revision);

    fs::remove_file(&source).expect("delete appended source");
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store.authorized_source_route_for_event(event_id).is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn pi_empty_core_append_then_move_supersedes_the_old_alias() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    let moved = root.join("renamed.jsonl");
    write_session(&source, "pi-append-move", &["initial"]);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let event_id = first_event_id(&store);
    let initial_revision = store
        .authorized_source_route_for_event(event_id)
        .expect("initial route")
        .source_revision()
        .to_owned();

    append_success_output(&source);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let appended_revision = store
        .authorized_source_route_for_event(event_id)
        .expect("empty-Core append route")
        .source_revision()
        .to_owned();
    assert_ne!(appended_revision, initial_revision);

    fs::rename(&source, &moved).expect("move appended source");
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let moved_route = store
        .authorized_source_route_for_event(event_id)
        .expect("moved route");
    assert_eq!(moved_route.path(), moved.as_path());
    assert_eq!(moved_route.source_revision(), appended_revision);
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn pi_unchanged_rename_recovers_a_deferred_root_manifest_publication() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("pi-sessions");
    let source = root.join("session.jsonl");
    let moved = root.join("renamed.jsonl");
    write_session(&source, "pi-root-retry", &["initial"]);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let event_id = first_event_id(&store);
    fs::rename(&source, &moved).expect("rename source");

    let bounded = import_with_limit(&root, &mut store, CaptureWorkLimit::OneSafeGroup);
    assert_eq!(bounded.work_result(), ProviderImportWorkResult::Changed);
    assert!(bounded.work_remaining);
    assert_eq!(
        store
            .authorized_source_route_for_event(event_id)
            .expect("relocated route")
            .path(),
        moved.as_path()
    );

    let manifest_retry = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        manifest_retry.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn pi_native_errors_preserve_typed_capture_classification() {
    let io_error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    assert!(matches!(
        map_native_error(PiNativePathError::Io {
            path: PathBuf::from("/pi/session.jsonl"),
            source: io_error,
        }),
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    assert!(matches!(
        map_native_error(PiNativePathError::SourceChanged),
        CaptureError::SourceChangedDuringCapture
    ));
    assert!(matches!(
        map_native_error(PiNativePathError::Normalization(
            CaptureError::SystemInvariant("Pi inner invariant")
        )),
        CaptureError::SystemInvariant("Pi inner invariant")
    ));
    let json_error = serde_json::from_str::<serde_json::Value>("{").expect_err("invalid test JSON");
    assert!(matches!(
        map_native_error(PiNativePathError::Encoding(json_error)),
        CaptureError::Json(_)
    ));
    assert!(matches!(
        map_native_error(PiNativePathError::InvalidSource {
            path: PathBuf::from("/pi/session.jsonl"),
            reason: "not a session".to_owned(),
        }),
        CaptureError::InvalidPayload(_)
    ));
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    fail_once: AtomicBool,
    behind: AtomicBool,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail_once: bool) -> Self {
        Self {
            store_path,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
            fail_once: AtomicBool::new(fail_once),
            behind: AtomicBool::new(false),
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
        "pi-nativepath-production-test-v1"
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
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new("test_failure", "retry output"));
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

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}

fn import(root: &Path, store: &mut Store, profile: ImportProfile) -> ProviderImportSummary {
    import_pi_session_jsonl(
        root,
        store,
        PiSessionImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
            import_profile: profile,
            ..PiSessionImportOptions::default()
        },
    )
    .expect("Pi NativePath import")
}

fn import_with_limit(
    root: &Path,
    store: &mut Store,
    capture_work_limit: CaptureWorkLimit,
) -> ProviderImportSummary {
    import_pi_session_jsonl(
        root,
        store,
        PiSessionImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
            capture_work_limit,
            ..PiSessionImportOptions::default()
        },
    )
    .expect("bounded Pi NativePath import")
}

fn first_event_id(store: &Store) -> Uuid {
    store
        .events_for_session(store.list_sessions().expect("sessions")[0].id)
        .expect("events")[0]
        .id
}

fn write_session(path: &Path, session_id: &str, messages: &[&str]) {
    fs::create_dir_all(path.parent().expect("source parent")).expect("source directory");
    let mut lines = vec![json!({
        "type": "session",
        "id": session_id,
        "version": 3,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace"
    })
    .to_string()];
    lines.extend(messages.iter().enumerate().map(|(index, content)| {
        json!({
            "type": "message",
            "id": format!("message-{index}"),
            "timestamp": "2026-07-25T12:00:01Z",
            "message": {"role": "user", "content": content}
        })
        .to_string()
    }));
    fs::write(path, format!("{}\n", lines.join("\n"))).expect("write session");
}

fn append_message(path: &Path, id: &str, content: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append source");
    writeln!(
        file,
        "{}",
        json!({
            "type": "message",
            "id": id,
            "timestamp": "2026-07-25T12:00:02Z",
            "message": {"role": "assistant", "content": content}
        })
    )
    .expect("append message");
}

fn append_success_output(path: &Path) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append source");
    writeln!(
        file,
        "{}",
        json!({
            "type": "message",
            "id": "successful-output",
            "timestamp": "2026-07-25T12:00:02Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "append-call",
                "success": true,
                "content": "successful output stays out of Core"
            }
        })
    )
    .expect("append successful output");
}

fn write_output_session(path: &Path) {
    fs::create_dir_all(path.parent().expect("source parent")).expect("source directory");
    fs::write(
        path,
        format!(
            "{}\n{}\n{}\n",
            json!({
                "type": "session",
                "id": "pi-output",
                "version": 3,
                "timestamp": "2026-07-25T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "user",
                "timestamp": "2026-07-25T12:00:01Z",
                "message": {"role": "user", "content": "core-first"}
            }),
            json!({
                "type": "message",
                "id": "result",
                "timestamp": "2026-07-25T12:00:02Z",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "call",
                    "success": true,
                    "content": OUTPUT_SENTINEL
                }
            })
        ),
    )
    .expect("write output session");
}

fn write_failed_command_session(path: &Path) {
    fs::create_dir_all(path.parent().expect("source parent")).expect("source directory");
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "pi-command",
                "version": 3,
                "timestamp": "2026-07-25T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "command",
                "timestamp": "2026-07-25T12:00:02Z",
                "message": {
                    "role": "bashExecution",
                    "command": "false",
                    "output": "bounded failure",
                    "exitCode": 1,
                    "durationMs": 250
                }
            })
        ),
    )
    .expect("write command session");
}

fn write_failed_patch_session(path: &Path) {
    fs::create_dir_all(path.parent().expect("source parent")).expect("source directory");
    let patch = "*** Begin Patch\n*** Update File: src/pi-patch.rs\n@@\n-old\n+new\n*** Update File: src/pi-patch.rs\n@@\n-old\n+new\n*** End Patch";
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "pi-patch",
                "version": 3,
                "timestamp": "2026-07-25T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "failed-patch",
                "timestamp": "2026-07-25T12:00:02Z",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "call-patch",
                    "success": false,
                    "content": patch,
                    "path": "src/structured-fallback.rs"
                }
            })
        ),
    )
    .expect("write failed patch session");
}

fn assert_core_has_no_output_body(store: &Store) {
    let events = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).expect("events"))
        .collect::<Vec<_>>();
    let encoded = serde_json::to_string(&events).expect("encode");
    assert!(!encoded.contains(OUTPUT_SENTINEL));
    assert!(!store
        .search_event_hits(OUTPUT_SENTINEL, 10)
        .expect("search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Pi)));
}
