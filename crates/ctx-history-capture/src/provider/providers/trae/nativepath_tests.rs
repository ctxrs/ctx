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
    complete_content::{
        VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    native_source::NativePosition,
    provider::importer::{
        provider_source_event_seq, provider_source_event_uuid, provider_source_root_identity,
        provider_source_session_uuid, BoundedParserCheckpoint,
    },
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
};

use super::super::{
    trae_complete_message, trae_complete_message_locator, trae_complete_value,
    TRAE_CN_INPUT_HISTORY_KEY,
};
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
fn structural_output_records_never_enter_core_as_messages() {
    const STRUCTURAL_SUCCESS: &str = "structural-success-output-private";
    const STRUCTURAL_FAILURE: &str = "structural-failure-output-private";

    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-structural-output");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    create_source(
        &source,
        &[
            json!({
                "id": "ordinary-message",
                "role": "user",
                "content": "ordinary visible content",
                "timestamp": "2026-07-25T00:00:00Z",
            }),
            json!({
                "id": "structural-success",
                "command": "printf private-success",
                "output": STRUCTURAL_SUCCESS,
                "exitCode": 0,
                "timestamp": "2026-07-25T00:00:01Z",
            }),
            json!({
                "id": "structural-failure",
                "command": "printf private-failure",
                "output": STRUCTURAL_FAILURE,
                "exitCode": 9,
                "timestamp": "2026-07-25T00:00:02Z",
            }),
        ],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    let events = trae_events(&store);
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == EventType::Message)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == EventType::ToolOutput)
            .count(),
        1
    );
    let encoded = serde_json::to_string(&events).expect("serialize events");
    assert!(!encoded.contains(STRUCTURAL_SUCCESS));
    assert!(!encoded.contains(STRUCTURAL_FAILURE));
    assert!(store
        .search_event_hits(STRUCTURAL_SUCCESS, 10)
        .expect("success search")
        .is_empty());
    assert!(store
        .search_event_hits(STRUCTURAL_FAILURE, 10)
        .expect("failure search")
        .is_empty());
    assert!(events
        .iter()
        .filter(|event| event.event_type == EventType::ToolOutput)
        .all(|event| {
            event
                .sync
                .metadata
                .pointer(&format!(
                    "/metadata/{VERIFIED_CONTENT_LOCATORS_METADATA_KEY}"
                ))
                .is_none()
        }));
}

#[test]
fn same_native_message_id_rewrites_and_insertions_keep_event_ids() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-stable-identity");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    let first = json!({
        "id": "stable-first",
        "role": "user",
        "content": "prechangeuniquetoken",
        "timestamp": "2026-07-25T00:00:00Z",
    });
    let second = json!({
        "id": "stable-second",
        "role": "assistant",
        "content": "second content remains stable",
        "timestamp": "2026-07-25T00:00:01Z",
    });
    create_source(&source, &[first.clone(), second.clone()]);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    let original_first = event_id_by_native_message_id(&store, "stable-first");
    let original_second = event_id_by_native_message_id(&store, "stable-second");

    let rewritten_first = json!({
        "id": "stable-first",
        "role": "user",
        "content": "postchangeuniquetoken",
        "timestamp": "2026-07-25T00:00:00Z",
    });
    replace_chat_value(&source, &[rewritten_first.clone(), second.clone()]);
    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    assert_eq!(
        event_id_by_native_message_id(&store, "stable-first"),
        original_first
    );
    assert_eq!(
        event_id_by_native_message_id(&store, "stable-second"),
        original_second
    );
    assert!(store
        .search_event_hits("prechangeuniquetoken", 10)
        .expect("old search")
        .is_empty());
    assert_eq!(
        store
            .search_event_hits("postchangeuniquetoken", 10)
            .expect("new search")
            .len(),
        1
    );

    let inserted = json!({
        "id": "inserted-before",
        "role": "user",
        "content": "inserted before stable messages",
        "timestamp": "2026-07-24T23:59:59Z",
    });
    replace_chat_value(&source, &[inserted, rewritten_first, second]);
    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    assert_eq!(
        event_id_by_native_message_id(&store, "stable-first"),
        original_first
    );
    assert_eq!(
        event_id_by_native_message_id(&store, "stable-second"),
        original_second
    );
    assert_eq!(trae_events(&store).len(), 3);
}

