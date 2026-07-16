use super::support::*;

#[test]
fn normalized_provider_import_accepts_v1_during_bounded_v2_transition() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-13T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut capture = provider_collision_capture(
        CaptureProvider::Hermes,
        "v1-compatible-session",
        "hermes_state_sqlite",
        "/tmp/v1-compatible-session.db",
        occurred_at,
    );
    capture.schema_version = 1;

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, capture)],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 1);
}

#[test]
fn normalized_provider_import_rejects_versions_outside_v1_v2_window() {
    for schema_version in [0, 3] {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let occurred_at = DateTime::parse_from_rfc3339("2026-07-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut capture = provider_collision_capture(
            CaptureProvider::Hermes,
            &format!("unsupported-v{schema_version}-session"),
            "hermes_state_sqlite",
            &format!("/tmp/unsupported-v{schema_version}-session.db"),
            occurred_at,
        );
        capture.schema_version = schema_version;

        let summary = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, capture)],
                files_touched: Vec::new(),
            },
            NormalizedProviderImportOptions::default(),
        )
        .unwrap();

        assert_eq!(summary.failed, 1, "schema v{schema_version}");
        assert!(summary.failures[0]
            .error
            .contains("unsupported provider capture envelope schema version"));
        assert!(store.list_sessions().unwrap().is_empty());
    }
}

#[test]
fn batched_provider_import_rejects_unwrapped_and_zero_sized_modes() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T11:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let capture = provider_collision_capture(
        CaptureProvider::Hermes,
        "invalid-batch-options",
        "hermes_state_sqlite",
        "/tmp/invalid-batch-options.db",
        occurred_at,
    );
    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![(1, capture)],
        files_touched: Vec::new(),
    };

    let unwrapped = import_normalized_provider_captures_in_batches(
        &mut store,
        normalization.clone(),
        NormalizedProviderImportOptions {
            wrap_transaction: false,
            ..NormalizedProviderImportOptions::default()
        },
        1,
    )
    .unwrap_err();
    assert!(unwrapped
        .to_string()
        .contains("requires transaction wrapping"));

    let zero = import_normalized_provider_captures_in_batches(
        &mut store,
        normalization,
        NormalizedProviderImportOptions {
            ..NormalizedProviderImportOptions::default()
        },
        0,
    )
    .unwrap_err();
    assert!(zero
        .to_string()
        .contains("batch size must be greater than zero"));
}

#[test]
fn normalized_provider_preflight_rejects_invalid_event_without_losing_valid_content() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-13T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("preflight.jsonl");
    let source_path = source_path.display().to_string();
    let accepted = provider_collision_capture(
        CaptureProvider::Hermes,
        "accepted-session",
        "hermes_state_sqlite",
        &source_path,
        occurred_at,
    );
    let mut rejected = provider_collision_capture(
        CaptureProvider::Hermes,
        "accepted-session",
        "hermes_state_sqlite",
        &source_path,
        occurred_at + chrono::Duration::seconds(1),
    );
    let rejected_event = rejected.event.as_mut().unwrap();
    rejected_event.event_type = EventType::CommandOutput;
    rejected_event.role = Some(EventRole::Tool);
    rejected_event.payload = json!({
        "command": "cargo test",
        "duration_ms": -1,
    });

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, accepted), (2, rejected)],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.imported_events, 1, "{:?}", summary.failures);
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("duration_ms must be nonnegative"));
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].external_session_id.as_deref(),
        Some("accepted-session")
    );
}

