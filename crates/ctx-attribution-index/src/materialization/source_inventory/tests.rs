use super::*;
use ctx_attribution_model::{CoreGenerationHead, CoreSourceState};
use ctx_history_core::{SourceAnchor, SourceKey, TypedKey};

fn source(index: u64) -> ActiveSource {
    ActiveSource {
        state: CoreSourceState {
            source: SourceKey::derive(
                "inventory",
                "fixture",
                "v1",
                1,
                SourceAnchor::ProviderNative {
                    namespace: "source".into(),
                    key: TypedKey::U64(index),
                },
            )
            .unwrap(),
            core_record_accumulator: "0".repeat(64),
            event_count: index % 31,
        },
        materializer_revision: "fixture-v1".into(),
    }
}

#[test]
fn inventory_spills_100003_sources_with_exact_lookup_and_receipt_totals() {
    let temp = tempfile::tempdir().unwrap();
    // The oracle retains only independently derived ordering keys, not source
    // descriptors. Production retains neither this vector nor a source map.
    let mut order = (0..100_003)
        .map(|n| (source(n).state.source.identity().digest(), n))
        .collect::<Vec<_>>();
    order.sort_unstable();
    let mut inventory = SourceInventory::new(temp.path()).unwrap();
    for (_, n) in &order {
        inventory.push(&source(*n)).unwrap();
    }
    assert_eq!(inventory.len(), 100_003);
    assert_eq!(inventory.rows.len().unwrap(), 100_003 * 76);
    assert!(inventory.values.len().unwrap() > 16 * 1024 * 1024);
    let head = CoreGenerationHead::from_source_snapshot(
        "a".repeat(64),
        1,
        1,
        "b".repeat(64),
        1,
        1,
        "c".repeat(64),
        inventory.snapshot(),
    )
    .unwrap();
    assert_eq!(head.source_count, 100_003);
    assert_eq!(
        head.event_count,
        (0..100_003_u64).map(|n| n % 31).sum::<u64>()
    );
    for ordinal in [0, 16_384, 100_001, 100_002] {
        let expected = source(order[ordinal].1);
        assert_eq!(inventory.at(ordinal).unwrap(), expected);
        assert_eq!(
            inventory.get_identity(order[ordinal].0).unwrap(),
            Some(expected)
        );
    }
    assert!(
        inventory
            .get_identity(source(100_003).state.source.identity().digest())
            .unwrap()
            .is_none()
    );
    assert!(inventory.push(&source(order[0].1)).is_err());
}

#[test]
fn metadata_byte_pages_and_overlapping_mutations_preserve_the_existing_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let mut changed = source(1);
    changed.state.event_count = 99;
    let upsert = |source: ActiveSource| SourceMutation::Upsert {
        state: source.state,
        materializer_revision: source.materializer_revision,
    };
    let mut runs = SourceMutationRuns::new(temp.path()).unwrap();
    runs.push_run(vec![
        upsert(changed.clone()),
        SourceMutation::Removed {
            source_id: crate::core_source_storage_id(&source(2).state.source),
        },
    ])
    .unwrap();
    runs.push_run(vec![
        upsert(source(3)),
        upsert(source(2)),
        upsert(source(1)),
    ])
    .unwrap();
    let inventory = runs.finish(temp.path()).unwrap();
    assert_eq!(inventory.len(), 2);
    assert_eq!(
        inventory
            .get_identity(changed.state.source.identity().digest())
            .unwrap(),
        Some(changed)
    );
    assert!(
        inventory
            .get_identity(source(2).state.source.identity().digest())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        inventory
            .get_identity(source(3).state.source.identity().digest())
            .unwrap(),
        Some(source(3))
    );
    let first = serde_json::to_vec(&upsert(inventory.at(0).unwrap()))
        .unwrap()
        .len();
    let second = serde_json::to_vec(&upsert(inventory.at(1).unwrap()))
        .unwrap()
        .len();
    let maximum = first.max(second);
    assert_eq!(inventory.metadata_page_end(0, maximum).unwrap(), 1);
    assert_eq!(inventory.metadata_page_end(1, maximum).unwrap(), 2);
    assert_eq!(inventory.metadata_segment_count(maximum).unwrap(), 2);
    assert!(inventory.metadata_page_end(0, first - 1).is_err());
}

#[test]
fn source_spill_rejects_truncated_lengths_and_changed_valid_json() {
    let temp = tempfile::tempdir().unwrap();
    let mut inventory = SourceInventory::new(temp.path()).unwrap();
    inventory.push(&source(1)).unwrap();
    let mut changed = source(1);
    changed.state.event_count = 2;
    let bytes = serde_json::to_vec(&SourceMutation::Upsert {
        state: changed.state,
        materializer_revision: changed.materializer_revision,
    })
    .unwrap();
    assert_eq!(bytes.len() as u64, inventory.value_bytes);
    inventory.values = RuntimeFile::new(temp.path()).unwrap();
    inventory.values.append(&bytes).unwrap();
    assert!(matches!(
        inventory.at(0),
        Err(MaterializationIndexError::Corrupt(
            "source inventory checksum"
        ))
    ));
    let mut row = inventory.row(0).unwrap();
    row[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    inventory.rows = RuntimeFile::new(temp.path()).unwrap();
    inventory.rows.append(&row).unwrap();
    assert!(matches!(
        inventory.at(0),
        Err(MaterializationIndexError::Corrupt(
            "source inventory frame length"
        ))
    ));
}
