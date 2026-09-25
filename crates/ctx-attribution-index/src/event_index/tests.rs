type PlaintextMutation = Box<dyn Fn(&mut [u8])>;

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

use ctx_attribution_model::{EventCopyProofKind, SessionRelationshipKind};
use ctx_history_core::{SourceAnchor, TypedKey};

use super::*;

const TEST_GENERATION: [u8; 32] = [0x67; 32];
const TEST_CHUNK_BYTES: u32 = 16 * 1024;

#[test]
fn bounded_omission_uses_spare_coverage_bit_and_old_bytes_remain_readable() {
    let mut coverage = SegmentCoreCoverage::default();
    assert_eq!(pack_coverage(&coverage).unwrap(), 0);
    coverage.bounded_omission_events = 1;
    assert_eq!(pack_coverage(&coverage).unwrap(), 0b0100_0000);
    assert_eq!(unpack_coverage(0b0100_0000).unwrap(), coverage);
    assert!(unpack_coverage(0b1000_0000).is_err());
}

#[test]
fn synthetic_corpus_metadata_bounds_are_allocation_free() {
    assert_eq!(MAX_EVENT_INDEX_ENTRIES, 4_194_304);
    assert_eq!(MAX_EVENT_INDEX_SOURCES, MAX_CORE_SOURCE_STATES);
    assert!(validate_input_counts(MAX_CORE_SOURCE_STATES, 2_000_000, 0).is_ok());
    assert!(matches!(
        validate_input_counts(0, MAX_EVENT_INDEX_ENTRIES + 1, 0),
        Err(EventIndexError::Bound("entry count"))
    ));
    assert!(matches!(
        validate_input_counts(MAX_CORE_SOURCE_STATES + 1, 0, 0),
        Err(EventIndexError::Bound("source count"))
    ));
    let measured_record_bytes = 2_000_000_u64 * RECORD_BYTES as u64;
    assert!(measured_record_bytes < MAX_RECORD_SECTION_BYTES);
    assert!(measured_record_bytes + MAX_SOURCE_SECTION_BYTES < MAX_SEGMENT_PLAINTEXT_BYTES);
}

#[test]
fn publication_accounting_includes_nested_source_containers_and_writer_frames()
-> Result<(), Box<dyn Error>> {
    let mut components = Vec::with_capacity(64);
    components.extend((0..32).map(|_| TypedKey::Null));
    let component_capacity = components.capacity() * std::mem::size_of::<TypedKey>();
    let source = EventIndexSource::new(
        SourceKey::derive(
            "event-index-accounting",
            "event_index_test",
            "test",
            1,
            SourceAnchor::ProviderNative {
                namespace: "publication-accounting".to_owned(),
                key: TypedKey::Composite(components),
            },
        )
        .map_err(|_| EventIndexError::Invalid("accounting source"))?,
    )?;
    let encoded = canonical_json(&source)?;
    let source_bytes = EventIndexWriter::publication_source_accounted_bytes(&source)?;
    assert!(source_bytes >= encoded.len() * 2 + component_capacity);

    let record = test_state(&source, 1, 1, 0x41)?;
    let tombstone = test_tombstone(&source, 2)?;
    let (compact, lineage) = compact_records(vec![record])?;
    assert_eq!(
        EventIndexWriter::publication_accounted_bytes(
            std::slice::from_ref(&source),
            &compact,
            std::slice::from_ref(&tombstone),
            &lineage,
        )?,
        source_bytes
            + EventIndexWriter::publication_record_accounted_bytes(&compact[0])?
            + EventIndexWriter::publication_session_accounted_bytes()
            + EventIndexWriter::publication_tombstone_accounted_bytes(&tombstone)?,
    );
    Ok(())
}

#[test]
fn lineage_physical_cost_is_exactly_four_e_plus_348_s_plus_235_c() -> Result<(), Box<dyn Error>> {
    let source = test_source(0x5b)?;
    let first = test_state(&source, 1, 1, 0x31)?;
    let mut copied = test_state(&source, 2, 2, 0x32)?;
    copied.lineage.origin_kind = IndexedCoreEventOriginKind::CopiedFromAncestor;
    copied.lineage.copied_from = Some(IndexedCopiedEventOrigin {
        ancestor_session_id: first.lineage.session_id,
        ancestor_event_id: first.event_id,
        proof: ctx_attribution_model::EventCopyProofKind::CertifiedOrderedPrefix,
    });
    let (records, lineage) = compact_records(vec![first, copied])?;

    assert_eq!(records.len(), 2);
    assert_eq!(lineage.sessions.len(), 2);
    assert_eq!(lineage.copied_origins.len(), 1);
    assert_eq!(
        lineage.encoded_bytes(records.len())?,
        (4 * records.len())
            + (SESSION_DICTIONARY_ROW_BYTES * lineage.sessions.len())
            + (COPIED_ORIGIN_ROW_BYTES * lineage.copied_origins.len())
    );
    assert_eq!(SESSION_DICTIONARY_ROW_BYTES, 348);
    assert_eq!(COPIED_ORIGIN_ROW_BYTES, 235);
    Ok(())
}

