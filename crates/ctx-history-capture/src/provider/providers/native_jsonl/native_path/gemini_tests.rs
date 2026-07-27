use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::EventType;
use rusqlite::params;
use serde_json::{json, Value};
use tempfile::tempdir;

use super::*;
use crate::provider::native_ingestion::{
    NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS,
};
use crate::{
    import_gemini_cli_history, GeminiCliImportOptions, OutputOutcome, ProOutputMaterializationPage,
    ProOutputObservation, ProOutputPageResult,
};

const MACHINE: &str = "gemini-production-route-proof";
const SUCCESS_BODY: &str = "GEMINI_PRODUCTION_SUCCESS_BODY";
const FAILURE_BODY: &str = "GEMINI_PRODUCTION_FAILURE_BODY";

#[test]
fn gemini_production_nativepath_core_first_failure_isolated_and_replay_catches_up_idempotently() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = root.join("tmp/project/chats/route-proof.jsonl");
    let records = [
        json!({
            "sessionId": "gemini-route-proof",
            "startTime": "2026-07-25T12:00:00.000Z",
            "lastUpdated": "2026-07-25T12:00:00.000Z",
            "kind": "main",
            "directories": ["/workspace/gemini-route-proof"]
        }),
        json!({
            "id": "message-1",
            "timestamp": "2026-07-25T12:00:01.000Z",
            "type": "user",
            "content": "core-visible message"
        }),
        json!({
            "id": "result-1",
            "timestamp": "2026-07-25T12:00:02.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "call-1",
                "name": "run_shell_command",
                "result": {
                    "content": SUCCESS_BODY,
                    "success": true,
                    "exitCode": 0,
                    "durationMs": 17
                }
            }]
        }),
        json!({
            "id": "result-2",
            "timestamp": "2026-07-25T12:00:03.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "call-2",
                "name": "run_shell_command",
                "result": {
                    "content": FAILURE_BODY,
                    "error": true,
                    "exitCode": 1
                }
            }]
        }),
    ];
    let expected_byte_start = jsonl(&records[..2]).len() as u64;
    let expected_byte_end = jsonl(&records[..3]).len() as u64;
    let transcript_bytes = jsonl(&records);
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, transcript_bytes).unwrap();

    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let failing = Arc::new(RecordingSink::new(store_path.clone(), true));
    let first = import(
        &root,
        &mut store,
        crate::ImportProfile::CoreAndPro(failing.clone()),
    );

    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert!(failing.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(failing.behind.load(Ordering::SeqCst), 1);
    assert_eq!(failing.pages.load(Ordering::SeqCst), 0);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Gemini)
        .unwrap();
    let core_events = store.events_for_session(session.id).unwrap();
    assert!(core_events
        .iter()
        .any(|event| event.event_type == EventType::Message));
    let serialized_core = serde_json::to_string(&core_events).unwrap();
    assert!(!serialized_core.contains(SUCCESS_BODY));
    assert!(!serialized_core.contains(FAILURE_BODY));
    assert!(!serialized_core.contains("output_preview"));
    assert!(!serialized_core.contains("locator"));
    assert!(core_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));

    let replay = Arc::new(RecordingSink::new(store_path, false));
    let catch_up = import(
        &root,
        &mut store,
        crate::ImportProfile::ProReplayOnly(replay.clone()),
    );
    assert_eq!(catch_up.work_result(), ProviderImportWorkResult::NoOp);
    assert!(replay.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(replay.behind.load(Ordering::SeqCst), 0);
    assert!(replay.pages.load(Ordering::SeqCst) > 0);

    let observations = replay.observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    let observation = &observations[0];
    assert_eq!(observation.content, SUCCESS_BODY.as_bytes());
    assert_eq!(observation.call_id.as_deref(), Some("call-1"));
    assert_eq!(
        observation.coordinate.native_record_id.as_deref(),
        Some("result-1")
    );
    assert_eq!(observation.coordinate.source_record_ordinal, Some(2));
    assert_eq!(
        observation.coordinate.source_record_subrecord_index,
        Some(0)
    );
    assert_eq!(observation.coordinate.byte_start, Some(expected_byte_start));
    assert_eq!(
        observation.coordinate.byte_end_exclusive,
        Some(expected_byte_end)
    );
    assert_eq!(observation.outcome.outcome, OutputOutcome::Success);
    assert_eq!(observation.outcome.exit_code, Some(0));
    assert_eq!(observation.outcome.duration_ms, Some(17));
    assert_eq!(observation.locator.version, 1);
    assert_eq!(observation.locator.kind, "gemini/nativepath/jsonl-result");
    let locator: Value = serde_json::from_slice(&observation.locator.payload).unwrap();
    let canonical_transcript = fs::canonicalize(&transcript).unwrap();
    assert_eq!(
        locator.get("path").and_then(Value::as_str),
        Some(canonical_transcript.to_str().unwrap())
    );
    assert_eq!(
        locator.get("byte_start").and_then(Value::as_u64),
        Some(expected_byte_start)
    );
    assert_eq!(
        locator.get("byte_end_exclusive").and_then(Value::as_u64),
        Some(expected_byte_end)
    );
    assert_eq!(observations[1].content, FAILURE_BODY.as_bytes());
    assert_eq!(observations[1].call_id.as_deref(), Some("call-2"));
    assert_eq!(observations[1].outcome.outcome, OutputOutcome::Failure);
    drop(observations);

    let sources = replay.sources.lock().unwrap();
    assert_eq!(sources.len(), replay.pages.load(Ordering::SeqCst));
    assert!(sources.iter().all(|source| {
        source.provider == CaptureProvider::Gemini.as_str()
            && source.namespace_id == root.display().to_string()
            && source.source_id == provider_path_identity(&canonical_transcript).unwrap()
    }));
    drop(sources);

    let pages_after_catch_up = replay.pages.load(Ordering::SeqCst);
    let idempotent = import(
        &root,
        &mut store,
        crate::ImportProfile::ProReplayOnly(replay.clone()),
    );
    assert_eq!(idempotent.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.pages.load(Ordering::SeqCst), pages_after_catch_up);
    assert_eq!(replay.observations.lock().unwrap().len(), 2);
}

