use std::fs::FileTimes;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::SystemTime,
};

use chrono::{TimeZone, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;

use super::*;
use crate::{
    complete_content::{
        jsonl::JsonlCompleteContentResolver, AuthorizedSourceRoute, CompleteContentErrorKind,
        CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
        CompleteMessageRequest, SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1,
    },
    native_source::NativePosition,
    provider::importer::BoundedParserCheckpoint,
    ImportProfile, ProOutputMaterializationPage, ProOutputPageResult, ProviderAdapterContext,
    ProviderImportOptions,
};

const FIXTURE_INDEX: &[u8] = include_bytes!(
    "../../../../../../../tests/fixtures/provider-history/junie/sessions/index.jsonl"
);
const FIXTURE_EVENTS: &[u8] = include_bytes!(
    "../../../../../../../tests/fixtures/provider-history/junie/sessions/session-260607-100000-acme/events.jsonl"
);

fn write_fixture(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("materialized fixture");
}

fn materialized_fixture_events() -> (tempfile::TempDir, PathBuf) {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let path = temp.path().join("events.jsonl");
    write_fixture(&path, FIXTURE_EVENTS);
    (temp, path)
}

fn initial_frontier() -> Frontier {
    let started = Utc
        .timestamp_millis_opt(1_783_339_200_000)
        .single()
        .expect("fixture timestamp");
    Frontier {
        offset: 0,
        next_ordinal: 0,
        next_event_index: 0,
        prefix_sha256: Sha256::digest([]).into(),
        state: RuntimeState {
            started_at_ms: started.timestamp_millis(),
            last_ts_ms: started.timestamp_millis(),
            ended_at_ms: None,
            title: Some("Junie fixture task".to_owned()),
            cwd: Some("/workspace/junie-fixture".to_owned()),
            saw_supported_event: false,
        },
        pending: None,
    }
}

fn released_jsonl_position(path: &Path, offset: u64) -> NativePosition {
    let bytes = fs::read(path).expect("released cursor source");
    let proof_len = offset.min(64 * 1024);
    let proof_start = usize::try_from(offset - proof_len).expect("proof start");
    let proof_end = usize::try_from(offset).expect("proof end");
    let mut digest = Sha256::new();
    digest.update(b"ctx-jsonl-append-boundary-sha256-v1\0");
    digest.update(offset.to_be_bytes());
    digest.update(
        u32::try_from(proof_len)
            .expect("bounded proof")
            .to_be_bytes(),
    );
    digest.update(&bytes[proof_start..proof_end]);
    let mut encoded = Vec::with_capacity(56);
    encoded.extend_from_slice(b"CTXJLBP\0");
    encoded.extend_from_slice(&[1, 1, 0, 0]);
    encoded.extend_from_slice(&offset.to_be_bytes());
    encoded.extend_from_slice(&u32::try_from(proof_len).unwrap().to_be_bytes());
    encoded.extend_from_slice(&digest.finalize());
    NativePosition::new("jsonl-byte-boundary-v1", encoded).expect("released JSONL position")
}

fn released_junie_cursor(
    path: &Path,
    source_revision: &str,
    offset: u64,
    next_ordinal: u64,
    provider_event_index: u64,
    source_ended: bool,
) -> String {
    let timestamp = Utc
        .timestamp_millis_opt(1_783_339_200_000)
        .single()
        .unwrap();
    let checkpoint = BoundedParserCheckpoint::from_serializable(&json!({
        "next_ordinal": next_ordinal,
        "next_line_number": next_ordinal,
        "provider_event_index": provider_event_index,
        "started_at": timestamp,
        "last_ts": timestamp,
        "ended_at": null,
        "title_anchor": null,
        "cwd_anchor": null,
        "saw_supported_event": provider_event_index != 0,
        "metadata_dirty": false,
        "source_ended": source_ended,
        "auxiliary_revision": 0,
        "accepted_captures": next_ordinal,
        "accepted_events": provider_event_index,
        "accepted_file_touches": 0,
        "structural_rejections": 0,
        "rejected_records": 0,
        "failures": [],
    }))
    .unwrap();
    CertifiedProviderCursor::new(
        source_revision,
        2,
        5,
        released_jsonl_position(path, offset),
        checkpoint,
    )
    .unwrap()
    .encode()
    .unwrap()
}

fn simple_junie_tree(root: &Path, session_id: &str, contents: &[u8]) -> PathBuf {
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1_783_339_200_000_i64,
                "taskName": "Junie NativePath test",
                "projectDir": "/workspace/junie",
            })
        ),
    )
    .unwrap();
    let events_path = session_dir.join("events.jsonl");
    fs::write(&events_path, contents).unwrap();
    events_path
}

