use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event,
    EventType, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::Store;
use serde_json::json;

use crate::complete_content::jsonl::JsonlCompleteContentResolver;
use crate::complete_content::{
    AuthorizedSourceRoute, CompleteContentErrorKind, CompleteContentHashAuthority,
    CompleteContentResolverRegistry, CompleteContentSourceFamily, CompleteMessageRequest,
    SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRole,
    COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    provider_scoped_source_uuid, provider_source_cursor_stream_for_path, provider_source_event_seq,
    provider_source_event_uuid, provider_source_identity, provider_source_session_uuid,
    provider_sync_metadata, timestamps,
};
use crate::test_support_paths::tempdir;
use crate::{
    stable_capture_uuid, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportWorkResult, MUX_SOURCE_FORMAT,
};

use super::import_mux_native_path;
use super::normalization::{mux_core_event, mux_message_timestamp_opt, MuxMessageRow};

struct TestSink {
    fail: bool,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    pages: AtomicUsize,
    behind: AtomicBool,
}

impl TestSink {
    fn recording() -> Self {
        Self {
            fail: false,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
            pages: AtomicUsize::new(0),
            behind: AtomicBool::new(false),
        }
    }

    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::recording()
        }
    }
}

impl ProOutputSink for TestSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "test"
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
        if self.fail {
            return Err(ProOutputSinkError::new("test_failure", "injected"));
        }
        let mut progress = self.progress.lock().unwrap();
        let prior = progress.get(&page.source);
        assert_eq!(
            page.expected_prior_source_epoch,
            prior.map(|prior| prior.source_epoch)
        );
        assert_eq!(
            page.expected_prior_cursor.as_ref(),
            prior.and_then(|prior| prior.cursor.as_ref())
        );
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        self.pages.fetch_add(1, Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        progress.insert(
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

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.store(true, Ordering::SeqCst);
    }
}

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "mux-nativepath-test".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    }
}

fn options() -> ProviderImportOptions {
    ProviderImportOptions {
        ..ProviderImportOptions::default()
    }
}

fn write_session(root: &Path, messages: usize) -> std::path::PathBuf {
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspaceId": "session-1",
            "createdAt": "2026-07-25T11:00:00Z",
            "projectPath": "/work/mux",
            "model": "mux-test",
        }))
        .unwrap(),
    )
    .unwrap();
    let mut chat = String::new();
    for index in 0..messages {
        chat.push_str(
            &serde_json::to_string(&json!({
                "id": format!("message-{index}"),
                "workspaceId": "session-1",
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "createdAt": "2026-07-25T11:01:00Z",
                "parts": [{"type": "text", "text": format!("message {index}")}],
                "metadata": {"historySequence": index},
            }))
            .unwrap(),
        );
        chat.push('\n');
    }
    fs::write(session.join("chat.jsonl"), chat).unwrap();
    session
}

fn append_success_output(session: &Path, id: &str, secret: &str, sequence: usize) {
    let output = json!({
        "id": id,
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:03:00Z",
        "parts": [{
            "type": "dynamic-tool",
            "toolCallId": format!("call-{sequence}"),
            "toolName": "shell",
            "state": "output-available",
            "success": true,
            "output": secret,
        }],
        "metadata": {"historySequence": sequence},
    });
    let mut chat = OpenOptions::new()
        .append(true)
        .open(session.join("chat.jsonl"))
        .unwrap();
    writeln!(chat, "{}", serde_json::to_string(&output).unwrap()).unwrap();
    chat.sync_all().unwrap();
}

fn released_message(
    id: &str,
    role: &str,
    sequence: usize,
    parts: serde_json::Value,
) -> serde_json::Value {
    json!({
        "id": id,
        "workspaceId": "released-session",
        "role": role,
        "createdAt": "2026-07-25T11:01:00Z",
        "parts": parts,
        "metadata": {"historySequence": sequence},
    })
}

