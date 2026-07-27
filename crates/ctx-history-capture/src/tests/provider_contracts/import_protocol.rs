use crate::provider::importer::{
    import_normalized_provider_captures, import_normalized_provider_captures_in_batches,
    import_provider_capture_line, ProviderImportCaches,
};
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::{
    provider_collision_capture, provider_collision_file_touch,
};
use crate::{
    compute_payload_hash, CaptureError, NormalizedProviderImportOptions, ProviderImportSummary,
    ProviderNormalizationResult,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

fn strip_provider_event_hash_authority(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    let metadata: String = conn
        .query_row("SELECT metadata_json FROM events LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut metadata: Value = serde_json::from_str(&metadata).unwrap();
    metadata
        .as_object_mut()
        .unwrap()
        .remove("provider_event_hash_authority");
    conn.execute(
        "UPDATE events SET metadata_json = ?1",
        [serde_json::to_string(&metadata).unwrap()],
    )
    .unwrap();
}

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
fn batched_provider_import_publishes_through_pinned_wal_and_resumes_idempotently() {
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
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
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

    let first_import = import_normalized_provider_captures_in_batches(
        &mut store,
        normalization.clone(),
        options.clone(),
        1,
    )
    .unwrap();
    assert_eq!(first_import.failed, 0, "{:?}", first_import.failures);
    assert_eq!(first_import.imported_sessions, 2);
    assert_eq!(first_import.imported_events, 2);
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "the pinned reader must retain its original snapshot"
    );
    reader.execute_batch("ROLLBACK").unwrap();

    // Search rows commit with their events. Segment/WAL maintenance is durable debt and does not
    // block publication behind the reader's old snapshot.
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(
        store
            .search_event_hits("batched-import-sentinel-first", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .search_event_hits("batched-import-sentinel-second", 10)
            .unwrap()
            .len(),
        1
    );

    let resumed = import_normalized_provider_captures_in_batches(
        &mut store,
        normalization.clone(),
        options.clone(),
        1,
    )
    .unwrap();
    assert_eq!(resumed.failed, 0, "{:?}", resumed.failures);
    assert_eq!(resumed.imported_events, 0);
    assert_eq!(resumed.skipped_events, 2);
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
fn provider_import_uses_shared_bounded_batches_through_pinned_reader() {
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

    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures,
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..NormalizedProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 64);
    assert_eq!(summary.imported_events, 64);
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "the pinned reader must retain its original snapshot"
    );
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
fn batched_provider_import_rotates_on_serialized_byte_budget_with_pinned_reader() {
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
    let summary = import_normalized_provider_captures_in_batches(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first), (2, second)],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..NormalizedProviderImportOptions::default()
        },
        64,
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "the pinned reader must retain its original snapshot"
    );
    reader.execute_batch("ROLLBACK").unwrap();

    // Byte-budget rotation still commits two bounded slices. Deferred FTS/WAL maintenance does
    // not turn the reader's pinned snapshot into a publication failure.
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(
        store
            .search_event_hits("byte-budget-sentinel-first", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .search_event_hits("byte-budget-sentinel-second", 10)
            .unwrap()
            .len(),
        1
    );
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
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
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
    let mut first = provider_collision_capture(
        CaptureProvider::Hermes,
        "atomic-conflict",
        "hermes_state_sqlite",
        &source_path,
        occurred_at,
    );
    first.event.as_mut().unwrap().provider_event_hash = Some("provider-event-original".to_owned());
    let mut conflicting = first.clone();
    let conflicting_event = conflicting.event.as_mut().unwrap();
    conflicting_event.provider_event_hash = Some("provider-event-conflict".to_owned());
    conflicting_event.payload = json!({"text": "conflicting payload"});

    let error = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(1, first), (2, conflicting)],
            files_touched: Vec::new(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..NormalizedProviderImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::Store(ctx_history_store::StoreError::ProviderEventConflict { .. })
    ));
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("same provider event payload", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn provider_import_reconciles_legacy_fallback_hash_drift() {
    for fast_event_inserts in [true, false] {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let mut store = Store::open(&db_path).unwrap();
        let occurred_at = DateTime::parse_from_rfc3339("2026-07-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let source_path = temp.path().join("fallback-drift.jsonl");
        let source_path = source_path.display().to_string();
        let mut legacy = provider_collision_capture(
            CaptureProvider::Codex,
            &format!("fallback-drift-{fast_event_inserts}"),
            "codex_session_jsonl",
            &source_path,
            occurred_at,
        );
        let legacy_payload = json!({
            "text": "stable provider content",
            "truncated": false,
            "content_retention": "full"
        });
        legacy.event.as_mut().unwrap().payload = legacy_payload.clone();
        let imported = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, legacy.clone())],
                files_touched: Vec::new(),
            },
            NormalizedProviderImportOptions {
                fast_event_inserts,
                ..NormalizedProviderImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(imported.imported_events, 1);
        let original_event = store.export_archive().unwrap().events.remove(0);
        assert_eq!(
            original_event.sync.metadata["provider_event_hash_authority"],
            "normalized_payload_fallback"
        );

        // Schema-v46/v47 indexes predate the explicit authority marker. The stored normalized
        // body is the migration evidence that its fnv1a64 key was generated by ctx.
        strip_provider_event_hash_authority(&db_path);
        let mut normalized = legacy;
        let normalized_payload = json!({
            "text": "stable provider content",
            "text_retention": {"status": "full"},
            "content_preview": {"text": "stable provider content", "retention": "full"}
        });
        normalized.event.as_mut().unwrap().payload = normalized_payload.clone();
        let normalized_hash = compute_payload_hash(&normalized_payload).unwrap();

        let reimported = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, normalized)],
                files_touched: Vec::new(),
            },
            NormalizedProviderImportOptions {
                fast_event_inserts,
                ..NormalizedProviderImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(reimported.failed, 0, "{:?}", reimported.failures);
        assert_eq!(reimported.imported_events, 0);
        assert_eq!(reimported.skipped_events, 1);

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.events.len(), 1);
        let migrated = &archive.events[0];
        assert_eq!(migrated.id, original_event.id);
        assert_eq!(migrated.payload["body"], normalized_payload);
        assert_eq!(
            migrated.payload["provider_event_hash"],
            normalized_hash.as_str()
        );
        assert!(migrated
            .dedupe_key
            .as_deref()
            .unwrap()
            .ends_with(&normalized_hash));
        assert_eq!(
            migrated.sync.metadata["provider_event_hash_authority"],
            "normalized_payload_fallback"
        );
    }
}