#[test]
fn complete_message_locator_uses_raw_container_ordinal() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-raw-ordinal");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    let complete_text = "raw ordinal complete content ".repeat(900);
    let conn = Connection::open(&source).expect("source");
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .expect("schema");
    let value = json!({
        "list": [
            17,
            {
                "id": "raw-session",
                "title": "Raw ordinal",
                "messages": [{
                    "id": "raw-message",
                    "role": "user",
                    "content": complete_text,
                    "timestamp": "2026-07-25T00:00:00Z",
                }],
            }
        ],
    })
    .to_string();
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        params![TRAE_CHAT_KEYS[0], value],
    )
    .expect("chat value");
    drop(conn);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    let event = trae_events(&store)
        .into_iter()
        .find(|event| {
            event
                .payload
                .get("native_message_id")
                .and_then(Value::as_str)
                == Some("raw-message")
        })
        .expect("raw message");
    assert_eq!(
        event
            .sync
            .metadata
            .get("native_session_index")
            .and_then(Value::as_u64),
        Some(1)
    );
    let locator_value = event
        .sync
        .metadata
        .pointer(&format!(
            "/metadata/{VERIFIED_CONTENT_LOCATORS_METADATA_KEY}"
        ))
        .expect("verified locator metadata");
    let locators =
        VerifiedContentLocatorsV1::from_metadata_value(locator_value).expect("valid locators");
    let persisted = locators
        .locator(VerifiedContentRole::MessageBody)
        .expect("message locator")
        .source_locator()
        .expect("source locator");
    let expected = trae_complete_message_locator(0, 1, 0).expect("expected locator");
    assert_eq!(persisted.kind(), expected.kind());
    assert_eq!(persisted.value(), expected.value());

    let conn = Connection::open(&source).expect("source");
    let complete_value = trae_complete_value(&conn, 0)
        .expect("complete value")
        .expect("chat value");
    let provider_session_id = format!(
        "{}/raw-session",
        workspace.file_name().unwrap().to_string_lossy()
    );
    let (_, recovered) = trae_complete_message(&complete_value, 0, 1, 0, &provider_session_id)
        .expect("resolve complete message")
        .expect("complete message");
    assert_eq!(recovered, complete_text.trim());
}

#[test]
fn corrupt_workspace_database_does_not_block_healthy_sibling() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let corrupt_workspace = root.join("a-corrupt");
    let healthy_workspace = root.join("b-healthy");
    fs::create_dir_all(&corrupt_workspace).expect("corrupt workspace");
    fs::create_dir_all(&healthy_workspace).expect("healthy workspace");
    fs::write(
        corrupt_workspace.join("state.vscdb"),
        b"this is not a sqlite database",
    )
    .expect("corrupt source");
    create_source(
        &healthy_workspace.join("state.vscdb"),
        &[json!({
            "id": "healthy-sibling",
            "role": "user",
            "content": "healthy sibling database survives",
            "timestamp": "2026-07-25T00:00:00Z",
        })],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let summary = import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    assert_eq!(summary.failed, 1);
    assert_eq!(trae_events(&store).len(), 1);
    assert_eq!(
        store
            .search_event_hits("healthy sibling database survives", 10)
            .expect("search")
            .len(),
        1
    );
}

#[test]
fn workspace_isolation_does_not_absorb_store_or_system_failures() {
    assert!(!trae_source_failure_is_local(
        &CaptureError::SystemInvariant("injected system failure")
    ));
    assert!(!trae_source_failure_is_local(&CaptureError::Store(
        StoreError::NotFound(Uuid::nil())
    )));
    assert!(!trae_source_failure_is_local(
        &CaptureError::InvalidPayload("injected control failure".to_owned())
    ));
}