#[test]
fn gemini_production_rewrite_preserves_stable_native_id_and_updates_fallback_payload() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = root.join("tmp/project/chats/rewrite.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let header = header_record("gemini-stable-rewrite");
    fs::write(
        &transcript,
        jsonl(&[
            header.clone(),
            json!({
                "id": "stable-message",
                "type": "user",
                "content": "before rewrite"
            }),
        ]),
    )
    .unwrap();
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    import(&root, &mut store, crate::ImportProfile::CoreOnly);
    let session = gemini_session(&store, "gemini-stable-rewrite");
    let before = store.events_for_session(session.id).unwrap();
    assert_eq!(before.len(), 1);
    let before_id = before[0].id;
    let before_hash = before[0].sync.metadata["provider_event_hash"]
        .as_str()
        .unwrap()
        .to_owned();

    fs::write(
        &transcript,
        jsonl(&[
            header,
            json!({"type": "metadata", "value": "position shift"}),
            json!({
                "id": "stable-message",
                "type": "user",
                "content": "after rewrite with changed content"
            }),
        ]),
    )
    .unwrap();
    import(&root, &mut store, crate::ImportProfile::CoreOnly);

    let after = store.events_for_session(session.id).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before_id);
    assert_ne!(
        after[0].sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap(),
        before_hash
    );
    assert_eq!(
        after[0].sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
    assert_eq!(
        after[0].payload["body"]["text"],
        "after rewrite with changed content"
    );
}

