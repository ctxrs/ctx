use crate::provider::importer::{
    provider_event_import_identity, provider_source_event_import_identity, provider_sync_metadata,
};
use crate::tests::support::fixtures::jsonl::jsonl_line;
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{import_pi_session_jsonl, stable_capture_uuid, PiSessionImportOptions};
use ctx_history_core::{CaptureProvider, Event, EventRole, EventType, Fidelity};
use ctx_history_store::Store;
use serde_json::{json, Value};
use std::fs;

#[test]
fn pi_session_import_replays_documented_session_jsonl_and_is_idempotent() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-session.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let second = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_events, 0);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-session-docs-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.sync.fidelity, Fidelity::Imported);
    assert_eq!(
        session.sync.metadata["source_format"].as_str(),
        Some("pi_session_jsonl")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].event_type, EventType::ToolCall);
    assert_eq!(events[2].event_type, EventType::Message);
    assert_eq!(events[2].role, Some(EventRole::Assistant));
    assert_eq!(events[3].event_type, EventType::Summary);
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::CommandOutput));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("fixture-secret"));
    let runs = store.runs_for_session(session_id).unwrap();
    assert!(runs.is_empty());
}

#[test]
fn pi_session_import_commits_header_only_session_jsonl() {
    let temp = tempdir();
    let path = temp.path().join("header-only-pi.jsonl");
    fs::write(
        &path,
        jsonl_line(json!({
            "type": "session",
            "id": "pi-header-only",
            "timestamp": "2026-07-03T12:00:00Z",
            "version": 1
        })),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_pi_session_jsonl(&path, &mut store, PiSessionImportOptions::default()).unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn pi_session_import_rejects_malformed_event_timestamp() {
    let temp = tempdir();
    let path = temp.path().join("bad-timestamp-pi.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "type": "session",
                "id": "pi-bad-timestamp",
                "timestamp": "2026-07-03T12:00:00Z",
                "version": 1
            })),
            jsonl_line(json!({
                "type": "message",
                "id": "pi-bad-event",
                "timestamp": "not-rfc3339",
                "message": {
                    "role": "user",
                    "content": "bad timestamp should not import"
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_pi_session_jsonl(
        &path,
        &mut store,
        PiSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0].error.contains("time parse error"));
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn pi_session_import_uses_entry_ids_when_lines_shift() {
    let temp = tempdir();
    let fixture = temp.path().join("pi-line-shift.jsonl");
    fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-line-shift\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"stable-entry\",\"parentId\":null,\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"pi line shift stable\"}]}}\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-line-shift");
    let first_event_id = store.events_for_session(session_id).unwrap()[0].id;

    fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-line-shift\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"model_change\",\"id\":\"inserted-entry\",\"parentId\":null,\"timestamp\":\"2026-06-24T12:00:00Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
                "{\"type\":\"message\",\"id\":\"stable-entry\",\"parentId\":\"inserted-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"pi line shift stable\"}]}}\n",
            ),
        )
        .unwrap();

    let second = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T16:01:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_events, 1, "{second:?}");
    assert_eq!(second.skipped_events, 1, "{second:?}");

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let shifted = events
        .iter()
        .find(|event| event.payload.to_string().contains("pi line shift stable"))
        .unwrap();
    assert_eq!(shifted.id, first_event_id);
}

#[test]
fn pi_session_identity_resolver_reuses_legacy_line_indexed_events() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_id = stable_capture_uuid("legacy-pi-source", "source");
    let legacy_index = 1;
    let event_hash = "0123456789abcdef";
    let legacy_identity =
        provider_source_event_import_identity(source_id, legacy_index, event_hash);
    store
        .upsert_event(&Event {
            id: legacy_identity.id,
            seq: legacy_identity.seq,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: "2026-06-24T12:00:01Z".parse().unwrap(),
            capture_source_id: None,
            payload: json!({"text": "legacy line indexed pi event"}),
            payload_blob_id: None,
            dedupe_key: Some(legacy_identity.dedupe_key.clone()),
            sync: provider_sync_metadata(Fidelity::Imported, json!({})),
        })
        .unwrap();

    let stable_index = crate::fnv1a64(b"pi:pi-legacy:stable-entry");

    let resolved = provider_event_import_identity(
        &store,
        CaptureProvider::Pi,
        "pi-legacy",
        source_id,
        stable_index,
        legacy_index + 1,
        event_hash,
        Some(legacy_index),
        true,
    )
    .unwrap();

    assert_eq!(resolved.id, legacy_identity.id);
    assert_eq!(resolved.dedupe_key, legacy_identity.dedupe_key);
}

#[test]
fn pi_session_import_accepts_non_message_only_entries() {
    let temp = tempdir();
    let fixture = temp.path().join("pi-non-message-only.jsonl");
    fs::write(
        &fixture,
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-non-message-only\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
            "{\"type\":\"compaction\",\"id\":\"compact-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"summary\":\"compacted plan only\"}\n",
            "{\"type\":\"model_change\",\"id\":\"model-entry\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
            "{\"type\":\"label\",\"id\":\"label-entry\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"label\":\"label only\"}\n",
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 3);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-non-message-only");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event_type, EventType::Summary);
    assert_eq!(events[1].event_type, EventType::Notice);
    assert_eq!(events[2].event_type, EventType::Notice);
}

