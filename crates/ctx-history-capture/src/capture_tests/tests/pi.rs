#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn provider_fixture_replay_supports_pi_and_preserves_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("pi.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.redacted, 0);
    let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].redaction_state, RedactionState::LocalPreview);
    assert!(events[1]
        .sync
        .metadata
        .to_string()
        .contains("fixture-token-value"));
    assert!(!events[1].sync.metadata.to_string().contains("[REDACTED]"));
}

#[test]
pub(crate) fn pi_session_import_replays_documented_session_jsonl_and_is_idempotent() {
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
    assert_eq!(first.imported_events, 6);
    assert_eq!(first.redacted, 0);

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
    assert_eq!(second.skipped_events, 6);

    let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-session-docs-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.sync.fidelity, Fidelity::Imported);
    assert_eq!(
        session.sync.metadata["source_format"].as_str(),
        Some("pi_session_jsonl")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 6);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].event_type, EventType::ToolCall);
    assert_eq!(events[2].event_type, EventType::ToolOutput);
    assert_eq!(events[3].event_type, EventType::CommandOutput);
    assert_eq!(events[4].event_type, EventType::Message);
    assert_eq!(events[4].role, Some(EventRole::Assistant));
    assert_eq!(events[5].event_type, EventType::Summary);
    assert!(events[3].payload.to_string().contains("cargo test"));
    assert!(events[3].payload.to_string().contains("fixture-secret"));
    assert!(!events[3].payload.to_string().contains("[REDACTED]"));
}

#[test]
pub(crate) fn pi_session_import_rejects_malformed_event_timestamp() {
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
    assert!(summary.failures[0]
        .error
        .contains("timestamp is not a valid RFC3339 timestamp"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
pub(crate) fn pi_session_import_uses_entry_ids_when_lines_shift() {
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

    let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-line-shift");
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
pub(crate) fn pi_session_identity_resolver_reuses_legacy_line_indexed_events() {
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
            redaction_state: RedactionState::LocalPreview,
            sync: provider_sync_metadata(Fidelity::Imported, json!({})),
        })
        .unwrap();

    let header = PiSessionHeader {
        id: "pi-legacy".to_owned(),
        version: Some(3),
        timestamp: "2026-06-24T12:00:00Z".parse().unwrap(),
        cwd: Some("/workspace".to_owned()),
        parent_session: None,
        raw: json!({}),
    };
    let stable_index =
        pi_provider_event_identity_index(&header, &json!({"id": "stable-entry"})).unwrap();

    let resolved = provider_event_import_identity(
        &store,
        CaptureProvider::Pi,
        "pi-legacy",
        source_id,
        stable_index,
        legacy_index + 1,
        event_hash,
        Some(legacy_index),
    )
    .unwrap();

    assert_eq!(resolved.id, legacy_identity.id);
    assert_eq!(resolved.dedupe_key, legacy_identity.dedupe_key);
}

#[test]
pub(crate) fn pi_session_import_reuses_legacy_line_indexed_event_by_entry_id_after_line_shift() {
    let temp = tempdir();
    let fixture = temp.path().join("pi-legacy-line-shift.jsonl");
    let provider_session_id = "pi-legacy-line-shift";
    let raw_path = fixture.display().to_string();
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::Pi,
        provider_session_id,
        "pi_session_jsonl",
        Some(&raw_path),
    );
    let session_id = provider_session_uuid(CaptureProvider::Pi, provider_session_id);
    let legacy_identity = provider_source_event_import_identity(source_id, 1, "legacy-hash");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let started_at = "2026-06-24T12:00:00Z".parse().unwrap();
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Pi,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/workspace".to_owned()),
                raw_source_path: Some(raw_path.clone()),
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at,
            ended_at: None,
            sync: provider_sync_metadata(Fidelity::Imported, json!({})),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Pi,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at,
            ended_at: None,
            timestamps: timestamps(started_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({})),
        })
        .unwrap();
    store
        .upsert_event(&Event {
            id: legacy_identity.id,
            seq: legacy_identity.seq,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: "2026-06-24T12:00:01Z".parse().unwrap(),
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": "pi",
                "provider_session_id": provider_session_id,
                "provider_event_index": 1,
                "body": {
                    "entry_id": "stable-entry",
                    "text": "legacy stable oracle",
                    "body": {"id": "stable-entry"}
                }
            }),
            payload_blob_id: None,
            dedupe_key: Some(legacy_identity.dedupe_key.clone()),
            redaction_state: RedactionState::LocalPreview,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({"metadata": {"entry_id": "stable-entry"}}),
            ),
        })
        .unwrap();

    fs::write(
        &fixture,
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-legacy-line-shift\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
            "{\"type\":\"model_change\",\"id\":\"inserted-entry\",\"parentId\":null,\"timestamp\":\"2026-06-24T12:00:00Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
            "{\"type\":\"message\",\"id\":\"stable-entry\",\"parentId\":\"inserted-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"new stable oracle\"}]}}\n",
        ),
    )
    .unwrap();

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
    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.skipped_events, 1);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| event.id == legacy_identity.id));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.payload.to_string().contains("stable-entry"))
            .count(),
        1
    );
}