#[test]
fn fallback_reimport_does_not_replace_legacy_provider_supplied_identity() {
    for fast_event_inserts in [true, false] {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let mut store = Store::open(&db_path).unwrap();
        let occurred_at = DateTime::parse_from_rfc3339("2026-07-17T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let source_path = temp.path().join("provider-identity.jsonl");
        let source_path = source_path.display().to_string();
        let mut supplied = provider_collision_capture(
            CaptureProvider::Claude,
            &format!("provider-identity-{fast_event_inserts}"),
            "claude_projects_jsonl",
            &source_path,
            occurred_at,
        );
        supplied.event.as_mut().unwrap().provider_event_hash =
            Some("provider-native-event-id".to_owned());
        import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, supplied.clone())],
                files_touched: Vec::new(),
            },
            NormalizedProviderImportOptions {
                fast_event_inserts,
                ..NormalizedProviderImportOptions::default()
            },
        )
        .unwrap();
        let original_event = store.export_archive().unwrap().events.remove(0);

        strip_provider_event_hash_authority(&db_path);
        let mut fallback = supplied;
        let event = fallback.event.as_mut().unwrap();
        event.provider_event_hash = None;
        event.payload = json!({"text": "replacement must be rejected"});
        let error = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, fallback)],
                files_touched: Vec::new(),
            },
            NormalizedProviderImportOptions {
                fast_event_inserts,
                ..NormalizedProviderImportOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CaptureError::Store(ctx_history_store::StoreError::ProviderEventConflict { .. })
        ));

        let events = store.export_archive().unwrap().events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, original_event.id);
        assert_eq!(events[0].payload, original_event.payload);
        assert_eq!(events[0].dedupe_key, original_event.dedupe_key);
        assert!(events[0]
            .sync
            .metadata
            .get("provider_event_hash_authority")
            .is_none());
    }
}
