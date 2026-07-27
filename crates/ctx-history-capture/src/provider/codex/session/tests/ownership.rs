use super::*;

#[test]
fn codex_rollout_with_multiple_session_headers_persists_each_source_before_touches() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("multi-session.jsonl");
    let touched_path = "src/multi-session.rs";
    fs::write(
        &path,
        [
            session_meta("first-session", None),
            message(0),
            session_meta("compacted-fork-session", None),
            file_touch_call(touched_path),
        ]
        .concat(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    let replay =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 2);
    assert_eq!(replay.skipped_events, 2);

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            [session_meta("appended-session", None), message(1)]
                .concat()
                .as_bytes(),
        )
        .unwrap();
    let appended =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(appended.imported_sessions, 1);
    assert_eq!(appended.imported_events, 1);

    let first = store
        .session_by_external_session(CaptureProvider::Codex, "first-session")
        .unwrap()
        .unwrap();
    let compacted = store
        .session_by_external_session(CaptureProvider::Codex, "compacted-fork-session")
        .unwrap()
        .unwrap();
    let appended = store
        .session_by_external_session(CaptureProvider::Codex, "appended-session")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(first.id).unwrap().len(), 1);
    assert_eq!(store.events_for_session(compacted.id).unwrap().len(), 1);
    assert_eq!(store.events_for_session(appended.id).unwrap().len(), 1);
    let touch_scope = store.file_touch_scope(touched_path).unwrap();
    assert_eq!(
        touch_scope.session_ids,
        [compacted.id].into_iter().collect()
    );
    assert_eq!(
        touch_scope.source_ids,
        [compacted.capture_source_id.unwrap()].into_iter().collect()
    );
}