#[test]
fn lineage_contract_preserves_persisted_discriminants() -> Result<(), Box<dyn Error>> {
    let source = test_source(0x5d)?;
    let session_id = test_session_id(&source, 1)?;
    for (relationship, persisted) in [
        (SessionRelationshipKind::Root, 0),
        (SessionRelationshipKind::Delegated, 1),
        (SessionRelationshipKind::Forked, 2),
        (SessionRelationshipKind::ResumedFrom, 3),
        (SessionRelationshipKind::WorkflowChild, 4),
        (SessionRelationshipKind::RelatedUnknown, 5),
    ] {
        let lineage = IndexedCoreEventLineage {
            session_id,
            parent_session_id: None,
            root_session_id: Some(session_id),
            session_relationship: relationship,
            origin_kind: IndexedCoreEventOriginKind::Unknown,
            copied_from: None,
        };
        let encoded = encode_session_lineage(&lineage)?;
        assert_eq!(encoded[SESSION_DICTIONARY_ROW_BYTES - 1], persisted);
        assert_eq!(
            decode_session_lineage(&encoded)?.session_relationship,
            relationship
        );
    }

    let unknown = IndexedCoreEventLineage {
        session_id,
        parent_session_id: None,
        root_session_id: None,
        session_relationship: SessionRelationshipKind::RelatedUnknown,
        origin_kind: IndexedCoreEventOriginKind::Unknown,
        copied_from: None,
    };
    let encoded_unknown = encode_session_lineage(&unknown)?;
    let root_marker_offset = (StableEntityId::CANONICAL_LEN * 2) + 1;
    let root_bytes = root_marker_offset + 1..root_marker_offset + 1 + StableEntityId::CANONICAL_LEN;
    assert_eq!(encoded_unknown[root_marker_offset], 0);
    assert!(
        encoded_unknown[root_bytes.clone()]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_eq!(decode_session_lineage(&encoded_unknown)?, unknown);
    assert!(decode_session_lineage(&encoded_unknown[..encoded_unknown.len() - 1]).is_err());

    let mut invalid_marker = encoded_unknown;
    invalid_marker[root_marker_offset] = 2;
    assert!(decode_session_lineage(&invalid_marker).is_err());
    let mut invalid_absent_payload = encoded_unknown;
    invalid_absent_payload[root_bytes.start] = 1;
    assert!(decode_session_lineage(&invalid_absent_payload).is_err());

    for (proof, persisted) in [
        (EventCopyProofKind::NativeEventIdentity, 0),
        (EventCopyProofKind::NativeCopiedFromField, 1),
        (EventCopyProofKind::NativeCallResultIdentity, 2),
        (EventCopyProofKind::CertifiedOrderedPrefix, 3),
    ] {
        let row = IndexedCopiedOriginRow {
            event_ordinal: 7,
            origin: IndexedCopiedEventOrigin {
                ancestor_session_id: session_id,
                ancestor_event_id: test_event_id(&source, 7)?,
                proof,
            },
        };
        let encoded = encode_copied_origin(&row)?;
        assert_eq!(encoded[COPIED_ORIGIN_ROW_BYTES - 1], persisted);
        assert_eq!(decode_copied_origin(&encoded)?, row);
    }
    Ok(())
}

#[test]
fn unknown_root_lineage_round_trips_without_synthesis() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let path = temp.path().join("unknown-root.ctxs");
    let source = test_source(0x5e)?;
    let mut state = test_state(&source, 1, 1, 0x33)?;
    state.lineage.root_session_id = None;
    state.lineage.session_relationship = SessionRelationshipKind::RelatedUnknown;

    write_index(&path, vec![source.clone()], vec![state.clone()], Vec::new())?;
    let mut reader = open_index(&path)?;
    let visible = reader
        .lookup(&source, state.event_id)?
        .ok_or("unknown-root event is absent")?;
    let EventIndexEntry::State { state: decoded, .. } = visible else {
        return Err("unknown-root event resolved as a tombstone".into());
    };
    assert_eq!(decoded.lineage, state.lineage);
    assert_eq!(decoded.lineage.root_session_id, None);
    Ok(())
}

