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
    provider::importer::{
        provider_session_uuid, provider_source_event_import_identity,
        released_jsonl_initial_position_for_test, BoundedParserCheckpoint, CertifiedProviderCursor,
    },
    test_support_paths::tempdir,
    CaptureWorkLimit, ProOutputMaterializationPage, ProOutputPageResult, QoderImportOptions,
};

const MACHINE: &str = "qoder-nativepath-test-machine";
const SUCCESS_BODY: &str = "QODER_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

#[test]
fn production_lifecycle_covers_all_source_changes_and_retires_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("qoder-life"),
            message("fresh-user", "user", "fresh-user"),
            tool_call("fresh-call"),
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
        .find(|session| session.provider == CaptureProvider::Qoder)
        .unwrap();
    let original_events = store.events_for_session(session.id).unwrap();
    assert_eq!(original_events.len(), 3);
    assert!(original_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
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
    append_record(
        &transcript,
        &message("append", "assistant", "append-assistant"),
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
            header("qoder-life"),
            message("rewrite-user", "user", &"rewrite-user-content-".repeat(24)),
            message(
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
        &[header("qoder-life"), message("short", "user", "short")],
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
        &[
            header("qoder-life"),
            message("replacement", "user", "replacement-generation"),
        ],
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
fn structural_results_never_enter_core_and_retain_only_typed_failure_metadata() {
    const TOP_LEVEL_SECRET: &str = "QODER_TOP_LEVEL_ONLY_SECRET";
    const MIXED_TEXT: &str = "QODER_MIXED_TEXT_MUST_NOT_ENTER_CORE";
    const MIXED_SECRET: &str = "QODER_MIXED_RESULT_SECRET";
    const FUTURE_SECRET: &str = "QODER_FUTURE_OPAQUE_SECRET";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("qoder-life"),
            message("retained-user", "user", "retained qoder message"),
            json!({
                "type": "user",
                "sessionId": "qoder-life",
                "uuid": "top-level-only",
                "timestamp": "2026-07-25T12:00:02Z",
                "cwd": "/workspace/qoder",
                "message": {
                    "role": "user",
                    "content": "generic user-shaped output"
                },
                "toolUseResult": {
                    "content": TOP_LEVEL_SECRET,
                    "callId": "call-top-level",
                    "toolName": "read_file",
                    "exitCode": 0
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-life",
                "uuid": "mixed-result",
                "timestamp": "2026-07-25T12:00:03Z",
                "cwd": "/workspace/qoder",
                "message": {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": MIXED_TEXT},
                        {
                            "type": "tool_result",
                            "tool_use_id": "call-mixed",
                            "name": "shell",
                            "content": MIXED_SECRET,
                            "is_error": false
                        }
                    ]
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-life",
                "uuid": "future-result",
                "timestamp": "2026-07-25T12:00:04Z",
                "cwd": "/workspace/qoder",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "mcp_tool_future_result",
                        "callId": "call-future",
                        "toolName": "future_tool",
                        "payload": {"opaque": FUTURE_SECRET},
                        "status": "failed",
                        "exitCode": 23,
                        "durationMs": 41
                    }]
                }
            }),
        ],
    );
    let mut store = Store::open(temp.path().join("privacy.sqlite")).unwrap();

    let summary = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(summary.imported_events, 3);
    let events = qoder_events(&store);
    assert_eq!(events.len(), 3);
    let serialized = serde_json::to_string(&events).unwrap();
    for secret in [
        TOP_LEVEL_SECRET,
        MIXED_TEXT,
        MIXED_SECRET,
        FUTURE_SECRET,
        "generic user-shaped output",
    ] {
        assert!(!serialized.contains(secret), "{secret} entered Qoder Core");
    }
    assert!(events.iter().all(|event| {
        !serde_json::to_string(&event.sync.metadata)
            .unwrap()
            .contains("result-body")
    }));

    let failure = events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    assert_eq!(failure.role, Some(ctx_history_core::EventRole::Tool));
    assert_eq!(failure.payload["body"]["result_outcome"], json!("failure"));
    assert_eq!(failure.payload["body"]["exit_code"], json!(23));
    assert_eq!(failure.payload["body"]["duration_ms"], json!(41));
    assert_eq!(failure.payload["body"]["call_id"], json!("call-future"));
    assert!(failure.payload["body"].get("output_preview").is_none());
}

#[test]
fn one_safe_group_commits_at_most_64_physical_normalization_units() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    let mut records = vec![header("qoder-life")];
    records.extend((0..128).map(|index| {
        json!({
            "type": "user",
            "sessionId": "qoder-life",
            "uuid": format!("bounded-result-{index}"),
            "timestamp": "2026-07-25T12:00:03Z",
            "cwd": "/workspace/qoder",
            "message": {"role": "user", "content": "result-shaped"},
            "toolUseResult": format!("omitted-success-{index}"),
            "exitCode": 0
        })
    }));
    write_transcript(&transcript, &records);
    let mut store = Store::open(temp.path().join("bounded.sqlite")).unwrap();

    let first = import_with_limit(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        CaptureWorkLimit::OneSafeGroup,
    );
    assert!(first.work_remaining);
    let committed = checkpoint(&store, &transcript);
    assert!(
        (1..=64).contains(&committed.next_raw_ordinal),
        "one safe group committed {} physical Qoder records",
        committed.next_raw_ordinal
    );
}

