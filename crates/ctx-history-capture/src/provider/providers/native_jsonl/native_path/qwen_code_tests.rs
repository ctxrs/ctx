use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use crate::{
    provider::importer::{BoundedParserCheckpoint, CertifiedProviderCursor},
    CaptureWorkLimit, ProOutputMaterializationPage, ProOutputPageResult, ProviderImportFailure,
};

use super::*;

const MACHINE: &str = "qwen-code-nativepath-test-machine";
const SUCCESS_BODY: &str = "QWEN_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

#[test]
fn production_lifecycle_covers_restart_append_rewrite_truncation_replacement_and_loss() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            message("qwen-life", "fresh-user", "user", "fresh-user"),
            tool_call("qwen-life", "fresh-call"),
        ],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 2);
    let session = qwen_session(&store, "qwen-life");
    assert!(session.is_primary);
    assert!(session.parent_session_id.is_none());
    assert!(session.root_session_id.is_none());
    let original_events = store.events_for_session(session.id).unwrap();
    assert_eq!(original_events.len(), 2);
    let routed_event = original_events[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let previous = checkpoint(&store, &transcript);
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Unchanged
    );
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

    let previous = checkpoint(&store, &transcript);
    append_record(
        &transcript,
        &message("qwen-life", "append", "assistant", "append-assistant"),
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
            message(
                "qwen-life",
                "rewrite-user",
                "user",
                &"rewrite-user-content-".repeat(24),
            ),
            message(
                "qwen-life",
                "rewrite-assistant",
                "assistant",
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
        &[message("qwen-life", "short", "user", "short")],
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
        &[message(
            "qwen-life",
            "replacement",
            "user",
            "replacement-generation",
        )],
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

    fs::remove_file(&transcript).unwrap();
    let source_missing = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        source_missing.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_transcript(
        &transcript,
        &[message(
            "qwen-life",
            "reappeared",
            "user",
            "reappeared-generation",
        )],
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let reappeared_event = store
        .events_for_session(qwen_session(&store, "qwen-life").id)
        .unwrap()
        .into_iter()
        .find(|event| {
            serde_json::to_string(event)
                .unwrap()
                .contains("reappeared-generation")
        })
        .unwrap()
        .id;
    assert!(store
        .authorized_source_route_for_event(reappeared_event)
        .is_ok());

    fs::remove_dir_all(&root).unwrap();
    let root_missing = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        root_missing.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .authorized_source_route_for_event(reappeared_event)
        .is_err());
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn core_commits_before_failed_pro_and_later_output_replay_is_independent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            message("qwen-core-first", "core-first", "user", "core-first"),
            tool_call("qwen-core-first", "call-with-output"),
            tool_result("qwen-core-first", "result-with-output", SUCCESS_BODY, false),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let failing_sink = Arc::new(RecordingSink::new(store_path.clone(), true));

    let fresh = import(
        &root,
        &mut store,
        ImportProfile::CoreAndPro(failing_sink.clone()),
    );
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(failing_sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(failing_sink.behind.load(Ordering::SeqCst), 1);
    let core_events = store
        .events_for_session(qwen_session(&store, "qwen-core-first").id)
        .unwrap();
    assert_eq!(core_events.len(), 2);
    assert!(core_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
    assert!(!serde_json::to_string(&core_events)
        .unwrap()
        .contains(SUCCESS_BODY));

    let replay_sink = Arc::new(RecordingSink::new(store_path.clone(), false));
    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(replay_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(replay_sink.saw_core_before_page.load(Ordering::SeqCst));
    assert!(replay_sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 1);
    let pages_after_replay = replay_sink.pages.load(Ordering::SeqCst);
    assert_eq!(
        import(
            &root,
            &mut store,
            ImportProfile::ProReplayOnly(replay_sink.clone()),
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
    );
    assert_eq!(replay_sink.pages.load(Ordering::SeqCst), pages_after_replay);

    let pro_only_path = temp.path().join("pro-only.sqlite");
    let mut pro_only_store = Store::open(&pro_only_path).unwrap();
    let pro_only_sink = Arc::new(RecordingSink::new(pro_only_path, false));
    assert_eq!(
        import(
            &root,
            &mut pro_only_store,
            ImportProfile::ProReplayOnly(pro_only_sink.clone()),
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
    );
    assert!(pro_only_store.list_sessions().unwrap().is_empty());
    assert!(!pro_only_sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(pro_only_sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(pro_only_sink.outputs.load(Ordering::SeqCst), 0);
}

#[test]
fn unchanged_released_cursor_upgrades_atomically_before_pro_replay() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            message("qwen-migrated", "migrated-user", "user", "migrated"),
            tool_result("qwen-migrated", "migrated-result", "migrated-output", false),
        ],
    );
    let store_path = temp.path().join("migrated.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    import(&root, &mut store, ImportProfile::CoreOnly);
    let authority = checkpoint(&store, &transcript);
    let canonical = fs::canonicalize(&transcript).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        &locator,
    );
    let mut stored = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    stored.cursor = released_cursor_for_checkpoint(&authority);
    store.upsert_sync_cursor(&stored).unwrap();

    let sink = Arc::new(RecordingSink::new(store_path, false));
    let migrated = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(migrated.imported_events, 0);
    assert_eq!(migrated.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    let upgraded = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    assert!(decode_direct_jsonl_native_cursor(
        &upgraded.cursor,
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
    )
    .is_some());
}

#[test]
fn multi_result_failures_are_private_in_core_and_successes_use_transient_pro_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    let multi_result = json!({
        "uuid": "outer-multi-result",
        "sessionId": "qwen-multi-result",
        "timestamp": "2026-07-25T12:00:03Z",
        "type": "tool_result",
        "cwd": "/workspace/qwen",
        "message": {
            "role": "tool",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call-success",
                    "content": "QWEN_TRANSIENT_SUCCESS_ONLY",
                    "is_error": false
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "call-failure",
                    "content": "QWEN_PRIVATE_FAILED_OUTPUT",
                    "is_error": true,
                    "exit_code": 19,
                    "duration_ms": 23
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "call-timeout",
                    "content": "QWEN_PRIVATE_TIMED_OUT_OUTPUT",
                    "timed_out": true,
                    "duration_ms": 29
                }
            ]
        }
    });
    write_transcript(
        &transcript,
        &[
            message(
                "qwen-multi-result",
                "multi-header",
                "user",
                "multi-result-header",
            ),
            multi_result,
        ],
    );
    let store_path = temp.path().join("multi.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path, false));
    let summary = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(summary.imported_events, 3);
    let events = store
        .events_for_session(qwen_session(&store, "qwen-multi-result").id)
        .unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(!rendered.contains("QWEN_TRANSIENT_SUCCESS_ONLY"));
    assert!(!rendered.contains("QWEN_PRIVATE_FAILED_OUTPUT"));
    assert!(!rendered.contains("QWEN_PRIVATE_TIMED_OUT_OUTPUT"));
    assert!(!rendered.contains("output_preview"));
    assert!(!rendered.contains("result_body"));
    let failures = events
        .iter()
        .filter(|event| event.event_type == EventType::ToolOutput)
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    assert!(failures
        .iter()
        .all(|event| { event.payload.pointer("/body/result_content_ref").is_none() }));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.observation_identities.lock().unwrap().as_slice(),
        &[(
            Some("outer-multi-result".to_owned()),
            Some("call-success".to_owned()),
            0
        )]
    );

    let mutated = json!({
        "uuid": "outer-multi-result",
        "sessionId": "qwen-multi-result",
        "timestamp": "2026-07-25T12:00:03Z",
        "type": "tool_result",
        "cwd": "/workspace/qwen",
        "message": {
            "role": "tool",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call-redacted",
                    "content": "QWEN_REDACTED_MUTATION_MUST_STAY_SILENT",
                    "redacted": true
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "call-mutated-success",
                    "content": "QWEN_MUTATED_TRANSIENT_SUCCESS",
                    "is_error": false
                }
            ]
        }
    });
    write_transcript(
        &transcript,
        &[
            message(
                "qwen-multi-result",
                "multi-header",
                "user",
                "multi-result-header",
            ),
            mutated,
        ],
    );
    let rewrite = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(rewrite.failed, 0, "{:?}", rewrite.failures);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);
    assert_eq!(
        *sink.last_disposition.lock().unwrap(),
        Some(ProOutputSourceDisposition::Rewrite),
        "pages={} outputs={} behind={} progress={:?}",
        sink.pages.load(Ordering::SeqCst),
        sink.outputs.load(Ordering::SeqCst),
        sink.behind.load(Ordering::SeqCst),
        sink.progress.lock().unwrap(),
    );
    assert_eq!(
        sink.progress.lock().unwrap().as_ref().unwrap().source_epoch,
        1
    );
    assert_eq!(
        sink.observation_identities.lock().unwrap().last(),
        Some(&(
            Some("outer-multi-result".to_owned()),
            Some("call-mutated-success".to_owned()),
            1
        ))
    );
    assert!(!sink
        .observation_identities
        .lock()
        .unwrap()
        .iter()
        .any(|(_, call_id, _)| call_id.as_deref() == Some("call-redacted")));
}