#[test]
fn copied_origin_reference_is_opaque_and_need_not_exist_in_the_index() -> Result<(), Box<dyn Error>>
{
    let temp = TempDir::new()?;
    let path = temp.path().join("unresolved-copy.ctxs");
    let source = test_source(0x5c)?;
    let mut copied = test_state(&source, 2, 2, 0x32)?;
    let absent_session = test_session_id(&source, 9_001)?;
    let absent_event = test_event_id(&source, 9_002)?;
    copied.lineage.origin_kind = IndexedCoreEventOriginKind::CopiedFromAncestor;
    copied.lineage.copied_from = Some(IndexedCopiedEventOrigin {
        ancestor_session_id: absent_session,
        ancestor_event_id: absent_event,
        proof: ctx_attribution_model::EventCopyProofKind::CertifiedOrderedPrefix,
    });

    write_index(
        &path,
        vec![source.clone()],
        vec![copied.clone()],
        Vec::new(),
    )?;
    let mut reader = open_index(&path)?;
    let visible = reader
        .lookup(&source, copied.event_id)?
        .ok_or("copied event is absent")?;
    let EventIndexEntry::State { state, .. } = visible else {
        return Err("copied event resolved as a tombstone".into());
    };
    assert_eq!(
        state.lineage.copied_from,
        Some(IndexedCopiedEventOrigin {
            ancestor_session_id: absent_session,
            ancestor_event_id: absent_event,
            proof: ctx_attribution_model::EventCopyProofKind::CertifiedOrderedPrefix,
        })
    );
    assert!(reader.lookup(&source, absent_event)?.is_none());
    Ok(())
}

#[test]
fn rich_sparse_lookup_and_paging_are_exact() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let path = temp.path().join("event-index.ctxs");
    let source_a = test_source(1)?;
    let source_b = test_source(2)?;
    let sparse_source = test_source(3)?;
    let a1 = test_state(&source_a, 1, 10, 0x11)?;
    let a2 = test_state(&source_a, 2, 20, 0x22)?;
    let a3 = test_state(&source_a, 3, 30, 0x33)?;
    let a4_tombstone = test_tombstone(&source_a, 4)?;
    let b1 = test_state(&source_b, 1, 40, 0x44)?;

    let stats = write_index(
        &path,
        vec![sparse_source.clone(), source_b.clone(), source_a.clone()],
        vec![a3.clone(), b1.clone(), a1.clone(), a2.clone()],
        vec![a4_tombstone.clone()],
    )?;
    assert_eq!(stats.source_count, 3);
    assert_eq!(stats.record_count, 4);
    assert_eq!(stats.tombstone_count, 1);

    let mut reader = open_index(&path)?;
    assert_eq!(reader.sources().len(), 3);
    let resolved = reader
        .source_by_storage_key(&source_a.storage_key)?
        .ok_or("source dictionary lookup failed")?;
    assert!(resolved.exact_eq(&source_a));
    assert!(
        reader
            .source_by_storage_key(&test_source(9)?.storage_key)?
            .is_none()
    );

    let exact = reader
        .lookup(&source_a, a2.event_id)?
        .ok_or("exact event lookup failed")?;
    assert_eq!(exact, visible_state(a2.clone(), false));
    assert!(
        reader
            .lookup(&source_a, test_event_id(&source_a, 99)?)?
            .is_none()
    );
    assert!(matches!(
        reader.lookup(&source_a, b1.event_id),
        Err(EventIndexError::Invalid("event source identity"))
    ));

    let first = reader.page(&source_a, None, 2)?;
    assert!(!first.terminal);
    assert_eq!(event_numbers(&first.entries), vec![1, 2]);
    let continuation = first.continuation.ok_or("missing continuation")?;
    let second = reader.page(&source_a, Some(continuation.event_id), 2)?;
    assert!(second.terminal);
    assert!(second.continuation.is_none());
    assert_eq!(event_numbers(&second.entries), vec![3, 4]);
    assert!(matches!(
        second.entries.last(),
        Some(EventIndexEntry::Tombstone(_))
    ));

    let sparse = reader.page(&sparse_source, None, 4)?;
    assert!(sparse.terminal);
    assert!(sparse.entries.is_empty());
    assert!(matches!(
        reader.page(&source_a, None, 0),
        Err(EventIndexError::Bound("page item count"))
    ));
    assert!(matches!(
        reader.page(&source_a, None, MAX_EVENT_INDEX_PAGE_ITEMS + 1),
        Err(EventIndexError::Bound("page item count"))
    ));
    Ok(())
}