#[test]
fn gemini_production_dedupes_duplicate_native_ids_across_pages() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = root.join("tmp/project/chats/duplicate-pages.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let mut records = vec![
        header_record("gemini-cross-page-duplicate"),
        json!({
            "id": "duplicate-id",
            "type": "user",
            "content": "first value"
        }),
    ];
    records.extend((0..62).map(|index| {
        json!({
            "id": format!("filler-{index:02}"),
            "type": "user",
            "content": format!("filler {index}")
        })
    }));
    records.push(json!({
        "id": "duplicate-id",
        "type": "gemini",
        "content": "second value wins canonical reconciliation"
    }));
    records.push(json!({
        "id": "later-valid",
        "type": "gemini",
        "content": "later record survives"
    }));
    fs::write(&transcript, jsonl(&records)).unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    import(&root, &mut store, crate::ImportProfile::CoreOnly);

    let session = gemini_session(&store, "gemini-cross-page-duplicate");
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 64);
    let duplicate = events
        .iter()
        .find(|event| {
            event.payload["native_identity"]
                == serde_json::to_value(GeminiEventIdentity::NativeRecordId(
                    "duplicate-id".to_owned(),
                ))
                .unwrap()
        })
        .unwrap();
    assert_eq!(
        duplicate.payload["body"]["text"],
        "second value wins canonical reconciliation"
    );
    assert!(events.iter().any(|event| {
        event.payload["native_identity"]
            == serde_json::to_value(GeminiEventIdentity::NativeRecordId(
                "later-valid".to_owned(),
            ))
            .unwrap()
    }));
}

