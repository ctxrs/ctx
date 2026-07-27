use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::{Store, StoreError};
use serde_json::json;
use tempfile::TempDir;

use crate::{
    complete_content::{
        jsonl::JsonlCompleteContentResolver, AuthorizedSourceRoute, CompleteContentErrorKind,
        CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
        CompleteMessageRequest, SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    summaries::MAX_RETAINED_PROVIDER_FAILURES,
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportWorkResult, MISTRAL_VIBE_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{import_mistral_vibe_nativepath, native_path::source_cursor_stream};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    messages: PathBuf,
    database: PathBuf,
}

fn fixture(lines: &[serde_json::Value]) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vibe");
    let messages = write_session(&root, "session-1", "mistral-nativepath-session", lines);
    let database = temp.path().join("history.sqlite");
    Fixture {
        _temp: temp,
        root,
        messages,
        database,
    }
}

fn write_session(
    root: &std::path::Path,
    directory: &str,
    session_id: &str,
    lines: &[serde_json::Value],
) -> PathBuf {
    let session = root.join(directory);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        json!({
            "session_id": session_id,
            "start_time": "2026-07-25T12:00:00Z",
            "environment": {"working_directory": "/workspace"},
        })
        .to_string(),
    )
    .unwrap();
    let mut transcript = lines
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    transcript.push('\n');
    let messages = session.join("messages.jsonl");
    fs::write(&messages, transcript).unwrap();
    messages
}

fn context(root: &std::path::Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "mistral-nativepath-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-25T13:00:00Z".parse::<DateTime<Utc>>().unwrap(),
    }
}

fn import(fixture: &Fixture, store: &mut Store) -> crate::Result<crate::ProviderImportSummary> {
    import_with_profile(fixture, store, ImportProfile::CoreOnly)
}

fn import_with_profile(
    fixture: &Fixture,
    store: &mut Store,
    import_profile: ImportProfile,
) -> crate::Result<crate::ProviderImportSummary> {
    import_mistral_vibe_nativepath(
        &fixture.root,
        store,
        context(&fixture.root),
        ProviderImportOptions {
            import_profile,
            ..ProviderImportOptions::default()
        },
    )
}

#[test]
fn nativepath_core_is_idempotent_resumes_append_and_elides_success_output() {
    let fixture = fixture(&[
        json!({
            "message_id": "message-1",
            "role": "user",
            "content": "hello",
            "timestamp": "2026-07-25T12:00:01Z",
        }),
        json!({
            "message_id": "call-1",
            "role": "assistant",
            "tool_calls": [{"id": "call-1", "function": {"name": "shell"}}],
            "timestamp": "2026-07-25T12:00:02Z",
        }),
        json!({
            "message_id": "result-success",
            "role": "tool",
            "tool_call_id": "call-1",
            "name": "shell",
            "status": "success",
            "content": "SUCCESS_BODY_MUST_NOT_ENTER_CORE",
            "timestamp": "2026-07-25T12:00:03Z",
        }),
        json!({
            "message_id": "result-failure",
            "role": "tool",
            "tool_call_id": "call-2",
            "name": "shell",
            "status": "error",
            "is_error": true,
            "content": "bounded failure evidence",
            "timestamp": "2026-07-25T12:00:04Z",
        }),
    ]);
    let mut store = Store::open(&fixture.database).unwrap();

    let initial = import(&fixture, &mut store).unwrap();
    assert_eq!(initial.imported_sessions, 1, "{:?}", initial.failures);
    let session = store.list_sessions().unwrap().pop().unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let event_types = events
        .iter()
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(
        initial.imported_events, 3,
        "types={event_types:?} failures={:?}",
        initial.failures
    );
    assert_eq!(events.len(), 3, "types={event_types:?}");
    let persisted = serde_json::to_string(&events).unwrap();
    assert!(!persisted.contains("SUCCESS_BODY_MUST_NOT_ENTER_CORE"));
    assert!(!persisted.contains("bounded failure evidence"));
    assert!(!persisted.contains("result_content_ref"));
    let failed_output = events
        .iter()
        .find(|event| {
            matches!(
                event.event_type,
                ctx_history_core::EventType::ToolOutput
                    | ctx_history_core::EventType::CommandOutput
            )
        })
        .unwrap();
    assert!(failed_output
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());

    let stream = source_cursor_stream(&fixture.messages).unwrap();
    let first_cursor = store
        .get_sync_cursor(None, "mistral-nativepath-machine", &stream)
        .unwrap()
        .unwrap()
        .cursor;
    let replay = import(&fixture, &mut store).unwrap();
    assert_eq!(replay.imported_events, 0, "{:?}", replay.failures);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        store
            .get_sync_cursor(None, "mistral-nativepath-machine", &stream)
            .unwrap()
            .unwrap()
            .cursor,
        first_cursor
    );

    let appended = json!({
        "message_id": "message-2",
        "role": "assistant",
        "content": "append",
        "timestamp": "2026-07-25T12:00:05Z",
    });
    let mut transcript = fs::read_to_string(&fixture.messages).unwrap();
    transcript.push_str(&appended.to_string());
    transcript.push('\n');
    fs::write(&fixture.messages, &transcript).unwrap();
    let append = import(&fixture, &mut store).unwrap();
    assert_eq!(append.imported_events, 1, "{:?}", append.failures);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 4);
}

