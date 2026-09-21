use crate::protocol::{CoreSourceRemoval, CoreSourceState, SourceAnchor, SourceKey, TypedKey};

use super::*;

fn source(index: u32) -> SourceKey {
    SourceKey::derive(
        "cursor-test",
        "jsonl",
        "v1",
        1,
        SourceAnchor::ProviderNative {
            namespace: "source".to_owned(),
            key: TypedKey::U64(u64::from(index)),
        },
    )
    .expect("test source identity must derive")
}

fn present(index: u32) -> CoreSourceDelta {
    let digest_byte = u8::try_from(index).expect("test index must fit in one byte");
    CoreSourceDelta::Present(CoreSourceState {
        source: source(index),
        core_record_accumulator: hex::encode([digest_byte; 32]),
        event_count: u64::from(index),
    })
}

#[test]
fn entries_are_exactly_sixty_four_bytes_and_hash_the_complete_delta() {
    assert_eq!(std::mem::size_of::<ReconciliationEntry>(), 64);
    let first = present(1);
    let mut changed = first.clone();
    let CoreSourceDelta::Present(state) = &mut changed else {
        unreachable!();
    };
    state.event_count += 1;
    let first_entry = reconciliation_entry(&first).expect("first delta must hash");
    let changed_entry = reconciliation_entry(&changed).expect("changed delta must hash");
    assert_eq!(
        first_entry.source_identity_sha256,
        changed_entry.source_identity_sha256
    );
    assert_ne!(first_entry.delta_sha256, changed_entry.delta_sha256);
}

#[test]
fn payload_bound_is_exact_for_maximum_and_real_inventory() {
    assert_eq!(MAX_RECONCILIATION_CURSOR_ENTRIES, 32_768);
    assert_eq!(
        cursor_payload_bytes(MAX_RECONCILIATION_CURSOR_ENTRIES).expect("maximum must fit"),
        2 * 1024 * 1024
    );
    assert_eq!(
        cursor_payload_bytes(6_084).expect("real inventory must fit"),
        389_376
    );
    assert!(cursor_payload_bytes(MAX_RECONCILIATION_CURSOR_ENTRIES + 1).is_err());
}

#[test]
fn changed_and_removed_counts_cannot_borrow_the_other_sides_limit() {
    assert_eq!(
        bounded_entry_limit(10, 100, 10, 100).expect("exact sides must fit"),
        110
    );
    assert!(bounded_entry_limit(10, 100, 11, 0).is_err());
    assert!(bounded_entry_limit(100, 10, 0, 11).is_err());
    assert!(bounded_entry_limit(MAX_CORE_SOURCE_STATES + 1, 0, 0, 0).is_err());
    assert!(bounded_entry_limit(0, MAX_CORE_SOURCE_STATES + 1, 0, 0).is_err());
}

#[test]
fn exact_match_distinguishes_identity_and_full_delta_hash() {
    let owner = CursorOwner {
        materialization_id: hex::encode([1; 32]),
        core_generation_id: hex::encode([2; 32]),
        graph_generation: 1,
        materializer_revision: "revision".to_owned(),
    };
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(2)
        .expect("cursor reservation must fit");
    let mut cursor = ReconciliationCursor {
        owner,
        entries,
        entry_limit: 2,
    };
    let delta = present(1);
    let reconciliation = CoreSourceReconciliation {
        materialize_index: 0,
        delta: delta.clone(),
    };
    let prepared = cursor
        .prepare_append(std::slice::from_ref(&reconciliation))
        .expect("append must prepare");
    cursor.commit_append(prepared).expect("append must commit");
    cursor
        .require_current(0, &reconciliation)
        .expect("exact current entry must match");
    cursor
        .require_committed(&reconciliation)
        .expect("exact completed entry must match");

    let CoreSourceDelta::Present(mut changed) = delta else {
        unreachable!();
    };
    changed.event_count += 1;
    let hostile = CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Present(changed),
    };
    assert!(matches!(
        cursor.require_current(0, &hostile),
        Err(SegmentMaterializerError::Conflict)
    ));
    assert!(matches!(
        cursor.require_committed(&hostile),
        Err(SegmentMaterializerError::Conflict)
    ));

    let removed = CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Removed(CoreSourceRemoval { source: source(2) }),
    };
    assert!(matches!(
        cursor.require_committed(&removed),
        Err(SegmentMaterializerError::Conflict)
    ));
}