fn test_context(root: &Path, machine_id: &str) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: machine_id.to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: Utc
            .timestamp_millis_opt(1_783_339_500_000)
            .single()
            .expect("import timestamp"),
    }
}

#[test]
fn successful_output_is_transient_and_absent_from_core_rows() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    assert_eq!(first.rows.len(), 1);
    assert!(first.outputs.is_empty());
    assert!(!first.terminal);

    let second_frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let second = parse_turn(&path, &second_frontier).expect("terminal safe turn");
    assert!(second.terminal);
    assert_eq!(second.outputs.len(), 1);
    assert_eq!(
        second.outputs[0].content,
        b"JUNIE_TERMINAL_OUTPUT saffron harbor"
    );
    assert!(second.rows.iter().all(|row| {
        !row.text.contains("JUNIE_TERMINAL_OUTPUT")
            && !row.body.to_string().contains("JUNIE_TERMINAL_OUTPUT")
    }));
}

#[test]
fn output_only_event_indexes_still_advance_the_core_frontier() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let second = parse_turn(
        &path,
        &Frontier {
            offset: first.end_offset,
            next_ordinal: first.end_ordinal,
            next_event_index: first.next_event_index,
            prefix_sha256: first.after_prefix_sha256,
            state: first.after_state,
            pending: None,
        },
    )
    .expect("terminal safe turn");
    assert_eq!(
        second.next_event_index - second.base_event_index,
        second.rows.len() as u64 + second.outputs.len() as u64
    );
}