fn write_released_v025_session(
    root: &Path,
) -> (std::path::PathBuf, Vec<(usize, serde_json::Value, bool)>) {
    let session = root.join("released-session");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspaceId": "released-session",
            "createdAt": "2026-07-25T11:00:00Z",
            "projectPath": "/work/mux",
            "model": "mux-v025",
        }))
        .unwrap(),
    )
    .unwrap();
    let first = released_message(
        "message-0",
        "user",
        0,
        json!([{"type": "text", "text": "first"}]),
    );
    let replaced = released_message(
        "message-1",
        "assistant",
        1,
        json!([{"type": "text", "text": "short"}]),
    );
    let output = released_message(
        "message-2",
        "assistant",
        2,
        json!([{
            "type": "dynamic-tool",
            "toolCallId": "call-2",
            "toolName": "shell",
            "state": "output-available",
            "success": true,
            "output": "released output",
        }]),
    );
    let last = released_message(
        "message-3",
        "assistant",
        3,
        json!([{"type": "text", "text": "last"}]),
    );
    let partial = released_message(
        "message-1",
        "assistant",
        1,
        json!([
            {"type": "text", "text": "short"},
            {"type": "text", "text": "continued"}
        ]),
    );
    let chat = format!(
        "{}\n{}\n{}\n{{not-json\n{}\n",
        first, replaced, output, last
    );
    fs::write(session.join("chat.jsonl"), chat).unwrap();
    fs::write(
        session.join("partial.json"),
        serde_json::to_vec(&partial).unwrap(),
    )
    .unwrap();
    (
        session,
        vec![
            (1, first, false),
            (1, partial, true),
            (3, output, false),
            (5, last, false),
        ],
    )
}

fn seed_released_v025_store(
    store: &Store,
    session_dir: &Path,
    merged: &[(usize, serde_json::Value, bool)],
    imported_at: DateTime<Utc>,
) -> (uuid::Uuid, uuid::Uuid, Vec<uuid::Uuid>) {
    let primary_path = session_dir.join("chat.jsonl");
    let primary_display = primary_path.display().to_string();
    let provider_session_id = "released-session";
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::Mux,
        provider_session_id,
        MUX_SOURCE_FORMAT,
        Some(&primary_display),
    );
    let source_identity = provider_source_identity(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        None,
        Some(&primary_display),
        None,
        &serde_json::Value::Null,
    )
    .unwrap();
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Mux,
                machine_id: "mux-nativepath-test".to_owned(),
                process_id: None,
                cwd: Some("/work/mux".to_owned()),
                raw_source_path: Some(primary_display.clone()),
                source_format: Some(MUX_SOURCE_FORMAT.to_owned()),
                source_root: Some(primary_display.clone()),
                source_identity: Some(source_identity.clone()),
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: "2026-07-25T11:00:00Z".parse().unwrap(),
            ended_at: None,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({"source_format": MUX_SOURCE_FORMAT}),
            ),
        })
        .unwrap();
    let session_id = provider_source_session_uuid(&source_identity, provider_session_id);
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: Some(session_id),
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Mux,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: "2026-07-25T11:00:00Z".parse().unwrap(),
            ended_at: None,
            timestamps: timestamps(imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({"provider_session_id": provider_session_id}),
            ),
        })
        .unwrap();

    let mut event_ids = Vec::new();
    for (index, (line_number, value, is_partial)) in merged.iter().enumerate() {
        let row = MuxMessageRow {
            line_number: *line_number,
            source_path: if *is_partial {
                session_dir.join("partial.json")
            } else {
                primary_path.clone()
            },
            value: value.clone(),
            is_partial: *is_partial,
        };
        let event_index = index as u64;
        let occurred_at = mux_message_timestamp_opt(value).unwrap();
        let native = mux_core_event(event_index, &row, occurred_at, Some("mux-v025"));
        let event_id = provider_source_event_uuid(source_id, event_index);
        let event_hash = native.provider_event_hash.clone();
        store
            .upsert_event(&Event {
                id: event_id,
                seq: provider_source_event_seq(source_id, event_index),
                history_record_id: None,
                session_id: Some(session_id),
                run_id: None,
                event_type: native.event_type,
                role: native.role,
                occurred_at: native.occurred_at,
                capture_source_id: Some(source_id),
                payload: json!({
                    "provider": CaptureProvider::Mux.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_event_index": event_index,
                    "provider_event_hash": event_hash,
                    "cursor": native.cursor,
                    "artifacts": [],
                    "body": native.payload,
                }),
                payload_blob_id: None,
                dedupe_key: Some(Store::provider_source_event_dedupe_key(
                    source_id,
                    event_index,
                    &native.provider_event_hash,
                )),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider_session_id": provider_session_id,
                        "provider_event_index": event_index,
                        "provider_event_hash": native.provider_event_hash,
                        "cursor": native.cursor,
                        "source_format": MUX_SOURCE_FORMAT,
                        "fixture_line": line_number,
                        "metadata": native.metadata,
                    }),
                ),
            })
            .unwrap();
        event_ids.push(event_id);
    }
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &primary_display,
    );
    store
        .upsert_sync_cursor(&SyncCursor {
            id: stable_capture_uuid(
                &format!(
                    "provider-cursor:{}:{}:{}",
                    CaptureProvider::Mux.as_str(),
                    "mux-nativepath-test",
                    cursor_stream
                ),
                "provider-sync-cursor",
            ),
            team_id: None,
            device_id: "mux-nativepath-test".to_owned(),
            stream: cursor_stream,
            cursor: format!("{}:line:5", primary_path.display()),
            last_synced_at: Some(imported_at),
            timestamps: timestamps(imported_at),
        })
        .unwrap();
    (source_id, session_id, event_ids)
}

