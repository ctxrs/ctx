use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use chrono::{DateTime, TimeDelta, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, Confidence, EntityTimestamps, Event, EventRole, EventType, FileTouched, Run,
    RunStatus, RunType, Session, SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
    SyncMetadata,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;

const FINGERPRINT: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
static NEXT_TEST_PUBLICATION: AtomicU64 = AtomicU64::new(1);

fn now() -> DateTime<Utc> {
    "2026-07-25T00:00:00Z".parse().unwrap()
}

fn open_store() -> (TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    (temp, store)
}

fn accounting() -> NativePathGroupAccounting {
    NativePathGroupAccounting::new(1, 1, 64).unwrap()
}

fn begin_unclassified_group<'a>(
    store: &'a Store,
    guard: &crate::EventSearchBulkGuard,
    coordinator: NativePathGroupAccounting,
) -> NativePathPublicationGroup<'a> {
    let admission = store.admit_event_search_bulk_group(guard).unwrap();
    store
        .begin_native_path_publication_group(admission, coordinator)
        .unwrap()
}

fn begin_group_details<'a>(
    store: &'a Store,
    guard: &crate::EventSearchBulkGuard,
) -> (NativePathPublicationGroup<'a>, NativePathCursorKey, usize) {
    let serial = NEXT_TEST_PUBLICATION.fetch_add(1, Ordering::SeqCst);
    let publication_id = format!("test-publication-{serial:020}");
    let key = NativePathCursorKey::new(
        None,
        "test-machine",
        format!("native-path:test-{serial:020}"),
    );
    let transition = NativePathCursorTransition::new(None, cursor(&key, "next", 1));
    let envelope = NativePathCommittedCursorEnvelope {
        version: NATIVE_PATH_CURSOR_ENVELOPE_VERSION,
        publication_id: publication_id.clone(),
        provider_cursor: transition.next.cursor.clone(),
        journal_checkpoint: None,
    };
    let mut committed = transition.next.clone();
    committed.cursor = encode_cursor_envelope(&envelope).unwrap();
    let inactive_cursor_bind_bytes = encoded_cursor_cas_bytes(None, &committed);

    let mut group = begin_unclassified_group(store, guard, accounting());
    assert_eq!(
        group
            .classify_cursor_set(&publication_id, std::slice::from_ref(&transition))
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    (group, key, inactive_cursor_bind_bytes)
}

fn begin_group<'a>(
    store: &'a Store,
    guard: &crate::EventSearchBulkGuard,
) -> NativePathPublicationGroup<'a> {
    begin_group_details(store, guard).0
}

fn publish_and_commit(mut group: NativePathPublicationGroup<'_>) -> Result<NativePathGroupReceipt> {
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()
}

fn source(id: Uuid) -> CaptureSource {
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine".to_owned(),
            process_id: Some(42),
            cwd: Some("/repo".to_owned()),
            raw_source_path: Some("/repo/session.jsonl".to_owned()),
            source_format: Some("codex-jsonl".to_owned()),
            source_root: Some("/repo".to_owned()),
            source_identity: Some(format!("source-{id}")),
            external_session_id: Some(format!("session-{id}")),
        },
        started_at: now(),
        ended_at: None,
        sync: SyncMetadata::default(),
    }
}

