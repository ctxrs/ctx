use std::{
    fs,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::{json, Value};

use super::*;
use crate::{
    test_support_paths::tempdir, ImportProfile, OutputNativeCursor, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
};

const MACHINE: &str = "auggie-nativepath-test-machine";
const IMPORTED_AT: &str = "2026-07-25T12:00:00Z";
const REQUEST_SUCCESS_BODY: &str = "AUGGIE_REQUEST_SUCCESS_BODY_MUST_NOT_ENTER_CORE";
const SUCCESS_BODY: &str = "AUGGIE_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

fn import(
    root: &Path,
    store: &mut Store,
    profile: ImportProfile,
    work_limit: crate::CaptureWorkLimit,
) -> ProviderImportSummary {
    import_auggie_sessions_nativepath(
        root,
        store,
        ProviderAdapterContext {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: IMPORTED_AT.parse().unwrap(),
        },
        ProviderImportOptions {
            capture_work_limit: work_limit,
            import_profile: profile,
            ..ProviderImportOptions::default()
        },
    )
    .unwrap()
}

fn write_session(path: &Path, session_id: &str, chat_history: Vec<Value>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": session_id,
            "created": "2026-07-25T11:00:00Z",
            "modified": "2026-07-25T11:30:00Z",
            "workspaceRoot": "/workspace/auggie",
            "chatHistory": chat_history,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn exchange(request_id: &str, request: &str, response: &str) -> Value {
    json!({
        "exchange": {
            "request_id": request_id,
            "request_message": request,
            "response_text": response,
        },
        "finishedAt": "2026-07-25T11:01:00Z",
    })
}

#[test]
fn nativepath_lifecycle_is_restart_safe_and_retires_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let transcript = root.join("session.json");
    write_session(
        &transcript,
        "auggie-life",
        vec![exchange("one", "fresh request", "fresh response")],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 2);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    let session = store
        .session_by_external_session(CaptureProvider::Auggie, "auggie-life")
        .unwrap()
        .unwrap();
    let original_events = store.events_for_session(session.id).unwrap();
    let routed_event = original_events[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let noop = import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(noop.imported_sessions, 0);
    assert_eq!(noop.imported_events, 0);
    assert_eq!(noop.skipped_sessions, 1);
    assert_eq!(noop.skipped_events, 2);

    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::Drain,
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_session(
        &transcript,
        "auggie-life",
        vec![
            exchange("one", "fresh request", "fresh response"),
            exchange("two", "append request", "append response"),
        ],
    );
    let append = import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(append.imported_events, 2);

    write_session(
        &transcript,
        "auggie-life",
        vec![
            exchange("rewrite", "rewritten request", "rewritten response"),
            exchange("two", "append request", "append response"),
        ],
    );
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::Drain,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    write_session(
        &transcript,
        "auggie-life",
        vec![exchange("short", "short request", "short response")],
    );
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::Drain,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = root.join("replacement.json");
    write_session(
        &replacement,
        "auggie-life",
        vec![exchange(
            "replacement",
            "replacement request",
            "replacement response",
        )],
    );
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::Drain,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::Drain,
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn corrupt_rewrite_preserves_committed_core_until_retry() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let transcript = root.join("session.json");
    write_session(
        &transcript,
        "auggie-corrupt",
        vec![exchange("one", "kept request", "kept response")],
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::Drain,
    );
    let session = store
        .session_by_external_session(CaptureProvider::Auggie, "auggie-corrupt")
        .unwrap()
        .unwrap();
    let routed_event = store.events_for_session(session.id).unwrap()[0].id;

    fs::write(&transcript, b"{\"sessionId\":\"auggie-corrupt\"").unwrap();
    let corrupt = import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(corrupt.failed, 1);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    write_session(
        &transcript,
        "auggie-corrupt",
        vec![exchange("fixed", "fixed request", "fixed response")],
    );
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::Drain,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );
}

#[test]
fn one_safe_group_resumes_bounded_pages_and_repairs_late_relationships() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let child = root.join("a-child.json");
    let parent = root.join("z-parent.json");
    let history = (0..40)
        .map(|index| {
            exchange(
                &format!("request-{index}"),
                &format!("request {index}"),
                &format!("response {index}"),
            )
        })
        .collect::<Vec<_>>();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        &child,
        serde_json::to_vec(&json!({
            "sessionId": "auggie-child",
            "parentSessionId": "auggie-parent",
            "created": "2026-07-25T11:00:00Z",
            "chatHistory": history,
        }))
        .unwrap(),
    )
    .unwrap();
    write_session(&parent, "auggie-parent", Vec::new());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        crate::CaptureWorkLimit::OneSafeGroup,
    );
    assert!(first.work_remaining);
    assert!(first.imported_events <= 60);

    let mut attempts = 0;
    loop {
        attempts += 1;
        let next = import(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            crate::CaptureWorkLimit::OneSafeGroup,
        );
        if !next.work_remaining {
            break;
        }
        assert!(attempts < 8);
    }
    let child = store
        .session_by_external_session(CaptureProvider::Auggie, "auggie-child")
        .unwrap()
        .unwrap();
    let parent = store
        .session_by_external_session(CaptureProvider::Auggie, "auggie-parent")
        .unwrap()
        .unwrap();
    assert_eq!(child.parent_session_id, Some(parent.id));
    assert_eq!(child.root_session_id, Some(parent.id));
    assert_eq!(store.events_for_session(child.id).unwrap().len(), 80);
}