#[test]
fn pending_output_page_replay_is_bound_to_the_exact_turn() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let parsed = parse_turn(&path, &frontier).expect("terminal safe turn");
    let mut pending_frontier = frontier;
    pending_frontier.pending = Some(PendingTurn {
        start_offset: parsed.start_offset,
        end_offset: parsed.end_offset,
        start_ordinal: parsed.start_ordinal,
        end_ordinal: parsed.end_ordinal,
        base_event_index: parsed.base_event_index,
        next_event_index: parsed.next_event_index,
        next_row: 0,
        row_count: parsed.outputs.len() as u32,
        turn_sha256: parsed.turn_sha256,
        terminal: parsed.terminal,
        after_state: parsed.after_state.clone(),
        after_prefix_sha256: parsed.after_prefix_sha256,
    });
    validate_output_pending_replay(&pending_frontier, &parsed).expect("exact replay");
    pending_frontier
        .pending
        .as_mut()
        .expect("pending")
        .turn_sha256[0] ^= 1;
    assert!(matches!(
        validate_output_pending_replay(&pending_frontier, &parsed),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn append_after_a_pending_terminal_turn_does_not_change_its_replay() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let path = temp.path().join("events.jsonl");
    write_fixture(&path, FIXTURE_EVENTS);
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let turn_frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let terminal = parse_turn(&path, &turn_frontier).expect("terminal turn");
    let mut pending_frontier = turn_frontier;
    pending_frontier.pending = Some(PendingTurn {
        start_offset: terminal.start_offset,
        end_offset: terminal.end_offset,
        start_ordinal: terminal.start_ordinal,
        end_ordinal: terminal.end_ordinal,
        base_event_index: terminal.base_event_index,
        next_event_index: terminal.next_event_index,
        next_row: 1,
        row_count: terminal.rows.len() as u32,
        turn_sha256: terminal.turn_sha256,
        terminal: true,
        after_state: terminal.after_state.clone(),
        after_prefix_sha256: terminal.after_prefix_sha256,
    });
    let mut append = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append source");
    writeln!(
        append,
        "{}",
        json!({"kind": "UserPromptEvent", "prompt": "appended after pending page"})
    )
    .expect("append prompt");
    drop(append);

    let replay = parse_turn(&path, &pending_frontier).expect("bounded pending replay");
    validate_pending_replay(&pending_frontier, &replay).expect("same pending turn");
    assert_eq!(replay.end_offset, terminal.end_offset);
    assert_eq!(replay.next_event_index, terminal.next_event_index);
}

#[test]
fn message_locator_reopens_exact_long_body_and_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-message-locator";
    let long_prompt = "JUNIE_LONG_USER snowman ☃ quoted body\n".repeat(600);
    let source = format!(
        "{}\n",
        json!({"kind": "UserPromptEvent", "prompt": long_prompt})
    );
    let events_path = simple_junie_tree(&root, session_id, source.as_bytes());
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let summary = import_junie_nativepath(
        &root,
        &mut store,
        test_context(&root, "junie-message-locator-machine"),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    let session = store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .unwrap()
        .unwrap();
    let capture_source = store
        .get_capture_source(session.capture_source_id.unwrap())
        .unwrap();
    let event = store.events_for_session(session.id).unwrap().remove(0);
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
    let access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: capture_source.id,
                provider: CaptureProvider::Junie,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: events_path.clone(),
                source_root: Some(root.clone()),
                source_identity: capture_source.descriptor.source_identity.clone(),
                source_snapshot: SourceSnapshot {
                    size_bytes: Some(source.len() as u64),
                    modified_at_ms: None,
                    sha256: None,
                },
            },
            event.id,
        )
        .unwrap();
    let request = CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
        source_access: access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: locator.content_profile().to_owned(),
        source_locator: locator.source_locator(),
        provider_session_id: Some(session_id.to_owned()),
        source_record_ordinal: event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap(),
        source_record_subrecord_index: event.sync.metadata["source_record_subrecord_index"]
            .as_u64()
            .unwrap() as u32,
        expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(locator.native_record_id().to_owned()),
        expected_record_digest: Some(locator.record_sha256().clone()),
        expected_content_ref: Some(locator.content_ref().clone()),
        indexed_text: event.payload["body"]["text"].as_str().unwrap().to_owned(),
        indexed_limit_chars: crate::PROVIDER_MAX_TEXT_CHARS,
    };
    let resolver = JsonlCompleteContentResolver::new();
    let resolved =
        CompleteContentResolver::resolve(&resolver, std::slice::from_ref(&request)).unwrap();
    assert_eq!(resolved[0].text, long_prompt);

    fs::write(&events_path, b"{}\n").unwrap();
    assert_eq!(
        CompleteContentResolver::resolve(&resolver, &[request])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceRecordMissing
    );
}

#[test]
fn released_cursor_proves_append_prefix_and_rewrites_into_a_replacement_generation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-released-cursor";
    let first_line = format!(
        "{}\n",
        json!({"kind": "UserPromptEvent", "prompt": "released-old"})
    );
    let events_path = simple_junie_tree(&root, session_id, first_line.as_bytes());
    let machine_id = "junie-released-cursor-machine";
    let context = test_context(&root, machine_id);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let locator_identity = provider_path_identity(&events_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let mut stored = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    let native = JunieStoreCursor::decode(committed.provider_cursor()).unwrap();
    stored.cursor = released_junie_cursor(
        &events_path,
        &native.source_revision,
        first_line.len() as u64,
        1,
        1,
        true,
    );
    store.upsert_sync_cursor(&stored).unwrap();

    let mut append = fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap();
    writeln!(
        append,
        "{}",
        json!({"kind": "UserPromptEvent", "prompt": "released-appended"})
    )
    .unwrap();
    append.sync_all().unwrap();
    drop(append);
    let appended = import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(appended.imported_events, 1);
    let committed_after_append = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let appended_native = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(&committed_after_append.cursor)
            .unwrap()
            .provider_cursor(),
    )
    .unwrap();
    assert_eq!(appended_native.generation, 0);
    assert_eq!(appended_native.frontier.next_ordinal, 2);

    let original_modified: SystemTime = fs::metadata(&events_path).unwrap().modified().unwrap();
    let current_bytes = fs::read(&events_path).unwrap();
    let current_revision = appended_native.source_revision.clone();
    let mut released = committed_after_append;
    released.cursor = released_junie_cursor(
        &events_path,
        &current_revision,
        current_bytes.len() as u64,
        2,
        2,
        true,
    );
    store.upsert_sync_cursor(&released).unwrap();
    let rewritten = String::from_utf8(current_bytes)
        .unwrap()
        .replace("released-old", "released-new");
    fs::write(&events_path, rewritten.as_bytes()).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&events_path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_modified))
        .unwrap();
    let observation = JunieSessionObservation::read(&discover(&root).unwrap().sessions[0]).unwrap();
    assert_eq!(observation.source_revision(), current_revision);

    let replacement =
        import_junie_nativepath(&root, &mut store, context, ProviderImportOptions::default())
            .unwrap();
    assert_eq!(replacement.imported_events, 2);
    let replacement_native = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(
            &store
                .get_sync_cursor(None, machine_id, &stream)
                .unwrap()
                .unwrap()
                .cursor,
        )
        .unwrap()
        .provider_cursor(),
    )
    .unwrap();
    assert_eq!(replacement_native.generation, 1);
    let session = store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .unwrap()
        .unwrap();
    let replacement_rows = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.sync.metadata["nativepath_generation"] == 1)
        .collect::<Vec<_>>();
    assert_eq!(replacement_rows.len(), 2);
    let replacement_payload = serde_json::to_string(&replacement_rows).unwrap();
    assert!(replacement_payload.contains("released-new"));
    assert!(!replacement_payload.contains("released-old"));
}

