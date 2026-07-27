use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use ctx_history_store::Store;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    native_source::NativePosition, provider::importer::BoundedParserCheckpoint, ImportProfile,
    OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProviderAdapterContext, ProviderImportOptions,
    ProviderImportSummary, ProviderImportWorkResult,
};

use super::super::TRAE_CN_INPUT_HISTORY_KEY;
use super::*;

const MACHINE: &str = "trae-nativepath-test-machine";
const SUCCESS_BODY: &str = "trae-success-output-must-never-enter-core";
const FAILURE_BODY: &str = "trae-failure-output-body-must-never-enter-core";

#[test]
fn production_core_lifecycle_is_nativepath_only() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-a");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    create_source(&source, &initial_messages());
    let record_id = Uuid::new_v4();
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let fresh = import(&root, &mut store, record_id, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    let events = trae_events(&store);
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Message));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert_core_excludes_output_bodies(&events);
    let routed_event = events
        .iter()
        .find(|event| event.event_type == EventType::Message)
        .expect("routed message")
        .id;

    let replay = import(&root, &mut store, record_id, ImportProfile::CoreOnly);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    let mut appended = initial_messages();
    appended.push(json!({
        "id": "assistant-append",
        "role": "assistant",
        "content": "append survives",
        "timestamp": "2026-07-25T00:00:04Z",
    }));
    replace_chat_value(&source, &appended);
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .search_event_hits("append survives", 10)
        .expect("append search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));

    let rewritten = vec![json!({
        "id": "rewrite-user",
        "role": "user",
        "content": "rewrite survives",
        "timestamp": "2026-07-25T00:00:05Z",
    })];
    replace_chat_value(&source, &rewritten);
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    let truncated = vec![json!({
        "id": "truncate-user",
        "role": "user",
        "content": "truncation survives",
        "timestamp": "2026-07-25T00:00:06Z",
    })];
    replace_chat_value(&source, &truncated);
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_file(&source).expect("remove source for replacement");
    create_source(
        &source,
        &[json!({
            "id": "replacement-user",
            "role": "user",
            "content": "replacement survives",
            "timestamp": "2026-07-25T00:00:07Z",
        })],
    );
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .search_event_hits("replacement survives", 10)
        .expect("replacement search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));

    fs::remove_file(&source).expect("remove source");
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import(&root, &mut store, record_id, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn core_and_pro_replay_are_independent_and_restartable() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-output");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    create_source(&source, &initial_messages());
    let record_id = Uuid::new_v4();
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let core = import(&root, &mut store, record_id, ImportProfile::CoreOnly);
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert_core_excludes_output_bodies(&trae_events(&store));

    let sink = Arc::new(RecordingSink::new(store_path.clone(), false));
    let replay = import(
        &root,
        &mut store,
        record_id,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.saw_core.load(Ordering::SeqCst));
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().expect("contents").as_slice(),
        [SUCCESS_BODY.as_bytes(), FAILURE_BODY.as_bytes()]
    );

    let second = import(
        &root,
        &mut store,
        record_id,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(second.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);

    let late_sink = Arc::new(RecordingSink::new(store_path.clone(), false));
    let late = import(
        &root,
        &mut store,
        record_id,
        ImportProfile::CoreAndPro(late_sink.clone()),
    );
    assert_eq!(late.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(late_sink.contents.lock().expect("late contents").len(), 2);

    let failing_temp = crate::test_support_paths::tempdir().expect("failing tempdir");
    let failing_root = failing_temp.path().join("workspaceStorage");
    let failing_workspace = failing_root.join("workspace-output");
    fs::create_dir_all(&failing_workspace).expect("failing workspace");
    create_source(&failing_workspace.join("state.vscdb"), &initial_messages());
    let failing_store_path = failing_temp.path().join("core.sqlite");
    let mut failing_store = Store::open(&failing_store_path).expect("failing store");
    let failing_sink = Arc::new(RecordingSink::new(failing_store_path, true));
    let core_despite_pro_failure = import(
        &failing_root,
        &mut failing_store,
        record_id,
        ImportProfile::CoreAndPro(failing_sink.clone()),
    );
    assert_eq!(
        core_despite_pro_failure.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(failing_sink.behind.load(Ordering::SeqCst) > 0);
    assert_core_excludes_output_bodies(&trae_events(&failing_store));
}

#[test]
fn one_safe_group_resumes_to_the_same_terminal_core() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-bounded");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    let messages = (0..130)
        .map(|index| {
            json!({
                "id": format!("message-{index}"),
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("bounded message {index}"),
                "timestamp": "2026-07-25T00:00:00Z",
            })
        })
        .collect::<Vec<_>>();
    create_source(&source, &messages);
    let record_id = Uuid::new_v4();
    let store_path = temp.path().join("bounded.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let mut saw_remaining = false;
    for attempt in 0..8 {
        let mut options = options(record_id, ImportProfile::CoreOnly);
        options.capture_work_limit = crate::CaptureWorkLimit::OneSafeGroup;
        let summary =
            import_trae_nativepath(&root, &mut store, context(&root), options).expect("import");
        saw_remaining |= summary.work_remaining;
        if summary.work_result() == ProviderImportWorkResult::NoOp {
            break;
        }
        if attempt == 0 {
            drop(store);
            store = Store::open(&store_path).expect("restart store");
        }
    }
    assert!(saw_remaining);
    assert_eq!(trae_events(&store).len(), messages.len());
    let mut options = options(record_id, ImportProfile::CoreOnly);
    options.capture_work_limit = crate::CaptureWorkLimit::OneSafeGroup;
    assert_eq!(
        import_trae_nativepath(&root, &mut store, context(&root), options)
            .expect("terminal replay")
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn malformed_sibling_is_rejected_without_blocking_valid_input() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-corrupt");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    let conn = Connection::open(&source).expect("source");
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .expect("schema");
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        params![TRAE_CHAT_KEYS[0], r#"{"list":[{"messages":[1,]}]}"#],
    )
    .expect("malformed value");
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        params![
            TRAE_CN_INPUT_HISTORY_KEY,
            json!([{"id": "valid-sibling", "inputText": "valid sibling survives"}]).to_string(),
        ],
    )
    .expect("valid value");
    drop(conn);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let summary = import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    assert_eq!(summary.failed, 1);
    assert_eq!(trae_events(&store).len(), 1);
    assert!(store
        .search_event_hits("valid sibling survives", 10)
        .expect("search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}

#[test]
fn released_cursor_migrates_once_and_unknown_legacy_cursor_fails_closed() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-migration");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    create_source(&source, &initial_messages());
    let canonical_source = fs::canonicalize(&source).expect("canonical source");
    let locator_identity = provider_path_identity(&canonical_source).expect("path identity");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        &locator_identity,
    );
    let legacy = CertifiedProviderCursor::new(
        "released-trae-source-revision",
        3,
        3,
        NativePosition::new("trae-itemtable-message-keyset-v1", vec![0]).expect("legacy position"),
        BoundedParserCheckpoint::from_serializable(&()).expect("legacy checkpoint"),
    )
    .expect("legacy cursor")
    .encode()
    .expect("encoded legacy cursor");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    store
        .upsert_sync_cursor(&provider_sync_cursor(
            MACHINE,
            stream.clone(),
            legacy,
            context(&root).imported_at,
        ))
        .expect("install legacy cursor");

    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    let migrated = store
        .get_sync_cursor(None, MACHINE, &stream)
        .expect("cursor lookup")
        .expect("migrated cursor");
    let committed =
        decode_native_path_committed_cursor(&migrated.cursor).expect("NativePath cursor wrapper");
    assert!(TraeNativeCursor::decode(committed.provider_cursor()).is_ok());

    store
        .upsert_sync_cursor(&provider_sync_cursor(
            MACHINE,
            stream,
            "unreleased-trae-offset:7".to_owned(),
            context(&root).imported_at,
        ))
        .expect("install unknown cursor");
    let error = import_trae_nativepath(
        &root,
        &mut store,
        context(&root),
        options(Uuid::new_v4(), ImportProfile::CoreOnly),
    )
    .expect_err("unknown legacy cursor must fail");
    assert!(matches!(error, CaptureError::InvalidPayload(_)));
}

struct RecordingSink {
    store_path: PathBuf,
    fail: AtomicBool,
    behind: AtomicUsize,
    pages: AtomicUsize,
    saw_core: AtomicBool,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail: bool) -> Self {
        Self {
            store_path,
            fail: AtomicBool::new(fail),
            behind: AtomicUsize::new(0),
            pages: AtomicUsize::new(0),
            saw_core: AtomicBool::new(false),
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "trae-nativepath-test-materializer-v1"
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
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new("trae_test", "injected Pro failure"));
        }
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("trae_test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("trae_test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_core.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.contents.lock().expect("contents").extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
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
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

fn initial_messages() -> Vec<Value> {
    vec![
        json!({
            "id": "user-1",
            "role": "user",
            "content": "core user message",
            "timestamp": "2026-07-25T00:00:01Z",
        }),
        json!({
            "id": "output-success",
            "role": "tool",
            "content": SUCCESS_BODY,
            "toolCallId": "call-success",
            "exitCode": 0,
            "timestamp": "2026-07-25T00:00:02Z",
        }),
        json!({
            "id": "output-failure",
            "role": "tool",
            "content": FAILURE_BODY,
            "toolCallId": "call-failure",
            "exitCode": 7,
            "timestamp": "2026-07-25T00:00:03Z",
        }),
    ]
}

fn create_source(path: &Path, messages: &[Value]) {
    let conn = Connection::open(path).expect("source");
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .expect("schema");
    write_chat_value(&conn, messages);
}

fn replace_chat_value(path: &Path, messages: &[Value]) {
    let conn = Connection::open(path).expect("source");
    write_chat_value(&conn, messages);
}

fn write_chat_value(conn: &Connection, messages: &[Value]) {
    let value = json!({
        "list": [{
            "id": "session-1",
            "title": "Trae NativePath test",
            "messages": messages,
        }],
    })
    .to_string();
    conn.execute(
        "insert or replace into ItemTable (key, value) values (?1, ?2)",
        params![TRAE_CHAT_KEYS[0], value],
    )
    .expect("chat value");
}

fn import(
    root: &Path,
    store: &mut Store,
    record_id: Uuid,
    profile: ImportProfile,
) -> ProviderImportSummary {
    import_trae_nativepath(root, store, context(root), options(record_id, profile)).expect("import")
}

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc),
    }
}

fn options(_record_id: Uuid, import_profile: ImportProfile) -> ProviderImportOptions {
    ProviderImportOptions {
        history_record_id: None,
        import_profile,
        ..ProviderImportOptions::default()
    }
}

fn trae_events(store: &Store) -> Vec<ctx_history_core::Event> {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::Trae)
        .flat_map(|session| {
            store
                .events_for_session(session.id)
                .expect("session events")
        })
        .collect()
}

fn assert_core_excludes_output_bodies(events: &[ctx_history_core::Event]) {
    let encoded = serde_json::to_string(events).expect("serialize Core events");
    assert!(!encoded.contains(SUCCESS_BODY));
    assert!(!encoded.contains(FAILURE_BODY));
}