fn imported_message_request(store: &Store, event: &Event) -> CompleteMessageRequest {
    let locators = event
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .and_then(VerifiedContentLocatorsV1::from_metadata_value)
        .unwrap();
    let locator = locators
        .locator(VerifiedContentRole::MessageBody)
        .unwrap()
        .clone();
    let route = store.authorized_source_route_for_event(event.id).unwrap();
    let source = store.get_capture_source(route.capture_source_id()).unwrap();
    let source_root = source
        .descriptor
        .source_root
        .as_deref()
        .map(std::path::PathBuf::from)
        .filter(|root| route.path().starts_with(root));
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: route.capture_source_id(),
                provider: route.provider(),
                source_format: route.source_format().to_owned(),
                family: locator.family(),
                raw_source_path: route.path().to_path_buf(),
                source_root,
                source_identity: Some(route.canonical_source_identity().to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event.id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Mux,
        source_format: MUX_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: locator.content_profile().to_owned(),
        source_locator: locator.source_locator(),
        provider_session_id: Some("session-1".to_owned()),
        source_record_ordinal: event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap(),
        source_record_subrecord_index: event.sync.metadata["source_record_subrecord_index"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .unwrap(),
        expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(locator.native_record_id().to_owned()),
        expected_record_digest: Some(locator.record_sha256().clone()),
        expected_content_ref: Some(locator.content_ref().clone()),
        indexed_text: event.payload["body"]["text"].as_str().unwrap().to_owned(),
        indexed_limit_chars: COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS,
    }
}

#[test]
fn nativepath_fresh_replay_and_append_are_idempotent() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 65);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(first.imported_events, 65);
    assert_eq!(first.failed, 0);

    let replay = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.imported_events, 0);

    let mut chat = OpenOptions::new()
        .append(true)
        .open(session.join("chat.jsonl"))
        .unwrap();
    writeln!(
        chat,
        "{}",
        serde_json::to_string(&json!({
            "id": "message-65",
            "workspaceId": "session-1",
            "role": "assistant",
            "createdAt": "2026-07-25T11:02:00Z",
            "parts": [{"type": "text", "text": "append"}],
            "metadata": {"historySequence": 65},
        }))
        .unwrap()
    )
    .unwrap();
    chat.sync_all().unwrap();

    let appended = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(appended.imported_events, 1);
    let canonical = store
        .session_by_external_session(CaptureProvider::Mux, "session-1")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(canonical.id).unwrap().len(), 66);
}

