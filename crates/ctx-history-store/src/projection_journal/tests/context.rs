use super::*;

#[test]
fn pages_are_count_bounded_frozen_and_contiguous() {
    let (_temp, store) = open_store();
    let total = PROJECTION_JOURNAL_PAGE_SIZE as u128 + 6;
    for value in 1_u128..=total {
        let id = Uuid::from_u128(value);
        store
            .upsert_event(&event(id, value as u64, json!({"body": value})))
            .unwrap();
    }
    let checkpoint = store.activate_projection_journal(FINGERPRINT).unwrap();
    assert_eq!(checkpoint.position.sequence, total as u64);

    let first = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(first.records.len(), PROJECTION_JOURNAL_PAGE_SIZE);
    assert_eq!(first.records.first().unwrap().sequence, 1);
    assert_eq!(
        first.records.last().unwrap().sequence,
        PROJECTION_JOURNAL_PAGE_SIZE as u64
    );
    assert!(first.has_more);
    assert_eq!(first.frozen_through, checkpoint);

    let second = store
        .projection_journal_snapshot(Some(first.next_position))
        .unwrap();
    assert_eq!(second.records.len(), 6);
    assert_eq!(
        second.records.first().unwrap().sequence,
        PROJECTION_JOURNAL_PAGE_SIZE as u64 + 1
    );
    assert_eq!(second.records.last().unwrap().sequence, total as u64);
    assert!(!second.has_more);
    assert_eq!(second.frozen_through, checkpoint);
    assert!(matches!(
        store.projection_journal_snapshot(Some(JournalPosition {
            generation: checkpoint.position.generation,
            sequence: total as u64 + 1,
        })),
        Err(StoreError::StaleProjectionJournalPosition { .. })
    ));
}

#[test]
fn page_byte_accounting_includes_json_array_brackets_and_commas() {
    assert_eq!(
        json_array_encoded_bytes_after_push(2, 0, 10),
        12,
        "the empty array's brackets stay in the encoded total"
    );
    assert_eq!(
        json_array_encoded_bytes_after_push(12, 1, 10),
        23,
        "a second item adds one comma"
    );
    assert_eq!(
        json_array_encoded_bytes_after_push(
            2,
            0,
            PROJECTION_JOURNAL_MAX_PAGE_BYTES.saturating_sub(2)
        ),
        PROJECTION_JOURNAL_MAX_PAGE_BYTES
    );
    assert!(
        json_array_encoded_bytes_after_push(
            2,
            0,
            PROJECTION_JOURNAL_MAX_PAGE_BYTES.saturating_sub(1)
        ) > PROJECTION_JOURNAL_MAX_PAGE_BYTES
    );
}

#[test]
fn forward_pages_are_maximal_exact_json_array_prefixes() {
    let (_temp, store) = open_store();
    let large_body = "x".repeat(2_900_000);
    for value in 1_u128..=4 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": &large_body, "ordinal": value}),
            ))
            .unwrap();
    }
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let first = store.projection_journal_snapshot(None).unwrap();
    assert!(first.has_more);
    assert_eq!(first.records.len(), 2);
    assert!(serde_json::to_vec(&first.records).unwrap().len() <= PROJECTION_JOURNAL_MAX_PAGE_BYTES);
    let second = store
        .projection_journal_snapshot(Some(first.next_position))
        .unwrap();
    let mut one_more = first.records;
    one_more.push(second.records[0].clone());
    assert!(
        serde_json::to_vec(&one_more).unwrap().len() > PROJECTION_JOURNAL_MAX_PAGE_BYTES,
        "the next contiguous record must be the first record excluded by the exact byte bound"
    );
}

#[test]
fn acknowledgements_atomically_retain_exact_bounded_context_suffix() {
    let (_temp, store) = open_store();
    for value in 1_u128..=70 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": value}),
            ))
            .unwrap();
    }
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let first = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(first.records.len(), 70);
    // The read page is larger than the durable 64-record chunks. The
    // acknowledgement retains the complete 64-record context plus the base
    // record whose digest anchors that context.
    let first_ack = record_checkpoint(&first.records[63]);
    store.acknowledge_projection_journal(&first_ack).unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(record_count), 0) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        70
    );
    let retained = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(retained.records.len(), 6);
    assert_eq!(retained.records[0].sequence, 65);
    assert_eq!(retained.context.base_checkpoint.position.sequence, 0);
    assert_eq!(retained.context.records.len(), 64);
    assert_eq!(retained.context.records[0].sequence, 1);
    assert_eq!(retained.context.records[63].sequence, 64);
    assert!(matches!(
        store.projection_journal_snapshot(Some(JournalPosition {
            generation: first_ack.position.generation,
            sequence: first_ack.position.sequence - 1,
        })),
        Err(StoreError::StaleProjectionJournalPosition { .. })
    ));
    store.acknowledge_projection_journal(&first_ack).unwrap();

    let final_ack = record_checkpoint(retained.records.last().unwrap());
    store.acknowledge_projection_journal(&final_ack).unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(record_count), 0) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        65
    );
    let empty = store.projection_journal_snapshot(None).unwrap();
    assert!(empty.records.is_empty());
    assert_eq!(empty.next_position, final_ack.position);
    assert_eq!(empty.context.base_checkpoint.position.sequence, 6);
    assert_eq!(empty.context.records.len(), 64);
    assert_eq!(empty.context.records[0].sequence, 7);
    assert_eq!(empty.context.records[63].sequence, 70);

    store
        .upsert_event(&event(Uuid::from_u128(71), 71, json!({"body": 71})))
        .unwrap();
    let delta = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(delta.records.len(), 1);
    assert_eq!(delta.records[0].sequence, 71);
    assert_eq!(delta.context.records.len(), 64);
    assert_eq!(delta.context.records[63].sequence, 70);
}