#[derive(Default)]
struct RecordingSink {
    fail_next: AtomicBool,
    behind: AtomicUsize,
    bodies: Mutex<Vec<Vec<u8>>>,
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &crate::OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new("injected", "injected failure"));
        }
        self.bodies.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: OutputNativeCursor {
                version: page.next_safe_cursor.version,
                payload: page.next_safe_cursor.payload.clone(),
            },
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: u32::try_from(page.observations.len()).unwrap(),
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn output_failure_never_rolls_back_core_and_later_pro_replay_is_independent() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let transcript = root.join("session.json");
    write_session(
        &transcript,
        "auggie-output",
        vec![json!({
            "exchange": {
                "request_id": "output-request",
                "request_message": "core request",
                "request_nodes": [
                    {
                        "type": "tool_result",
                        "call_id": "request-call-1",
                        "is_error": false,
                        "content": REQUEST_SUCCESS_BODY,
                    }
                ],
                "response_nodes": [
                    {"text_node": {"content": "core response"}},
                    {
                        "type": "tool_result",
                        "call_id": "call-1",
                        "is_error": false,
                        "content": SUCCESS_BODY,
                    }
                ]
            },
            "finishedAt": "2026-07-25T11:01:00Z",
        })],
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::default());
    sink.fail_next.store(true, Ordering::SeqCst);

    let core = import(
        &root,
        &mut store,
        ImportProfile::CoreAndPro(sink.clone()),
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(core.imported_sessions, 1);
    assert_eq!(core.imported_events, 2);
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
    let session = store
        .session_by_external_session(CaptureProvider::Auggie, "auggie-output")
        .unwrap()
        .unwrap();
    let rendered = serde_json::to_string(&store.events_for_session(session.id).unwrap()).unwrap();
    assert!(!rendered.contains(SUCCESS_BODY));
    assert!(!rendered.contains(REQUEST_SUCCESS_BODY));

    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
        crate::CaptureWorkLimit::Drain,
    );
    assert_eq!(replay.imported_events, 0);
    assert!(sink
        .bodies
        .lock()
        .unwrap()
        .iter()
        .any(|body| body == SUCCESS_BODY.as_bytes()));
    assert!(sink
        .bodies
        .lock()
        .unwrap()
        .iter()
        .any(|body| body == REQUEST_SUCCESS_BODY.as_bytes()));
}

#[test]
fn completed_message_metadata_does_not_invent_a_tool_result() {
    let entry = json!({"completed": true, "source": "agent"});
    let exchange = json!({"request_id": "request-1"});
    let event = auggie_event(AuggieEventInput {
        provider_session_id: "session-1",
        provider_event_index: 0,
        chat_index: 0,
        role: EventRole::Assistant,
        label: "response",
        occurred_at: "2026-07-21T00:00:00Z".parse().unwrap(),
        text: "created commit 0123456789abcdef0123456789abcdef01234567".to_owned(),
        entry: &entry,
        exchange: &exchange,
        raw_source_path: "/tmp/auggie/session.json",
    });

    assert_eq!(event.event_type, EventType::Message);
    assert_eq!(event.payload["result_outcome"], Value::Null);
    assert_eq!(event.payload["result_evidence"], Value::Null);
}