#[test]
fn nativepath_rejects_duplicate_session_ids_before_publishing_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vibe");
    for (directory, content) in [("session-a", "first"), ("session-b", "second")] {
        write_session(
            &root,
            directory,
            "duplicate-session",
            &[json!({
                "message_id": format!("{directory}-message"),
                "role": "user",
                "content": content,
            })],
        );
    }
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let error = import_mistral_vibe_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::CaptureError::InvalidProviderTranscriptPath {
            reason: "Mistral Vibe history root contains duplicate session IDs",
            ..
        }
    ));
    assert!(store.list_capture_sources().unwrap().is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn nativepath_keeps_same_session_ids_independent_across_roots_and_pro_restart() {
    let first = fixture(&[
        json!({
            "message_id": "first-message",
            "role": "user",
            "content": "first root",
        }),
        json!({
            "message_id": "first-output",
            "role": "tool",
            "tool_call_id": "first-call",
            "status": "success",
            "content": "FIRST_ROOT_OUTPUT",
        }),
    ]);
    let second = fixture(&[
        json!({
            "message_id": "second-message",
            "role": "user",
            "content": "second root",
        }),
        json!({
            "message_id": "second-output",
            "role": "tool",
            "tool_call_id": "second-call",
            "status": "success",
            "content": "SECOND_ROOT_OUTPUT",
        }),
    ]);
    let database = first._temp.path().join("shared.sqlite");
    let mut store = Store::open(&database).unwrap();
    let sink = Arc::new(RecordingSink::new(database.clone()));

    for fixture in [&first, &second] {
        let summary =
            import_with_profile(fixture, &mut store, ImportProfile::CoreAndPro(sink.clone()))
                .unwrap();
        assert_eq!(
            summary.work_result(),
            ProviderImportWorkResult::Changed,
            "{:?}",
            summary.failures
        );
    }
    assert_eq!(store.list_capture_sources().unwrap().len(), 2);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(sink.progress.lock().unwrap().len(), 2);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    let mut contents = sink.contents.lock().unwrap().clone();
    contents.sort();
    assert_eq!(
        contents,
        [
            b"FIRST_ROOT_OUTPUT".to_vec(),
            b"SECOND_ROOT_OUTPUT".to_vec()
        ]
    );
    let pages = sink.pages.load(Ordering::SeqCst);

    drop(store);
    let mut reopened = Store::open(&database).unwrap();
    for fixture in [&first, &second] {
        let replay = import_with_profile(
            fixture,
            &mut reopened,
            ImportProfile::ProReplayOnly(sink.clone()),
        )
        .unwrap();
        assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    }
    assert_eq!(sink.progress.lock().unwrap().len(), 2);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages);
    assert_eq!(reopened.list_capture_sources().unwrap().len(), 2);
    assert_eq!(reopened.list_sessions().unwrap().len(), 2);
}