#[test]
fn replacement_shadow_exposes_newest_state_and_suppresses_older() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let source = test_source(4)?;
    let event_id = test_event_id(&source, 7)?;
    let old = IndexedCoreEventState {
        source_storage_key: source.storage_key.clone(),
        event_id,
        lineage: test_lineage(&source, 7)?,
        event_sequence: 7,
        core_record_sha256: "1".repeat(64),
        core_record_leaf_sha256: "2".repeat(64),
        flat_record_count: 0,
        event_output_root: "3".repeat(64),
        coverage: SegmentCoreCoverage::default(),
    };
    let replacement = IndexedCoreEventState {
        core_record_sha256: "2".repeat(64),
        event_output_root: "4".repeat(64),
        ..old.clone()
    };
    let shadow = IndexedCoreEventTombstone {
        source_storage_key: source.storage_key.clone(),
        event_id,
        prior_event_state_sha256: "3".repeat(64),
    };
    let deleted_event = test_event_id(&source, 8)?;
    let deletion = IndexedCoreEventTombstone {
        source_storage_key: source.storage_key.clone(),
        event_id: deleted_event,
        prior_event_state_sha256: format!("{:064x}", 0x8a_u16),
    };
    let old_path = temp.path().join("old.ctxs");
    let newest_path = temp.path().join("newest.ctxs");
    write_index(
        &old_path,
        vec![source.clone()],
        vec![old, test_state(&source, 8, 8, 0x88)?],
        Vec::new(),
    )?;
    write_index(
        &newest_path,
        vec![source.clone()],
        vec![replacement.clone()],
        vec![shadow, deletion.clone()],
    )?;

    let mut newest = open_index(&newest_path)?;
    assert_eq!(
        newest.lookup(&source, event_id)?,
        Some(visible_state(replacement.clone(), true))
    );
    assert!(matches!(
        newest.lookup(&source, deleted_event)?,
        Some(EventIndexEntry::Tombstone(tombstone)) if tombstone == deletion
    ));
    let newest_page = newest.page(&source, None, 8)?;
    assert_eq!(newest_page.entries.len(), 2);
    assert_eq!(
        newest_page.entries.first(),
        Some(&visible_state(replacement.clone(), true))
    );

    // A later orchestrator can process segments newest-first and retain the
    // first visible entry per compact key. The replacement wins, while the
    // tombstone-only deletion suppresses the old event. The replacement shadow
    // remains explicit on the winning current state.
    let mut merged = BTreeMap::<EventIndexKey, EventIndexEntry>::new();
    for entry in newest_page.entries {
        merged.entry(entry.key()).or_insert(entry);
    }
    let mut older = open_index(&old_path)?;
    for entry in older.page(&source, None, 8)?.entries {
        merged.entry(entry.key()).or_insert(entry);
    }
    assert_eq!(merged.len(), 2);
    assert_eq!(
        merged.get(&replacement.key()),
        Some(&visible_state(replacement, true))
    );
    assert!(matches!(
        merged.get(&deletion.key()),
        Some(EventIndexEntry::Tombstone(_))
    ));
    Ok(())
}

#[test]
fn source_dictionary_is_single_copy_and_bytes_are_deterministic() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let first_path = temp.path().join("first.ctxs");
    let second_path = temp.path().join("second.ctxs");
    let source = test_source(5)?;
    let records = (1..=32)
        .map(|event| test_state(&source, event, event, event as u8))
        .collect::<Result<Vec<_>, _>>()?;
    let mut reversed = records.clone();
    reversed.reverse();
    let shadow = test_tombstone(&source, 8)?;

    let first_stats = write_index(
        &first_path,
        vec![source.clone()],
        reversed,
        vec![shadow.clone()],
    )?;
    let second_stats = write_index(&second_path, vec![source.clone()], records, vec![shadow])?;
    assert_eq!(first_stats, second_stats);
    assert_eq!(fs::read(&first_path)?, fs::read(&second_path)?);

    let mut stored = segment_file(&first_path)?;
    let plaintext = stored.read_all()?;
    assert_eq!(
        plaintext
            .windows(source.source.provider().len())
            .filter(|window| *window == source.source.provider().as_bytes())
            .count(),
        1
    );
    let reader = open_index(&first_path)?;
    let reconstructed = reader
        .source_by_storage_key(&source.storage_key)?
        .ok_or("source reconstruction failed")?;
    assert!(reconstructed.source.exact_descriptor_eq(&source.source));
    Ok(())
}