#[test]
fn gemini_production_imports_explicit_copied_tree_and_direct_file() {
    let temp = tempdir().unwrap();
    let copied = temp.path().join("copied-export");
    let copied_transcript = copied.join("tmp/project/chats/copied.jsonl");
    fs::create_dir_all(copied_transcript.parent().unwrap()).unwrap();
    fs::create_dir_all(copied.join("tmp/project/telemetry")).unwrap();
    fs::write(
        &copied_transcript,
        jsonl(&[
            header_record("gemini-copied-tree"),
            json!({"id": "copied-message", "type": "user", "content": "copied"}),
        ]),
    )
    .unwrap();
    fs::write(
        copied.join("tmp/project/telemetry/unrelated.jsonl"),
        b"{\"unrelated\":true}\n",
    )
    .unwrap();
    let direct = temp.path().join("direct-session.jsonl");
    fs::write(
        &direct,
        jsonl(&[
            header_record("gemini-direct-file"),
            json!({"id": "direct-message", "type": "user", "content": "direct"}),
        ]),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    import(
        copied_transcript.parent().unwrap(),
        &mut store,
        crate::ImportProfile::CoreOnly,
    );
    import(&direct, &mut store, crate::ImportProfile::CoreOnly);

    let sessions = store.list_sessions().unwrap();
    assert!(sessions
        .iter()
        .any(|session| { session.external_session_id.as_deref() == Some("gemini-copied-tree") }));
    assert!(sessions
        .iter()
        .any(|session| { session.external_session_id.as_deref() == Some("gemini-direct-file") }));
}

#[test]
fn gemini_production_retires_missing_root_without_losing_historical_rows() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = root.join("tmp/project/chats/retired.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(
        &transcript,
        jsonl(&[
            header_record("gemini-retired-root"),
            json!({"id": "retained-message", "type": "user", "content": "historical"}),
        ]),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    assert_eq!(
        import(&root, &mut store, crate::ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let session = gemini_session(&store, "gemini-retired-root");
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    fs::remove_dir_all(&root).unwrap();
    let retired = import(&root, &mut store, crate::ImportProfile::CoreOnly);
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(retired.failed, 0, "{:?}", retired.failures);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    assert_eq!(
        import(&root, &mut store, crate::ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn gemini_production_retires_missing_transcript_after_a_completed_inventory() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = root.join("tmp/project/chats/missing.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(
        &transcript,
        jsonl(&[
            header_record("gemini-missing-transcript"),
            json!({"id": "retained-message", "type": "user", "content": "historical"}),
        ]),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    import(&root, &mut store, crate::ImportProfile::CoreOnly);
    fs::remove_file(&transcript).unwrap();
    let retired = import(&root, &mut store, crate::ImportProfile::CoreOnly);
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(retired.failed, 0, "{:?}", retired.failures);
    assert_eq!(
        import(&root, &mut store, crate::ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn gemini_production_migrates_exact_released_positional_event() {
    assert_released_shape_migrates(false);
}

#[test]
fn gemini_production_migrates_exact_released_positional_alias() {
    assert_released_shape_migrates(true);
}

fn assert_released_shape_migrates(through_alias: bool) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = root.join("tmp/project/chats/released.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let old_content = "released positional failure content";
    let header = header_record("gemini-released-shape");
    let result_record = |content: &str| {
        json!({
            "id": "released-result",
            "timestamp": "2026-07-25T12:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "released-call",
                "name": "run_shell_command",
                "result": {
                    "content": content,
                    "error": true,
                    "exitCode": 1
                }
            }]
        })
    };
    fs::write(
        &transcript,
        jsonl(&[header.clone(), result_record(old_content)]),
    )
    .unwrap();
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    import(&root, &mut store, crate::ImportProfile::CoreOnly);

    let session = gemini_session(&store, "gemini-released-shape");
    let mut current = store.events_for_session(session.id).unwrap().remove(0);
    let current_id = current.id;
    let source_id = current.capture_source_id.unwrap();
    let discovered = discover_gemini_transcripts(&root).unwrap();
    let source = discovered.transcripts.first().unwrap();
    let mut reader =
        read_gemini_transcript_pages_with_profile(source, None, GeminiNativePathProfile::CoreOnly)
            .unwrap();
    let retained = loop {
        let page = reader.next_page().unwrap().unwrap();
        if let Some(event) = page
            .events
            .into_iter()
            .find(|event| event.event_type == EventType::ToolOutput)
        {
            break event;
        }
    };
    let released_provider_event_index = released_gemini_event_index(&retained).unwrap();
    assert_eq!(released_provider_event_index, 1);
    let released_hash = hex_digest(retained.released_body_sha256);
    let released_identity = provider_source_event_import_identity(
        source_id,
        released_provider_event_index,
        &released_hash,
    );
    let released_native_identity =
        GeminiEventIdentity::NativeRecordId(retained.released_identity.clone());
    current.dedupe_key = Some(released_identity.dedupe_key.clone());
    current.payload["provider_event_index"] = json!(released_provider_event_index);
    current.payload["provider_event_hash"] = json!(released_hash.clone());
    current.payload["native_identity"] = serde_json::to_value(&released_native_identity).unwrap();
    current.payload["body"] = json!({
        "kind": "output_diagnostic",
        "call_id": "released-call",
        "tool_name": "run_shell_command",
        "outcome": "failure",
        "exit_code": 1,
        "duration_ms": null,
        "output_preview": old_content,
    });
    current.payload["preview"] = json!(old_content);
    current.payload["searchable_text"] = json!(old_content);
    current
        .payload
        .as_object_mut()
        .unwrap()
        .remove("released_native_identity");
    current.sync.metadata["provider_event_index"] = json!(released_provider_event_index);
    current.sync.metadata["provider_event_hash"] = json!(released_hash);
    current.sync.metadata["provider_event_hash_authority"] = json!("provider_supplied");
    current.sync.metadata["native_identity"] =
        serde_json::to_value(&released_native_identity).unwrap();
    for key in [
        "stable_provider_event_index",
        "released_provider_event_index",
        "released_native_identity",
    ] {
        current.sync.metadata.as_object_mut().unwrap().remove(key);
    }

    let expected_canonical_id = if through_alias {
        store.upsert_event(&current).unwrap();
        drop(store);
        let connection = rusqlite::Connection::open(&store_path).unwrap();
        connection
            .execute(
                "INSERT INTO event_aliases(alias_id, event_id, reason, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                params![
                    released_identity.id.to_string(),
                    current_id.to_string(),
                    "gemini exact released positional test alias",
                    0_i64,
                ],
            )
            .unwrap();
        current_id
    } else {
        drop(store);
        let connection = rusqlite::Connection::open(&store_path).unwrap();
        connection
            .execute(
                "DELETE FROM events WHERE id = ?1",
                params![current_id.to_string()],
            )
            .unwrap();
        drop(connection);
        current.id = released_identity.id;
        current.seq = released_identity.seq;
        let replacement_store = Store::open(&store_path).unwrap();
        replacement_store.upsert_event(&current).unwrap();
        released_identity.id
    };

    let mut migration_header = header.clone();
    migration_header["lastUpdated"] = json!("2026-07-25T12:00:02.000Z");
    fs::write(
        &transcript,
        jsonl(&[
            migration_header,
            result_record(old_content),
            json!({"type": "metadata", "value": "force exact migration scan"}),
        ]),
    )
    .unwrap();
    let mut store = Store::open(&store_path).unwrap();
    import(&root, &mut store, crate::ImportProfile::CoreOnly);
    let migrated = store.events_for_session(session.id).unwrap();
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].id, expected_canonical_id);
    assert_eq!(
        migrated[0].sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
    drop(store);

    let new_content = "rewritten failure content after released migration";
    fs::write(&transcript, jsonl(&[header, result_record(new_content)])).unwrap();
    let mut store = Store::open(&store_path).unwrap();
    import(&root, &mut store, crate::ImportProfile::CoreOnly);

    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, expected_canonical_id);
    assert_eq!(
        events[0].sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
    assert_eq!(
        events[0].sync.metadata["provider_event_index"],
        released_provider_event_index
    );
    let serialized = serde_json::to_string(&events[0]).unwrap();
    assert!(!serialized.contains(old_content));
    assert!(!serialized.contains(new_content));
    assert!(!serialized.contains("output_preview"));
    assert!(!serialized.contains("locator"));
    assert_eq!(
        store.get_event(released_identity.id).unwrap().id,
        expected_canonical_id
    );
}

#[test]
fn gemini_65_records_span_pages_and_commit_in_one_publication_group() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let mut records = vec![gemini_boundary_header("unit-boundary")];
    records.extend((0..NATIVE_INGESTION_PAGE_MAX_UNITS + 1).map(|index| {
        json!({
            "id": format!("message-{index}"),
            "timestamp": "2026-07-27T12:00:01.000Z",
            "type": "user",
            "content": format!("message-{index}")
        })
    }));
    write_gemini_boundary_transcript(&root, "project", "unit-boundary", &records);
    let discovery = discover_gemini_transcripts(&root).unwrap();
    assert_eq!(discovery.transcripts.len(), 1);
    let pending = gemini_pending_pages(&discovery.transcripts[0]);
    assert!(pending.len() >= 2);
    assert_eq!(
        pending
            .iter()
            .map(|pending| pending.page.logical_units)
            .sum::<usize>(),
        NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );
    assert!(pending.iter().all(|pending| {
        pending.page.logical_units <= NATIVE_INGESTION_PAGE_MAX_UNITS
            && pending.page.conservative_serialized_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES
    }));

    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let first = import_gemini_with_limit(&root, &mut store, CaptureWorkLimit::OneSafeGroup);

    assert_eq!(first.imported_events, NATIVE_INGESTION_PAGE_MAX_UNITS + 1);
    assert!(first.work_remaining);
    let session = gemini_session(&store, "unit-boundary");
    assert_eq!(
        store.events_for_session(session.id).unwrap().len(),
        NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );

    let drained = import_gemini_with_limit(&root, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(drained.work_result(), ProviderImportWorkResult::NoOp);
    assert!(!drained.work_remaining);
}

#[test]
fn gemini_group_rotates_before_the_six_mib_retained_target() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    for (project, session, content) in [
        ("a-project", "byte-a", "first"),
        ("b-project", "byte-b", "second"),
    ] {
        write_gemini_boundary_transcript(
            &root,
            project,
            session,
            &[
                gemini_boundary_header(session),
                json!({
                    "id": format!("message-{session}"),
                    "timestamp": "2026-07-27T12:00:01.000Z",
                    "type": "user",
                    "content": content
                }),
            ],
        );
    }
    let discovery = discover_gemini_transcripts(&root).unwrap();
    assert_eq!(discovery.transcripts.len(), 2);
    let page_bytes = GEMINI_GROUP_MAX_BYTES / 2 + 1;
    let pending = discovery
        .transcripts
        .iter()
        .map(|source| {
            let mut pending = gemini_pending_pages(source).pop().unwrap();
            pending.page.conservative_serialized_bytes = page_bytes;
            pending
        })
        .collect::<Vec<_>>();
    assert!(page_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES);
    assert!(page_bytes.saturating_mul(pending.len()) > GEMINI_GROUP_MAX_BYTES);

    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let (summary, stopped) = publish_one_safe_gemini_group(&root, &mut store, pending);

    assert!(stopped);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .filter(|session| session.provider == CaptureProvider::Gemini)
            .count(),
        1
    );
}

#[test]
fn gemini_group_rotates_before_the_estimated_mutation_target() {
    const TOUCHES_PER_EVENT: usize = 24;
    const EVENTS_PER_SOURCE: usize = NATIVE_INGESTION_PAGE_MAX_UNITS - 1;

    let temp = tempdir().unwrap();
    let root = temp.path().join(".gemini");
    for (project, session) in [("a-project", "mutation-a"), ("b-project", "mutation-b")] {
        let mut records = vec![gemini_boundary_header(session)];
        records.extend((0..EVENTS_PER_SOURCE).map(|index| {
            json!({
                "id": format!("{session}-{index}"),
                "timestamp": "2026-07-27T12:00:01.000Z",
                "type": "user",
                "content": format!("{session}-{index}")
            })
        }));
        write_gemini_boundary_transcript(&root, project, session, &records);
    }
    let discovery = discover_gemini_transcripts(&root).unwrap();
    assert_eq!(discovery.transcripts.len(), 2);
    let pending = discovery
        .transcripts
        .iter()
        .map(|source| {
            let mut pending = gemini_pending_pages(source).pop().unwrap();
            assert_eq!(pending.page.events.len(), EVENTS_PER_SOURCE);
            for event in &mut pending.page.events {
                event.safe_file_touches = (0..TOUCHES_PER_EVENT)
                    .map(|index| format!("path-{index}.txt"))
                    .collect();
            }
            pending.page.conservative_serialized_bytes = 1024 * 1024;
            pending
        })
        .collect::<Vec<_>>();
    let page_mutations = EVENTS_PER_SOURCE
        .saturating_mul(1 + TOUCHES_PER_EVENT)
        .saturating_add(4);
    assert!(page_mutations <= GEMINI_GROUP_MAX_ESTIMATED_MUTATIONS);
    assert!(page_mutations.saturating_mul(pending.len()) > GEMINI_GROUP_MAX_ESTIMATED_MUTATIONS);

    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let (summary, stopped) = publish_one_safe_gemini_group(&root, &mut store, pending);

    assert!(stopped);
    assert_eq!(summary.imported_events, EVENTS_PER_SOURCE);
}

fn publish_one_safe_gemini_group(
    root: &Path,
    store: &mut Store,
    pending: Vec<GeminiPendingPage>,
) -> (ProviderImportSummary, bool) {
    let committed_store = Store::open_read_only(store.path()).unwrap();
    let bulk_guard = store.begin_event_search_bulk_mode().unwrap();
    let context = GeminiPublicationContext {
        machine_id: MACHINE,
        source_root: root,
        imported_at: "2026-07-27T12:00:00Z".parse().unwrap(),
        history_record_id: None,
    };
    let result = {
        let mut accumulator = GeminiGroupAccumulator::new(
            store,
            &committed_store,
            &bulk_guard,
            context,
            CaptureWorkLimit::OneSafeGroup,
            None,
        );
        for page in pending {
            accumulator.push(page).unwrap();
        }
        let summary = accumulator.finish().unwrap();
        (summary, accumulator.stopped)
    };
    store.finish_event_search_bulk_mode(&bulk_guard).unwrap();
    result
}

fn gemini_pending_pages(source: &GeminiTranscriptSource) -> Vec<GeminiPendingPage> {
    let mut reader =
        read_gemini_transcript_pages_with_profile(source, None, GeminiNativePathProfile::CoreOnly)
            .unwrap();
    let mut pending = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        let next_checkpoint = reader
            .outcome()
            .map(|outcome| outcome.checkpoint.clone())
            .unwrap_or_else(|| checkpoint_from_frontier(source, &page.next_safe_frontier));
        pending.push(GeminiPendingPage {
            source: source.clone(),
            page,
            next_checkpoint,
            output_pages: Vec::new(),
        });
    }
    pending
}

fn write_gemini_boundary_transcript(root: &Path, project: &str, session: &str, records: &[Value]) {
    let path = root
        .join("tmp")
        .join(project)
        .join("chats")
        .join(format!("{session}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, jsonl(records)).unwrap();
}

fn gemini_boundary_header(session: &str) -> Value {
    json!({
        "sessionId": session,
        "startTime": "2026-07-27T12:00:00.000Z",
        "lastUpdated": "2026-07-27T12:00:00.000Z",
        "kind": "main",
        "directories": ["/workspace/gemini-boundary"]
    })
}

fn header_record(session_id: &str) -> Value {
    json!({
        "sessionId": session_id,
        "startTime": "2026-07-25T12:00:00.000Z",
        "lastUpdated": "2026-07-25T12:00:00.000Z",
        "kind": "main",
        "directories": ["/workspace/gemini-test"]
    })
}

fn gemini_session(store: &Store, external_session_id: &str) -> Session {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| {
            session.provider == CaptureProvider::Gemini
                && session.external_session_id.as_deref() == Some(external_session_id)
        })
        .unwrap()
}

struct RecordingSink {
    store_path: PathBuf,
    fail_first: AtomicBool,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    behind: AtomicUsize,
    saw_core_before_page: AtomicBool,
    sources: Mutex<Vec<OutputSourceIdentity>>,
    observations: Mutex<Vec<ProOutputObservation>>,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail_first: bool) -> Self {
        Self {
            store_path,
            fail_first: AtomicBool::new(fail_first),
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            sources: Mutex::new(Vec::new()),
            observations: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "gemini-nativepath-test-materializer-v1"
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
        if core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .iter()
            .any(|session| session.provider == CaptureProvider::Gemini)
        {
            self.saw_core_before_page.store(true, Ordering::SeqCst);
        }
        if self.fail_first.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "intentional Gemini output failure",
            ));
        }
        let committed_cursor = page.next_safe_cursor.clone();
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        self.sources.lock().unwrap().push(page.source);
        self.observations.lock().unwrap().extend(page.observations);
        self.pages.fetch_add(1, Ordering::SeqCst);
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs,
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

fn import(
    root: &Path,
    store: &mut Store,
    import_profile: crate::ImportProfile,
) -> ProviderImportSummary {
    import_gemini_cli_history(
        root,
        store,
        GeminiCliImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap()
}

fn import_gemini_with_limit(
    root: &Path,
    store: &mut Store,
    capture_work_limit: CaptureWorkLimit,
) -> ProviderImportSummary {
    import_gemini_cli_history(
        root,
        store,
        GeminiCliImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-27T12:00:00Z".parse().unwrap(),
            capture_work_limit,
            import_profile: crate::ImportProfile::CoreOnly,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap()
}

fn jsonl(values: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    bytes
}