#[test]
fn pi_session_import_accepts_tool_only_entries() {
    let temp = tempdir();
    let fixture = temp.path().join("pi-tool-only.jsonl");
    fs::write(
        &fixture,
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-tool-only\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
            "{\"type\":\"message\",\"id\":\"tool-call-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"name\":\"bash\",\"input\":{\"command\":\"true\"}}]}}\n",
            "{\"type\":\"message\",\"id\":\"tool-result-entry\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"message\":{\"role\":\"toolResult\",\"content\":\"ok\"}}\n",
            "{\"type\":\"message\",\"id\":\"bash-entry\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"message\":{\"role\":\"bashExecution\",\"command\":\"true\",\"output\":\"ok\"}}\n",
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-tool-only");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::ToolCall);
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::CommandOutput));
    assert!(store.runs_for_session(session_id).unwrap().is_empty());
}

#[test]
fn pi_session_import_keeps_metadata_entries_when_real_messages_exist() {
    let temp = tempdir();
    let fixture = temp.path().join("pi-non-message-text.jsonl");
    fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-non-message-text\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"real-user-entry\",\"timestamp\":\"2026-06-24T12:00:00.500Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"mixed real prompt\"}]}}\n",
                "{\"type\":\"compaction\",\"id\":\"compact-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"summary\":\"compacted plan oracle\"}\n",
                "{\"type\":\"branch_summary\",\"id\":\"branch-entry\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"summary\":\"branch summary oracle\"}\n",
                "{\"type\":\"custom_message\",\"id\":\"custom-message-entry\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"content\":[{\"type\":\"text\",\"text\":\"pi-custom-message-sentinel\"}]}\n",
                "{\"type\":\"session_info\",\"id\":\"session-info-entry\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"name\":\"pi-session-info-sentinel\"}\n",
                "{\"type\":\"model_change\",\"id\":\"model-entry\",\"timestamp\":\"2026-06-24T12:00:05Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
                "{\"type\":\"thinking_level_change\",\"id\":\"thinking-entry\",\"timestamp\":\"2026-06-24T12:00:06Z\",\"thinkingLevel\":\"high\"}\n",
                "{\"type\":\"label\",\"id\":\"label-entry\",\"timestamp\":\"2026-06-24T12:00:07Z\",\"label\":\"pi-label-sentinel\"}\n",
                "{\"type\":\"custom\",\"id\":\"custom-entry\",\"timestamp\":\"2026-06-24T12:00:08Z\",\"customType\":\"pi-custom-type-sentinel\"}\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 9);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-non-message-text");
    let events = store.events_for_session(session_id).unwrap();
    let texts = events
        .iter()
        .filter_map(|event| event.payload.pointer("/body/text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            "mixed real prompt",
            "compacted plan oracle",
            "branch summary oracle",
        ]
    );
    let payloads = serde_json::to_string(&events).unwrap();
    for expected in [
        "pi-session-info-sentinel",
        "gemini-2.5-flash",
        "high",
        "pi-label-sentinel",
        "pi-custom-type-sentinel",
    ] {
        assert!(
            payloads.contains(expected),
            "missing metadata {expected:?} in payloads {payloads}"
        );
    }
    assert!(!payloads.contains("pi-custom-message-sentinel"));
    for expected in [
        "mixed real prompt",
        "compacted plan oracle",
        "branch summary oracle",
    ] {
        assert!(
            store
                .search_event_hits(expected, 10)
                .unwrap()
                .iter()
                .any(|hit| hit.provider == Some(CaptureProvider::Pi)),
            "missing searchable real text {expected:?}"
        );
    }
    for omitted in [
        "pi-custom-message-sentinel",
        "pi-session-info-sentinel",
        "gemini-2.5-flash",
        "high",
        "pi-label-sentinel",
        "pi-custom-type-sentinel",
    ] {
        assert!(
            !store
                .search_event_hits(omitted, 10)
                .unwrap()
                .iter()
                .any(|hit| hit.provider == Some(CaptureProvider::Pi)),
            "unexpected searchable metadata/non-message text {omitted:?}"
        );
    }
}

#[test]
fn pi_session_import_replays_default_session_directory_tree() {
    let temp = tempdir();
    let root = temp.path().join(".pi/agent/sessions/--workspace--");
    fs::create_dir_all(&root).unwrap();
    fs::write(
            root.join("2026-06-24T12-00-00-000Z_pi-dir-alpha.jsonl"),
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-dir-alpha\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"pi-dir-alpha-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"alpha directory import\"}]}}\n",
            ),
        )
        .unwrap();
    fs::write(
            root.join("2026-06-24T12-01-00-000Z_pi-dir-beta.jsonl"),
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-dir-beta\",\"timestamp\":\"2026-06-24T12:01:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"pi-dir-beta-user\",\"timestamp\":\"2026-06-24T12:01:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"beta directory import\"}]}}\n",
            ),
        )
        .unwrap();
    let sessions_root = temp.path().join(".pi/agent/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_pi_session_jsonl(
        &sessions_root,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(sessions_root.clone()),
            imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 2);

    let second = import_pi_session_jsonl(
        &sessions_root,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(sessions_root.clone()),
            imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_events, 0);

    let alpha = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-dir-alpha");
    let beta = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-dir-beta");
    assert_eq!(store.events_for_session(alpha).unwrap().len(), 1);
    assert_eq!(store.events_for_session(beta).unwrap().len(), 1);
}