#[test]
fn empty_truncated_and_missing_sources_publish_idempotent_terminal_pro_rewrites() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    let store_path = temp.path().join("retirement.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path, false));

    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, []).unwrap();
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
    let empty_progress = sink.progress.lock().unwrap().clone().unwrap();
    assert!(empty_progress.terminal);
    assert_eq!(empty_progress.source_epoch, 0);
    assert_eq!(
        *sink.last_disposition.lock().unwrap(),
        Some(ProOutputSourceDisposition::NewSource)
    );
    let empty_pages = sink.pages.load(Ordering::SeqCst);
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(sink.pages.load(Ordering::SeqCst), empty_pages);

    write_transcript(
        &transcript,
        &[
            message("qwen-retirement", "retirement-user", "user", "user"),
            tool_result(
                "qwen-retirement",
                "retirement-result",
                "retirement-output",
                false,
            ),
        ],
    );
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    let populated = sink.progress.lock().unwrap().clone().unwrap();
    assert_eq!(populated.source_epoch, 1);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);

    fs::write(&transcript, []).unwrap();
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    let truncated = sink.progress.lock().unwrap().clone().unwrap();
    assert!(truncated.terminal);
    assert_eq!(truncated.source_epoch, 2);
    assert_eq!(
        *sink.last_disposition.lock().unwrap(),
        Some(ProOutputSourceDisposition::Rewrite)
    );
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    let truncated_pages = sink.pages.load(Ordering::SeqCst);
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(sink.pages.load(Ordering::SeqCst), truncated_pages);

    write_transcript(
        &transcript,
        &[
            message("qwen-retirement", "returned-user", "user", "returned"),
            tool_result(
                "qwen-retirement",
                "returned-result",
                "returned-output",
                false,
            ),
        ],
    );
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    let returned = sink.progress.lock().unwrap().clone().unwrap();
    assert_eq!(returned.source_epoch, 3);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);

    fs::remove_file(&transcript).unwrap();
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    let missing = sink.progress.lock().unwrap().clone().unwrap();
    assert!(missing.terminal);
    assert_eq!(missing.source_epoch, 4);
    assert_eq!(
        *sink.last_disposition.lock().unwrap(),
        Some(ProOutputSourceDisposition::Rewrite)
    );
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    let missing_pages = sink.pages.load(Ordering::SeqCst);
    import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(sink.pages.load(Ordering::SeqCst), missing_pages);
}