#[test]
fn provider_line_preflight_rejects_before_persisting_scaffolding() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-13T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut capture = provider_collision_capture(
        CaptureProvider::Hermes,
        "rejected-session",
        "hermes_state_sqlite",
        "/tmp/rejected-session.db",
        occurred_at,
    );
    let event = capture.event.as_mut().unwrap();
    event.event_type = EventType::CommandOutput;
    event.role = Some(EventRole::Tool);
    event.payload = json!({"command": "cargo test", "duration_ms": -1});

    let error = import_provider_capture_line(
        &mut store,
        &capture,
        &NormalizedProviderImportOptions::default(),
        1,
        &mut ProviderImportCaches::default(),
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("duration_ms must be nonnegative"));
    assert!(store.list_capture_sources().unwrap().is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn batched_provider_import_stops_on_pinned_wal_and_resumes_idempotently() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("batched-provider.jsonl");
    let source_path = source_path.display().to_string();
    let mut first = provider_collision_capture(
        CaptureProvider::Hermes,
        "batched-provider-first",
        "hermes_state_sqlite",
        &source_path,
        occurred_at,
    );
    first.event.as_mut().unwrap().payload = json!({"text": "batched-import-sentinel-first"});
    let mut second = provider_collision_capture(
        CaptureProvider::Hermes,
        "batched-provider-second",
        "hermes_state_sqlite",
        &source_path,
        occurred_at + chrono::Duration::seconds(1),
    );
    second.event.as_mut().unwrap().payload = json!({"text": "batched-import-sentinel-second"});
    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![(1, first), (2, second)],
        files_touched: Vec::new(),
    };
    let options = NormalizedProviderImportOptions {
        fast_event_inserts: true,
        ..NormalizedProviderImportOptions::default()
    };

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let initial_events = reader
        .query_row("SELECT COUNT(*) FROM events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(initial_events, 0);

    let error = import_normalized_provider_captures_in_batches(
        &mut store,
        normalization.clone(),
        options.clone(),
        1,
    )
    .unwrap_err();
    assert!(error.to_string().contains("ctx index is busy"), "{error}");
    reader.execute_batch("ROLLBACK").unwrap();

    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store
            .search_event_hits("batched-import-sentinel-first", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("batched-import-sentinel-second", 10)
        .unwrap()
        .is_empty());

    let resumed = import_normalized_provider_captures_in_batches(
        &mut store,
        normalization.clone(),
        options.clone(),
        1,
    )
    .unwrap();
    assert_eq!(resumed.failed, 0, "{:?}", resumed.failures);
    assert_eq!(resumed.imported_events, 1);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(
        store
            .search_event_hits("batched-import-sentinel-second", 10)
            .unwrap()
            .len(),
        1
    );

    let replayed =
        import_normalized_provider_captures_in_batches(&mut store, normalization, options, 1)
            .unwrap();
    assert_eq!(replayed.imported_events, 0);
    assert_eq!(replayed.skipped_events, 2);
    assert_eq!(
        store
            .search_event_hits("batched-import-sentinel", 10)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn provider_import_uses_shared_bounded_batches() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T12:15:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("shared-bounded-provider.jsonl");
    let source_path = source_path.display().to_string();
    let captures = (0..64)
        .map(|index| {
            let mut capture = provider_collision_capture(
                CaptureProvider::Hermes,
                &format!("shared-bounded-{index}"),
                "hermes_state_sqlite",
                &source_path,
                occurred_at + chrono::Duration::seconds(index),
            );
            capture.event.as_mut().unwrap().payload =
                json!({"text": format!("shared-bounded-sentinel-{index}")});
            (index as usize + 1, capture)
        })
        .collect();
    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    let error = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures,
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            ..NormalizedProviderImportOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("ctx index is busy"), "{error}");
    reader.execute_batch("ROLLBACK").unwrap();

    assert_eq!(store.list_sessions().unwrap().len(), 64);
    assert_eq!(
        store
            .search_event_hits("shared-bounded-sentinel", 100)
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn provider_import_uses_shared_bulk_search_guard() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T12:20:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("shared-bulk-provider.jsonl");
    let source_path = source_path.display().to_string();
    let other_store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let guard = other_store.begin_event_search_bulk_mode().unwrap();

    let error = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(
                1,
                provider_collision_capture(
                    CaptureProvider::Claude,
                    "shared-bulk",
                    "claude_projects_jsonl",
                    &source_path,
                    occurred_at,
                ),
            )],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            ..NormalizedProviderImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("another bulk search import is active"));
    other_store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn batched_provider_import_rotates_on_serialized_byte_budget() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T12:30:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("byte-batched-provider.db");
    let source_path = source_path.display().to_string();
    let mut first = provider_collision_capture(
        CaptureProvider::Hermes,
        "byte-batched-first",
        "hermes_state_sqlite",
        &source_path,
        occurred_at,
    );
    first.event.as_mut().unwrap().payload =
        json!({"text": format!("byte-budget-sentinel-first {}", "a".repeat(4_500_000))});
    let mut second = provider_collision_capture(
        CaptureProvider::Hermes,
        "byte-batched-second",
        "hermes_state_sqlite",
        &source_path,
        occurred_at + chrono::Duration::seconds(1),
    );
    second.event.as_mut().unwrap().payload =
        json!({"text": format!("byte-budget-sentinel-second {}", "b".repeat(4_500_000))});

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let error = import_normalized_provider_captures_in_batches(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first), (2, second)],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            ..NormalizedProviderImportOptions::default()
        },
        64,
    )
    .unwrap_err();
    assert!(error.to_string().contains("ctx index is busy"), "{error}");
    reader.execute_batch("ROLLBACK").unwrap();

    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store
            .search_event_hits("byte-budget-sentinel-first", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("byte-budget-sentinel-second", 10)
        .unwrap()
        .is_empty());
    store.optimize_search_index().unwrap();
}