#[test]
fn codex_multiagent_rollouts_keep_thread_ownership_across_inherited_headers_and_moves() {
    let temp = tempdir().unwrap();
    let root_path = temp.path().join("rollout-root.jsonl");
    let first_path = temp.path().join("rollout-child-a.jsonl");
    let second_path = temp.path().join("rollout-child-b.jsonl");
    let moved_path = temp.path().join("moved-child-a.jsonl");
    let root_id = "root-thread";
    let first_id = "child-thread-a";
    let second_id = "child-thread-b";
    let inherited_root = root_rollout_session_meta(root_id);
    let first_owner_header = child_rollout_session_meta(first_id, root_id);
    let second_owner_header = child_rollout_session_meta(second_id, root_id);
    fs::write(
        &root_path,
        [
            inherited_root.as_str(),
            eventless_patch("src/root.rs").as_str(),
        ]
        .concat(),
    )
    .unwrap();
    fs::write(
        &first_path,
        [
            first_owner_header.as_str(),
            inherited_root.as_str(),
            message(0).as_str(),
        ]
        .concat(),
    )
    .unwrap();
    fs::write(
        &second_path,
        [second_owner_header, inherited_root.clone(), message(1)].concat(),
    )
    .unwrap();
    let database = temp.path().join("work.sqlite");
    let mut store = Store::open(&database).unwrap();
    let options = CodexSessionImportOptions {
        source_path: Some(temp.path().to_path_buf()),
        ..CodexSessionImportOptions::default()
    };

    let root = import_codex_session_jsonl(&root_path, &mut store, options.clone()).unwrap();
    let first = import_codex_session_jsonl(&first_path, &mut store, options.clone()).unwrap();
    let second = import_codex_session_jsonl(&second_path, &mut store, options.clone()).unwrap();
    assert_eq!(root.failed, 0, "{:?}", root.failures);
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(root.imported_sessions, 1);
    assert_eq!(root.imported_events, 0);
    assert_eq!(store.capture_source_count().unwrap(), 3);
    assert_eq!(store.list_sessions().unwrap().len(), 3);
    let first_session = store
        .session_by_external_session(CaptureProvider::Codex, first_id)
        .unwrap()
        .unwrap();
    let second_session = store
        .session_by_external_session(CaptureProvider::Codex, second_id)
        .unwrap()
        .unwrap();
    let root_session = store
        .session_by_external_session(CaptureProvider::Codex, root_id)
        .unwrap()
        .unwrap();
    assert_eq!(root_session.parent_session_id, None);
    assert_eq!(root_session.root_session_id, None);
    assert_eq!(first_session.parent_session_id, Some(root_session.id));
    assert_eq!(first_session.root_session_id, Some(root_session.id));
    assert_eq!(second_session.parent_session_id, Some(root_session.id));
    assert_eq!(second_session.root_session_id, Some(root_session.id));
    assert_eq!(store.events_for_session(first_session.id).unwrap().len(), 1);
    assert_eq!(
        store.events_for_session(second_session.id).unwrap().len(),
        1
    );
    assert!(store
        .events_for_session(root_session.id)
        .unwrap()
        .is_empty());
    for id in [root_id, first_id, second_id] {
        assert_eq!(
            store
                .sessions_by_external_session_limited(CaptureProvider::Codex, id, 2)
                .unwrap()
                .len(),
            1,
            "duplicate session lookup for {id}"
        );
    }
    let source_ids = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .map(|source| source.descriptor.external_session_id.unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        source_ids,
        [
            root_id.to_owned(),
            first_id.to_owned(),
            second_id.to_owned()
        ]
        .into_iter()
        .collect()
    );

    for path in [&root_path, &first_path, &second_path] {
        let replay = import_codex_session_jsonl(path, &mut store, options.clone()).unwrap();
        assert_eq!(replay.failed, 0, "{:?}", replay.failures);
        assert_eq!(replay.imported, 0, "reimport changed {}", path.display());
    }
    assert_eq!(store.capture_source_count().unwrap(), 3);
    assert_eq!(store.list_sessions().unwrap().len(), 3);

    OpenOptions::new()
        .append(true)
        .open(&first_path)
        .unwrap()
        .write_all(
            [inherited_root.as_str(), message(2).as_str()]
                .concat()
                .as_bytes(),
        )
        .unwrap();
    let appended = import_codex_session_jsonl(&first_path, &mut store, options.clone()).unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(appended.imported_sessions, 0);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(store.events_for_session(first_session.id).unwrap().len(), 2);

    fs::rename(&first_path, &moved_path).unwrap();
    let moved = import_codex_session_jsonl(&moved_path, &mut store, options.clone()).unwrap();
    assert_eq!(moved.failed, 0, "{:?}", moved.failures);
    assert_eq!(store.capture_source_count().unwrap(), 3);
    assert_eq!(store.list_sessions().unwrap().len(), 3);
    let moved_source = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.descriptor.external_session_id.as_deref() == Some(first_id))
        .unwrap();
    assert_eq!(
        moved_source.descriptor.raw_source_path.as_deref(),
        Some(moved_path.to_string_lossy().as_ref())
    );
    assert_eq!(store.events_for_session(first_session.id).unwrap().len(), 2);

    let path_identity = provider_path_identity(&moved_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &path_identity,
    );
    let cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    let checkpoint: CodexParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    let anchor = checkpoint.header_anchor.unwrap();
    assert_eq!(anchor.start_offset, 0);
    assert_eq!(anchor.end_offset, first_owner_header.len() as u64);
}

#[test]
fn codex_producer_is_bounded_and_production_import_reads_source_once() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("bounded.jsonl");
    let mut source_bytes = session_meta("bounded-session", None);
    for index in 0..130 {
        source_bytes.push_str(&message(index));
    }
    fs::write(&path, &source_bytes).unwrap();

    let source = SourceObservation::new(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        "codex-test-source",
        "codex-test-revision",
        "codex-test-cursor",
        CODEX_CAPTURE_REVISION,
        CODEX_POLICY_REVISION,
        None,
    )
    .unwrap();
    let record_kind = ProviderRecordKind::new(CODEX_RECORD_KIND).unwrap();
    let file = File::open(&path).unwrap();
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source,
        b"bounded.jsonl".to_vec(),
        record_kind,
        source_bytes.len() as u64,
        0,
        0,
        false,
    )
    .unwrap();
    let mut batches = 0usize;
    let mut records = 0usize;
    while let Some(batch) = producer.next_batch().unwrap() {
        assert!(batch.records().len() <= CAPTURE_BATCH_MAX_RECORDS);
        assert!(batch.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
        batches += 1;
        records += batch.records().len();
    }
    assert_eq!(batches, 3);
    assert_eq!(records, 131);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (summary, opens) = count_codex_source_file_opens(|| {
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
    });
    let summary = summary.unwrap();
    assert_eq!(opens, 1);
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 130);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "bounded-session")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 130);
}