#[test]
fn terminal_released_cursor_is_upgraded_by_an_empty_safe_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-terminal-released";
    let source = b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"released terminal\"}\n";
    let events_path = simple_junie_tree(&root, session_id, source);
    let machine_id = "junie-terminal-released-machine";
    let context = test_context(&root, machine_id);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &provider_path_identity(&events_path).unwrap(),
    );
    let mut stored = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let native = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(&stored.cursor)
            .unwrap()
            .provider_cursor(),
    )
    .unwrap();
    stored.cursor = released_junie_cursor(
        &events_path,
        &native.source_revision,
        source.len() as u64,
        1,
        1,
        true,
    );
    store.upsert_sync_cursor(&stored).unwrap();

    let upgraded = import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(upgraded.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(upgraded.imported_events, 0);
    let committed = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let upgraded_cursor = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(&committed.cursor)
            .unwrap()
            .provider_cursor(),
    )
    .unwrap();
    assert!(upgraded_cursor.terminal);
    assert_eq!(upgraded_cursor.generation, 0);
    assert_eq!(upgraded_cursor.frontier.offset, source.len() as u64);
    let session = store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    let replay =
        import_junie_nativepath(&root, &mut store, context, ProviderImportOptions::default())
            .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn released_cursor_is_retired_when_its_source_is_deleted_before_migration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-released-deletion";
    let source = b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"released deletion\"}\n";
    let events_path = simple_junie_tree(&root, session_id, source);
    let machine_id = "junie-released-deletion-machine";
    let context = test_context(&root, machine_id);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .unwrap()
        .unwrap();
    let event_id = store.events_for_session(session.id).unwrap()[0].id;
    let locator_identity = provider_path_identity(&events_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let mut stored = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let native = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(&stored.cursor)
            .unwrap()
            .provider_cursor(),
    )
    .unwrap();
    stored.cursor = released_junie_cursor(
        &events_path,
        &native.source_revision,
        source.len() as u64,
        1,
        1,
        true,
    );
    store.upsert_sync_cursor(&stored).unwrap();
    fs::remove_file(&events_path).unwrap();

    let retired = import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store.authorized_source_route_for_event(event_id).is_err());
    let retired_cursor = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(
            &store
                .get_sync_cursor(None, machine_id, &stream)
                .unwrap()
                .unwrap()
                .cursor,
        )
        .unwrap()
        .provider_cursor(),
    )
    .unwrap();
    assert!(retired_cursor.retired);
    assert!(retired_cursor.terminal);

    let replay =
        import_junie_nativepath(&root, &mut store, context, ProviderImportOptions::default())
            .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn rejection_only_terminal_file_publishes_a_bounded_safe_cursor() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-rejections";
    let malformed = "{malformed\n".repeat(24);
    let events_path = simple_junie_tree(&root, session_id, malformed.as_bytes());
    let machine_id = "junie-rejection-only-machine";
    let context = test_context(&root, machine_id);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let summary = import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 24);
    assert_eq!(summary.failures.len(), MAX_JUNIE_FAILURES);
    assert_eq!(summary.imported_events, 0);
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &provider_path_identity(&events_path).unwrap(),
    );
    let stored = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let cursor = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(&stored.cursor)
            .unwrap()
            .provider_cursor(),
    )
    .unwrap();
    assert!(cursor.terminal);
    assert_eq!(cursor.frontier.offset, malformed.len() as u64);
    assert_eq!(cursor.rejected_records, 24);
    assert!(store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .unwrap()
        .is_some());

    let replay =
        import_junie_nativepath(&root, &mut store, context, ProviderImportOptions::default())
            .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.failed, 0);
}