#[test]
fn cross_chunk_lookup_and_paging() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let path = temp.path().join("cross-chunk.ctxs");
    let source = test_source(6)?;
    let mut records = (1..=500)
        .map(|event| test_state(&source, event, event * 3, (event % 16) as u8))
        .collect::<Result<Vec<_>, _>>()?;
    records.reverse();
    let stats = write_index(&path, vec![source.clone()], records, Vec::new())?;
    assert!(stats.plaintext_bytes > u64::from(TEST_CHUNK_BYTES) * 3);

    let mut reader = open_index(&path)?;
    let exact_id = test_event_id(&source, 257)?;
    assert!(matches!(
        reader.lookup(&source, exact_id)?,
        Some(EventIndexEntry::State { state, shadows_older: false })
            if state.event_sequence == 771
    ));
    let mut after = None;
    let mut seen = Vec::new();
    loop {
        let page = reader.page(&source, after, 113)?;
        seen.extend(event_numbers(&page.entries));
        if page.terminal {
            break;
        }
        after = page.continuation.map(|continuation| continuation.event_id);
    }
    assert_eq!(seen, (1..=500).collect::<Vec<_>>());
    Ok(())
}

#[test]
fn writer_rejects_bounds_duplicates_and_wrong_role() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let source = test_source(7)?;
    let bounded_path = temp.path().join("bounded.ctxs");
    assert!(matches!(
        EventIndexWriter::write(
            segment_writer(&bounded_path)?,
            vec![source.clone(); MAX_EVENT_INDEX_SOURCES + 1],
            Vec::new(),
            Vec::new(),
        ),
        Err(EventIndexError::Bound("source count"))
    ));

    let duplicate = test_state(&source, 1, 1, 1)?;
    let duplicate_path = temp.path().join("duplicate.ctxs");
    assert!(matches!(
        EventIndexWriter::write(
            segment_writer(&duplicate_path)?,
            vec![source.clone()],
            vec![duplicate.clone(), duplicate],
            Vec::new(),
        ),
        Err(EventIndexError::Conflict)
    ));

    let invalid_path = temp.path().join("invalid.ctxs");
    let mut invalid = test_state(&source, 2, 2, 2)?;
    invalid.core_record_sha256 = "A".repeat(64);
    assert!(matches!(
        EventIndexWriter::write(
            segment_writer(&invalid_path)?,
            vec![source.clone()],
            vec![invalid],
            Vec::new(),
        ),
        Err(EventIndexError::Invalid("Core record SHA-256"))
    ));

    let invalid_coverage_path = temp.path().join("invalid-coverage.ctxs");
    let mut invalid_coverage = test_state(&source, 3, 3, 3)?;
    invalid_coverage.coverage.file_evidence_events = 2;
    assert!(matches!(
        EventIndexWriter::write(
            segment_writer(&invalid_coverage_path)?,
            vec![source.clone()],
            vec![invalid_coverage],
            Vec::new(),
        ),
        Err(EventIndexError::Invalid("per-event coverage"))
    ));

    let valid_path = temp.path().join("valid.ctxs");
    write_index(&valid_path, vec![source], Vec::new(), Vec::new())?;
    assert!(SegmentFile::open(&valid_path, TEST_GENERATION, 0x46_4c_41_54,).is_err());
    Ok(())
}

