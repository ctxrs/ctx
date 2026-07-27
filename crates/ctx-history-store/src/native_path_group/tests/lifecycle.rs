use super::*;

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
fn nested_bulk_guard_can_publish_while_outer_root_is_live() {
    let (_temp, store) = open_store();
    let outer = store.begin_event_search_bulk_mode().unwrap();
    let nested = store.begin_event_search_bulk_mode().unwrap();
    let mut group = begin_group(&store, &nested);
    group
        .upsert_capture_source(&source(Uuid::from_u128(690)))
        .unwrap();
    publish_and_commit(group).unwrap();

    store.finish_event_search_bulk_mode(&nested).unwrap();
    drop(nested);
    store.finish_event_search_bulk_mode(&outer).unwrap();
}

#[test]
fn nested_bulk_guard_and_admission_expire_with_their_root_epoch() {
    let (_temp, store) = open_store();
    let outer = store.begin_event_search_bulk_mode().unwrap();
    let nested = store.begin_event_search_bulk_mode().unwrap();
    let stale_admission = store.admit_event_search_bulk_group(&nested).unwrap();

    drop(outer);
    assert!(matches!(
        store.admit_event_search_bulk_group(&nested),
        Err(StoreError::InvalidBulkSearchGuard)
    ));
    drop(nested);

    let replacement = store.begin_event_search_bulk_mode().unwrap();
    assert!(matches!(
        store.begin_native_path_publication_group(stale_admission, accounting()),
        Err(StoreError::InvalidBulkSearchGroupAdmission)
    ));
    let admission = store.admit_event_search_bulk_group(&replacement).unwrap();
    store
        .begin_native_path_publication_group(admission, accounting())
        .unwrap()
        .rollback()
        .unwrap();
    store.finish_event_search_bulk_mode(&replacement).unwrap();
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