#[test]
fn malformed_index_rows_are_counted_with_bounded_failure_details() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-index-rejections";
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let mut index = "{malformed-index\n".repeat(24);
    index.push_str(&format!(
        "{}\n",
        json!({"sessionId": session_id, "taskName": "valid sibling"})
    ));
    fs::write(root.join("index.jsonl"), index).unwrap();
    fs::write(
        session_dir.join("events.jsonl"),
        b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"valid event\"}\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let summary = import_junie_nativepath(
        &root,
        &mut store,
        test_context(&root, "junie-index-rejections-machine"),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 24);
    assert_eq!(summary.failures.len(), MAX_JUNIE_FAILURES);
    assert!(summary
        .failures
        .iter()
        .all(|failure| failure.error == "Junie index row is not valid JSON"));
    assert_eq!(summary.imported_events, 1);
}

#[test]
fn persisted_junie_cursor_corruption_is_a_store_or_system_invariant() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let events_path = simple_junie_tree(
        &root,
        "session-junie-corrupt-cursor",
        b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"cursor test\"}\n",
    );
    let machine_id = "junie-corrupt-cursor-machine";
    let context = test_context(&root, machine_id);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &provider_path_identity(&events_path).unwrap(),
    );
    let mut stored = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    let original = stored.cursor.clone();
    let original_native = JunieStoreCursor::decode(
        decode_native_path_committed_cursor(&original)
            .unwrap()
            .provider_cursor(),
    )
    .unwrap();
    let mut envelope: Value = serde_json::from_str(&stored.cursor).unwrap();
    envelope["provider_cursor"] = Value::String("{}".to_owned());
    stored.cursor = serde_json::to_string(&envelope).unwrap();
    store.upsert_sync_cursor(&stored).unwrap();
    assert!(matches!(
        import_junie_nativepath(
            &root,
            &mut store,
            context.clone(),
            ProviderImportOptions::default()
        ),
        Err(CaptureError::SystemInvariant(_))
    ));

    stored.cursor = CertifiedProviderCursor::new(
        &original_native.source_revision,
        2,
        5,
        released_jsonl_position(&events_path, 0),
        BoundedParserCheckpoint::from_serializable(&json!({})).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store.upsert_sync_cursor(&stored).unwrap();
    assert!(matches!(
        import_junie_nativepath(
            &root,
            &mut store,
            context.clone(),
            ProviderImportOptions::default()
        ),
        Err(CaptureError::SystemInvariant(_))
    ));

    stored.cursor = original;
    let mut malformed_envelope: Value = serde_json::from_str(&stored.cursor).unwrap();
    malformed_envelope["version"] = Value::from(999);
    stored.cursor = serde_json::to_string(&malformed_envelope).unwrap();
    store.upsert_sync_cursor(&stored).unwrap();
    assert!(matches!(
        import_junie_nativepath(&root, &mut store, context, ProviderImportOptions::default()),
        Err(CaptureError::Store(_))
    ));
}