#[test]
fn nativepath_reconciled_relocation_keeps_core_and_pro_authority() {
    let fixture = fixture(&[
        json!({
            "message_id": "relocated-message",
            "role": "user",
            "content": "relocated root",
        }),
        json!({
            "message_id": "relocated-output",
            "role": "tool",
            "tool_call_id": "relocated-call",
            "status": "success",
            "content": "RELOCATED_ROOT_OUTPUT",
        }),
    ]);
    let mut store = Store::open(&fixture.database).unwrap();
    let sink = Arc::new(RecordingSink::new(fixture.database.clone()));
    import_with_profile(
        &fixture,
        &mut store,
        ImportProfile::CoreAndPro(sink.clone()),
    )
    .unwrap();
    let original_source = store.list_capture_sources().unwrap().pop().unwrap();
    let original_session = store.list_sessions().unwrap().pop().unwrap();
    let original_event_ids = store
        .events_for_session(original_session.id)
        .unwrap()
        .into_iter()
        .map(|event| event.id)
        .collect::<Vec<_>>();
    let original_progress = sink
        .progress
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let original_pages = sink.pages.load(Ordering::SeqCst);

    let moved_root = fixture._temp.path().join("vibe-moved");
    fs::rename(&fixture.root, &moved_root).unwrap();
    let moved = import_mistral_vibe_nativepath(
        &moved_root,
        &mut store,
        context(&moved_root),
        ProviderImportOptions {
            import_profile: ImportProfile::CoreAndPro(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(moved.work_result(), ProviderImportWorkResult::Changed);

    let relocated_source = store.list_capture_sources().unwrap().pop().unwrap();
    assert_eq!(relocated_source.id, original_source.id);
    assert_eq!(
        relocated_source.descriptor.source_identity,
        original_source.descriptor.source_identity
    );
    assert_eq!(
        relocated_source.descriptor.source_root.as_deref(),
        Some(moved_root.to_str().unwrap())
    );
    let relocated_session = store.list_sessions().unwrap().pop().unwrap();
    assert_eq!(relocated_session.id, original_session.id);
    assert_eq!(
        store
            .events_for_session(relocated_session.id)
            .unwrap()
            .into_iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        original_event_ids
    );
    assert_eq!(
        sink.progress
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        original_progress
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), original_pages);

    drop(store);
    let mut reopened = Store::open(&fixture.database).unwrap();
    let replay = import_mistral_vibe_nativepath(
        &moved_root,
        &mut reopened,
        context(&moved_root),
        ProviderImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), original_pages);
}

#[test]
fn nativepath_rejects_invalid_shapes_and_replays_bounded_details_on_noop() {
    let mut lines = (0..MAX_RETAINED_PROVIDER_FAILURES + 6)
        .map(|index| match index % 3 {
            0 => serde_json::Value::Null,
            1 => json!([]),
            _ => json!({}),
        })
        .collect::<Vec<_>>();
    lines.push(json!({
        "message_id": "retained-after-invalid",
        "role": "future_role",
        "content": "forward-compatible valid content",
    }));
    let fixture = fixture(&lines);
    let mut store = Store::open(&fixture.database).unwrap();

    let first = import(&fixture, &mut store).unwrap();
    assert_eq!(first.failed, MAX_RETAINED_PROVIDER_FAILURES + 6);
    assert_eq!(first.failures.len(), MAX_RETAINED_PROVIDER_FAILURES);
    assert_eq!(first.imported_events, 1);
    let expected_failures = first.failures.clone();
    let noop = import(&fixture, &mut store).unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(noop.failed, first.failed);
    assert_eq!(noop.failures, expected_failures);

    drop(store);
    let mut reopened = Store::open(&fixture.database).unwrap();
    let restart = import(&fixture, &mut reopened).unwrap();
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(restart.failed, first.failed);
    assert_eq!(restart.failures, expected_failures);
}

#[test]
fn nativepath_all_invalid_rows_have_no_useful_committed_content() {
    let fixture = fixture(&[
        serde_json::Value::Null,
        json!(42),
        json!([]),
        json!({}),
        json!({"role": "assistant"}),
        json!({"role": "tool"}),
    ]);
    let mut store = Store::open(&fixture.database).unwrap();

    let summary = import(&fixture, &mut store).unwrap();
    assert_eq!(summary.failed, 6);
    assert_eq!(summary.failures.len(), 6);
    assert!(!summary.has_accepted_content());
    let session = store.list_sessions().unwrap().pop().unwrap();
    assert!(store.events_for_session(session.id).unwrap().is_empty());
}

#[test]
fn nativepath_direct_file_uses_session_root_for_exact_long_message_recovery() {
    let long_message = "Mistral direct-file exact body 雪\n".repeat(700);
    assert!(long_message.chars().count() > PROVIDER_MAX_TEXT_CHARS);
    let fixture = fixture(&[json!({
        "message_id": "direct-long-message",
        "role": "assistant",
        "content": long_message,
    })]);
    let mut store = Store::open(&fixture.database).unwrap();

    let summary = import_mistral_vibe_nativepath(
        &fixture.messages,
        &mut store,
        context(&fixture.messages),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.imported_events, 1, "{:?}", summary.failures);
    let source = store.list_capture_sources().unwrap().pop().unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(
            fs::canonicalize(&fixture.messages)
                .unwrap()
                .to_str()
                .unwrap()
        )
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(
            fs::canonicalize(fixture.messages.parent().unwrap())
                .unwrap()
                .to_str()
                .unwrap()
        )
    );

    let request = complete_message_request(&store);
    let recovered = JsonlCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(recovered[0].text, long_message);

    fs::write(
        fixture.messages.parent().unwrap().join("meta.json"),
        json!({
            "session_id": "mutated-direct-session",
            "start_time": "2026-07-25T12:00:00Z",
        })
        .to_string(),
    )
    .unwrap();
    let error = JsonlCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

fn complete_message_request(store: &Store) -> CompleteMessageRequest {
    let session = store.list_sessions().unwrap().pop().unwrap();
    let event = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .find(|event| {
            event
                .sync
                .metadata
                .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
                .is_some_and(serde_json::Value::is_object)
        })
        .unwrap();
    let source = store
        .get_capture_source(event.capture_source_id.unwrap())
        .unwrap();
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: source.id,
                provider: CaptureProvider::MistralVibe,
                source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: PathBuf::from(source.descriptor.raw_source_path.clone().unwrap()),
                source_root: source.descriptor.source_root.clone().map(PathBuf::from),
                source_identity: source.descriptor.source_identity.clone(),
                source_snapshot: SourceSnapshot::default(),
            },
            event.id,
        )
        .unwrap();
    CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::MistralVibe,
        source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: locator.content_profile().to_owned(),
        source_locator: locator.source_locator(),
        provider_session_id: session.external_session_id,
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
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    }
}

