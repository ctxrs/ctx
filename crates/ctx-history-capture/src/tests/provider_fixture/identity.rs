#[test]
fn provider_import_scopes_provenance_by_source_format_and_path() {
    let temp = tempdir();
    let shared_path = temp
        .path()
        .join("shared-source.jsonl")
        .display()
        .to_string();
    assert_provider_source_collision_is_distinct(
        "provider_format_a",
        &shared_path,
        "provider_format_b",
        &shared_path,
    );

    let first_path = temp.path().join("first-source.jsonl").display().to_string();
    let second_path = temp
        .path()
        .join("second-source.jsonl")
        .display()
        .to_string();
    assert_provider_source_collision_is_distinct(
        "provider_format",
        &first_path,
        "provider_format",
        &second_path,
    );
}

#[test]
fn provider_import_scopes_sessions_when_provider_session_id_collides_across_sources() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "shared-provider-session";
    let source_format = "provider_format";
    let first_path = temp.path().join("first-source.jsonl").display().to_string();
    let second_path = temp
        .path()
        .join("second-source.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let first = provider_collision_capture(
        provider,
        provider_session_id,
        source_format,
        &first_path,
        occurred_at,
    );
    let second = provider_collision_capture(
        provider,
        provider_session_id,
        source_format,
        &second_path,
        occurred_at + chrono::Duration::seconds(1),
    );

    let first_summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first.clone())],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first_summary.failed, 0, "{:?}", first_summary.failures);
    assert_eq!(first_summary.imported_sessions, 1);

    let legacy_session_id = provider_session_uuid(provider, provider_session_id);
    let first_source_identity = provider_source_root_identity(provider, source_format, &first_path);
    let first_source_session_id =
        provider_source_session_uuid(&first_source_identity, provider_session_id);
    assert!(store.get_session(legacy_session_id).is_err());
    assert!(store.get_session(first_source_session_id).is_ok());
    assert_eq!(
        store
            .events_for_session(first_source_session_id)
            .unwrap()
            .len(),
        1
    );

    let second_summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, second.clone())],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(second_summary.failed, 0, "{:?}", second_summary.failures);
    assert_eq!(second_summary.imported_sessions, 1);

    let second_source_identity =
        provider_source_root_identity(provider, source_format, &second_path);
    let second_source_session_id =
        provider_source_session_uuid(&second_source_identity, provider_session_id);
    assert!(store.get_session(second_source_session_id).is_ok());
    assert_eq!(
        store
            .events_for_session(second_source_session_id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .events_for_session(first_source_session_id)
            .unwrap()
            .len(),
        1
    );

    let first_reimport = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first)],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first_reimport.failed, 0, "{:?}", first_reimport.failures);
    assert_eq!(first_reimport.imported_sessions, 0);
    assert_eq!(first_reimport.skipped_sessions, 1);
    assert!(store.get_session(first_source_session_id).is_ok());

    let second_reimport = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, second)],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(second_reimport.failed, 0, "{:?}", second_reimport.failures);
    assert_eq!(second_reimport.imported_sessions, 0);
    assert_eq!(second_reimport.skipped_sessions, 1);
}