#[test]
fn successful_output_body_never_enters_core() {
    const SECRET: &str = "mux-success-output-secret";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 1);
    append_success_output(&session, "tool-output", SECRET, 1);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();

    let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!archive.contains(SECRET));
}

#[test]
fn pro_replay_only_requires_exact_committed_core() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(&root, 1);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut replay_options = options();
    replay_options.import_profile = ImportProfile::ProReplayOnly(Arc::new(TestSink::recording()));

    let error =
        import_mux_native_path(&root, &mut store, context(&root), replay_options).unwrap_err();

    assert!(error
        .to_string()
        .contains("requires committed NativePath Core"));
}

#[test]
fn later_pro_activation_replays_outputs_independently() {
    const SECRET: &str = "mux-later-pro-output-secret";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 10);
    append_success_output(&session, "tool-output", SECRET, 10);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    let sink = Arc::new(TestSink::recording());
    let mut replay_options = options();
    replay_options.import_profile = ImportProfile::ProReplayOnly(sink.clone());
    let replay = import_mux_native_path(&root, &mut store, context(&root), replay_options).unwrap();

    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.pages.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [SECRET.as_bytes()]
    );
    let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!archive.contains(SECRET));
}

#[test]
fn pro_failure_never_blocks_or_rolls_back_core() {
    const SECRET: &str = "mux-failed-pro-output-secret";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 1);
    append_success_output(&session, "tool-output", SECRET, 1);
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(TestSink::failing());
    let mut import_options = options();
    import_options.import_profile = ImportProfile::CoreAndPro(sink.clone());

    let summary =
        import_mux_native_path(&root, &mut store, context(&root), import_options).unwrap();

    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.failures[0].error,
        "Mux Pro output replay is behind Core"
    );
    assert!(sink.behind.load(Ordering::SeqCst));
    let replay = Arc::new(TestSink::recording());
    let mut replay_options = options();
    replay_options.import_profile = ImportProfile::ProReplayOnly(replay.clone());
    let replay_summary =
        import_mux_native_path(&root, &mut store, context(&root), replay_options).unwrap();
    assert_eq!(replay_summary.failed, 0);
    assert_eq!(
        replay.contents.lock().unwrap().as_slice(),
        [SECRET.as_bytes()]
    );

    drop(store);
    let committed = Store::open_read_only(&store_path).unwrap();
    let canonical = committed
        .session_by_external_session(CaptureProvider::Mux, "session-1")
        .unwrap()
        .unwrap();
    assert_eq!(committed.events_for_session(canonical.id).unwrap().len(), 1);
}

