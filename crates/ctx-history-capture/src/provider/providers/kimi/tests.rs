use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{CaptureProvider, EventType, RunStatus};
use serde_json::{json, Value};

use crate::{
    test_support_paths::tempdir, CaptureError, CaptureWorkLimit, ImportProfile,
    OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProviderAdapterContext, ProviderImportOptions,
    ProviderImportWorkResult, KIMI_CODE_CLI_SOURCE_FORMAT,
};

use super::{
    import_kimi_nativepath_tree,
    layout::{
        KimiWireLayout, KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES, KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES,
    },
};

const MACHINE: &str = "kimi-nativepath-production-test";
const SUCCESS_BODY: &str = "KIMI_SUCCESS_BODY_MUST_NEVER_ENTER_CORE";
const FAILURE_BODY: &str = "KIMI_FAILURE_DIAGNOSTIC";

fn kimi_wire_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".kimi-code");
    let session_dir = root.join("sessions/work/session-1");
    let agent_dir = session_dir.join("agents/main");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(
        root.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "session-1",
                "sessionDir": session_dir,
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session_dir.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "updatedAt": "2026-07-17T12:00:10Z",
            "title": "Kimi NativePath",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    let wire = agent_dir.join("wire.jsonl");
    write_records(
        &wire,
        &[
            json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
            message("fresh"),
            output("success", 0, SUCCESS_BODY),
            output("failure", 17, FAILURE_BODY),
        ],
    );
    (temp, root, wire)
}

fn message(text: &str) -> Value {
    json!({
        "type": "turn.prompt",
        "time": 1_784_289_600_001_i64,
        "input": text
    })
}

fn output(call_id: &str, exit_code: i64, content: &str) -> Value {
    json!({
        "type": "context.append_loop_event",
        "time": 1_784_289_600_002_i64 + exit_code,
        "event": {
            "type": "tool.result",
            "toolName": "bash",
            "call_id": call_id,
            "exit_code": exit_code,
            "output": content
        }
    })
}

fn write_records(path: &Path, records: &[Value]) {
    let contents = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(path, contents).unwrap();
}

fn append_record(path: &Path, record: &Value) {
    writeln!(
        OpenOptions::new().append(true).open(path).unwrap(),
        "{record}"
    )
    .unwrap();
}

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    }
}

fn options(profile: ImportProfile) -> ProviderImportOptions {
    ProviderImportOptions {
        capture_work_limit: CaptureWorkLimit::Drain,
        import_profile: profile,
        ..ProviderImportOptions::default()
    }
}