#[test]
fn context_is_the_largest_count_and_byte_bounded_contiguous_suffix() {
    let (_temp, store) = open_store();
    let large_body = "x".repeat(1_500_000);
    for value in 1_u128..=7 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": &large_body, "ordinal": value}),
            ))
            .unwrap();
    }
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let mut all_records = Vec::new();
    loop {
        let snapshot = store.projection_journal_snapshot(None).unwrap();
        all_records.extend(snapshot.records.iter().cloned());
        let acknowledged = record_checkpoint(snapshot.records.last().unwrap());
        store.acknowledge_projection_journal(&acknowledged).unwrap();
        if !snapshot.has_more {
            break;
        }
    }

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert!(snapshot.records.is_empty());
    assert!(!snapshot.context.records.is_empty());
    assert!(snapshot.context.records.len() < 7);
    assert_eq!(snapshot.context.records.last().unwrap().sequence, 7);
    assert_eq!(
        snapshot.context.base_checkpoint.position.sequence + 1,
        snapshot.context.records.first().unwrap().sequence
    );
    assert!(
        serde_json::to_vec(&snapshot.context.records).unwrap().len()
            <= PROJECTION_JOURNAL_CONTEXT_MAX_BYTES
    );
    let base_sequence = snapshot.context.base_checkpoint.position.sequence;
    assert!(
        base_sequence > 0,
        "large context must have a physical anchor"
    );
    let anchor = all_records
        .iter()
        .find(|record| record.sequence == base_sequence)
        .expect("retained context anchor")
        .clone();
    let mut with_predecessor = Vec::with_capacity(snapshot.context.records.len() + 1);
    with_predecessor.push(anchor);
    with_predecessor.extend(snapshot.context.records.iter().cloned());
    assert!(
        serde_json::to_vec(&with_predecessor).unwrap().len() > PROJECTION_JOURNAL_CONTEXT_MAX_BYTES,
        "the selected suffix must be maximal under the byte bound"
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(record_count), 0) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        i64::try_from(snapshot.context.records.len() + 1).unwrap(),
        "durable retention must contain only context plus its digest anchor"
    );
}

#[test]
fn byte_pruned_context_survives_reopen_and_helper_ahead_reconciliation() {
    let (temp, store) = open_store();
    let large_body = "x".repeat(1_500_000);
    for value in 1_u128..=7 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": &large_body, "ordinal": value}),
            ))
            .unwrap();
    }
    let active = store.activate_projection_journal(FINGERPRINT).unwrap();
    store.acknowledge_projection_journal(&active).unwrap();
    assert!(
        store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(record_count), 0) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()
            < 7,
        "byte-aware acknowledgement must prune records outside context plus anchor"
    );

    for value in 8_u128..=9 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": value}),
            ))
            .unwrap();
    }
    let pending = store.projection_journal_snapshot(None).unwrap();
    let helper_ahead = record_checkpoint(pending.records.last().unwrap());
    let generation = helper_ahead.position.generation;
    drop(store);

    let reopened = Store::open(temp.path().join("ctx.db")).unwrap();
    let reconciled = reopened
        .reconcile_projection_journal(Some(&helper_ahead))
        .unwrap();
    assert_eq!(reconciled.position, helper_ahead.position);
    assert_eq!(reconciled.position.generation, generation);
    let settled = reopened.projection_journal_snapshot(None).unwrap();
    assert!(settled.records.is_empty());
    assert_eq!(
        settled.context.records.last().unwrap().sequence,
        helper_ahead.position.sequence
    );
    assert!(
        serde_json::to_vec(&settled.context.records).unwrap().len()
            <= PROJECTION_JOURNAL_CONTEXT_MAX_BYTES
    );
}