#[test]
fn reader_rejects_semantic_corruption() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let source = test_source(8)?;
    let original = temp.path().join("original.ctxs");
    write_index(
        &original,
        vec![source.clone()],
        vec![test_state(&source, 1, 1, 1)?, test_state(&source, 2, 2, 2)?],
        vec![test_tombstone(&source, 3)?, test_tombstone(&source, 4)?],
    )?;

    let cases: Vec<(&str, PlaintextMutation)> = vec![
        ("header", Box::new(|bytes| bytes[0] ^= 1)),
        (
            "legacy-v5-header",
            Box::new(|bytes| {
                bytes[..8].copy_from_slice(b"CTXEVI05");
                bytes[8..10].copy_from_slice(&5_u16.to_le_bytes());
            }),
        ),
        (
            "offset",
            Box::new(|bytes| {
                let offset = raw_u64(bytes, 56) + 1;
                bytes[56..64].copy_from_slice(&offset.to_le_bytes());
            }),
        ),
        (
            "count",
            Box::new(|bytes| {
                bytes[12..16]
                    .copy_from_slice(&((MAX_EVENT_INDEX_SOURCES as u32) + 1).to_le_bytes());
            }),
        ),
        (
            "oversized",
            Box::new(|bytes| {
                bytes[48..56].copy_from_slice(&(MAX_SOURCE_SECTION_BYTES + 1).to_le_bytes());
            }),
        ),
        (
            "record-order",
            Box::new(|bytes| {
                let start = raw_u64(bytes, 56) as usize;
                swap_fixed_rows(bytes, start, RECORD_BYTES);
            }),
        ),
        (
            "record-duplicate",
            Box::new(|bytes| {
                let start = raw_u64(bytes, 56) as usize;
                copy_fixed_key(bytes, start, RECORD_BYTES);
            }),
        ),
        (
            "tombstone-duplicate",
            Box::new(|bytes| {
                let start = raw_u64(bytes, 72) as usize;
                copy_fixed_key(bytes, start, TOMBSTONE_BYTES);
            }),
        ),
        (
            "digest",
            Box::new(|bytes| {
                let start = raw_u64(bytes, 56) as usize + 4 + EVENT_DIGEST_BYTES + 8;
                bytes[start] = b'G';
            }),
        ),
        (
            "identity",
            Box::new(|bytes| {
                let start = raw_u64(bytes, 56) as usize;
                bytes[start..start + 4].copy_from_slice(&99_u32.to_le_bytes());
            }),
        ),
        (
            "source-dictionary",
            Box::new(|bytes| {
                let marker = b"core_source_";
                let position = bytes
                    .windows(marker.len())
                    .position(|window| window == marker)
                    .unwrap();
                let digit = &mut bytes[position + marker.len()];
                *digit = if *digit == b'a' { b'b' } else { b'a' };
            }),
        ),
    ];
    for (name, mutate) in cases {
        let corrupted = temp.path().join(format!("corrupt-{name}.ctxs"));
        rewrite_plaintext(&original, &corrupted, mutate)?;
        if matches!(
            name,
            "header" | "legacy-v5-header" | "offset" | "count" | "oversized" | "source-dictionary"
        ) {
            assert!(open_index(&corrupted).is_err(), "accepted {name} at open");
        } else {
            let mut reader = open_index(&corrupted)?;
            assert!(
                reader.page(&source, None, 8).is_err(),
                "accepted lazily reached {name}"
            );
        }
    }

    let invalid_coverage = temp.path().join("corrupt-coverage.ctxs");
    rewrite_plaintext(
        &original,
        &invalid_coverage,
        Box::new(|bytes| {
            let start = raw_u64(bytes, 56) as usize;
            bytes[start + RECORD_BYTES - 5] |= 0b1000_0000;
        }),
    )?;
    let mut reader = open_index(&invalid_coverage)?;
    assert!(matches!(
        reader.page(&source, None, 1),
        Err(EventIndexError::Corrupt("packed per-event coverage"))
    ));
    Ok(())
}

#[test]
fn reader_rejects_checked_tamper_and_truncation() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let source = test_source(9)?;
    let original = temp.path().join("original.ctxs");
    write_index(
        &original,
        vec![source.clone()],
        vec![test_state(&source, 1, 1, 1)?],
        Vec::new(),
    )?;

    let mut stored = fs::read(&original)?;
    let last = stored.len().checked_sub(1).ok_or("empty segment file")?;
    stored[last] ^= 1;
    let tampered = temp.path().join("tampered.ctxs");
    crate::filesystem::create_private_file_new(&tampered)?.write_all(&stored)?;
    match open_index(&tampered) {
        Ok(mut lazy_reader) => {
            assert!(
                lazy_reader
                    .lookup(&source, test_event_id(&source, 1)?)
                    .is_err()
            );
        }
        Err(EventIndexError::File(SegmentFileError::Corrupt("block checksum"))) => {}
        Err(error) => return Err(error.into()),
    }

    let mut truncated_bytes = fs::read(&original)?;
    truncated_bytes.pop();
    let truncated = temp.path().join("truncated.ctxs");
    crate::filesystem::create_private_file_new(&truncated)?.write_all(&truncated_bytes)?;
    assert!(open_index(&truncated).is_err());
    Ok(())
}

