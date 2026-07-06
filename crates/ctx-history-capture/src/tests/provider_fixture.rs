use super::support::*;

#[test]
fn provider_fixture_replay_supports_antigravity_gemini_and_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let antigravity = provider_fixture("antigravity.jsonl");
    let antigravity_summary = import_provider_fixture_jsonl(
        &antigravity,
        &mut store,
        fixed_import_options(antigravity.clone()),
    )
    .unwrap();
    assert_eq!(antigravity_summary.failed, 0);
    assert_eq!(antigravity_summary.imported_sessions, 2);
    assert_eq!(antigravity_summary.imported_events, 3);
    assert_eq!(antigravity_summary.imported_edges, 1);
    let antigravity_parent = provider_session_uuid(CaptureProvider::Antigravity, "agy-session-1");
    let antigravity_child =
        provider_session_uuid(CaptureProvider::Antigravity, "agy-session-1-worker");
    assert_eq!(
        store
            .get_session(antigravity_child)
            .unwrap()
            .parent_session_id,
        Some(antigravity_parent)
    );

    let gemini = provider_fixture("gemini.jsonl");
    let gemini_summary =
        import_provider_fixture_jsonl(&gemini, &mut store, fixed_import_options(gemini.clone()))
            .unwrap();
    assert_eq!(gemini_summary.failed, 0);
    assert_eq!(gemini_summary.imported_sessions, 1);
    assert_eq!(gemini_summary.imported_events, 2);
    let gemini_session = provider_session_uuid(CaptureProvider::Gemini, "gemini-session-1");
    let gemini_events = store.events_for_session(gemini_session).unwrap();
    assert_eq!(gemini_events[1].event_type, EventType::ToolOutput);
    assert_eq!(
        gemini_events[1].sync.metadata["metadata"]["telemetry_outfile"].as_str(),
        Some(".gemini/telemetry.log")
    );

    let cursor = provider_fixture("cursor.jsonl");
    let cursor_summary =
        import_provider_fixture_jsonl(&cursor, &mut store, fixed_import_options(cursor.clone()))
            .unwrap();
    assert_eq!(cursor_summary.failed, 0);
    assert_eq!(cursor_summary.imported_sessions, 1);
    assert_eq!(cursor_summary.imported_events, 2);
    let cursor_session = provider_session_uuid(CaptureProvider::Cursor, "cursor-session-1");
    let cursor_events = store.events_for_session(cursor_session).unwrap();
    assert_eq!(cursor_events[1].event_type, EventType::ToolCall);
    assert_eq!(
        cursor_events[0].sync.metadata["metadata"]["docs_surface"].as_str(),
        Some("Cursor CLI sessions and stream-json output")
    );
}

#[test]
fn provider_fixture_replay_is_idempotent_for_native_supported_providers() {
    for (name, provider, external_session_id, sessions, events, edges) in [
        (
            "claude.jsonl",
            CaptureProvider::Claude,
            "claude-session-1",
            1,
            2,
            0,
        ),
        (
            "opencode.jsonl",
            CaptureProvider::OpenCode,
            "opencode-session-1",
            2,
            3,
            1,
        ),
        (
            "antigravity.jsonl",
            CaptureProvider::Antigravity,
            "agy-session-1",
            2,
            3,
            1,
        ),
        (
            "gemini.jsonl",
            CaptureProvider::Gemini,
            "gemini-session-1",
            1,
            2,
            0,
        ),
        (
            "cursor.jsonl",
            CaptureProvider::Cursor,
            "cursor-session-1",
            1,
            2,
            0,
        ),
    ] {
        let temp = tempdir();
        let fixture = provider_fixture(name);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{name}: {:?}", first.failures);
        assert_eq!(first.imported_sessions, sessions, "{name}");
        assert_eq!(first.imported_events, events, "{name}");
        assert_eq!(first.imported_edges, edges, "{name}");

        let second = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{name}: {:?}", second.failures);
        assert_eq!(second.imported_sessions, 0, "{name}");
        assert_eq!(second.imported_events, 0, "{name}");
        assert_eq!(second.imported_edges, 0, "{name}");
        assert_eq!(second.skipped_sessions, sessions, "{name}");
        assert_eq!(second.skipped_events, events, "{name}");
        assert_eq!(second.skipped_edges, edges, "{name}");

        let session_id = provider_session_uuid(provider, external_session_id);
        assert!(!store.events_for_session(session_id).unwrap().is_empty());
    }
}

#[test]
fn provider_fixture_replay_supports_search_only_temp_fixtures() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    for (
        fixture_name,
        provider,
        external_session_id,
        fixture_sessions,
        fixture_events,
        fixture_edges,
    ) in [
        (
            "copilot_cli.jsonl",
            CaptureProvider::CopilotCli,
            "copilot-cli-session-1",
            1,
            2,
            0,
        ),
        (
            "factory_ai_droid.jsonl",
            CaptureProvider::FactoryAiDroid,
            "factory-ai-droid-session-1",
            2,
            3,
            1,
        ),
    ] {
        let fixture = provider_fixture(fixture_name);
        let (fixture, sessions, events, edges) = if fixture.exists() {
            (fixture, fixture_sessions, fixture_events, fixture_edges)
        } else {
            (
                write_minimal_provider_fixture(&temp, provider, external_session_id),
                1,
                1,
                0,
            )
        };
        let mut options = fixed_import_options(fixture.clone());
        options.expected_provider = Some(provider);

        let first = import_provider_fixture_jsonl(&fixture, &mut store, options.clone()).unwrap();
        assert_eq!(first.failed, 0, "{provider}: {:?}", first.failures);
        assert_eq!(first.imported_sessions, sessions, "{provider}");
        assert_eq!(first.imported_events, events, "{provider}");
        assert_eq!(first.imported_edges, edges, "{provider}");

        let second = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();
        assert_eq!(second.failed, 0, "{provider}: {:?}", second.failures);
        assert_eq!(second.imported_sessions, 0, "{provider}");
        assert_eq!(second.imported_events, 0, "{provider}");
        assert_eq!(second.imported_edges, 0, "{provider}");
        assert_eq!(second.skipped_sessions, sessions, "{provider}");
        assert_eq!(second.skipped_events, events, "{provider}");
        assert_eq!(second.skipped_edges, edges, "{provider}");

        let session_id = provider_session_uuid(provider, external_session_id);
        let session = store.get_session(session_id).unwrap();
        assert_eq!(session.provider, provider);
        assert!(!store.events_for_session(session_id).unwrap().is_empty());
    }
}

#[test]
fn provider_fixture_replay_persists_cursor_checkpoint_and_source_contract_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("codex.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    let cursor = store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_cursor_stream(CaptureProvider::Codex, "normalized_provider_fixture_jsonl"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, "codex-sub-cursor-0");

    let source = store
        .capture_source_by_external_session(CaptureProvider::Codex, "codex-session-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_format"].as_str(),
        Some("normalized_provider_fixture_jsonl")
    );
    assert_eq!(
        source.sync.metadata["source_trust"].as_str(),
        Some("fixture")
    );
    assert_eq!(
        source.sync.metadata["raw_retention"].as_str(),
        Some("path_reference")
    );
    assert_eq!(
        source.sync.metadata["redaction_boundary"].as_str(),
        Some("before_export")
    );
    assert!(source.sync.metadata["source_idempotency_key"]
        .as_str()
        .is_some());
}

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
                raw_source_path: None,
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
            redaction_state: RedactionState::LocalPreview,
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