#[test]
fn invalid_result_shape_remains_authoritative_across_restart() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("qoder-life"),
            json!({
                "type": "user",
                "sessionId": "qoder-life",
                "uuid": "invalid-result",
                "timestamp": "2026-07-25T12:00:03Z",
                "cwd": "/workspace/qoder",
                "message": {"role": "user", "content": "must not be a message"},
                "toolUseResult": {
                    "content": {"future": "unsupported-result-shape"},
                    "callId": "call-invalid"
                }
            }),
        ],
    );
    let store_path = temp.path().join("rejection.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let first = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(first.failed, 1);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].line, 2);

    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    let restarted = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restarted.failed, 1);
    assert_eq!(restarted.failures.len(), 1);
    assert_eq!(restarted.failures[0].line, 2);
    assert_eq!(qoder_events(&store).len(), 1);
}

#[test]
fn same_uuid_rewrite_updates_content_without_changing_identity() {
    const OLD_TEXT: &str = "QODER_STALE_REWRITE_CONTENT";
    const NEW_TEXT: &str = "QODER_CURRENT_REWRITE_CONTENT_WITH_A_DIFFERENT_LENGTH";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("qoder-life"),
            message("stable-message", "user", OLD_TEXT),
        ],
    );
    let mut store = Store::open(temp.path().join("rewrite.sqlite")).unwrap();
    import(&root, &mut store, ImportProfile::CoreOnly);
    let original = qoder_events(&store)
        .into_iter()
        .find(|event| serde_json::to_string(event).unwrap().contains(OLD_TEXT))
        .unwrap();

    write_transcript(
        &transcript,
        &[
            header("qoder-life"),
            message("stable-message", "user", NEW_TEXT),
        ],
    );
    let rewritten = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rewritten.work_result(), ProviderImportWorkResult::Changed);
    let events = qoder_events(&store);
    assert_eq!(events.len(), 2);
    let current = store.get_event(original.id).unwrap();
    assert!(
        serde_json::to_string(&current).unwrap().contains(NEW_TEXT),
        "same-UUID Qoder rewrite retained stale Core content"
    );
    assert_eq!(current.id, original.id);
    assert!(!serde_json::to_string(&events).unwrap().contains(OLD_TEXT));
}

#[test]
fn released_positional_identity_survives_native_upgrade_and_reorder() {
    const STABLE_TEXT: &str = "QODER_RELEASED_EVENT_MUST_STAY_STABLE";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    let released_records = vec![
        header("qoder-life"),
        message("released-stable-message", "user", STABLE_TEXT),
    ];
    write_transcript(&transcript, &released_records);

    let donor_path = temp.path().join("released-donor.sqlite");
    let mut donor = Store::open(&donor_path).unwrap();
    import(&root, &mut donor, ImportProfile::CoreOnly);
    let source = donor
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.descriptor.provider == CaptureProvider::Qoder)
        .unwrap();
    let current_session = donor
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Qoder)
        .unwrap();
    let donor_events = donor.events_for_session(current_session.id).unwrap();
    drop(donor);

    let mut store = Store::open(temp.path().join("released-upgrade.sqlite")).unwrap();
    store.upsert_capture_source(&source).unwrap();
    let legacy_session_id = provider_session_uuid(CaptureProvider::Qoder, "qoder-life");
    let mut released_session = current_session;
    released_session.id = legacy_session_id;
    store.upsert_session(&released_session).unwrap();

    let mut released_message_id = None;
    for mut event in donor_events {
        let raw_ordinal = event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap();
        let native_id = released_records[usize::try_from(raw_ordinal).unwrap()]["uuid"]
            .as_str()
            .unwrap();
        let identity = provider_source_event_import_identity(source.id, raw_ordinal, native_id);
        event.id = identity.id;
        event.seq = identity.seq;
        event.session_id = Some(legacy_session_id);
        event.dedupe_key = Some(identity.dedupe_key);
        event.sync.metadata["provider_event_hash"] = json!(native_id);
        event.sync.metadata["provider_event_hash_authority"] = json!("provider_supplied");
        event.payload["provider_event_hash"] = json!(native_id);
        if native_id == "released-stable-message" {
            released_message_id = Some(event.id);
        }
        store.upsert_event(&event).unwrap();
    }
    let released_message_id = released_message_id.unwrap();
    seed_released_cursor(&store, &transcript);

    import(&root, &mut store, ImportProfile::CoreOnly);
    let upgraded = qoder_events(&store);
    assert_eq!(upgraded.len(), 2);
    assert_eq!(
        upgraded
            .iter()
            .find(|event| serde_json::to_string(event).unwrap().contains(STABLE_TEXT))
            .unwrap()
            .id,
        released_message_id
    );
    assert_eq!(
        store.get_event(released_message_id).unwrap().sync.metadata
            ["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );

    write_transcript(
        &transcript,
        &[
            header("qoder-life"),
            message(
                "inserted-before-released",
                "user",
                "inserted before released",
            ),
            message("released-stable-message", "user", STABLE_TEXT),
        ],
    );
    import(&root, &mut store, ImportProfile::CoreOnly);
    let reordered = qoder_events(&store);
    assert_eq!(reordered.len(), 3);
    let stable = reordered
        .iter()
        .filter(|event| serde_json::to_string(event).unwrap().contains(STABLE_TEXT))
        .collect::<Vec<_>>();
    assert_eq!(stable.len(), 1);
    assert_eq!(stable[0].id, released_message_id);
    assert_eq!(
        reordered
            .iter()
            .filter(|event| serde_json::to_string(event)
                .unwrap()
                .contains("inserted before released"))
            .count(),
        1
    );
}

#[test]
fn production_is_core_first_with_independent_pro_replay() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".qoder/projects");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            header("qoder-core-first"),
            message("core-first", "user", "core-first"),
            tool_call("call-with-output"),
            tool_result("result-with-output", SUCCESS_BODY),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    let core_events = store
        .events_for_session(
            store
                .list_sessions()
                .unwrap()
                .into_iter()
                .find(|session| session.provider == CaptureProvider::Qoder)
                .unwrap()
                .id,
        )
        .unwrap();
    assert!(core_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
    assert!(!serde_json::to_string(&core_events)
        .unwrap()
        .contains(SUCCESS_BODY));
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
        "qoder-nativepath-test-materializer-v1"
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
    import_with_limit(root, store, import_profile, CaptureWorkLimit::Drain)
}