#[test]
fn sparse_lineage_open_work_is_proportional_to_sessions_and_copies_not_events()
-> Result<(), Box<dyn Error>> {
    const SESSION_COUNT: u64 = 128;
    const COPIED_EVENT_COUNT: u64 = 128;
    const FIRST_COPIED_EVENT: u64 = 1_025;

    let temp = TempDir::new()?;
    let source = test_source(0x5a)?;
    let root_session = test_session_id(&source, 1)?;
    let copied_session = test_session_id(&source, SESSION_COUNT + 1)?;
    let mut observed_chunk_reads = Vec::new();

    for event_count in [16_384_u64, 131_072] {
        let path = temp.path().join(format!("sparse-{event_count}.ctxs"));
        let mut records = Vec::with_capacity(usize::try_from(event_count)?);
        for event_number in 1..=event_count {
            let mut record = test_state(&source, event_number, event_number, 0x51)?;
            let session_number = ((event_number - 1) % SESSION_COUNT) + 1;
            let session_id = test_session_id(&source, session_number)?;
            record.lineage.session_id = session_id;
            record.lineage.root_session_id = Some(session_id);
            record.lineage.parent_session_id = None;
            record.lineage.session_relationship =
                ctx_attribution_model::SessionRelationshipKind::Root;
            if (FIRST_COPIED_EVENT..FIRST_COPIED_EVENT + COPIED_EVENT_COUNT).contains(&event_number)
            {
                let ancestor_number = event_number - FIRST_COPIED_EVENT + 1;
                record.lineage.session_id = copied_session;
                record.lineage.parent_session_id = Some(root_session);
                record.lineage.root_session_id = Some(root_session);
                record.lineage.session_relationship =
                    ctx_attribution_model::SessionRelationshipKind::Forked;
                record.lineage.origin_kind = IndexedCoreEventOriginKind::CopiedFromAncestor;
                record.lineage.copied_from = Some(IndexedCopiedEventOrigin {
                    ancestor_session_id: test_session_id(&source, ancestor_number)?,
                    ancestor_event_id: test_event_id(&source, ancestor_number)?,
                    proof: ctx_attribution_model::EventCopyProofKind::CertifiedOrderedPrefix,
                });
            }
            records.push(record);
        }
        let stats = write_index(&path, vec![source.clone()], records, Vec::new())?;
        assert_eq!(stats.record_count, u32::try_from(event_count)?);
        assert_eq!(stats.session_count, u32::try_from(SESSION_COUNT + 1)?);
        assert_eq!(stats.copied_event_count, u32::try_from(COPIED_EVENT_COUNT)?);

        let reader = open_index(&path)?;
        let work = reader.sparse_lineage_open_work();
        assert_eq!(work.session_rows, usize::try_from(SESSION_COUNT + 1)?);
        assert_eq!(
            work.copied_origin_rows,
            usize::try_from(COPIED_EVENT_COUNT)?
        );
        assert_eq!(
            work.copied_event_ref_reads,
            usize::try_from(COPIED_EVENT_COUNT)?
        );
        assert_eq!(work.ordinary_event_ref_reads, 0);
        observed_chunk_reads.push(reader.open_checked_chunk_reads());
    }

    assert!(observed_chunk_reads[0] > 0);
    assert!(
        observed_chunk_reads[1] <= observed_chunk_reads[0].saturating_add(2),
        "ordinary-event growth changed sparse open work: {observed_chunk_reads:?}"
    );
    Ok(())
}

fn visible_state(state: IndexedCoreEventState, shadows_older: bool) -> EventIndexEntry {
    EventIndexEntry::State {
        state,
        shadows_older,
    }
}

fn event_numbers(entries: &[EventIndexEntry]) -> Vec<u64> {
    entries
        .iter()
        .map(|entry| {
            let digest = entry.event_id().digest();
            u64::from_be_bytes(digest[24..].try_into().unwrap())
        })
        .collect()
}

fn test_source(tag: u8) -> Result<EventIndexSource, EventIndexError> {
    let source = SourceKey::derive(
        format!("event-index-provider-{tag}"),
        "event_index_test",
        "test",
        1,
        SourceAnchor::CatalogLineage([tag; 32]),
    )
    .map_err(|_| EventIndexError::Invalid("test source"))?;
    EventIndexSource::new(source)
}

fn test_event_id(
    source: &EventIndexSource,
    event_number: u64,
) -> Result<StableEntityId, EventIndexError> {
    let mut digest = [0_u8; EVENT_DIGEST_BYTES];
    digest[24..].copy_from_slice(&event_number.to_be_bytes());
    event_identity(source, digest)
}