#[test]
fn provider_import_scopes_parent_edges_when_provider_session_ids_collide_across_sources() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let provider = CaptureProvider::Claude;
    let parent_provider_session_id = "shared-parent-session";
    let child_provider_session_id = "shared-child-session";
    let source_format = "provider_format";
    let first_path = temp.path().join("first-source.jsonl").display().to_string();
    let second_path = temp
        .path()
        .join("second-source.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);

    let first_parent = provider_collision_capture(
        provider,
        parent_provider_session_id,
        source_format,
        &first_path,
        occurred_at,
    );
    let mut first_child = provider_collision_capture(
        provider,
        child_provider_session_id,
        source_format,
        &first_path,
        occurred_at + chrono::Duration::seconds(1),
    );
    first_child.session.parent_provider_session_id = Some(parent_provider_session_id.to_owned());
    first_child.session.root_provider_session_id = Some(parent_provider_session_id.to_owned());
    first_child.session.agent_type = AgentType::Subagent;
    first_child.session.is_primary = false;

    let second_parent = provider_collision_capture(
        provider,
        parent_provider_session_id,
        source_format,
        &second_path,
        occurred_at + chrono::Duration::seconds(2),
    );
    let mut second_child = provider_collision_capture(
        provider,
        child_provider_session_id,
        source_format,
        &second_path,
        occurred_at + chrono::Duration::seconds(3),
    );
    second_child.session.parent_provider_session_id = Some(parent_provider_session_id.to_owned());
    second_child.session.root_provider_session_id = Some(parent_provider_session_id.to_owned());
    second_child.session.agent_type = AgentType::Subagent;
    second_child.session.is_primary = false;

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![
                (1, first_parent),
                (2, first_child),
                (3, second_parent),
                (4, second_child),
            ],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 4);
    assert_eq!(summary.imported_edges, 2);

    let first_source_identity = provider_source_root_identity(provider, source_format, &first_path);
    let first_parent_session_id =
        provider_source_session_uuid(&first_source_identity, parent_provider_session_id);
    let first_child_session_id =
        provider_source_session_uuid(&first_source_identity, child_provider_session_id);
    let second_source_identity =
        provider_source_root_identity(provider, source_format, &second_path);
    let second_parent_session_id =
        provider_source_session_uuid(&second_source_identity, parent_provider_session_id);
    let second_child_session_id =
        provider_source_session_uuid(&second_source_identity, child_provider_session_id);

    let first_child_session = store.get_session(first_child_session_id).unwrap();
    assert_eq!(
        first_child_session.parent_session_id,
        Some(first_parent_session_id)
    );
    assert_eq!(
        first_child_session.root_session_id,
        Some(first_parent_session_id)
    );

    let second_child_session = store.get_session(second_child_session_id).unwrap();
    assert_eq!(
        second_child_session.parent_session_id,
        Some(second_parent_session_id)
    );
    assert_eq!(
        second_child_session.root_session_id,
        Some(second_parent_session_id)
    );

    let conn = Connection::open(&db_path).unwrap();
    let edges = conn
        .prepare("SELECT id, from_session_id, to_session_id FROM session_edges ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let mut expected_edges = vec![
        (
            provider_source_edge_uuid(
                &first_source_identity,
                child_provider_session_id,
                "parent_child",
            )
            .to_string(),
            first_parent_session_id.to_string(),
            first_child_session_id.to_string(),
        ),
        (
            provider_source_edge_uuid(
                &second_source_identity,
                child_provider_session_id,
                "parent_child",
            )
            .to_string(),
            second_parent_session_id.to_string(),
            second_child_session_id.to_string(),
        ),
    ];
    expected_edges.sort();
    assert_eq!(edges, expected_edges);
}

#[test]
fn provider_import_scopes_cursor_progress_by_source_path() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let source_format = "provider_format";
    let first_path = temp.path().join("first-source.jsonl").display().to_string();
    let second_path = temp
        .path()
        .join("second-source.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut first = provider_collision_capture(
        provider,
        "shared-provider-session",
        source_format,
        &first_path,
        occurred_at,
    );
    first.event.as_mut().unwrap().cursor = Some("first-cursor".to_owned());
    let mut second = provider_collision_capture(
        provider,
        "shared-provider-session",
        source_format,
        &second_path,
        occurred_at,
    );
    second.event.as_mut().unwrap().cursor = Some("second-cursor".to_owned());

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first), (2, second)],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);
    let first_stream = provider_source_cursor_stream(provider, source_format, Some(&first_path));
    let second_stream = provider_source_cursor_stream(provider, source_format, Some(&second_path));
    assert_ne!(first_stream, second_stream);
    assert!(!first_stream.contains(first_path.as_str()));
    assert!(!second_stream.contains(second_path.as_str()));
    assert_eq!(
        store
            .get_sync_cursor(None, "test-machine", &first_stream)
            .unwrap()
            .unwrap()
            .cursor,
        "first-cursor"
    );
    assert_eq!(
        store
            .get_sync_cursor(None, "test-machine", &second_stream)
            .unwrap()
            .unwrap()
            .cursor,
        "second-cursor"
    );
    assert!(store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_cursor_stream(provider, source_format),
        )
        .unwrap()
        .is_none());
}

