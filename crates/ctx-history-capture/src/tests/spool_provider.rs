use super::support::*;

#[test]
fn spool_writer_closes_tmp_file_atomically_to_jsonl() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    let envelope = fixture_envelope(fixture_options("atomic", "Atomic capture")).unwrap();
    let mut writer = SpoolWriter::create(&inbox, "test-machine").unwrap();
    let tmp_path = writer.tmp_path().to_path_buf();
    let final_path = writer.final_path().to_path_buf();

    writer.write_envelope(&envelope).unwrap();
    assert!(tmp_path.exists());
    assert!(!final_path.exists());

    let closed_path = writer.finish().unwrap();
    assert_eq!(closed_path, final_path);
    assert!(!tmp_path.exists());
    assert!(final_path.exists());
    assert_eq!(read_jsonl(&final_path).unwrap(), vec![envelope]);
}

#[test]
fn failed_import_retains_raw_failed_file_and_error_metadata() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    let pending = inbox.join("capture-bad.jsonl");
    fs::write(&pending, "not json\n").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_spool(&inbox, &mut store).unwrap();

    assert_eq!(summary.failed_files, 1);
    assert_eq!(summary.processed_files, 1);
    let failed = inbox.join("capture-bad.jsonl.failed");
    let sidecar = inbox.join("capture-bad.jsonl.failed.error.json");
    assert!(failed.exists());
    assert!(sidecar.exists());
    assert_eq!(fs::read_to_string(failed).unwrap(), "not json\n");
    assert!(fs::read_to_string(sidecar)
        .unwrap()
        .contains("not a valid capture envelope"));
    assert_eq!(spool_counts(&inbox).unwrap().failed, 1);
}

#[test]
fn import_rejects_non_regular_pending_spool_entry() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    fs::create_dir_all(inbox.join("capture-dir.jsonl")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        import_spool(&inbox, &mut store),
        Err(CaptureError::InvalidPath(path)) if path.ends_with("capture-dir.jsonl")
    ));
    assert!(inbox.join("capture-dir.jsonl").is_dir());
}

#[cfg(unix)]
#[test]
fn import_rejects_symlink_pending_spool_entry() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    let target = temp.path().join("outside.jsonl");
    fs::write(&target, "not json\n").unwrap();
    let pending = inbox.join("capture-link.jsonl");
    symlink(&target, &pending).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        import_spool(&inbox, &mut store),
        Err(CaptureError::InvalidPath(path)) if path.ends_with("capture-link.jsonl")
    ));
    assert!(pending.exists());
    assert_eq!(fs::read_to_string(target).unwrap(), "not json\n");
}

#[test]
fn import_is_idempotent_by_dedupe_key() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    let envelope = fixture_envelope(fixture_options("same-dedupe", "First title")).unwrap();
    let mut first = SpoolWriter::create(&inbox, "test-machine").unwrap();
    first.write_envelope(&envelope).unwrap();
    first.finish().unwrap();
    let mut second = SpoolWriter::create(&inbox, "test-machine").unwrap();
    second.write_envelope(&envelope).unwrap();
    second.finish().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_spool(&inbox, &mut store).unwrap();

    assert_eq!(summary.failed_files, 0);
    assert_eq!(summary.processed_files, 2);
    let records = store.list_records(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, stable_capture_uuid("same-dedupe", "record"));
    assert_eq!(records[0].id.get_version_num(), 7);
    assert_eq!(records[0].title, "First title");
    assert_eq!(spool_counts(&inbox).unwrap().done, 2);
}

#[test]
fn provider_fixture_replay_imports_codex_session_tree_and_is_idempotent() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let fixture = provider_fixture("codex.jsonl");
    let mut store = Store::open(&db_path).unwrap();

    let first =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.imported_edges, 1);
    assert_eq!(first.skipped_events, 0);

    let second =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_events, 3);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Codex, "codex-session-1");
    let child_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-session-1-subagent-a");
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(
        parent.external_session_id.as_deref(),
        Some("codex-session-1")
    );
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
    assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
    drop(store);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 1);
    let (from_session_id, to_session_id, edge_type): (String, String, String) = conn
        .query_row(
            "SELECT from_session_id, to_session_id, edge_type FROM session_edges",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(from_session_id, parent_id.to_string());
    assert_eq!(to_session_id, child_id.to_string());
    assert_eq!(edge_type, "parent_child");
}

#[test]
fn provider_fixture_replay_defers_child_edges_until_parent_is_known() {
    let temp = tempdir();
    let fixture = provider_fixture("out-of-order-subagent.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.imported_edges, 1);
    assert_eq!(summary.skipped_edges, 0);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Codex, "out-of-order-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Codex, "out-of-order-child");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(edge_count, 1);
}

#[test]
fn provider_fixture_replay_supports_pi_and_preserves_metadata() {
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
    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-session-1");
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

    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-session-docs-1");
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
fn pi_session_import_rejects_header_only_session_jsonl() {
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

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real message content"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
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
    assert!(summary.failures[0]
        .error
        .contains("timestamp is not a valid RFC3339 timestamp"));
    assert!(store.list_sessions().unwrap().is_empty());
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
        Some(legacy_index as u64),
        true,
    )
    .unwrap();

    assert_eq!(resolved.id, legacy_identity.id);
    assert_eq!(resolved.dedupe_key, legacy_identity.dedupe_key);
}

#[test]
fn pi_session_import_reuses_legacy_line_indexed_event_by_entry_id_after_line_shift() {
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
    let source_identity =
        provider_source_root_identity(CaptureProvider::Pi, "pi_session_jsonl", &raw_path);
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
                source_format: Some("pi_session_jsonl".to_owned()),
                source_root: Some(raw_path.clone()),
                source_identity: Some(source_identity),
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
            capture_source_id: Some(source_id),
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
fn pi_session_import_rejects_non_message_only_entries() {
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

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real message content"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn pi_session_import_rejects_tool_only_entries() {
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

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real message content"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
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
    assert_eq!(summary.imported_events, 9);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-non-message-text");
    let events = store.events_for_session(session_id).unwrap();
    let texts = events
        .iter()
        .filter_map(|event| event.payload.pointer("/body/text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    for expected in [
        "mixed real prompt",
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
    assert_eq!(second.skipped_events, 2);

    let alpha = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-dir-alpha");
    let beta = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-dir-beta");
    assert_eq!(store.events_for_session(alpha).unwrap().len(), 1);
    assert_eq!(store.events_for_session(beta).unwrap().len(), 1);
}