fn test_session_id(
    source: &EventIndexSource,
    session_number: u64,
) -> Result<StableEntityId, EventIndexError> {
    let mut digest = [0_u8; EVENT_DIGEST_BYTES];
    digest[24..].copy_from_slice(&session_number.to_be_bytes());
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let identity: StableEntityId = serde_json::from_value(serde_json::json!({
        "contract_version": ctx_history_core::IDENTITY_VERSION,
        "entity_kind": StableEntityKind::Session,
        "digest": digest,
        "source_digest": source.source.identity().digest(),
        "source_descriptor_digest": source.source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))
    .map_err(|_| EventIndexError::Invalid("test session identity"))?;
    identity
        .validate_contract()
        .map_err(|_| EventIndexError::Invalid("test session identity"))?;
    Ok(identity)
}

fn test_lineage(
    source: &EventIndexSource,
    session_number: u64,
) -> Result<IndexedCoreEventLineage, EventIndexError> {
    let session_id = test_session_id(source, session_number)?;
    Ok(IndexedCoreEventLineage {
        session_id,
        parent_session_id: None,
        root_session_id: Some(session_id),
        session_relationship: ctx_attribution_model::SessionRelationshipKind::Root,
        origin_kind: IndexedCoreEventOriginKind::UniqueToSession,
        copied_from: None,
    })
}

fn test_state(
    source: &EventIndexSource,
    event_number: u64,
    event_sequence: u64,
    digest_digit: u8,
) -> Result<IndexedCoreEventState, EventIndexError> {
    Ok(IndexedCoreEventState {
        source_storage_key: source.storage_key.clone(),
        event_id: test_event_id(source, event_number)?,
        lineage: test_lineage(source, event_number)?,
        event_sequence,
        core_record_sha256: format!("{digest_digit:064x}"),
        core_record_leaf_sha256: format!("{:064x}", u16::from(digest_digit) + 1),
        flat_record_count: 0,
        event_output_root: format!("{:064x}", u16::from(digest_digit) + 2),
        coverage: SegmentCoreCoverage {
            repository_candidate_events: u64::from(event_number & 1 != 0),
            logical_binding_events: u64::from(event_number & 2 != 0),
            certified_live_root_access_events: u64::from(event_number & 4 != 0),
            file_evidence_events: u64::from(event_number & 8 != 0),
            exact_commit_evidence_events: u64::from(event_number & 16 != 0),
            exact_pull_request_evidence_events: u64::from(event_number & 32 != 0),
            bounded_omission_events: u64::from(event_number & 64 != 0),
        },
    })
}

fn test_tombstone(
    source: &EventIndexSource,
    event_number: u64,
) -> Result<IndexedCoreEventTombstone, EventIndexError> {
    Ok(IndexedCoreEventTombstone {
        source_storage_key: source.storage_key.clone(),
        event_id: test_event_id(source, event_number)?,
        prior_event_state_sha256: "a".repeat(64),
    })
}

fn segment_writer(path: &Path) -> Result<SegmentWriter, SegmentFileError> {
    SegmentWriter::create(
        path,
        TEST_GENERATION,
        EVENT_STATE_INDEX_ROLE,
        TEST_CHUNK_BYTES,
    )
}

fn segment_file(path: &Path) -> Result<SegmentFile, SegmentFileError> {
    SegmentFile::open(path, TEST_GENERATION, EVENT_STATE_INDEX_ROLE)
}

fn write_index(
    path: &Path,
    sources: Vec<EventIndexSource>,
    records: Vec<IndexedCoreEventState>,
    tombstones: Vec<IndexedCoreEventTombstone>,
) -> Result<EventIndexStats, EventIndexError> {
    EventIndexWriter::write(segment_writer(path)?, sources, records, tombstones)
}

fn compact_records(
    records: Vec<IndexedCoreEventState>,
) -> Result<(Vec<CompactIndexedCoreEventState>, EventLineageTables), EventIndexError> {
    let mut accumulator = EventLineageAccumulator::new();
    let mut compact = Vec::with_capacity(records.len());
    for record in records {
        let (session_ref, _, _) = accumulator.retain(record.key(), record.lineage)?;
        compact.push(CompactIndexedCoreEventState {
            source_storage_key: record.source_storage_key,
            event_id: record.event_id,
            session_ref,
            event_sequence: record.event_sequence,
            core_record_sha256: record.core_record_sha256,
            core_record_leaf_sha256: record.core_record_leaf_sha256,
            flat_record_count: record.flat_record_count,
            event_output_root: record.event_output_root,
            coverage: record.coverage,
        });
    }
    let lineage = accumulator.finish(&mut compact)?;
    Ok((compact, lineage))
}

fn open_index(path: &Path) -> Result<EventIndexReader, EventIndexError> {
    EventIndexReader::open(segment_file(path)?)
}

fn rewrite_plaintext(
    source: &Path,
    destination: &Path,
    mutate: PlaintextMutation,
) -> Result<(), Box<dyn Error>> {
    let mut file = segment_file(source)?;
    let mut plaintext = file.read_all()?;
    mutate(plaintext.as_mut_slice());
    let mut writer = segment_writer(destination)?;
    writer.write_all(plaintext.as_slice())?;
    writer.finish()?;
    Ok(())
}

fn raw_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn swap_fixed_rows(bytes: &mut [u8], start: usize, width: usize) {
    let (through_first, after_first) = bytes.split_at_mut(start + width);
    through_first[start..].swap_with_slice(&mut after_first[..width]);
}

fn copy_fixed_key(bytes: &mut [u8], start: usize, width: usize) {
    let key_bytes = 4 + EVENT_DIGEST_BYTES;
    let (through_first, after_first) = bytes.split_at_mut(start + width);
    after_first[..key_bytes].copy_from_slice(&through_first[start..start + key_bytes]);
}
