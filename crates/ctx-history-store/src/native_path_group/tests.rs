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
    begin_group_with_accounting(store, guard, accounting())
}

fn begin_group_with_accounting<'a>(
    store: &'a Store,
    guard: &crate::EventSearchBulkGuard,
    coordinator: NativePathGroupAccounting,
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

    let mut group = begin_unclassified_group(store, guard, coordinator);
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

// Core bind-value bytes are the exact SQLite encoding of the canonical rows,
// which only the Store can measure and which is larger than the
// provider-serialized page bytes admission bounds. It is reported exactly and
// never rejects: a group filled to the retained-page admission ceiling would
// otherwise be admitted and then refused.
#[test]
fn typed_source_values_derive_exact_core_byte_accounting_without_rejecting() {
    const FORMER_CORE_BYTE_CEILING: usize = 8 * 1024 * 1024;
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let (mut exact, _exact_key, cursor_bind_bytes) = begin_group_details(&store, &guard);
    let mut exact_source = source(Uuid::from_u128(1));
    exact_source.sync.metadata = Value::String(String::new());
    let fixed = capture_source_bind_bytes(&exact_source).unwrap();
    exact_source.sync.metadata =
        Value::String("x".repeat(FORMER_CORE_BYTE_CEILING - fixed - cursor_bind_bytes));
    assert_eq!(
        capture_source_bind_bytes(&exact_source).unwrap() + cursor_bind_bytes,
        FORMER_CORE_BYTE_CEILING
    );
    exact.upsert_capture_source(&exact_source).unwrap();
    let receipt = publish_and_commit(exact).unwrap();
    assert_eq!(receipt.attempted_mutation_units(), 2);
    assert_eq!(receipt.core_bound_value_bytes(), FORMER_CORE_BYTE_CEILING);

    let mut one_over_source = exact_source.clone();
    one_over_source.id = Uuid::from_u128(2);
    one_over_source.sync.metadata =
        Value::String(one_over_source.sync.metadata.as_str().unwrap().to_owned() + "x");
    let (mut one_over, _one_over_key, one_over_cursor_bind_bytes) =
        begin_group_details(&store, &guard);
    assert_eq!(one_over_cursor_bind_bytes, cursor_bind_bytes);
    one_over.upsert_capture_source(&one_over_source).unwrap();
    let one_over_receipt = publish_and_commit(one_over).unwrap();
    assert_eq!(
        one_over_receipt.core_bound_value_bytes(),
        FORMER_CORE_BYTE_CEILING + 1
    );
    assert_eq!(
        store.get_capture_source(one_over_source.id).unwrap().id,
        one_over_source.id
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn event_accounting_includes_store_derived_fts_bytes() {
    const BODY_BYTES: usize = 64 * 1024;
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let mut large = event(Uuid::from_u128(3), 1, None, None);
    large.event_type = EventType::Message;
    large.role = Some(EventRole::User);
    large.payload = Value::String("x".repeat(BODY_BYTES));
    let typed_bytes = event_bind_bytes(&large).unwrap();

    let mut group = begin_group(&store, &guard);
    group
        .reconcile_provider_event(
            &large,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap();
    let receipt = publish_and_commit(group).unwrap();

    // The charge is the typed bind encoding plus the Store-derived search
    // projection the same mutation writes, so it always exceeds the typed row.
    assert!(
        receipt.core_bound_value_bytes() > typed_bytes,
        "expected Store-derived projection bytes beyond {typed_bytes}: {}",
        receipt.core_bound_value_bytes()
    );
    assert_eq!(store.get_event(large.id).unwrap().id, large.id);
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
fn event_identity_alias_is_atomic_idempotent_and_resolves_to_canonical_event() {
    let (_temp, store) = open_store();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let canonical_id = Uuid::from_u128(0x100);
    let alias_id = Uuid::from_u128(0x200);
    let canonical = event(canonical_id, 1, None, None);

    let mut group = begin_group(&store, &guard);
    assert!(group
        .reconcile_provider_event(
            &canonical,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    group
        .bind_event_identity_alias(alias_id, canonical_id, now().timestamp_millis())
        .unwrap();
    group
        .bind_event_identity_alias(alias_id, canonical_id, now().timestamp_millis())
        .unwrap();
    publish_and_commit(group).unwrap();

    assert_eq!(store.get_event(alias_id).unwrap().id, canonical_id);
    assert_eq!(
        store.event_alias_target_id(alias_id).unwrap(),
        Some(canonical_id)
    );
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
fn rollback_failure_quarantines_connection_until_store_is_reopened() {
    let (temp, store) = open_store();
    let db_path = temp.path().join("ctx.db");
    let guard = store.begin_event_search_bulk_mode().unwrap();
    let staged = source(Uuid::from_u128(32));
    let invalid_session = session(Uuid::from_u128(33), Some(Uuid::from_u128(999)));
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&staged).unwrap();
    assert!(group.insert_session_if_absent(&invalid_session).is_err());

    Store::fail_next_rollback_for_test();
    assert!(matches!(
        group.commit(),
        Err(StoreError::StoreConnectionQuarantined)
    ));
    assert!(store.connection_quarantined.get());
    assert_eq!(store.batch_depth.get(), 1);
    assert!(!store.conn.is_autocommit());
    assert!(store.native_path_group_token.get().is_some());
    assert!(store.native_path_group_poisoned.load(Ordering::SeqCst));
    assert!(matches!(
        store.upsert_capture_source(&source(Uuid::from_u128(34))),
        Err(StoreError::StoreConnectionQuarantined)
    ));

    drop(guard);
    drop(store);

    let reopened = Store::open(&db_path).unwrap();
    assert!(matches!(
        reopened.get_capture_source(staged.id),
        Err(StoreError::NotFound(_))
    ));
    let recovered = source(Uuid::from_u128(35));
    reopened.upsert_capture_source(&recovered).unwrap();
    assert_eq!(
        reopened.get_capture_source(recovered.id).unwrap(),
        recovered
    );
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

// A store upgraded from a released version holds rows the coordinator did not
// write in this run. One typed mutation over such a source re-journals every
// canonical observation it changes, which is far more journal volume than the
// coordinator can see, size, or split. Admission cannot bound it, so commit
// must not reject it.
#[test]
fn one_typed_mutation_commits_journal_fanout_past_the_former_group_ceilings() {
    const FORMER_RECORD_CEILING: usize = 4_096;
    const FORMER_BYTE_CEILING: usize = 8 * 1024 * 1024;
    let (_temp, store, mut source) =
        projected_source_with_events(&vec![4_096; FORMER_RECORD_CEILING + 1]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    source.sync.metadata = json!({"upgraded": true});
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    let receipt = publish_and_commit(group).unwrap();

    assert_eq!(receipt.attempted_mutation_units(), 2);
    assert_eq!(receipt.journal_records(), FORMER_RECORD_CEILING + 1);
    assert!(
        receipt.journal_uncompressed_bytes() > FORMER_BYTE_CEILING,
        "fanout should exceed the former byte ceiling: {}",
        receipt.journal_uncompressed_bytes()
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(record_count), 0), COALESCE(SUM(uncompressed_bytes), 0)
                 FROM projection_journal_chunks",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
        (
            i64::try_from(receipt.journal_records()).unwrap(),
            i64::try_from(receipt.journal_uncompressed_bytes()).unwrap(),
        )
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

// The released v0.26 candidate admitted a group by provider-serialized page
// bytes and then enforced an equal ceiling on the journal encoding of the same
// content, which is always larger. A group filled to the Core admission
// ceiling must commit.
#[test]
fn group_at_the_core_admission_ceiling_commits_its_larger_journal_encoding() {
    let (_temp, store, mut source) = projected_source_with_events(&vec![64_000; 160]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    source.sync.metadata = json!({"at_ceiling": true});
    let coordinator =
        NativePathGroupAccounting::new(160, 1, NATIVE_PATH_MAX_RETAINED_PAGE_BYTES).unwrap();
    let (mut group, _key, _bytes) = begin_group_with_accounting(&store, &guard, coordinator);
    group.upsert_capture_source(&source).unwrap();
    let receipt = publish_and_commit(group).unwrap();

    assert_eq!(
        receipt.coordinator_accounting().retained_page_bytes(),
        NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
    );
    assert!(
        receipt.journal_uncompressed_bytes() > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
        "journal encoding should exceed the retained-page admission bound: {}",
        receipt.journal_uncompressed_bytes()
    );
    assert!(receipt.core_bound_value_bytes() > 0);
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

// Every limit the group enforces when it commits must already have been
// charged before the mutation that could exceed it, so no admitted group is
// ever rejected.
#[test]
fn committed_receipt_reports_only_admission_bounded_limits() {
    let (_temp, store, mut source) = projected_source_with_events(&[256; 8]);
    let guard = store.begin_event_search_bulk_mode().unwrap();
    source.sync.metadata = json!({"receipt": true});
    let mut group = begin_group(&store, &guard);
    group.upsert_capture_source(&source).unwrap();
    let receipt = publish_and_commit(group).unwrap();

    assert!(receipt.attempted_mutation_units() <= NATIVE_PATH_MAX_MUTATION_UNITS);
    assert!(receipt.core_bound_value_bytes() > 0);
    assert_eq!(receipt.journal_records(), 8);
    assert!(receipt.journal_uncompressed_bytes() > 0);
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

mod generation;
mod lifecycle;
mod publication;
