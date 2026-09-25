use super::*;

#[test]
fn event_index_preflight_preserves_staged_page_boundaries() {
    let at_limit = u32::try_from(MAX_PUBLICATION_TOMBSTONES).unwrap();
    assert!(!index_page_requires_flush(
        at_limit - 2,
        2,
        1,
        Some(1),
        false
    ));
    assert!(index_page_requires_flush(
        at_limit - 1,
        2,
        1,
        Some(1),
        false
    ));
}

#[test]
fn event_index_preflight_rolls_over_when_removed_sources_regress() {
    let mut plan = SegmentPublicationReferencePlan {
        event_index_open_records: 1,
        event_index_open_accounted_bytes: 1,
        event_index_open_source_id: Some("core_source_f".to_owned()),
        ..Default::default()
    };
    let source = EventIndexSource {
        storage_key: "core_source_a".to_owned(),
        source: crate::protocol::SourceKey::derive(
            "publication-order",
            "event_index_test",
            "test",
            1,
            crate::protocol::SourceAnchor::CatalogLineage([0x5a; 32]),
        )
        .unwrap(),
    };

    plan.prepare_index_item(&source)
        .expect("a source-order regression starts a fresh segment");

    assert_eq!(plan.event_index_segments, 1);
    assert_eq!(plan.event_index_open_records, 0);
    assert_eq!(
        plan.event_index_open_source_id.as_deref(),
        Some("core_source_a")
    );
}

#[test]
fn manifest_preflight_boundary_is_exactly_four_thousand_ninety_six() {
    let at_limit = PublicationReferenceShape {
        candidate: PublicationRoleCounts {
            flat: MAX_MANIFEST_SEGMENTS - 5,
            event_index: 0,
            source: 1,
        },
        retained: PublicationRoleCounts {
            flat: 2,
            event_index: 1,
            source: 1,
        },
    };
    assert_eq!(at_limit.total_references().unwrap(), MAX_MANIFEST_SEGMENTS);
    assert!(at_limit.fits_manifest().unwrap());

    let over_limit = PublicationReferenceShape {
        candidate: PublicationRoleCounts {
            flat: MAX_MANIFEST_SEGMENTS - 4,
            ..at_limit.candidate
        },
        retained: at_limit.retained,
    };
    assert_eq!(
        over_limit.total_references().unwrap(),
        MAX_MANIFEST_SEGMENTS + 1
    );
    assert!(!over_limit.fits_manifest().unwrap());
}

#[cfg(test)]
#[test]
fn event_index_rolls_before_its_byte_cap_even_when_record_count_is_low() {
    let max = MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES as u64;
    assert!(!index_page_requires_flush(
        1,
        1,
        max - 500,
        Some(500),
        false
    ));
    assert!(index_page_requires_flush(1, 1, max - 500, Some(501), false));
    assert!(!index_page_requires_flush(0, 1, 0, Some(501), false));
    assert!(index_page_requires_flush(1, 1, 0, Some(501), true));
}