#[test]
fn nativepath_holds_incomplete_tail_and_retires_a_disappeared_root() {
    let fixture = fixture(&[json!({
        "message_id": "message-1",
        "role": "user",
        "content": "complete",
    })]);
    let mut store = Store::open(&fixture.database).unwrap();
    import(&fixture, &mut store).unwrap();
    let session = store.list_sessions().unwrap().pop().unwrap();
    let event_id = store.events_for_session(session.id).unwrap()[0].id;

    let partial = json!({
        "message_id": "message-2",
        "role": "assistant",
        "content": "complete after newline",
    })
    .to_string();
    let mut transcript = fs::read_to_string(&fixture.messages).unwrap();
    transcript.push_str(&partial);
    fs::write(&fixture.messages, &transcript).unwrap();
    let incomplete = import(&fixture, &mut store).unwrap();
    assert_eq!(incomplete.imported_events, 0, "{:?}", incomplete.failures);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    transcript.push('\n');
    fs::write(&fixture.messages, transcript).unwrap();
    let completed = import(&fixture, &mut store).unwrap();
    assert_eq!(completed.imported_events, 1, "{:?}", completed.failures);
    store
        .authorized_source_route_for_event(event_id)
        .expect("live Mistral source route must remain authorized");

    fs::remove_dir_all(&fixture.root).unwrap();
    let retired = import(&fixture, &mut store).unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(matches!(
        store.authorized_source_route_for_event(event_id),
        Err(StoreError::AuthorizedSourceRouteUnavailable { .. })
    ));
    let retry = import(&fixture, &mut store).unwrap();
    assert_eq!(retry.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn nativepath_reconciles_rewrite_truncation_and_live_replacement() {
    let fixture = fixture(&[json!({
        "message_id": "original-1",
        "role": "user",
        "content": "original",
    })]);
    let mut store = Store::open(&fixture.database).unwrap();
    import(&fixture, &mut store).unwrap();

    fs::write(
        &fixture.messages,
        format!(
            "{}\n{}\n",
            json!({
                "message_id": "rewrite-1",
                "role": "user",
                "content": "rewrite first",
            }),
            json!({
                "message_id": "rewrite-2",
                "role": "assistant",
                "content": "rewrite second",
            }),
        ),
    )
    .unwrap();
    let rewrite = import(&fixture, &mut store).unwrap();
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store
        .search_event_hits("rewrite second", 10)
        .unwrap()
        .is_empty());

    fs::write(
        &fixture.messages,
        format!(
            "{}\n",
            json!({
                "message_id": "truncate-1",
                "role": "user",
                "content": "truncated source",
            })
        ),
    )
    .unwrap();
    let truncation = import(&fixture, &mut store).unwrap();
    assert_eq!(truncation.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store
        .search_event_hits("truncated source", 10)
        .unwrap()
        .is_empty());

    fs::remove_file(&fixture.messages).unwrap();
    fs::write(
        &fixture.messages,
        format!(
            "{}\n",
            json!({
                "message_id": "replacement-1",
                "role": "assistant",
                "content": "replacement source",
            })
        ),
    )
    .unwrap();
    let replacement = import(&fixture, &mut store).unwrap();
    assert_eq!(replacement.work_result(), ProviderImportWorkResult::Changed);
    assert!(!store
        .search_event_hits("replacement source", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn nativepath_replays_output_after_later_pro_activation_without_touching_core() {
    const OUTPUT: &str = "MISTRAL_SUCCESS_OUTPUT_ONLY_IN_PRO";
    let fixture = fixture(&[
        json!({
            "message_id": "message-1",
            "role": "user",
            "content": "core first",
        }),
        json!({
            "message_id": "result-success",
            "role": "tool",
            "tool_call_id": "call-1",
            "name": "read_file",
            "status": "success",
            "content": OUTPUT,
        }),
    ]);
    let mut store = Store::open(&fixture.database).unwrap();
    let core = import(&fixture, &mut store).unwrap();
    assert_eq!(core.imported_events, 1, "{:?}", core.failures);
    let session = store.list_sessions().unwrap().pop().unwrap();
    assert!(
        !serde_json::to_string(&store.events_for_session(session.id).unwrap())
            .unwrap()
            .contains(OUTPUT)
    );

    let sink = Arc::new(RecordingSink::new(fixture.database.clone()));
    let replay = import_with_profile(
        &fixture,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [OUTPUT.as_bytes()]
    );
    let pages = sink.pages.load(Ordering::SeqCst);
    import_with_profile(
        &fixture,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages);
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
            pages: AtomicUsize::new(0),
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
        "mistral-nativepath-test-v1"
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
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
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