#[test]
fn provider_import_leaves_legacy_provider_cursor_without_panicking() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let source_format = "provider_format";
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let legacy_stream = provider_cursor_stream(provider, source_format);
    store
        .upsert_sync_cursor(&ctx_history_core::SyncCursor {
            id: stable_capture_uuid("legacy-provider-cursor", "provider-sync-cursor"),
            team_id: None,
            device_id: "test-machine".to_owned(),
            stream: legacy_stream.clone(),
            cursor: "legacy-cursor".to_owned(),
            last_synced_at: Some(occurred_at),
            timestamps: timestamps(occurred_at),
        })
        .unwrap();

    let mut capture = provider_collision_capture(
        provider,
        "default-source-session",
        source_format,
        "",
        occurred_at,
    );
    capture.source.raw_source_path = None;
    capture.source.idempotency_key = None;
    capture.event.as_mut().unwrap().cursor = Some("new-cursor".to_owned());
    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, capture)],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(
        store
            .get_sync_cursor(None, "test-machine", &legacy_stream)
            .unwrap()
            .unwrap()
            .cursor,
        "legacy-cursor"
    );
    let source_stream = provider_source_cursor_stream(provider, source_format, None);
    assert_eq!(
        store
            .get_sync_cursor(None, "test-machine", &source_stream)
            .unwrap()
            .unwrap()
            .cursor,
        "new-cursor"
    );
}