fn import_with_limit(
    root: &Path,
    store: &mut Store,
    import_profile: ImportProfile,
    capture_work_limit: CaptureWorkLimit,
) -> ProviderImportSummary {
    crate::import_qoder_history(
        root,
        store,
        QoderImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            capture_work_limit,
            ..QoderImportOptions::default()
        },
    )
    .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("sanitized-workspace/transcript/qoder-life.jsonl")
}

fn header(session_id: &str) -> Value {
    json!({
        "type": "session_meta",
        "sessionId": session_id,
        "uuid": format!("{session_id}-meta"),
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/qoder",
        "data": {
            "meta_type": "session_info",
            "content": {"mode": "agent", "session_type": "assistant"}
        }
    })
}

fn message(id: &str, kind: &str, content: &str) -> Value {
    json!({
        "type": kind,
        "sessionId": "qoder-life",
        "uuid": id,
        "timestamp": "2026-07-25T12:00:01Z",
        "cwd": "/workspace/qoder",
        "message": {"role": kind, "content": content},
        "model": "qoder-agent",
    })
}

fn tool_call(id: &str) -> Value {
    json!({
        "type": "assistant",
        "sessionId": "qoder-life",
        "uuid": id,
        "timestamp": "2026-07-25T12:00:02Z",
        "cwd": "/workspace/qoder",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call-1",
                "name": "read_file",
                "input": {"file_path": "README.md"}
            }]
        },
        "model": "qoder-agent",
    })
}

fn tool_result(id: &str, result: &str) -> Value {
    json!({
        "type": "user",
        "sessionId": "qoder-life",
        "uuid": id,
        "timestamp": "2026-07-25T12:00:03Z",
        "cwd": "/workspace/qoder",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "content": "lower-priority-result",
                "is_error": false
            }]
        },
        "toolUseResult": result,
        "model": "qoder-agent",
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
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(&cursor.cursor, CaptureProvider::Qoder, QODER_SOURCE_FORMAT)
        .unwrap()
}

fn qoder_events(store: &Store) -> Vec<ctx_history_core::Event> {
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Qoder)
        .unwrap();
    store.events_for_session(session.id).unwrap()
}

fn seed_released_cursor(store: &Store, path: &Path) {
    let canonical = fs::canonicalize(path).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
        &locator,
    );
    let cursor = CertifiedProviderCursor::new(
        "released-qoder-source-revision",
        4,
        7,
        released_jsonl_initial_position_for_test(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: stable_capture_uuid(
                &format!("released-qoder-cursor:{stream}"),
                "provider-sync-cursor",
            ),
            team_id: None,
            device_id: MACHINE.to_owned(),
            stream,
            cursor,
            last_synced_at: None,
            timestamps: timestamps(DateTime::<Utc>::UNIX_EPOCH),
        })
        .unwrap();
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