#[test]
pub(crate) fn pi_session_import_extracts_text_from_non_message_entries() {
    let temp = tempdir();
    let fixture = temp.path().join("pi-non-message-text.jsonl");
    fs::write(
        &fixture,
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-non-message-text\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
            "{\"type\":\"compaction\",\"id\":\"compact-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"summary\":\"compacted plan oracle\"}\n",
            "{\"type\":\"branch_summary\",\"id\":\"branch-entry\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"summary\":\"branch summary oracle\"}\n",
            "{\"type\":\"custom_message\",\"id\":\"custom-message-entry\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"content\":[{\"type\":\"text\",\"text\":\"custom message oracle\"}]}\n",
            "{\"type\":\"session_info\",\"id\":\"session-info-entry\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"name\":\"session info oracle\"}\n",
            "{\"type\":\"model_change\",\"id\":\"model-entry\",\"timestamp\":\"2026-06-24T12:00:05Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
            "{\"type\":\"thinking_level_change\",\"id\":\"thinking-entry\",\"timestamp\":\"2026-06-24T12:00:06Z\",\"thinkingLevel\":\"high\"}\n",
            "{\"type\":\"label\",\"id\":\"label-entry\",\"timestamp\":\"2026-06-24T12:00:07Z\",\"label\":\"label oracle\"}\n",
            "{\"type\":\"custom\",\"id\":\"custom-entry\",\"timestamp\":\"2026-06-24T12:00:08Z\",\"customType\":\"custom type oracle\"}\n",
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
    assert_eq!(summary.imported_events, 8);
    let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-non-message-text");
    let events = store.events_for_session(session_id).unwrap();
    let texts = events
        .iter()
        .filter_map(|event| event.payload.pointer("/body/text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    for expected in [
        "compacted plan oracle",
        "branch summary oracle",
        "custom message oracle",
        "session info oracle",
        "google/gemini-2.5-flash",
        "high",
        "label oracle",
        "custom type oracle",
    ] {
        assert!(
            texts.contains(&expected),
            "missing {expected:?} in texts {texts:?}"
        );
    }
}

#[test]
pub(crate) fn pi_session_import_replays_default_session_directory_tree() {
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
    assert_eq!(second.skipped_events, 2);

    let alpha = provider_session_uuid(CaptureProvider::Pi, "pi-dir-alpha");
    let beta = provider_session_uuid(CaptureProvider::Pi, "pi-dir-beta");
    assert_eq!(store.events_for_session(alpha).unwrap().len(), 1);
    assert_eq!(store.events_for_session(beta).unwrap().len(), 1);
}