#[test]
fn provider_import_reuses_existing_legacy_provider_event_identity() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "legacy-provider-session";
    let source_format = "provider_format";
    let raw_source_path = temp
        .path()
        .join("legacy-source.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let legacy_source_id = provider_source_uuid(provider, provider_session_id);
    let new_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        source_format,
        Some(&raw_source_path),
    );
    let session_id = provider_session_uuid(provider, provider_session_id);
    let legacy_event_id = provider_event_uuid(provider, provider_session_id, 0);
    let legacy_touch_id = provider_file_touch_uuid(provider, provider_session_id, 0);
    let source_identity = provider_source_root_identity(provider, source_format, &raw_source_path);
    let event_hash = compute_payload_hash(&json!({"text": "same provider event payload"})).unwrap();
    assert_ne!(legacy_source_id, new_source_id);

    store
        .upsert_capture_source(&CaptureSource {
            id: legacy_source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/workspace/example".to_owned()),
                raw_source_path: Some(raw_source_path.clone()),
                source_format: Some(source_format.to_owned()),
                source_root: Some(raw_source_path.clone()),
                source_identity: Some(source_identity),
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: occurred_at,
            ended_at: None,
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(legacy_source_id),
            provider,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: None,
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_event(&Event {
            id: legacy_event_id,
            seq: provider_event_seq(provider, provider_session_id, 0),
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at,
            capture_source_id: Some(legacy_source_id),
            payload: json!({"body": {"text": "same provider event payload"}}),
            payload_blob_id: None,
            dedupe_key: Some(Store::provider_event_dedupe_key(
                provider,
                provider_session_id,
                0,
                &event_hash,
            )),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_file_touched(&FileTouched {
            id: legacy_touch_id,
            history_record_id: None,
            run_id: None,
            event_id: Some(legacy_event_id),
            vcs_workspace_id: None,
            path: "src/lib.rs".to_owned(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(1),
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(legacy_source_id),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();

    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![(
            1,
            provider_collision_capture(
                provider,
                provider_session_id,
                source_format,
                &raw_source_path,
                occurred_at,
            ),
        )],
        files_touched: vec![(
            1,
            provider_collision_file_touch(
                provider,
                provider_session_id,
                source_format,
                &raw_source_path,
                occurred_at,
            ),
        )],
    };

    let summary = import_normalized_provider_captures(
        &mut store,
        normalization,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped_events, 1);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, legacy_event_id);
    assert_eq!(events[0].capture_source_id, Some(legacy_source_id));

    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 1);
    assert_eq!(archive.files_touched[0].id, legacy_touch_id);
    assert_eq!(archive.files_touched[0].event_id, Some(legacy_event_id));
    assert_eq!(archive.files_touched[0].source_id, Some(new_source_id));
}

#[test]
fn provider_import_does_not_reuse_legacy_session_without_source_proof() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "unknown-legacy-provider-session";
    let source_format = "provider_format";
    let raw_source_path = temp
        .path()
        .join("unknown-legacy-source.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let legacy_session_id = provider_session_uuid(provider, provider_session_id);
    store
        .upsert_session(&Session {
            id: legacy_session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: None,
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    source_format,
                    &raw_source_path,
                    occurred_at,
                ),
            )],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(
        store
            .get_session(legacy_session_id)
            .unwrap()
            .capture_source_id,
        None
    );
    let source_identity = provider_source_root_identity(provider, source_format, &raw_source_path);
    let source_session_id = provider_source_session_uuid(&source_identity, provider_session_id);
    assert!(store.get_session(source_session_id).is_ok());
    let sessions = store
        .sessions_by_external_session_limited(provider, provider_session_id, 10)
        .unwrap()
        .into_iter()
        .map(|session| session.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        sessions,
        BTreeSet::from([legacy_session_id, source_session_id])
    );
    assert_eq!(
        store.events_for_session(source_session_id).unwrap().len(),
        1
    );
    assert!(store
        .events_for_session(legacy_session_id)
        .unwrap()
        .is_empty());
}

#[test]
fn provider_source_event_seq_keeps_large_provider_indices_distinct() {
    let source_id = Uuid::parse_str("018fe2e4-2266-7000-8000-000000000001").unwrap();

    assert_ne!(
        provider_source_event_seq(source_id, 0),
        provider_source_event_seq(source_id, 1_048_576)
    );
    assert_eq!(
        provider_source_event_seq(source_id, 1_048_576) & 0xffff_ffff,
        1_048_576
    );
}

#[test]
fn native_provider_import_rejects_tool_only_without_real_message() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let mut capture = provider_collision_capture(
        provider,
        "tool-only-native-session",
        "provider_format",
        "/tmp/tool-only-native-session.jsonl",
        DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
            .unwrap()
            .with_timezone(&Utc),
    );
    let event = capture.event.as_mut().unwrap();
    event.event_type = EventType::ToolCall;
    event.role = Some(EventRole::Tool);
    event.payload = json!({"text": "tool: shell | status: success"});

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, capture)],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store.search_event_hits("tool", 10).unwrap().is_empty());
}

#[test]
fn native_provider_import_skips_mixed_metadata_only_session() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let real_capture = provider_collision_capture(
        provider,
        "real-native-session",
        "provider_format",
        "/tmp/mixed-native-session.jsonl",
        occurred_at,
    );
    let mut metadata_only_capture = provider_collision_capture(
        provider,
        "metadata-only-native-session",
        "provider_format",
        "/tmp/mixed-native-session.jsonl",
        occurred_at,
    );
    metadata_only_capture.event = None;
    let metadata_only_touch = provider_collision_file_touch(
        provider,
        "metadata-only-native-session",
        "provider_format",
        "/tmp/mixed-native-session.jsonl",
        occurred_at,
    );

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, real_capture), (2, metadata_only_capture)],
            files_touched: vec![(2, metadata_only_touch)],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.skipped_sessions, 1);
    assert!(store
        .sessions_by_external_session_limited(provider, "metadata-only-native-session", 10)
        .unwrap()
        .is_empty());
    assert_eq!(store.export_archive().unwrap().files_touched.len(), 0);
}