#[test]
fn nativepath_lifecycle_handles_restart_append_mutations_and_disappearance() {
    let (temp, root, wire) = kimi_wire_fixture();
    let store_path = temp.path().join("lifecycle.sqlite");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();

    let fresh = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreOnly),
    )
    .unwrap();
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(
        fresh.imported_events,
        2,
        "summary={fresh:?} archive={:?}",
        store.export_archive().unwrap().events
    );

    let noop = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreOnly),
    )
    .unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    assert_eq!(
        import_kimi_nativepath_tree(
            &root,
            &mut store,
            context(&root),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .work_result(),
        ProviderImportWorkResult::NoOp
    );

    append_record(&wire, &message("append"));
    assert_eq!(
        import_kimi_nativepath_tree(
            &root,
            &mut store,
            context(&root),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .imported_events,
        1
    );

    write_records(
        &wire,
        &[
            json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
            message("rewrite"),
        ],
    );
    assert_eq!(
        import_kimi_nativepath_tree(
            &root,
            &mut store,
            context(&root),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::write(&wire, "{\"type\":\"metadata\"}\n").unwrap();
    assert_eq!(
        import_kimi_nativepath_tree(
            &root,
            &mut store,
            context(&root),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = wire.with_extension("replacement");
    write_records(
        &replacement,
        &[
            json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
            message("replacement"),
        ],
    );
    fs::rename(&replacement, &wire).unwrap();
    assert_eq!(
        import_kimi_nativepath_tree(
            &root,
            &mut store,
            context(&root),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_file(&wire).unwrap();
    assert_eq!(
        import_kimi_nativepath_tree(
            &root,
            &mut store,
            context(&root),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    let (root_temp, root_only, _root_wire) = kimi_wire_fixture();
    let mut root_store =
        ctx_history_store::Store::open(root_temp.path().join("root-missing.sqlite")).unwrap();
    assert_eq!(
        import_kimi_nativepath_tree(
            &root_only,
            &mut root_store,
            context(&root_only),
            options(ImportProfile::CoreOnly),
        )
        .unwrap()
        .work_result(),
        ProviderImportWorkResult::Changed
    );
    fs::remove_dir_all(&root_only).unwrap();
    let missing = import_kimi_nativepath_tree(
        &root_only,
        &mut root_store,
        context(&root_only),
        options(ImportProfile::CoreOnly),
    );
    assert!(missing.is_ok());
}

#[test]
fn core_commits_before_independent_pro_and_omits_success_bodies() {
    let (temp, root, _wire) = kimi_wire_fixture();
    let store_path = temp.path().join("core-pro.sqlite");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));
    let summary = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreAndPro(sink.clone())),
    )
    .unwrap();
    assert_eq!(
        summary.imported_events,
        2,
        "summary={summary:?} archive={:?}",
        store.export_archive().unwrap().events
    );
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [SUCCESS_BODY.as_bytes(), FAILURE_BODY.as_bytes()]
    );

    let core = serde_json::to_string(&store.export_archive().unwrap().events).unwrap();
    assert!(!core.contains(SUCCESS_BODY));
    assert!(core.contains(FAILURE_BODY));
    let events = store.list_sessions().unwrap();
    let session = events
        .iter()
        .find(|session| session.provider == CaptureProvider::KimiCodeCli)
        .unwrap();
    let canonical = store.events_for_session(session.id).unwrap();
    assert!(canonical.iter().all(|event| {
        event.event_type != EventType::ToolOutput
            || !event.payload.to_string().contains(SUCCESS_BODY)
    }));
    let runs = store.runs_for_session(session.id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert_eq!(runs[0].exit_code, Some(17));
    assert_eq!(
        canonical
            .iter()
            .find(|event| event.event_type == EventType::CommandOutput)
            .and_then(|event| event.run_id),
        Some(runs[0].id)
    );
}

#[test]
fn pro_failure_never_rolls_back_core_and_later_activation_replays() {
    let (temp, root, _wire) = kimi_wire_fixture();
    let store_path = temp.path().join("pro-retry.sqlite");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let failing = Arc::new(FailingSink::default());
    let summary = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreAndPro(failing.clone())),
    )
    .unwrap();
    assert_eq!(summary.imported_events, 2);
    assert!(!store.list_sessions().unwrap().is_empty());
    assert!(failing.behind.load(Ordering::SeqCst) > 0);

    let replay = Arc::new(RecordingSink::new(store_path));
    let replay_summary = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay_summary.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.outputs.load(Ordering::SeqCst), 2);
}

#[test]
fn corrupt_records_and_incomplete_tail_resume_without_partial_publication() {
    let (temp, root, wire) = kimi_wire_fixture();
    fs::write(
        &wire,
        "{bad-json\n{\"type\":\"turn.prompt\",\"input\":\"tail",
    )
    .unwrap();
    let mut store = ctx_history_store::Store::open(temp.path().join("corrupt.sqlite")).unwrap();
    let first = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreOnly),
    )
    .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.imported_events, 0);

    let mut file = OpenOptions::new().append(true).open(&wire).unwrap();
    writeln!(file, "\"}}").unwrap();
    drop(file);
    let resumed = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreOnly),
    )
    .unwrap();
    assert_eq!(resumed.imported_events, 1);
}