#[test]
fn codex_revision_upgrade_repairs_missing_outputs_without_duplicates() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("revision-upgrade.jsonl");
    let database = temp.path().join("work.sqlite");
    let mut source = session_meta("revision-upgrade", None);
    source.push_str(&jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "call-repair",
            "input": "git commit -m fixture"
        }
    })));
    source.push_str(&jsonl_line(json!({
            "timestamp": "2026-07-18T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-repair",
                "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
                    {"type": "input_text", "text": "[fixture db817fa] repaired output\n"}
                ]
            }
        })));
    fs::write(&path, source).unwrap();
    let mut store = Store::open(&database).unwrap();
    let options = CodexSessionImportOptions {
        machine_id: "codex-revision-upgrade-machine".to_owned(),
        imported_at: "2026-07-18T12:01:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };

    let first = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.imported_events, 2);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "revision-upgrade")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let output = events
        .iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .unwrap();

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("DELETE FROM events WHERE id = ?1", [output.id.to_string()])
        .unwrap();
    let (stream, encoded): (String, String) = connection
        .query_row(
            "SELECT stream, cursor FROM sync_cursors WHERE device_id = ?1",
            [&options.machine_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut legacy: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(legacy["p"], CODEX_CAPTURE_REVISION);
    assert_eq!(legacy["o"], CODEX_POLICY_REVISION);
    legacy["p"] = json!(CODEX_CAPTURE_REVISION - 1);
    legacy["o"] = json!(CODEX_POLICY_REVISION - 1);
    connection
        .execute(
            "UPDATE sync_cursors SET cursor = ?1 WHERE device_id = ?2 AND stream = ?3",
            rusqlite::params![
                serde_json::to_string(&legacy).unwrap(),
                &options.machine_id,
                stream
            ],
        )
        .unwrap();
    drop(connection);

    let repaired = import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(repaired.failed, 0, "{:?}", repaired.failures);
    assert_eq!(repaired.imported_events, 1);
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.event_type == EventType::CommandOutput
                    && event
                        .payload
                        .pointer("/body/call_id")
                        .and_then(Value::as_str)
                        == Some("call-repair")
            })
            .count(),
        1
    );
    assert!(events
        .iter()
        .any(|event| event.payload.to_string().contains("db817fa")));

    let idempotent = import_codex_session_jsonl(&path, &mut store, options).unwrap();
    assert_eq!(idempotent.failed, 0, "{:?}", idempotent.failures);
    assert_eq!(idempotent.imported_events, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
}

#[test]
fn codex_child_before_parent_uses_and_enriches_eventless_placeholder() {
    let temp = tempdir().unwrap();
    let child_path = temp.path().join("a-child.jsonl");
    let parent_path = temp.path().join("z-parent.jsonl");
    fs::write(
        &child_path,
        [
            session_meta("child-session", Some("parent-session")),
            message(0),
        ]
        .concat(),
    )
    .unwrap();
    fs::write(
        &parent_path,
        [session_meta("parent-session", None), message(1)].concat(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = CodexSessionImportOptions {
        source_path: Some(temp.path().to_path_buf()),
        ..CodexSessionImportOptions::default()
    };

    let child = import_codex_session_jsonl(&child_path, &mut store, options.clone()).unwrap();
    assert_eq!(child.failed, 0, "{:?}", child.failures);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    let placeholder = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("parent-session"))
        .unwrap();
    assert_eq!(placeholder.sync.fidelity, Fidelity::Partial);
    assert_eq!(placeholder.sync.metadata["relationship_placeholder"], true);

    let parent = import_codex_session_jsonl(&parent_path, &mut store, options).unwrap();
    assert_eq!(parent.failed, 0, "{:?}", parent.failures);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    let parent = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("parent-session"))
        .unwrap();
    let child = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("child-session"))
        .unwrap();
    assert_eq!(parent.sync.fidelity, Fidelity::Imported);
    assert_ne!(parent.sync.metadata["relationship_placeholder"], true);
    assert_eq!(child.parent_session_id, Some(parent.id));
}
