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
    assert_eq!(inventory.rows.byte_len().unwrap(), 100_003 * 76);
    assert!(inventory.values.byte_len().unwrap() > 16 * 1024 * 1024);
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

#[test]
fn repeated_source_comparisons_bound_inventory_row_reads() {
    let temp = tempfile::tempdir().unwrap();
    let mut sources = (0..4097).map(source).collect::<Vec<_>>();
    sources.sort_by_key(|source| source.state.source.identity().digest());
    let mut prior = SourceInventory::new(temp.path()).unwrap();
    let mut next = SourceInventory::new(temp.path()).unwrap();
    for source in &sources {
        prior.push(source).unwrap();
        next.push(source).unwrap();
    }
    let before = ROW_READS.with(std::cell::Cell::get);
    // Each immutable event-index layer can revisit the same source dictionary.
    for _ in 0..3 {
        for source in &sources {
            let id = crate::core_source_storage_id(&source.state.source);
            assert!(!next.changed_from(&prior, &id).unwrap());
        }
    }
    let reads = ROW_READS.with(std::cell::Cell::get) - before;
    eprintln!("inventory comparison work: sources=4097 passes=3 row_reads={reads}");
    assert!(
        reads <= 4097 * 3 * 2 * 3,
        "each lookup should need at most three disk rows, not a full inventory search"
    );
}

#[test]
fn lookup_keys_stay_bounded_across_appends_and_missing_identities() {
    let temp = tempfile::tempdir().unwrap();
    let mut sources = (0..8193).map(source).collect::<Vec<_>>();
    sources.sort_by_key(|source| source.state.source.identity().digest());
    let mut inventory = SourceInventory::new(temp.path()).unwrap();
    assert!(inventory.is_empty());
    assert!(inventory.get_identity([0; 32]).unwrap().is_none());
    assert!(inventory.at(0).is_err());
    assert!(inventory.get("invalid").is_err());
    for (index, source) in sources.iter().enumerate() {
        let identity = source.state.source.identity().digest();
        assert!(inventory.get_identity(identity).unwrap().is_none());
        if index % 2 != 0 {
            continue;
        }
        inventory.push(source).unwrap();
        assert!(inventory.lookup_keys.capacity() <= MAX_LOOKUP_KEYS);
        assert_eq!(
            inventory.get_identity(identity).unwrap(),
            Some(source.clone())
        );
        // Appending after a read, including each key-thinning boundary, must
        // preserve both the first and newest source.
        assert_eq!(inventory.at(0).unwrap(), sources[0]);
        assert_eq!(
            inventory
                .get_identity(sources[0].state.source.identity().digest())
                .unwrap(),
            Some(sources[0].clone())
        );
    }
    for (index, source) in sources.iter().enumerate().rev() {
        assert_eq!(
            inventory
                .get_identity(source.state.source.identity().digest())
                .unwrap(),
            (index % 2 == 0).then(|| source.clone())
        );
    }
    for absent in [[0; 32], [255; 32]] {
        assert!(inventory.get_identity(absent).unwrap().is_none());
    }
}

#[test]
fn lookup_keys_do_not_replace_row_or_value_integrity_checks() {
    let temp = tempfile::tempdir().unwrap();
    for truncate_rows in [false, true] {
        let mut inventory = SourceInventory::new(temp.path()).unwrap();
        let expected = source(1);
        let id = crate::core_source_storage_id(&expected.state.source);
        inventory.push(&expected).unwrap();
        assert_eq!(inventory.get(&id).unwrap(), Some(expected));
        if truncate_rows {
            inventory.rows = RuntimeFile::new(temp.path()).unwrap();
            assert!(matches!(
                inventory.get(&id),
                Err(MaterializationIndexError::Io { .. })
            ));
        } else {
            inventory.values = RuntimeFile::new(temp.path()).unwrap();
            assert!(matches!(
                inventory.get(&id),
                Err(MaterializationIndexError::Corrupt(
                    "truncated source inventory"
                ))
            ));
        }
    }
    let mut inventory = SourceInventory::new(temp.path()).unwrap();
    let mut expected = source(1);
    let id = crate::core_source_storage_id(&expected.state.source);
    inventory.push(&expected).unwrap();
    assert_eq!(inventory.get(&id).unwrap(), Some(expected.clone()));
    expected.state.event_count = 2;
    let changed = serde_json::to_vec(&SourceMutation::Upsert {
        state: expected.state,
        materializer_revision: expected.materializer_revision,
    })
    .unwrap();
    assert_eq!(changed.len() as u64, inventory.value_bytes);
    inventory.values = RuntimeFile::new(temp.path()).unwrap();
    inventory.values.append(&changed).unwrap();
    assert!(matches!(
        inventory.get(&id),
        Err(MaterializationIndexError::Corrupt(
            "source inventory checksum"
        ))
    ));
}

#[test]
fn comparison_preserves_descriptor_revision_content_count_and_removal_changes() {
    let temp = tempfile::tempdir().unwrap();
    let original = source(1);
    let id = crate::core_source_storage_id(&original.state.source);
    let mut prior = SourceInventory::new(temp.path()).unwrap();
    prior.push(&original).unwrap();
    let empty = SourceInventory::new(temp.path()).unwrap();
    assert!(!empty.changed_from(&empty, &id).unwrap());
    assert!(empty.changed_from(&prior, &id).unwrap());
    assert!(prior.changed_from(&empty, &id).unwrap());
    assert!(!prior.changed_from(&prior, &id).unwrap());
    for change in 0..4 {
        let mut changed = original.clone();
        match change {
            0 => changed.materializer_revision = "fixture-v2".into(),
            1 => changed.state.event_count += 1,
            2 => changed.state.core_record_accumulator = "1".repeat(64),
            _ => {
                changed.state.source = SourceKey::derive(
                    "inventory",
                    "fixture",
                    "v2",
                    1,
                    SourceAnchor::ProviderNative {
                        namespace: "source".into(),
                        key: TypedKey::U64(1),
                    },
                )
                .unwrap()
            }
        }
        assert_eq!(
            changed.state.source.identity(),
            original.state.source.identity()
        );
        let mut next = SourceInventory::new(temp.path()).unwrap();
        next.push(&changed).unwrap();
        assert_eq!(next.get(&id).unwrap(), Some(changed));
        assert!(next.changed_from(&prior, &id).unwrap());
        assert!(prior.changed_from(&next, &id).unwrap());
    }
}