#[test]
fn incomplete_tail_resumes_and_root_disappearance_retires_routes() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 1);
    let chat_path = session.join("chat.jsonl");
    let mut chat = OpenOptions::new().append(true).open(&chat_path).unwrap();
    write!(
        chat,
        "{{\"id\":\"message-1\",\"workspaceId\":\"session-1\",\"role\":\"assistant\""
    )
    .unwrap();
    chat.sync_all().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let incomplete = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(incomplete.imported_events, 1);
    assert_eq!(incomplete.failed, 0);
    assert!(incomplete.work_remaining);

    let mut chat = OpenOptions::new().append(true).open(&chat_path).unwrap();
    writeln!(
        chat,
        ",\"createdAt\":\"2026-07-25T11:04:00Z\",\"parts\":[{{\"type\":\"text\",\"text\":\"complete\"}}],\"metadata\":{{\"historySequence\":1}}}}"
    )
    .unwrap();
    chat.sync_all().unwrap();
    let resumed = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(resumed.imported_events, 1);
    assert_eq!(resumed.failed, 0);

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    let replay = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn replacement_and_truncation_reset_source_authority() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = write_session(&root, 3);
    let chat_path = session.join("chat.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();

    let mut replacement = fs::read_to_string(&chat_path).unwrap();
    replacement.push_str(
        &serde_json::to_string(&json!({
            "id": "message-3",
            "workspaceId": "session-1",
            "role": "assistant",
            "createdAt": "2026-07-25T11:05:00Z",
            "parts": [{"type": "text", "text": "replacement append"}],
            "metadata": {"historySequence": 3},
        }))
        .unwrap(),
    );
    replacement.push('\n');
    let replacement_path = session.join("chat.replacement");
    fs::write(&replacement_path, replacement).unwrap();
    fs::rename(&replacement_path, &chat_path).unwrap();
    let replaced = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(replaced.imported_events, 4);

    let truncated = format!(
        "{}\n",
        serde_json::to_string(&json!({
            "id": "message-after-truncation",
            "workspaceId": "session-1",
            "role": "user",
            "createdAt": "2026-07-25T11:06:00Z",
            "parts": [{"type": "text", "text": "rewritten"}],
            "metadata": {"historySequence": 0},
        }))
        .unwrap()
    );
    fs::write(&chat_path, truncated).unwrap();
    let rewritten = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(rewritten.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(rewritten.failed, 0);

    let replay = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn released_v025_plain_cursor_preserves_primary_source_merged_ids_and_counts() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let (session_dir, merged) = write_released_v025_session(&root);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let imported_at = "2026-07-25T12:00:00Z".parse().unwrap();
    let (released_source_id, released_session_id, released_event_ids) =
        seed_released_v025_store(&store, &session_dir, &merged, imported_at);

    let migrated = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(migrated.imported_sessions, 0);
    assert_eq!(migrated.skipped_sessions, 1);
    assert_eq!(migrated.imported_events, 0);
    assert_eq!(migrated.skipped_events, 3);
    assert_eq!(migrated.failed, 1);

    let session = store.get_session(released_session_id).unwrap();
    assert_eq!(session.capture_source_id, Some(released_source_id));
    let source = store.get_capture_source(released_source_id).unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(
            session_dir
                .join("chat.jsonl")
                .display()
                .to_string()
                .as_str()
        )
    );
    let migrated_events = store.events_for_session(released_session_id).unwrap();
    assert_eq!(
        migrated_events
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        released_event_ids
    );
    assert_eq!(
        migrated_events
            .iter()
            .map(|event| event.sync.metadata["provider_event_index"]
                .as_u64()
                .unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );

    let unchanged = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(unchanged.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(unchanged.imported_sessions, 0);
    assert_eq!(unchanged.skipped_sessions, 1);
    assert_eq!(unchanged.imported_events, 0);
    assert_eq!(unchanged.skipped_events, 3);
    assert_eq!(unchanged.failed, 1);

    let appended_value = released_message(
        "message-4",
        "user",
        4,
        json!([{"type": "text", "text": "appended"}]),
    );
    let mut chat = OpenOptions::new()
        .append(true)
        .open(session_dir.join("chat.jsonl"))
        .unwrap();
    writeln!(chat, "{appended_value}").unwrap();
    chat.sync_all().unwrap();
    drop(chat);

    let appended = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(appended.imported_sessions, 0);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(appended.failed, 0);
    let expected_append_id = provider_source_event_uuid(released_source_id, 4);
    let after_append = store.events_for_session(released_session_id).unwrap();
    assert_eq!(after_append.len(), 5);
    assert_eq!(
        &after_append[..4]
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        &released_event_ids
    );
    assert_eq!(after_append[4].id, expected_append_id);

    let append_replay =
        import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(append_replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(append_replay.imported_events, 0);
    assert_eq!(append_replay.skipped_events, 4);
    assert_eq!(append_replay.failed, 1);
}

#[test]
fn imported_large_messages_resolve_after_append_and_fail_closed_after_mutation() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_dir = write_session(&root, 0);
    let chat_body = format!("{}-chat", "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 32));
    let partial_body = format!(
        "{}-partial",
        "p".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 32)
    );
    let chat_message = json!({
        "id": "large-chat",
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:01:00Z",
        "parts": [{"type": "text", "text": chat_body}],
        "metadata": {"historySequence": 0},
    });
    let failed_output = json!({
        "id": "failed-output",
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:02:00Z",
        "parts": [{
            "type": "dynamic-tool",
            "toolCallId": "failed-call",
            "toolName": "shell",
            "state": "output-available",
            "success": false,
            "output": "diagnostic only",
        }],
        "metadata": {"historySequence": 1},
    });
    fs::write(
        session_dir.join("chat.jsonl"),
        format!("{chat_message}\n{failed_output}\n"),
    )
    .unwrap();
    let partial_message = json!({
        "id": "large-partial",
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:03:00Z",
        "parts": [{"type": "text", "text": partial_body}],
        "metadata": {"historySequence": 2},
    });
    fs::write(
        session_dir.join("partial.json"),
        serde_json::to_vec(&partial_message).unwrap(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(first.imported_events, 3);
    let session = store
        .session_by_external_session(CaptureProvider::Mux, "session-1")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let chat_event_id = events
        .iter()
        .find(|event| {
            event.event_type == EventType::Message
                && event.sync.metadata["metadata"]["is_partial"] == json!(false)
        })
        .unwrap()
        .id;
    let partial_event_id = events
        .iter()
        .find(|event| {
            event.event_type == EventType::Message
                && event.sync.metadata["metadata"]["is_partial"] == json!(true)
        })
        .unwrap()
        .id;
    let output_event = events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    assert!(output_event
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());

    let appended = json!({
        "id": "after-large",
        "workspaceId": "session-1",
        "role": "user",
        "createdAt": "2026-07-25T11:04:00Z",
        "parts": [{"type": "text", "text": "after append"}],
        "metadata": {"historySequence": 3},
    });
    let mut chat = OpenOptions::new()
        .append(true)
        .open(session_dir.join("chat.jsonl"))
        .unwrap();
    writeln!(chat, "{appended}").unwrap();
    chat.sync_all().unwrap();
    drop(chat);
    let append_summary =
        import_mux_native_path(&root, &mut store, context(&root), options()).unwrap();
    assert_eq!(append_summary.imported_events, 1);

    let chat_event = store.get_event(chat_event_id).unwrap();
    let partial_event = store.get_event(partial_event_id).unwrap();
    let chat_request = imported_message_request(&store, &chat_event);
    let partial_request = imported_message_request(&store, &partial_event);
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(JsonlCompleteContentResolver::new());
    assert_eq!(
        registry
            .resolve(std::slice::from_ref(&chat_request))
            .unwrap()[0]
            .text,
        chat_body
    );
    assert_eq!(
        registry
            .resolve(std::slice::from_ref(&partial_request))
            .unwrap()[0]
            .text,
        partial_body
    );

    let changed_chat_body = format!("{}-evil", "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 32));
    let changed_chat = json!({
        "id": "large-chat",
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:01:00Z",
        "parts": [{"type": "text", "text": changed_chat_body}],
        "metadata": {"historySequence": 0},
    });
    let current_chat = fs::read_to_string(session_dir.join("chat.jsonl")).unwrap();
    let remainder = current_chat.split_once('\n').unwrap().1;
    fs::write(
        session_dir.join("chat.jsonl"),
        format!("{changed_chat}\n{remainder}"),
    )
    .unwrap();
    assert_eq!(
        registry
            .resolve(std::slice::from_ref(&chat_request))
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );

    let changed_partial_body = format!(
        "{}-changed",
        "p".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 32)
    );
    let changed_partial = json!({
        "id": "large-partial",
        "workspaceId": "session-1",
        "role": "assistant",
        "createdAt": "2026-07-25T11:03:00Z",
        "parts": [{"type": "text", "text": changed_partial_body}],
        "metadata": {"historySequence": 2},
    });
    fs::write(
        session_dir.join("partial.json"),
        serde_json::to_vec(&changed_partial).unwrap(),
    )
    .unwrap();
    assert_eq!(
        registry
            .resolve(std::slice::from_ref(&partial_request))
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );
}