fn session(id: Uuid, source_id: Option<Uuid>) -> Session {
    Session {
        id,
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: source_id,
        provider: CaptureProvider::Codex,
        external_session_id: Some(format!("session-{id}")),
        external_agent_id: Some(format!("agent-{id}")),
        agent_type: AgentType::Subagent,
        role_hint: Some("worker".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: now(),
        ended_at: None,
        timestamps: EntityTimestamps {
            created_at: now(),
            updated_at: now(),
        },
        sync: SyncMetadata::default(),
    }
}

fn event(id: Uuid, seq: u64, source_id: Option<Uuid>, session_id: Option<Uuid>) -> Event {
    Event {
        id,
        seq,
        history_record_id: None,
        session_id,
        run_id: None,
        event_type: EventType::Notice,
        role: Some(EventRole::Assistant),
        occurred_at: now(),
        capture_source_id: source_id,
        payload: json!({"message": format!("event-{seq}")}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata::default(),
    }
}

fn run(id: Uuid, source_id: Uuid, session_id: Uuid) -> Run {
    Run {
        id,
        history_record_id: None,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: RunStatus::Failed,
        started_at: now(),
        ended_at: Some(now()),
        exit_code: Some(1),
        cwd: Some("/repo".to_owned()),
        command_preview: Some("cargo test".to_owned()),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: EntityTimestamps {
            created_at: now(),
            updated_at: now(),
        },
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    }
}

fn file_touch(id: Uuid, event_id: Uuid, source_id: Uuid) -> FileTouched {
    FileTouched {
        id,
        history_record_id: None,
        run_id: None,
        event_id: Some(event_id),
        vcs_workspace_id: None,
        path: "src/lib.rs".to_owned(),
        change_kind: None,
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        timestamps: EntityTimestamps {
            created_at: now(),
            updated_at: now(),
        },
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    }
}

fn actor(value: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: value.id,
        root_session_id: value.root_session_id.unwrap_or(value.id),
        parent_session_id: value.parent_session_id,
        external_session_id: value.external_session_id.clone(),
        external_agent_id: value.external_agent_id.clone(),
        agent_type: value.agent_type.as_str().to_owned(),
        role_hint: value.role_hint.clone(),
        is_primary: value.is_primary,
    }
}

fn edge(id: Uuid, child: Uuid, parent: Uuid) -> SessionEdge {
    SessionEdge {
        id,
        from_session_id: child,
        to_session_id: parent,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: None,
        timestamps: EntityTimestamps {
            created_at: now(),
            updated_at: now(),
        },
        sync: SyncMetadata::default(),
    }
}

fn cursor(key: &NativePathCursorKey, payload: &str, offset_seconds: i64) -> SyncCursor {
    let updated_at = now() + TimeDelta::seconds(offset_seconds);
    SyncCursor {
        id: Uuid::new_v4(),
        team_id: key.team_id().map(ToOwned::to_owned),
        device_id: key.device_id().to_owned(),
        stream: key.stream().to_owned(),
        cursor: payload.to_owned(),
        last_synced_at: Some(updated_at),
        timestamps: EntityTimestamps {
            created_at: now(),
            updated_at,
        },
    }
}

fn seed_projection_events(store: &Store, source_id: Uuid, payload_sizes: &[usize]) {
    store
        .with_atomic_write(|| {
            let mut statement = store.conn.prepare(
                "INSERT INTO events
                 (id, seq, event_type, role, occurred_at_ms, capture_source_id,
                  payload_json, metadata_json)
                 VALUES (?1, ?2, 'notice', 'assistant', 0, ?3, ?4, '{}')",
            )?;
            for (index, payload_size) in payload_sizes.iter().copied().enumerate() {
                let seq = u64::try_from(index + 1).unwrap();
                statement.execute(params![
                    Uuid::from_u128(u128::from(seq) + 100).to_string(),
                    i64::try_from(seq).unwrap(),
                    source_id.to_string(),
                    serde_json::to_string(&json!({"message": "x".repeat(payload_size)}))?,
                ])?;
            }
            Ok(())
        })
        .unwrap();
}

fn projected_source_with_events(payload_sizes: &[usize]) -> (TempDir, Store, CaptureSource) {
    let (temp, store) = open_store();
    let source = source(Uuid::from_u128(1));
    store.upsert_capture_source(&source).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    seed_projection_events(&store, source.id, payload_sizes);
    (temp, store, source)
}

fn assert_nested_begin_poison_rolls_back_and_recovers(
    store: &Store,
    guard: &crate::EventSearchBulkGuard,
    original: &CaptureSource,
    admission: crate::EventSearchBulkGroupAdmission,
    coordinator: NativePathGroupAccounting,
    marker: &str,
) {
    let mut updated = original.clone();
    updated.sync.metadata = json!({"nested_begin": marker});
    let (mut group, cursor_key, _) = begin_group_details(store, guard);
    group.upsert_capture_source(&updated).unwrap();
    assert!(group.prepare_journal_checkpoint().unwrap().is_some());
    group.publish_cursor_set().unwrap();

    assert!(matches!(
        store
            .begin_native_path_publication_group(admission, coordinator)
            .err()
            .unwrap(),
        StoreError::NativePathGroupAlreadyActive
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert_eq!(store.get_capture_source(original.id).unwrap(), *original);
    assert!(store
        .get_sync_cursor(
            cursor_key.team_id(),
            cursor_key.device_id(),
            cursor_key.stream()
        )
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );

    let mut recovery = begin_group(store, guard);
    recovery.upsert_capture_source(&updated).unwrap();
    assert!(publish_and_commit(recovery).unwrap().checkpoint().is_some());
    assert_eq!(store.get_capture_source(original.id).unwrap(), updated);
}

#[test]
fn coordinator_limits_accept_exact_and_refuse_one_over() {
    assert!(NativePathGroupAccounting::new(
        NATIVE_PATH_MAX_GROUP_PAGES,
        NATIVE_PATH_MAX_GROUP_SOURCES,
        NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
    )
    .is_ok());
    for result in [
        NativePathGroupAccounting::new(
            NATIVE_PATH_MAX_GROUP_PAGES + 1,
            NATIVE_PATH_MAX_GROUP_SOURCES,
            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
        ),
        NativePathGroupAccounting::new(
            NATIVE_PATH_MAX_GROUP_PAGES,
            NATIVE_PATH_MAX_GROUP_SOURCES + 1,
            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
        ),
        NativePathGroupAccounting::new(
            NATIVE_PATH_MAX_GROUP_PAGES,
            NATIVE_PATH_MAX_GROUP_SOURCES,
            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES + 1,
        ),
    ] {
        assert!(matches!(
            result,
            Err(StoreError::NativePathGroupLimitExceeded { .. })
        ));
    }
}

#[test]
fn typed_source_values_derive_exact_core_byte_limit_and_one_over_rolls_back() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let (mut exact, _exact_key, cursor_bind_bytes) = begin_group_details(&store, &guard);
    let mut exact_source = source(Uuid::from_u128(1));
    exact_source.sync.metadata = Value::String(String::new());
    let fixed = capture_source_bind_bytes(&exact_source).unwrap();
    exact_source.sync.metadata =
        Value::String("x".repeat(NATIVE_PATH_MAX_CORE_BOUND_BYTES - fixed - cursor_bind_bytes));
    assert_eq!(
        capture_source_bind_bytes(&exact_source).unwrap() + cursor_bind_bytes,
        NATIVE_PATH_MAX_CORE_BOUND_BYTES
    );
    exact.upsert_capture_source(&exact_source).unwrap();
    let receipt = publish_and_commit(exact).unwrap();
    assert_eq!(receipt.attempted_mutation_units(), 2);
    assert_eq!(
        receipt.core_bound_value_bytes(),
        NATIVE_PATH_MAX_CORE_BOUND_BYTES
    );

    let mut one_over_source = exact_source.clone();
    one_over_source.id = Uuid::from_u128(2);
    one_over_source.sync.metadata =
        Value::String(one_over_source.sync.metadata.as_str().unwrap().to_owned() + "x");
    let (mut one_over, _one_over_key, one_over_cursor_bind_bytes) =
        begin_group_details(&store, &guard);
    assert_eq!(one_over_cursor_bind_bytes, cursor_bind_bytes);
    one_over.upsert_capture_source(&one_over_source).unwrap();
    one_over.prepare_journal_checkpoint().unwrap();
    let error = one_over.publish_cursor_set().unwrap_err();
    assert!(matches!(
        error,
        StoreError::NativePathGroupLimitExceeded {
            limit: "Core bound-value encoding bytes",
            actual,
            maximum: NATIVE_PATH_MAX_CORE_BOUND_BYTES,
        } if actual == NATIVE_PATH_MAX_CORE_BOUND_BYTES + 1
    ));
    assert!(matches!(
        one_over.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(one_over_source.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn event_accounting_includes_store_derived_fts_and_rolls_back_on_overflow() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let mut oversized = event(Uuid::from_u128(3), 1, None, None);
    oversized.event_type = EventType::Message;
    oversized.role = Some(EventRole::User);
    oversized.payload = Value::String(String::new());
    let fixed = event_bind_bytes(&oversized).unwrap();
    oversized.payload = Value::String("x".repeat(NATIVE_PATH_MAX_CORE_BOUND_BYTES - fixed));
    assert_eq!(
        event_bind_bytes(&oversized).unwrap(),
        NATIVE_PATH_MAX_CORE_BOUND_BYTES
    );

    let mut group = begin_group(&store, &guard);
    assert!(matches!(
        group.reconcile_provider_event(
            &oversized,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        ),
        Err(StoreError::NativePathGroupLimitExceeded {
            limit: "Core bound-value encoding bytes",
            actual,
            maximum: NATIVE_PATH_MAX_CORE_BOUND_BYTES,
        }) if actual > NATIVE_PATH_MAX_CORE_BOUND_BYTES
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_event(oversized.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn mutation_limit_requires_one_typed_call_per_unit_without_partial_rotation() {
    let (temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let exact_source = source(Uuid::from_u128(10));
    let mut exact = begin_group(&store, &guard);
    for _ in 0..NATIVE_PATH_MAX_MUTATION_UNITS - 1 {
        exact.upsert_capture_source(&exact_source).unwrap();
    }

    let observer = Connection::open(temp.path().join("ctx.db")).unwrap();
    assert_eq!(
        observer
            .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0,
        "the non-rotating group must not expose intermediate writes"
    );
    let receipt = publish_and_commit(exact).unwrap();
    assert_eq!(
        receipt.attempted_mutation_units(),
        NATIVE_PATH_MAX_MUTATION_UNITS
    );
    assert_eq!(
        observer
            .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );

    let refused_source = source(Uuid::from_u128(11));
    let mut refused = begin_group(&store, &guard);
    for _ in 0..NATIVE_PATH_MAX_MUTATION_UNITS {
        refused.upsert_capture_source(&refused_source).unwrap();
    }
    refused.prepare_journal_checkpoint().unwrap();
    assert!(matches!(
        refused.publish_cursor_set(),
        Err(StoreError::NativePathGroupLimitExceeded {
            limit: "attempted Store mutation units",
            actual,
            maximum: NATIVE_PATH_MAX_MUTATION_UNITS,
        }) if actual == NATIVE_PATH_MAX_MUTATION_UNITS + 1
    ));
    assert!(matches!(
        refused.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(refused_source.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn narrow_typed_surface_writes_only_canonical_model_operations() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let source = source(Uuid::from_u128(20));
    let session = session(Uuid::from_u128(21), Some(source.id));
    let event = event(Uuid::from_u128(22), 1, Some(source.id), Some(session.id));
    let run = run(Uuid::from_u128(23), source.id, session.id);
    let touch = file_touch(Uuid::from_u128(24), event.id, source.id);

    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    assert!(group.insert_session_if_absent(&session).unwrap());
    assert!(group
        .reconcile_provider_event(
            &event,
            ProviderEventHashAuthority::NormalizedPayloadFallback
        )
        .unwrap());
    group.upsert_run(&run).unwrap();
    group.upsert_file_touched(&touch).unwrap();
    let receipt = publish_and_commit(group).unwrap();

    assert_eq!(receipt.attempted_mutation_units(), 6);
    assert_eq!(store.get_capture_source(source.id).unwrap(), source);
    assert_eq!(store.get_session(session.id).unwrap(), session);
    assert_eq!(store.get_event(event.id).unwrap(), event);
    assert_eq!(store.get_run(run.id).unwrap(), run);
    assert!(store.file_touched_exists(touch.id).unwrap());
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn retention_noops_charge_units_without_inventing_bound_values() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let mut output_event = event(Uuid::from_u128(25), 1, None, None);
    output_event.event_type = EventType::CommandOutput;
    output_event.payload = json!({"result_outcome": "success", "exit_code": 0});
    let mut output_run = run(
        Uuid::from_u128(26),
        Uuid::from_u128(27),
        Uuid::from_u128(28),
    );
    output_run.status = RunStatus::Succeeded;
    output_run.sync.metadata = json!({"source": "provider_command_output"});

    let (mut group, _key, cursor_bind_bytes) = begin_group_details(&store, &guard);
    assert!(!group
        .reconcile_provider_event(
            &output_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    group.upsert_run(&output_run).unwrap();
    let receipt = publish_and_commit(group).unwrap();

    assert_eq!(receipt.attempted_mutation_units(), 3);
    assert_eq!(receipt.core_bound_value_bytes(), cursor_bind_bytes);
    assert!(matches!(
        store.get_event(output_event.id),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.get_run(output_run.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn failed_typed_mutation_poison_rolls_back_prior_success_when_error_is_ignored() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let source = source(Uuid::from_u128(30));
    let invalid_session = session(Uuid::from_u128(31), Some(Uuid::from_u128(999)));
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    assert!(group.insert_session_if_absent(&invalid_session).is_err());
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(source.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn sealed_journal_error_poison_rolls_back_when_ignored() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let first = source(Uuid::from_u128(40));
    let second = source(Uuid::from_u128(41));
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&first).unwrap();
    group.prepare_journal_checkpoint().unwrap();
    assert!(matches!(
        group.upsert_capture_source(&second),
        Err(StoreError::NativePathJournalSealed)
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(first.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn ignored_reclassification_error_rolls_back_core_journal_and_cursor() {
    let (_temp, store, mut updated_source) = projected_source_with_events(&[0]);
    let original = updated_source.clone();
    updated_source.sync.metadata = json!({"must_rollback": "reclassification"});
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:duplicate");
    let transition = NativePathCursorTransition::new(None, cursor(&key, "next", 1));
    let (mut group, cursor_key, _) = begin_group_details(&store, &guard);
    group.upsert_capture_source(&updated_source).unwrap();
    assert!(matches!(
        group.classify_cursor_set("publication-duplicate", &[transition.clone(), transition]),
        Err(StoreError::InvalidNativePathCursorSet)
    ));
    assert!(matches!(
        group.prepare_journal_checkpoint(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert_eq!(
        store.get_capture_source(updated_source.id).unwrap(),
        original
    );
    assert!(store
        .get_sync_cursor(
            cursor_key.team_id(),
            cursor_key.device_id(),
            cursor_key.stream()
        )
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn complete_cursor_set_rejects_zero_subset_and_extra_transitions() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let first_key = NativePathCursorKey::new(None, "machine", "native-path:complete-first");
    let second_key = NativePathCursorKey::new(None, "machine", "native-path:complete-second");
    let first = NativePathCursorTransition::new(None, cursor(&first_key, "first-next", 1));
    let second = NativePathCursorTransition::new(None, cursor(&second_key, "second-next", 1));
    let cases = [
        (
            "zero-source-publication",
            NativePathGroupAccounting::new(1, 0, 64).unwrap(),
            Vec::new(),
        ),
        (
            "zero-transitions",
            NativePathGroupAccounting::new(1, 1, 64).unwrap(),
            Vec::new(),
        ),
        (
            "missing-transition-subset",
            NativePathGroupAccounting::new(1, 2, 64).unwrap(),
            vec![first.clone()],
        ),
        (
            "duplicate-transition",
            NativePathGroupAccounting::new(1, 2, 64).unwrap(),
            vec![first.clone(), first.clone()],
        ),
        (
            "extra-transition",
            NativePathGroupAccounting::new(1, 1, 64).unwrap(),
            vec![first, second],
        ),
    ];

    for (publication_id, coordinator, transitions) in cases {
        let mut group = begin_unclassified_group(&store, &guard, coordinator);
        assert!(matches!(
            group.classify_cursor_set(publication_id, &transitions),
            Err(StoreError::InvalidNativePathCursorSet)
        ));
        assert!(matches!(
            group.prepare_journal_checkpoint(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
        assert!(matches!(
            group.commit(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
    }
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn cursor_mismatch_missing_and_malformed_sets_poison() {
    let (_temp, store) = open_store();
    let key = NativePathCursorKey::new(None, "machine", "native-path:conflict");
    store
        .upsert_sync_cursor(&cursor(&key, "actual", 0))
        .unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let mut mismatch = begin_unclassified_group(&store, &guard, accounting());
    let transition =
        NativePathCursorTransition::new(Some("expected".to_owned()), cursor(&key, "next", 1));
    assert!(matches!(
        mismatch.classify_cursor_set("publication-conflict", &[transition]),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        mismatch.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));

    let missing_key = NativePathCursorKey::new(None, "machine", "native-path:missing");
    let mut missing = begin_unclassified_group(&store, &guard, accounting());
    let missing_transition = NativePathCursorTransition::new(
        Some("required".to_owned()),
        cursor(&missing_key, "next", 1),
    );
    assert!(matches!(
        missing.classify_cursor_set("publication-missing", &[missing_transition]),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        missing.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn publish_before_checkpoint_is_finish_misuse_and_rolls_back() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:publish-early");
    let transition = NativePathCursorTransition::new(None, cursor(&key, "next", 1));
    let mut group = begin_unclassified_group(&store, &guard, accounting());
    group
        .classify_cursor_set("publication-early", &[transition])
        .unwrap();
    let source = source(Uuid::from_u128(60));
    group.upsert_capture_source(&source).unwrap();
    assert!(matches!(
        group.publish_cursor_set(),
        Err(StoreError::InvalidNativePathCursorSet)
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(source.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn unfinished_expected_cursor_set_cannot_commit_core_rows() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:unfinished");
    let transition = NativePathCursorTransition::new(None, cursor(&key, "next", 1));
    let source = source(Uuid::from_u128(61));
    let mut group = begin_unclassified_group(&store, &guard, accounting());
    group
        .classify_cursor_set("publication-unfinished", &[transition])
        .unwrap();
    group.upsert_capture_source(&source).unwrap();

    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(source.id),
        Err(StoreError::NotFound(_))
    ));
    assert!(store
        .get_sync_cursor(key.team_id(), key.device_id(), key.stream())
        .unwrap()
        .is_none());
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn unclassified_group_cannot_commit_core_or_journal_rows() {
    let (_temp, store, mut updated_source) = projected_source_with_events(&[0]);
    let original = updated_source.clone();
    updated_source.sync.metadata = json!({"must_rollback": "no-cursor-set"});
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let mut group = begin_unclassified_group(&store, &guard, accounting());
    group.upsert_capture_source(&updated_source).unwrap();

    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert_eq!(
        store.get_capture_source(updated_source.id).unwrap(),
        original
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn ignored_cursor_publication_overflow_rolls_back_core_journal_and_cursor() {
    let (_temp, store, mut updated_source) = projected_source_with_events(&[0]);
    let original = updated_source.clone();
    updated_source.sync.metadata = json!({"must_rollback": "cursor-publication"});
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let (mut group, cursor_key, _) = begin_group_details(&store, &guard);
    let mut retained_noop = run(
        Uuid::from_u128(62),
        Uuid::from_u128(63),
        Uuid::from_u128(64),
    );
    retained_noop.status = RunStatus::Succeeded;
    retained_noop.sync.metadata = json!({"source": "provider_command_output"});
    for _ in 0..NATIVE_PATH_MAX_MUTATION_UNITS - 1 {
        group.upsert_run(&retained_noop).unwrap();
    }
    group.upsert_capture_source(&updated_source).unwrap();
    assert!(group.prepare_journal_checkpoint().unwrap().is_some());
    assert!(matches!(
        group.publish_cursor_set(),
        Err(StoreError::NativePathGroupLimitExceeded {
            limit: "attempted Store mutation units",
            actual,
            maximum: NATIVE_PATH_MAX_MUTATION_UNITS,
        }) if actual == NATIVE_PATH_MAX_MUTATION_UNITS + 1
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert_eq!(
        store.get_capture_source(updated_source.id).unwrap(),
        original
    );
    assert!(store
        .get_sync_cursor(
            cursor_key.team_id(),
            cursor_key.device_id(),
            cursor_key.stream()
        )
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn outer_bulk_finish_misuse_poison_rolls_back_when_ignored() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let source = source(Uuid::from_u128(70));
    let (mut group, cursor_key, _) = begin_group_details(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    group.prepare_journal_checkpoint().unwrap();
    group.publish_cursor_set().unwrap();
    assert!(matches!(
        store.finish_event_search_bulk_mode(&guard),
        Err(StoreError::InvalidBulkSearchGuard)
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(source.id),
        Err(StoreError::NotFound(_))
    ));
    assert!(store
        .get_sync_cursor(
            cursor_key.team_id(),
            cursor_key.device_id(),
            cursor_key.stream()
        )
        .unwrap()
        .is_none());
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn every_in_group_bulk_admission_refusal_poisons_a_published_transaction() {
    {
        let (_temp, store, mut source) = projected_source_with_events(&[0]);
        let original = source.clone();
        source.sync.metadata = json!({"must_rollback": "bulk-admission"});
        let guard = store.begin_event_search_bulk_mode().unwrap();
        let mut group = begin_group(&store, &guard);
        group.upsert_capture_source(&source).unwrap();
        group.prepare_journal_checkpoint().unwrap();
        group.publish_cursor_set().unwrap();

        assert!(matches!(
            store.admit_event_search_bulk_group(&guard),
            Err(StoreError::InvalidBulkSearchGuard)
        ));
        assert!(matches!(
            group.commit(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
        assert_eq!(store.get_capture_source(source.id).unwrap(), original);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM projection_journal_chunks",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        store.finish_event_search_bulk_mode(&guard).unwrap();
    }

    {
        let (_temp, store) = open_store();
        let (_wrong_temp, wrong_store) = open_store();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        let wrong_guard = wrong_store.begin_event_search_bulk_mode().unwrap();
        let source = source(Uuid::from_u128(72));
        let mut group = begin_group(&store, &guard);
        group.upsert_capture_source(&source).unwrap();
        group.prepare_journal_checkpoint().unwrap();
        group.publish_cursor_set().unwrap();

        assert!(matches!(
            store.admit_event_search_bulk_group(&wrong_guard),
            Err(StoreError::InvalidBulkSearchGuard)
        ));
        assert!(matches!(
            group.commit(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
        assert!(matches!(
            store.get_capture_source(source.id),
            Err(StoreError::NotFound(_))
        ));
        store.finish_event_search_bulk_mode(&guard).unwrap();
        wrong_store
            .finish_event_search_bulk_mode(&wrong_guard)
            .unwrap();
    }

    {
        let (_temp, store) = open_store();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        let source = source(Uuid::from_u128(73));
        let mut group = begin_group(&store, &guard);
        group.upsert_capture_source(&source).unwrap();
        group.prepare_journal_checkpoint().unwrap();
        group.publish_cursor_set().unwrap();
        store
            .event_search_bulk_group_admission_outstanding
            .store(true, Ordering::SeqCst);

        assert!(matches!(
            store.admit_event_search_bulk_group(&guard),
            Err(StoreError::InvalidBulkSearchGuard)
        ));
        store
            .event_search_bulk_group_admission_outstanding
            .store(false, Ordering::SeqCst);
        assert!(matches!(
            group.commit(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
        assert!(matches!(
            store.get_capture_source(source.id),
            Err(StoreError::NotFound(_))
        ));
        store.finish_event_search_bulk_mode(&guard).unwrap();
    }

    {
        let (_temp, store) = open_store();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        let source = source(Uuid::from_u128(74));
        let mut group = begin_group(&store, &guard);
        group.upsert_capture_source(&source).unwrap();
        group.prepare_journal_checkpoint().unwrap();
        group.publish_cursor_set().unwrap();
        store.event_search_bulk_depth.fetch_add(1, Ordering::SeqCst);

        assert!(matches!(
            store.admit_event_search_bulk_group(&guard),
            Err(StoreError::InvalidBulkSearchGuard)
        ));
        store.event_search_bulk_depth.fetch_sub(1, Ordering::SeqCst);
        assert!(matches!(
            group.commit(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
        assert!(matches!(
            store.get_capture_source(source.id),
            Err(StoreError::NotFound(_))
        ));
        store.finish_event_search_bulk_mode(&guard).unwrap();
    }

    {
        let (_temp, store) = open_store();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        let source = source(Uuid::from_u128(75));
        let mut group = begin_group(&store, &guard);
        group.upsert_capture_source(&source).unwrap();
        group.prepare_journal_checkpoint().unwrap();
        group.publish_cursor_set().unwrap();

        assert!(matches!(
            store.begin_event_search_bulk_mode(),
            Err(StoreError::InvalidBulkSearchGuard)
        ));
        assert!(matches!(
            group.commit(),
            Err(StoreError::NativePathGroupPoisoned)
        ));
        assert!(matches!(
            store.get_capture_source(source.id),
            Err(StoreError::NotFound(_))
        ));
        store.finish_event_search_bulk_mode(&guard).unwrap();
    }
}

#[test]
fn nested_begin_with_foreign_valid_admission_poison_rolls_back_and_recovers() {
    let (_temp, store, source) = projected_source_with_events(&[0]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let (_foreign_temp, foreign_store) = open_store();
    let foreign_guard = foreign_store.begin_event_search_bulk_mode().unwrap();
    let foreign_admission = foreign_store
        .admit_event_search_bulk_group(&foreign_guard)
        .unwrap();

    assert_nested_begin_poison_rolls_back_and_recovers(
        &store,
        &guard,
        &source,
        foreign_admission,
        accounting(),
        "foreign-valid-admission",
    );

    store.finish_event_search_bulk_mode(&guard).unwrap();
    foreign_store
        .finish_event_search_bulk_mode(&foreign_guard)
        .unwrap();
}

#[test]
fn nested_begin_with_stale_foreign_admission_poison_rolls_back_and_recovers() {
    let (_temp, store, source) = projected_source_with_events(&[0]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let (_foreign_temp, foreign_store) = open_store();
    let stale_foreign_admission = {
        let foreign_guard = foreign_store.begin_event_search_bulk_mode().unwrap();
        let admission = foreign_store
            .admit_event_search_bulk_group(&foreign_guard)
            .unwrap();
        drop(foreign_guard);
        admission
    };

    assert_nested_begin_poison_rolls_back_and_recovers(
        &store,
        &guard,
        &source,
        stale_foreign_admission,
        accounting(),
        "stale-foreign-admission",
    );

    store.finish_event_search_bulk_mode(&guard).unwrap();
    let cleanup_guard = foreign_store.begin_event_search_bulk_mode().unwrap();
    foreign_store
        .finish_event_search_bulk_mode(&cleanup_guard)
        .unwrap();
}

#[test]
fn nested_begin_preempts_invalid_coordinator_and_poison_rolls_back_and_recovers() {
    let (_temp, store, source) = projected_source_with_events(&[0]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let (_foreign_temp, foreign_store) = open_store();
    let foreign_guard = foreign_store.begin_event_search_bulk_mode().unwrap();
    let foreign_admission = foreign_store
        .admit_event_search_bulk_group(&foreign_guard)
        .unwrap();
    let invalid_coordinator = NativePathGroupAccounting {
        page_count: NATIVE_PATH_MAX_GROUP_PAGES + 1,
        source_count: 1,
        retained_page_bytes: 64,
    };

    assert_nested_begin_poison_rolls_back_and_recovers(
        &store,
        &guard,
        &source,
        foreign_admission,
        invalid_coordinator,
        "invalid-coordinator",
    );

    store.finish_event_search_bulk_mode(&guard).unwrap();
    foreign_store
        .finish_event_search_bulk_mode(&foreign_guard)
        .unwrap();
}

#[test]
fn unowned_write_and_lifecycle_errors_poison_the_group() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let source = source(Uuid::from_u128(80));
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    assert!(store
        .conn
        .execute(
            "UPDATE capture_sources SET machine_id = 'bypass' WHERE id = ?1",
            [source.id.to_string()],
        )
        .is_err());
    assert!(matches!(
        store.disable_projection_journal(),
        Err(StoreError::NativePathJournalLifecycleDuringGroup)
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(source.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn journal_collector_batches_513_records_and_reports_exact_physical_bytes() {
    let (_temp, store, mut source) = projected_source_with_events(&vec![0; 513]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    source.sync.metadata = json!({"updated": true});
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    let receipt = publish_and_commit(group).unwrap();
    assert_eq!(receipt.attempted_mutation_units(), 2);
    assert_eq!(receipt.journal_records(), 513);

    let chunks = store
        .conn
        .prepare(
            "SELECT record_count, uncompressed_bytes
             FROM projection_journal_chunks ORDER BY first_sequence",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        chunks.iter().map(|(count, _)| *count).collect::<Vec<_>>(),
        vec![64, 64, 64, 64, 64, 64, 64, 64, 1]
    );
    assert!(chunks
        .iter()
        .all(|(count, bytes)| *count <= 512 && *bytes <= 8 * 1024 * 1024));
    assert_eq!(
        chunks
            .iter()
            .map(|(_, bytes)| *bytes as usize)
            .sum::<usize>(),
        receipt.journal_uncompressed_bytes()
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn exact_journal_record_limit_commits_from_one_bounded_typed_mutation() {
    let (_temp, store, mut source) =
        projected_source_with_events(&vec![0; NATIVE_PATH_MAX_JOURNAL_RECORDS]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    source.sync.metadata = json!({"exact": true});
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    let receipt = publish_and_commit(group).unwrap();
    assert_eq!(receipt.attempted_mutation_units(), 2);
    assert_eq!(receipt.journal_records(), NATIVE_PATH_MAX_JOURNAL_RECORDS);
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn journal_record_overflow_error_poison_rolls_back_typed_mutation_when_ignored() {
    let (_temp, store, mut source) =
        projected_source_with_events(&vec![0; NATIVE_PATH_MAX_JOURNAL_RECORDS + 1]);
    let original = source.clone();
    source.sync.metadata = json!({"must_rollback": true});
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let mut group = begin_group(&store, &guard);
    assert!(matches!(
        group.upsert_capture_source(&source),
        Err(StoreError::NativePathGroupLimitExceeded {
            limit: "actual journal records",
            actual: 4_097,
            maximum: NATIVE_PATH_MAX_JOURNAL_RECORDS,
        })
    ));
    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert_eq!(store.get_capture_source(source.id).unwrap(), original);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn journal_byte_limit_accepts_exact_and_one_over_poison_rolls_back() {
    // Sixty-five records force a second physical chunk before the group byte
    // boundary, so adding one payload byte also adds exactly one accounting
    // byte instead of changing array framing at the page boundary.
    let base_sizes = vec![125_000; 65];
    let (_base_temp, base_store, mut base_source) = projected_source_with_events(&base_sizes);
    let base_guard = base_store.begin_event_search_bulk_mode().unwrap();
    base_source.sync.metadata = json!({"measure": true});
    let mut base_group = begin_group(&base_store, &base_guard);
    base_group.upsert_capture_source(&base_source).unwrap();
    let measured = publish_and_commit(base_group)
        .unwrap()
        .journal_uncompressed_bytes();
    base_store
        .finish_event_search_bulk_mode(&base_guard)
        .unwrap();
    assert!(measured < NATIVE_PATH_MAX_JOURNAL_BYTES);

    let mut exact_sizes = base_sizes;
    exact_sizes[64] += NATIVE_PATH_MAX_JOURNAL_BYTES - measured;
    let (_exact_temp, exact_store, mut exact_source) = projected_source_with_events(&exact_sizes);
    let exact_guard = exact_store.begin_event_search_bulk_mode().unwrap();
    exact_source.sync.metadata = json!({"exact": true});
    let mut exact_group = begin_group(&exact_store, &exact_guard);
    exact_group.upsert_capture_source(&exact_source).unwrap();
    assert_eq!(
        publish_and_commit(exact_group)
            .unwrap()
            .journal_uncompressed_bytes(),
        NATIVE_PATH_MAX_JOURNAL_BYTES
    );
    exact_store
        .finish_event_search_bulk_mode(&exact_guard)
        .unwrap();

    exact_sizes[64] += 1;
    let (_over_temp, over_store, mut over_source) = projected_source_with_events(&exact_sizes);
    let original = over_source.clone();
    over_source.sync.metadata = json!({"must_rollback": true});
    let over_guard = over_store.begin_event_search_bulk_mode().unwrap();
    let mut over_group = begin_group(&over_store, &over_guard);
    let over_error = over_group.upsert_capture_source(&over_source).unwrap_err();
    assert!(
        matches!(
            over_error,
            StoreError::NativePathGroupLimitExceeded {
                limit: "uncompressed journal encoding bytes",
                actual,
                maximum: NATIVE_PATH_MAX_JOURNAL_BYTES,
            } if actual == NATIVE_PATH_MAX_JOURNAL_BYTES + 1
        ),
        "unexpected one-over error: {over_error:?}"
    );
    assert!(matches!(
        over_group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert_eq!(
        over_store.get_capture_source(over_source.id).unwrap(),
        original
    );
    over_store
        .finish_event_search_bulk_mode(&over_guard)
        .unwrap();
}

#[test]
fn store_owned_cursor_classification_publishes_and_recovers_exact_checkpoint() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let key = NativePathCursorKey::new(None, "machine", "native-path:checkpoint");
    let transition = NativePathCursorTransition::new(None, cursor(&key, "provider-next", 1));

    let mut group = begin_unclassified_group(&store, &guard, accounting());
    assert_eq!(
        group
            .classify_cursor_set("publication-1", std::slice::from_ref(&transition))
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    group
        .reconcile_provider_event(
            &event(Uuid::from_u128(500), 1, None, None),
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    let checkpoint = group
        .prepare_journal_checkpoint()
        .unwrap()
        .expect("active journal checkpoint");
    group.publish_cursor_set().unwrap();
    let receipt = group.commit().unwrap();
    assert_eq!(receipt.checkpoint(), Some(&checkpoint));

    let stored = store
        .get_sync_cursor(key.team_id(), key.device_id(), key.stream())
        .unwrap()
        .unwrap();
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    assert_eq!(committed.publication_id(), "publication-1");
    assert_eq!(committed.provider_cursor(), "provider-next");
    assert_eq!(committed.journal_checkpoint(), Some(&checkpoint));

    let regenerated = NativePathCursorTransition::new(None, cursor(&key, "provider-next", 99));
    assert_ne!(
        regenerated.next().timestamps.updated_at,
        stored.timestamps.updated_at
    );
    let mut retry = begin_unclassified_group(&store, &guard, accounting());
    assert_eq!(
        retry
            .classify_cursor_set("publication-1", &[regenerated])
            .unwrap(),
        NativePathCursorSetClassification::AllNextSameGroup {
            checkpoint: Some(checkpoint.clone())
        }
    );
    let retry_receipt = retry.commit().unwrap();
    assert_eq!(retry_receipt.checkpoint(), Some(&checkpoint));
    assert_eq!(retry_receipt.attempted_mutation_units(), 0);
    assert_eq!(
        store
            .get_sync_cursor(key.team_id(), key.device_id(), key.stream())
            .unwrap(),
        Some(stored),
        "all-next recovery must not rewrite cursor timestamps or identity"
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn mixed_cursor_states_and_checkpoint_mismatch_conflict_without_callbacks() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let first_key = NativePathCursorKey::new(None, "machine", "native-path:first");
    let second_key = NativePathCursorKey::new(None, "machine", "native-path:second");
    let transitions = vec![
        NativePathCursorTransition::new(None, cursor(&first_key, "first-next", 1)),
        NativePathCursorTransition::new(None, cursor(&second_key, "second-next", 1)),
    ];

    let two_sources = NativePathGroupAccounting::new(1, 2, 64).unwrap();
    let mut publish = begin_unclassified_group(&store, &guard, two_sources);
    publish
        .classify_cursor_set("publication-set", &transitions)
        .unwrap();
    publish.prepare_journal_checkpoint().unwrap();
    publish.publish_cursor_set().unwrap();
    publish.commit().unwrap();

    let mut subset_retry = begin_unclassified_group(&store, &guard, two_sources);
    assert!(matches!(
        subset_retry.classify_cursor_set("publication-set", std::slice::from_ref(&transitions[0])),
        Err(StoreError::InvalidNativePathCursorSet)
    ));
    assert!(matches!(
        subset_retry.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));

    let mut second = store
        .get_sync_cursor(
            second_key.team_id(),
            second_key.device_id(),
            second_key.stream(),
        )
        .unwrap()
        .unwrap();
    second.cursor = "expected-second".to_owned();
    second.timestamps.updated_at += TimeDelta::seconds(1);
    store.upsert_sync_cursor(&second).unwrap();

    let mixed_transitions = vec![
        NativePathCursorTransition::new(
            Some("expected-first".to_owned()),
            cursor(&first_key, "first-next", 10),
        ),
        NativePathCursorTransition::new(
            Some("expected-second".to_owned()),
            cursor(&second_key, "second-next", 10),
        ),
    ];
    let mut mixed = begin_unclassified_group(&store, &guard, two_sources);
    assert!(matches!(
        mixed.classify_cursor_set("publication-set", &mixed_transitions),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        mixed.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));

    // Republish both, then give only one row a different, independently valid
    // checkpoint. Exact common-checkpoint verification must reject the set.
    store.conn.execute("DELETE FROM sync_cursors", []).unwrap();
    let mut republish = begin_unclassified_group(&store, &guard, two_sources);
    republish
        .classify_cursor_set("publication-set", &transitions)
        .unwrap();
    let old_checkpoint = republish.prepare_journal_checkpoint().unwrap().unwrap();
    republish.publish_cursor_set().unwrap();
    republish.commit().unwrap();

    let mut advance = begin_group(&store, &guard);
    advance
        .reconcile_provider_event(
            &event(Uuid::from_u128(501), 2, None, None),
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    let new_checkpoint = publish_and_commit(advance)
        .unwrap()
        .checkpoint()
        .cloned()
        .unwrap();
    assert_ne!(old_checkpoint, new_checkpoint);

    let mut second = store
        .get_sync_cursor(
            second_key.team_id(),
            second_key.device_id(),
            second_key.stream(),
        )
        .unwrap()
        .unwrap();
    let mut envelope = decode_cursor_envelope(&second.cursor).unwrap();
    envelope.journal_checkpoint = Some(new_checkpoint);
    second.cursor = encode_cursor_envelope(&envelope).unwrap();
    second.timestamps.updated_at += TimeDelta::seconds(1);
    store.upsert_sync_cursor(&second).unwrap();

    let retry_transitions = vec![
        NativePathCursorTransition::new(None, cursor(&first_key, "first-next", 20)),
        NativePathCursorTransition::new(None, cursor(&second_key, "second-next", 20)),
    ];
    let mut mismatch = begin_unclassified_group(&store, &guard, two_sources);
    assert!(matches!(
        mismatch.classify_cursor_set("publication-set", &retry_transitions),
        Err(StoreError::NativePathCursorConflict)
    ));
    assert!(matches!(
        mismatch.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn wal_threshold_blocks_next_group_until_pinned_reader_releases() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("ctx.db");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let mut first = begin_group(&store, &guard);
    first
        .upsert_capture_source(&source(Uuid::from_u128(600)))
        .unwrap();
    publish_and_commit(first).unwrap();
    store.checkpoint_wal_truncate_required().unwrap();

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );

    let mut second = begin_group(&store, &guard);
    second
        .upsert_capture_source(&source(Uuid::from_u128(601)))
        .unwrap();
    publish_and_commit(second).unwrap();

    let _limits = Store::event_search_bulk_test_limits(Some(1), None);
    assert!(matches!(
        store.admit_event_search_bulk_group(&guard),
        Err(StoreError::WalCheckpointBusy {
            log_frames,
            checkpointed_frames,
        }) if log_frames > checkpointed_frames
    ));
    reader.execute_batch("ROLLBACK").unwrap();
    let admitted = store.admit_event_search_bulk_group(&guard).unwrap();
    store
        .begin_native_path_publication_group(admitted, accounting())
        .unwrap()
        .rollback()
        .unwrap();
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn only_one_bulk_group_admission_can_be_outstanding() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let first = store.admit_event_search_bulk_group(&guard).unwrap();
    assert!(matches!(
        store.finish_event_search_bulk_mode(&guard),
        Err(StoreError::InvalidBulkSearchGuard)
    ));
    assert!(matches!(
        store.admit_event_search_bulk_group(&guard),
        Err(StoreError::BulkSearchGroupAdmissionOutstanding)
    ));
    drop(first);
    let replacement = store.admit_event_search_bulk_group(&guard).unwrap();
    store
        .begin_native_path_publication_group(replacement, accounting())
        .unwrap()
        .rollback()
        .unwrap();
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn relationship_edge_is_journal_neutral_only_when_actor_is_exactly_unchanged() {
    let (_temp, store) = open_store();
    let parent = session(Uuid::from_u128(700), None);
    let mut child = session(Uuid::from_u128(701), None);
    child.parent_session_id = Some(parent.id);
    child.root_session_id = Some(parent.id);
    store.upsert_session(&parent).unwrap();
    store.upsert_session(&child).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO events
             (id, seq, session_id, event_type, role, occurred_at_ms, payload_json, metadata_json)
             VALUES (?1, 1, ?2, 'notice', 'assistant', 0, '{\"lineage\":true}', '{}')",
            params![Uuid::from_u128(702).to_string(), child.id.to_string()],
        )
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let relationship = edge(Uuid::from_u128(703), child.id, parent.id);
    let mut group = begin_group(&store, &guard);
    group
        .upsert_projection_neutral_session_edge(&actor(&child), &relationship)
        .unwrap();
    let receipt = publish_and_commit(group).unwrap();
    assert_eq!(receipt.journal_records(), 0);
    assert!(store.session_edge_exists(relationship.id).unwrap());

    let rejected_edge = edge(Uuid::from_u128(704), child.id, parent.id);
    let mut changed_actor = actor(&child);
    changed_actor.parent_session_id = None;
    let mut rejected = begin_group(&store, &guard);
    assert!(matches!(
        rejected.upsert_projection_neutral_session_edge(&changed_actor, &rejected_edge),
        Err(StoreError::ProjectionChangingSessionRelationship)
    ));
    assert!(matches!(
        rejected.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(!store.session_edge_exists(rejected_edge.id).unwrap());
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn source_generation_retires_omissions_replays_and_allows_exact_restoration() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let source_id = Uuid::from_u128(800);
    let session_id = Uuid::from_u128(801);
    let retained_event_id = Uuid::from_u128(802);
    let retired_event_id = Uuid::from_u128(803);
    let mut capture_source = source(source_id);
    let observation = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: "codex-jsonl".to_owned(),
        machine_id: "machine".to_owned(),
        locator_identity: "locator-800".to_owned(),
        cursor_stream: "native-path:source-800".to_owned(),
        proposed_source_identity: format!("source-{source_id}"),
        raw_source_path: Some("/repo/session.jsonl".to_owned()),
        source_revision: "revision-1".to_owned(),
        observed_at_ms: 1,
    };
    let mut retained_event = event(retained_event_id, 1, Some(source_id), Some(session_id));
    let retained_hash = compute_payload_hash(&retained_event.payload).unwrap();
    retained_event.dedupe_key = Some(Store::provider_source_event_dedupe_key(
        source_id,
        1,
        &retained_hash,
    ));
    retained_event.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
    let mut retired_event = event(retired_event_id, 2, Some(source_id), Some(session_id));
    let retired_hash = compute_payload_hash(&retired_event.payload).unwrap();
    retired_event.dedupe_key = Some(Store::provider_source_event_dedupe_key(
        source_id,
        2,
        &retired_hash,
    ));
    retired_event.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
    let key = NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: observation.source_format.clone(),
        machine_id: observation.machine_id.clone(),
        canonical_source_identity: observation.proposed_source_identity.clone(),
        locator_identity: observation.locator_identity.clone(),
        cursor_stream: observation.cursor_stream.clone(),
        source_revision: observation.source_revision.clone(),
        generation_id: "generation-1".to_owned(),
    };

    let mut group = begin_group(&store, &guard);
    let resolution = group
        .reconcile_provider_source_locator(&observation)
        .unwrap();
    capture_source.descriptor.source_identity = Some(resolution.canonical_source_identity.clone());
    group.upsert_capture_source(&capture_source).unwrap();
    group
        .bind_capture_source_provider_route(source_id, &resolution.route_binding())
        .unwrap();
    group
        .upsert_session(&session(session_id, Some(source_id)))
        .unwrap();
    group
        .reconcile_provider_event(
            &retained_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    group
        .reconcile_provider_event(
            &retired_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    group
        .stage_source_generation_page(
            &key,
            &NativePathRetainedSourceEntities {
                capture_source_ids: vec![source_id],
                session_ids: vec![session_id],
                event_ids: vec![retained_event_id],
                ..NativePathRetainedSourceEntities::default()
            },
        )
        .unwrap();
    publish_and_commit(group).unwrap();

    let retirement_cursor_key =
        NativePathCursorKey::new(None, "test-machine", "native-path:retirement-preview");
    let retirement_transition =
        NativePathCursorTransition::new(None, cursor(&retirement_cursor_key, "done", 2));
    let mut retirement = begin_unclassified_group(&store, &guard, accounting());
    let preview = retirement
        .preview_source_generation_retirement_page(&key, None, 16)
        .unwrap();
    assert_eq!(
        retirement
            .classify_cursor_set(
                "source-retirement-preview",
                std::slice::from_ref(&retirement_transition),
            )
            .unwrap(),
        NativePathCursorSetClassification::AllExpected
    );
    let page = retirement
        .retire_source_generation_page(&key, None, 16, 2)
        .unwrap();
    assert_eq!(page, preview);
    assert!(page.done);
    assert_eq!(page.retired, 1);
    publish_and_commit(retirement).unwrap();

    assert!(store
        .get_event(retained_event_id)
        .unwrap()
        .sync
        .deleted_at
        .is_none());
    assert!(store
        .get_event(retired_event_id)
        .unwrap()
        .sync
        .deleted_at
        .is_some());

    let mut replay = begin_group(&store, &guard);
    assert_eq!(
        replay
            .retire_source_generation_page(&key, None, 16, 2)
            .unwrap(),
        page
    );
    publish_and_commit(replay).unwrap();

    let mut restore = begin_group(&store, &guard);
    assert!(!restore
        .reconcile_provider_event(
            &retired_event,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    publish_and_commit(restore).unwrap();
    assert!(store
        .get_event(retired_event_id)
        .unwrap()
        .sync
        .deleted_at
        .is_none());
    store.finish_event_search_bulk_mode(&guard).unwrap();
}