#[test]
fn malformed_record_and_incomplete_tail_resume_at_the_exact_safe_frontier() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".qwen/projects");
    let transcript = transcript_path(&root);
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let first =
        serde_json::to_vec(&message("qwen-partial", "first", "user", "first-valid")).unwrap();
    let second = serde_json::to_vec(&message(
        "qwen-partial",
        "second",
        "assistant",
        "second-valid",
    ))
    .unwrap();
    let tail = serde_json::to_vec(&message(
        "qwen-partial",
        "tail",
        "assistant",
        "completed-after-retry",
    ))
    .unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&first);
    bytes.push(b'\n');
    bytes.extend_from_slice(b"{malformed-json}\n");
    bytes.extend_from_slice(&second);
    bytes.push(b'\n');
    bytes.extend_from_slice(&tail[..tail.len() - 1]);
    fs::write(&transcript, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first_import = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(first_import.imported_sessions, 1);
    assert_eq!(first_import.imported_events, 2);
    assert_eq!(first_import.failed, 1);
    assert!(matches!(
        first_import.failures.as_slice(),
        [ProviderImportFailure { line: 2, .. }]
    ));
    let partial_checkpoint = checkpoint(&store, &transcript);
    assert!(!partial_checkpoint.terminal);
    assert!(partial_checkpoint.complete_prefix_end < partial_checkpoint.source_observation.length);

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    file.write_all(b"}\n").unwrap();
    drop(file);
    assert_eq!(
        classify(&transcript, &root, &partial_checkpoint),
        DirectJsonlSourceChange::Append
    );
    let completed = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(completed.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(completed.imported_events, 1);
    assert_eq!(completed.failed, 1);
    let rendered = serde_json::to_string(
        &store
            .events_for_session(qwen_session(&store, "qwen-partial").id)
            .unwrap(),
    )
    .unwrap();
    assert!(rendered.contains("completed-after-retry"));
    let unchanged_with_rejection = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(unchanged_with_rejection.failed, 1);
    assert_ne!(
        unchanged_with_rejection.work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn provider_owned_parser_preserves_result_precedence_redaction_and_failure_policy() {
    let successful = tool_result(
        "qwen-parser",
        "successful",
        "higher-priority-content",
        false,
    );
    let results = enumerate_qwen_code_results(&successful).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, Some("higher-priority-content"));
    assert_eq!(results[0].call_id, Some("call-1"));
    assert_eq!(results[0].tool_name, None);
    assert_eq!(results[0].outcome.outcome, OutputOutcome::Success);

    let redacted = json!({
        "type": "tool_result",
        "sessionId": "qwen-parser",
        "redacted": true,
        "message": {
            "role": "tool",
            "content": [
                {"type": "tool_result", "content": "must-not-escape"},
                {"type": "tool_result", "content": "must-not-escape-either"}
            ]
        }
    });
    let redacted_results = enumerate_qwen_code_results(&redacted).unwrap();
    assert_eq!(redacted_results.len(), 2);
    assert!(redacted_results
        .iter()
        .all(|result| result.content.is_none()));

    let failed = tool_result(
        "qwen-parser",
        "failed",
        "diagnostic-must-not-enter-core",
        true,
    );
    let failed_results = enumerate_qwen_code_results(&failed).unwrap();
    assert_eq!(failed_results[0].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(
        qwen_code_event_type(&failed),
        ctx_history_core::EventType::ToolOutput
    );
}

type ObservationIdentity = (Option<String>, Option<String>, u32);

struct RecordingSink {
    store_path: PathBuf,
    fail_pages: AtomicBool,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    behind: AtomicUsize,
    saw_core_before_page: AtomicBool,
    last_disposition: Mutex<Option<ProOutputSourceDisposition>>,
    observation_identities: Mutex<Vec<ObservationIdentity>>,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail_pages: bool) -> Self {
        Self {
            store_path,
            fail_pages: AtomicBool::new(fail_pages),
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            last_disposition: Mutex::new(None),
            observation_identities: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "qwen-code-nativepath-test-materializer-v1"
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
        *self.last_disposition.lock().unwrap() = Some(page.disposition);
        self.observation_identities
            .lock()
            .unwrap()
            .extend(page.observations.iter().map(|observation| {
                (
                    observation.coordinate.native_record_id.clone(),
                    observation.call_id.clone(),
                    observation
                        .coordinate
                        .source_record_subrecord_index
                        .unwrap_or_default(),
                )
            }));
        if self.fail_pages.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "injected_qwen_output_failure",
                "injected output materialization failure",
            ));
        }
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
    import_qwen_code_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: root,
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            history_record_id: None,
            capture_work_limit: CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            import_profile,
        },
    )
    .unwrap()
}