#[test]
fn v025_positional_provider_hash_event_migrates_exactly_and_stays_stable() {
    let temp = crate::test_support_paths::tempdir().expect("tempdir");
    let root = temp.path().join("workspaceStorage");
    let workspace = root.join("workspace-v025");
    let source = workspace.join("state.vscdb");
    fs::create_dir_all(&workspace).expect("workspace");
    let original_message = json!({
        "id": "v025-native-message",
        "role": "user",
        "content": "v025originaltoken",
        "timestamp": "2026-07-25T00:00:00Z",
    });
    create_source(&source, std::slice::from_ref(&original_message));
    let canonical_source = fs::canonicalize(&source).expect("canonical source");
    let provider_session_id = "workspace-v025/session-1";
    let native_record_id = format!("{provider_session_id}:v025-native-message");
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::Trae,
        provider_session_id,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        canonical_source.to_str(),
    );
    let released_source_identity = provider_source_root_identity(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        root.to_str().expect("root path"),
    );
    let session_id = provider_source_session_uuid(&released_source_identity, provider_session_id);
    let released_event_id = provider_source_event_uuid(source_id, 0);
    let imported_at = context(&root).imported_at;
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Trae,
                machine_id: MACHINE.to_owned(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(canonical_source.display().to_string()),
                source_format: Some(TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned()),
                source_root: Some(root.display().to_string()),
                source_identity: Some(released_source_identity.clone()),
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: imported_at,
            ended_at: Some(imported_at),
            sync: provider_sync_metadata(
                Fidelity::Partial,
                json!({
                    "provider_session_id": provider_session_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "source_identity": released_source_identity,
                }),
            ),
        })
        .expect("released source");
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Trae,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: imported_at,
            ended_at: Some(imported_at),
            timestamps: timestamps(imported_at),
            sync: provider_sync_metadata(
                Fidelity::Partial,
                json!({
                    "provider_session_id": provider_session_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "metadata": {
                        "chat_key": TRAE_CHAT_KEYS[0],
                        "native_session_id": "session-1",
                        "native_workspace_id": "workspace-v025",
                    },
                }),
            ),
        })
        .expect("released session");
    store
        .upsert_event(&Event {
            id: released_event_id,
            seq: provider_source_event_seq(source_id, 0),
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: imported_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": "trae",
                "provider_session_id": provider_session_id,
                "provider_event_index": 0,
                "provider_event_hash": native_record_id,
                "cursor": format!("{}:{native_record_id}", TRAE_CHAT_KEYS[0]),
                "artifacts": [],
                "body": {
                    "event_id": native_record_id,
                    "native_workspace_id": "workspace-v025",
                    "native_message_id": "v025-native-message",
                    "text": "v025originaltoken",
                    "truncated": false,
                    "body": original_message,
                    "content_retention": "full",
                },
            }),
            payload_blob_id: None,
            dedupe_key: Some(Store::provider_source_event_dedupe_key(
                source_id,
                0,
                &native_record_id,
            )),
            sync: provider_sync_metadata(
                Fidelity::Partial,
                json!({
                    "provider_session_id": provider_session_id,
                    "provider_event_index": 0,
                    "provider_event_hash": native_record_id,
                    "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "metadata": {
                        "source": "trae_state_vscdb_itemtable",
                        "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                        "chat_key": TRAE_CHAT_KEYS[0],
                        "native_message_id": "v025-native-message",
                        "role": "user",
                    },
                }),
            ),
        })
        .expect("released event");

    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    let migrated = store.get_event(released_event_id).expect("migrated event");
    assert_eq!(trae_events(&store).len(), 1);
    assert_eq!(
        migrated.payload.get("event_id").and_then(Value::as_str),
        Some(native_record_id.as_str())
    );
    assert_eq!(
        migrated
            .sync
            .metadata
            .get("provider_event_hash_authority")
            .and_then(Value::as_str),
        Some("normalized_payload_fallback")
    );
    assert_eq!(
        migrated
            .sync
            .metadata
            .get("provider_event_index")
            .and_then(Value::as_u64),
        Some(native_message_event_index(
            TRAE_CHAT_KEYS[0],
            &native_record_id
        ))
    );
    assert!(migrated
        .dedupe_key
        .as_deref()
        .is_some_and(|key| key.ends_with(&compute_payload_hash(&migrated.payload).unwrap())));

    let rewritten = json!({
        "id": "v025-native-message",
        "role": "user",
        "content": "v025rewrittentoken",
        "timestamp": "2026-07-25T00:00:00Z",
    });
    replace_chat_value(&source, std::slice::from_ref(&rewritten));
    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    assert_eq!(
        event_id_by_native_message_id(&store, "v025-native-message"),
        released_event_id
    );
    assert!(store
        .search_event_hits("v025originaltoken", 10)
        .expect("old search")
        .is_empty());
    assert_eq!(
        store
            .search_event_hits("v025rewrittentoken", 10)
            .expect("new search")
            .len(),
        1
    );

    replace_chat_value(
        &source,
        &[
            json!({
                "id": "v026-inserted-before",
                "role": "assistant",
                "content": "v026insertedtoken",
                "timestamp": "2026-07-24T23:59:59Z",
            }),
            rewritten,
        ],
    );
    import(&root, &mut store, Uuid::new_v4(), ImportProfile::CoreOnly);
    assert_eq!(
        event_id_by_native_message_id(&store, "v025-native-message"),
        released_event_id
    );
    assert_eq!(trae_events(&store).len(), 2);
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

fn event_id_by_native_message_id(store: &Store, native_message_id: &str) -> Uuid {
    let matches = trae_events(store)
        .into_iter()
        .filter(|event| {
            event
                .payload
                .get("native_message_id")
                .and_then(Value::as_str)
                == Some(native_message_id)
        })
        .map(|event| event.id)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected one event for native message {native_message_id}"
    );
    matches[0]
}

fn assert_core_excludes_output_bodies(events: &[ctx_history_core::Event]) {
    let encoded = serde_json::to_string(events).expect("serialize Core events");
    assert!(!encoded.contains(SUCCESS_BODY));
    assert!(!encoded.contains(FAILURE_BODY));
}