#[test]
fn batched_provider_import_chunks_edges_and_file_touches() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T13:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("batched-graph.jsonl");
    let source_path = source_path.display().to_string();
    let parent = provider_collision_capture(
        CaptureProvider::Hermes,
        "batched-parent",
        "hermes_state_sqlite",
        &source_path,
        occurred_at,
    );
    let mut child = provider_collision_capture(
        CaptureProvider::Hermes,
        "batched-child",
        "hermes_state_sqlite",
        &source_path,
        occurred_at + chrono::Duration::seconds(1),
    );
    child.session.parent_provider_session_id = Some("batched-parent".to_owned());
    let files_touched = vec![
        (
            1,
            provider_collision_file_touch(
                CaptureProvider::Hermes,
                "batched-parent",
                "hermes_state_sqlite",
                &source_path,
                occurred_at,
            ),
        ),
        (
            2,
            provider_collision_file_touch(
                CaptureProvider::Hermes,
                "batched-child",
                "hermes_state_sqlite",
                &source_path,
                occurred_at + chrono::Duration::seconds(1),
            ),
        ),
    ];
    let summary = import_normalized_provider_captures_in_batches(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, parent), (2, child)],
            files_touched,
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            ..NormalizedProviderImportOptions::default()
        },
        1,
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_edges, 1);
    assert_eq!(store.export_archive().unwrap().files_touched.len(), 2);
}

#[test]
fn provider_import_propagates_store_conflicts_and_rolls_back_active_batch() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-07-11T14:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let source_path = temp.path().join("atomic-conflict.jsonl");
    let source_path = source_path.display().to_string();
    let first = provider_collision_capture(
        CaptureProvider::Hermes,
        "atomic-conflict",
        "hermes_state_sqlite",
        &source_path,
        occurred_at,
    );
    let mut conflicting = first.clone();
    conflicting.event.as_mut().unwrap().payload = json!({"text": "conflicting payload"});

    let error = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first), (2, conflicting)],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            ..NormalizedProviderImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(error, CaptureError::Store(_)), "{error:?}");
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("same provider event payload", 10)
        .unwrap()
        .is_empty());
}

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
    let antigravity_parent =
        provider_fixture_session_id(CaptureProvider::Antigravity, "agy-session-1", &antigravity);
    let antigravity_child = provider_fixture_session_id(
        CaptureProvider::Antigravity,
        "agy-session-1-worker",
        &antigravity,
    );
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
    let gemini_session =
        provider_fixture_session_id(CaptureProvider::Gemini, "gemini-session-1", &gemini);
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
    let cursor_session =
        provider_fixture_session_id(CaptureProvider::Cursor, "cursor-session-1", &cursor);
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

        let session_id = provider_fixture_session_id(provider, external_session_id, &fixture);
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

        let session_id = provider_fixture_session_id(provider, external_session_id, &fixture);
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
    let source_path = fixture.display().to_string();
    let cursor_stream = provider_source_cursor_stream(
        CaptureProvider::Codex,
        "normalized_provider_fixture_jsonl",
        Some(&source_path),
    );
    let cursor = store
        .get_sync_cursor(None, "test-machine", &cursor_stream)
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
    assert!(source.sync.metadata["source_idempotency_key"]
        .as_str()
        .is_some());
    assert_eq!(
        source.sync.metadata["cursor"]["after"]["stream"].as_str(),
        Some(cursor_stream.as_str())
    );
    assert!(!cursor_stream.contains(source_path.as_str()));
}