#[test]
fn reconciliation_recovers_ack_crashes_and_helper_loss_from_canonical_store() {
    let (_temp, store) = open_store();
    for value in 1_u128..=3 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": value}),
            ))
            .unwrap();
    }
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let baseline = store.projection_journal_snapshot(None).unwrap();
    let partial = record_checkpoint(&baseline.records[1]);
    let complete = record_checkpoint(&baseline.records[2]);
    store.acknowledge_projection_journal(&partial).unwrap();

    store.begin_immediate_batch().unwrap();
    store.acknowledge_projection_journal(&complete).unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    store.rollback_batch().unwrap();
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .records
            .len(),
        1
    );

    let same_generation = store.reconcile_projection_journal(Some(&complete)).unwrap();
    assert_eq!(same_generation.position, complete.position);
    assert!(store
        .projection_journal_snapshot(None)
        .unwrap()
        .records
        .is_empty());

    let regenerated = store.reconcile_projection_journal(None).unwrap();
    assert_eq!(regenerated.position.generation, 2);
    assert_eq!(regenerated.position.sequence, 3);
    let rebuilt = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(rebuilt.records.len(), 3);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT generation) FROM projection_journal_entities",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn acknowledgement_does_not_invalidate_an_existing_wal_reader() {
    let (temp, writer) = open_store();
    for value in 1_u128..=4 {
        writer
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": value}),
            ))
            .unwrap();
    }
    writer.activate_projection_journal(FINGERPRINT).unwrap();
    let reader = Store::open(temp.path().join("ctx.db")).unwrap();
    reader.conn.execute_batch("BEGIN").unwrap();
    let frozen = reader.projection_journal_snapshot(None).unwrap();
    let complete = record_checkpoint(frozen.records.last().unwrap());
    writer.acknowledge_projection_journal(&complete).unwrap();
    assert_eq!(
        reader
            .conn
            .query_row(
                "SELECT COALESCE(SUM(record_count), 0) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    reader.conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        writer
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_journal_chunks",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn forged_acknowledgements_fail_without_pruning() {
    let (_temp, store) = open_store();
    store
        .upsert_event(&event(Uuid::new_v4(), 1, json!({"body": "one"})))
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    let mut forged = record_checkpoint(&snapshot.records[0]);
    forged.cumulative_digest = "f".repeat(64);
    assert!(matches!(
        store.acknowledge_projection_journal(&forged),
        Err(StoreError::InvalidProjectionJournalData(_))
    ));
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .records
            .len(),
        1
    );
}

#[test]
fn acknowledged_journal_storage_is_bounded_and_reused_by_deltas() {
    let (temp, store) = open_store();
    let db_path = temp.path().join("ctx.db");
    let repeated = "deterministic journal compression corpus ".repeat(64);
    for value in 1_u128..=512 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": repeated, "ordinal": value}),
            ))
            .unwrap();
    }
    store.checkpoint_wal_truncate_required().unwrap();
    let canonical_bytes = sqlite_family_bytes(&db_path);
    store.activate_projection_journal(FINGERPRINT).unwrap();
    store.checkpoint_wal_truncate_required().unwrap();
    let activation_bytes = sqlite_family_bytes(&db_path);
    let activation_growth = activation_bytes.saturating_sub(canonical_bytes);
    assert!(
        activation_growth.saturating_mul(100) <= canonical_bytes.saturating_mul(25),
        "activation growth {activation_growth} exceeds 25% of canonical {canonical_bytes}"
    );

    loop {
        let page = store.projection_journal_snapshot(None).unwrap();
        if page.records.is_empty() {
            break;
        }
        let acknowledged = record_checkpoint(page.records.last().unwrap());
        store.acknowledge_projection_journal(&acknowledged).unwrap();
    }
    store.checkpoint_wal_truncate_required().unwrap();
    let acknowledged_bytes = sqlite_family_bytes(&db_path);
    assert!(
        acknowledged_bytes
            .saturating_sub(canonical_bytes)
            .saturating_mul(100)
            <= canonical_bytes.saturating_mul(25),
        "acknowledged storage exceeds 25% of canonical Store"
    );

    for value in 1_u128..=52 {
        store
            .upsert_event(&event(
                Uuid::from_u128(value),
                value as u64,
                json!({"body": repeated, "ordinal": value, "revision": 2}),
            ))
            .unwrap();
    }
    store.checkpoint_wal_truncate_required().unwrap();
    let delta_bytes = sqlite_family_bytes(&db_path).saturating_sub(acknowledged_bytes);
    assert!(
        delta_bytes.saturating_mul(100) <= canonical_bytes.saturating_mul(10),
        "incremental journal storage {delta_bytes} exceeds 10% of canonical {canonical_bytes}"
    );
}