#[test]
fn native_store_path_is_idempotent_and_handles_append_rewrite_and_deletion() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let root = temp.path().join("sessions");
    let session_id = "session-260607-100000-acme";
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).expect("session directory");
    write_fixture(&root.join("index.jsonl"), FIXTURE_INDEX);
    let events_path = session_dir.join("events.jsonl");
    write_fixture(&events_path, FIXTURE_EVENTS);

    let context = ProviderAdapterContext {
        machine_id: "junie-nativepath-test-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: Utc
            .timestamp_millis_opt(1_783_339_500_000)
            .single()
            .expect("import timestamp"),
    };
    let options = ProviderImportOptions::default();
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let first = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("initial import");
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 4);
    let session = store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .expect("session query")
        .expect("Junie session");
    let events = store.events_for_session(session.id).expect("events");
    assert!(!events.iter().any(|event| {
        matches!(
            event.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        )
    }));
    assert!(!serde_json::to_string(&events)
        .expect("events JSON")
        .contains("JUNIE_TERMINAL_OUTPUT"));

    drop(store);
    let mut store = Store::open(&store_path).expect("reopened store");
    let after_restart =
        import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
            .expect("restart replay");
    assert_eq!(after_restart.work_result(), ProviderImportWorkResult::NoOp);

    let replay = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("idempotent replay");
    assert_eq!(replay.imported_events, 0);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        4
    );

    let mut append = fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .expect("append source");
    writeln!(
        append,
        "{}",
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_450_000_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "appended-result",
                "result": "JUNIE_APPENDED_RESULT"
            }}
        })
    )
    .expect("append result");
    writeln!(
        append,
        "{}",
        json!({"kind": "UserPromptEvent", "prompt": "JUNIE_APPENDED_USER"})
    )
    .expect("append prompt");
    append.sync_all().expect("sync append");
    drop(append);
    let appended = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("append import");
    assert_eq!(appended.imported_events, 2);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        6
    );

    fs::write(
        &events_path,
        b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"JUNIE_REPLACEMENT_USER\"}\n",
    )
    .expect("rewrite source");
    let rewritten = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("rewrite import");
    assert_eq!(rewritten.imported_events, 1);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        7
    );

    fs::remove_file(&events_path).expect("remove source");
    let retired = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("route retirement");
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    let retired_again =
        import_junie_nativepath(&root, &mut store, context, options).expect("retirement replay");
    assert_eq!(retired_again.work_result(), ProviderImportWorkResult::NoOp);
}

struct RecordingSink {
    store_path: PathBuf,
    fail: AtomicBool,
    behind: AtomicUsize,
    progress: Mutex<Option<ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail: bool) -> Self {
        Self {
            store_path,
            fail: AtomicBool::new(fail),
            behind: AtomicUsize::new(0),
            progress: Mutex::new(None),
            contents: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "junie-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().expect("progress").clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "Junie Pro output test failure",
            ));
        }
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            return Err(ProOutputSinkError::new(
                "core_not_committed",
                "Junie output page arrived before Core committed",
            ));
        }
        self.contents.lock().expect("contents").extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().expect("progress") = Some(ProOutputProgress {
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
            accepted_outputs: u32::try_from(page.observations.len()).expect("bounded outputs"),
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn pro_failure_does_not_roll_back_core_and_later_activation_replays_output() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let root = temp.path().join("sessions");
    let session_dir = root.join("session-260607-100000-acme");
    fs::create_dir_all(&session_dir).expect("session directory");
    write_fixture(&root.join("index.jsonl"), FIXTURE_INDEX);
    write_fixture(&session_dir.join("events.jsonl"), FIXTURE_EVENTS);
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let context = ProviderAdapterContext {
        machine_id: "junie-nativepath-pro-test-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: Utc
            .timestamp_millis_opt(1_783_339_500_000)
            .single()
            .expect("import timestamp"),
    };
    let sink = Arc::new(RecordingSink::new(store_path, true));
    let core = import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions {
            import_profile: ImportProfile::CoreAndPro(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .expect("Core survives Pro failure");
    assert_eq!(core.imported_events, 4);
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
    let session = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Junie)
        .expect("Junie session");
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        4
    );

    sink.fail.store(false, Ordering::SeqCst);
    let replay = import_junie_nativepath(
        &root,
        &mut store,
        context,
        ProviderImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .expect("later Pro activation");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        sink.contents.lock().expect("contents").as_slice(),
        [b"JUNIE_TERMINAL_OUTPUT saffron harbor".as_slice()]
    );
}