#[test]
fn released_cursor_is_migration_only_and_upgrades_without_duplicates() {
    let (temp, root, wire) = kimi_wire_fixture();
    let mut store = ctx_history_store::Store::open(temp.path().join("migration.sqlite")).unwrap();
    import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreOnly),
    )
    .unwrap();
    let locator = crate::provider::importer::provider_path_identity(&wire).unwrap();
    let stream = crate::provider::importer::provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &locator,
    );
    let mut released = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    released.cursor = r#"{"v":1,"released_kimi_cursor":true}"#.to_owned();
    store.upsert_sync_cursor(&released).unwrap();

    let migrated = import_kimi_nativepath_tree(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreOnly),
    )
    .unwrap();
    assert_eq!(migrated.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(migrated.imported_events, 0);
    assert_eq!(migrated.skipped_events, 2);
    let upgraded = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    let committed =
        ctx_history_store::decode_native_path_committed_cursor(&upgraded.cursor).unwrap();
    let certified =
        crate::provider::importer::CertifiedProviderCursor::decode(committed.provider_cursor())
            .unwrap();
    assert_eq!(certified.parser_revision(), 5);
    assert_eq!(store.export_archive().unwrap().events.len(), 2);
}

#[derive(Default)]
struct FailingSink {
    behind: AtomicUsize,
}

impl ProOutputSink for FailingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "kimi-test-materializer-v1"
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
        Err(ProOutputSinkError::new("injected", "injected failure"))
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
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
        "kimi-test-materializer-v1"
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
        let core = ctx_history_store::Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().unwrap().insert(
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
}

#[test]
fn kimi_wire_observation_detects_auxiliary_state_changes() {
    let (_temp, _root, wire) = kimi_wire_fixture();
    let observation = super::source::KimiWireObservation::read(&wire).unwrap();
    assert!(observation.revalidate(&wire).unwrap());
    let session_dir = wire.parent().unwrap().parent().unwrap().parent().unwrap();
    fs::write(
        session_dir.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "title": "changed state",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    assert!(!observation.revalidate(&wire).unwrap());
}

#[test]
fn kimi_layout_derives_one_exact_root_despite_malicious_nesting() {
    let (temp, root, wire) = kimi_wire_fixture();
    let session_dir = wire.parent().unwrap().parent().unwrap().parent().unwrap();
    fs::write(
        session_dir.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"sessionId": "session-1", "workDir": "/malicious/nearby"})
        ),
    )
    .unwrap();

    let mut layout = KimiWireLayout::read(&wire).unwrap();
    assert_eq!(
        layout.take_index_entry().unwrap().work_dir.as_deref(),
        Some("/workspace/kimi")
    );

    let invalid_wire = temp
        .path()
        .join("not-sessions/work/session-1/agents/main/wire.jsonl");
    fs::create_dir_all(invalid_wire.parent().unwrap()).unwrap();
    fs::write(&invalid_wire, "{}\n").unwrap();
    assert!(matches!(
        KimiWireLayout::read(&invalid_wire),
        Err(CaptureError::InvalidProviderTranscriptPath { .. })
    ));
    assert!(root.join("session_index.jsonl").is_file());
}

#[test]
fn kimi_layout_bounds_remain_exact() {
    let (_temp, root, wire) = kimi_wire_fixture();
    let index_path = root.join("session_index.jsonl");
    OpenOptions::new()
        .write(true)
        .open(&index_path)
        .unwrap()
        .set_len(KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES as u64)
        .unwrap();
    assert!(KimiWireLayout::read(&wire).is_ok());

    let mut index = fs::read(&index_path).unwrap();
    index.truncate(
        index
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap()
            .saturating_add(1),
    );
    index.extend(std::iter::repeat_n(
        b'\n',
        KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES,
    ));
    fs::write(index_path, index).unwrap();
    assert!(KimiWireLayout::read(&wire).is_err());
}