fn qwen_session(store: &Store, provider_session_id: &str) -> ctx_history_core::Session {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| {
            session.provider == CaptureProvider::QwenCode
                && session.external_session_id.as_deref() == Some(provider_session_id)
        })
        .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("sanitized-workspace/chats/qwen-life.jsonl")
}

fn message(session_id: &str, id: &str, kind: &str, content: &str) -> Value {
    json!({
        "uuid": id,
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:01Z",
        "type": kind,
        "cwd": "/workspace/qwen",
        "message": {
            "role": kind,
            "content": [{"type": "text", "text": content}]
        },
        "model": "qwen3-coder",
    })
}

fn tool_call(session_id: &str, id: &str) -> Value {
    json!({
        "uuid": id,
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:02Z",
        "type": "assistant",
        "cwd": "/workspace/qwen",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call-1",
                "name": "Write",
                "input": {"path": "src/qwen.txt", "content": "proof"}
            }]
        },
        "model": "qwen3-coder",
    })
}

fn tool_result(session_id: &str, id: &str, result: &str, is_error: bool) -> Value {
    json!({
        "uuid": id,
        "sessionId": session_id,
        "timestamp": "2026-07-25T12:00:03Z",
        "type": "tool_result",
        "cwd": "/workspace/qwen",
        "message": {
            "role": "tool",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "content": result,
                "is_error": is_error
            }]
        },
        "toolCallResult": {
            "tool": "Write",
            "path": "src/qwen.txt",
            "output": "lower-priority-output",
            "is_error": is_error
        },
        "model": "qwen3-coder",
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

fn released_cursor_for_checkpoint(checkpoint: &DirectJsonlCheckpoint) -> String {
    let offset = checkpoint.complete_prefix_end;
    let proof_length = u32::try_from(offset.min(64 * 1024)).unwrap();
    let mut bytes = Vec::with_capacity(56);
    bytes.extend_from_slice(b"CTXJLBP\0");
    bytes.extend_from_slice(&[1, 1, 0, 0]);
    bytes.extend_from_slice(&offset.to_be_bytes());
    bytes.extend_from_slice(&proof_length.to_be_bytes());
    bytes.extend_from_slice(&[0; 32]);
    let position =
        crate::native_source::NativePosition::new("jsonl-byte-boundary-v1", bytes).unwrap();
    let session = checkpoint.session.as_ref().unwrap();
    let parser_checkpoint = BoundedParserCheckpoint::from_serializable(&json!({
        "session": {
            "native_session_id": session.native_session_id,
            "provider_session_id": session.provider_session_id,
            "parent_provider_session_id": session.parent_provider_session_id,
            "external_agent_id": session.external_agent_id,
            "agent_type": session.agent_type,
            "status": session.status,
            "started_at": session.started_at,
            "cwd": session.cwd,
            "header_anchor": {
                "ordinal": 0,
                "start": 0,
                "end": 0,
                "payload_sha256": vec![0_u8; 32],
            }
        },
        "next_ordinal": checkpoint.next_raw_ordinal,
        "accepted_captures": checkpoint.accepted_events,
        "accepted_events": checkpoint.accepted_events,
        "accepted_file_touches": checkpoint.accepted_file_touches,
        "rejected_records": checkpoint.rejected_records,
    }))
    .unwrap();
    CertifiedProviderCursor::new(
        direct_jsonl_source_revision(&checkpoint.source_observation),
        4,
        7,
        position,
        parser_checkpoint,
    )
    .unwrap()
    .with_rejected_records(checkpoint.rejected_records)
    .encode()
    .unwrap()
}

fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
    let canonical = fs::canonicalize(path).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(
        &cursor.cursor,
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
    )
    .unwrap()
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
