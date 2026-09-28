use super::*;

use crate::graph::segment_state::SegmentCompletedControl;
use crate::materializer::CoreGenerationStart;
use crate::protocol::{CoreMaterializationReceipt, CoreMaterializationReceiptIdentity};

#[derive(serde::Serialize)]
struct BeginIntegrity<'a> {
    head: &'a CoreGenerationHead,
    expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
}

#[test]
fn checked_direct_predecessor_legacy_controls_rebuild_and_activate() -> TestResult {
    for predecessor in [
        crate::graph::segment_graph::PRE_DIRECT_MATERIALIZATION_SEGMENT_SCHEMA_IDENTITY,
        crate::graph::segment_graph::PRE_OPTIONAL_ROOT_SEGMENT_SCHEMA_IDENTITY,
    ] {
        checked_predecessor_rebuilds(predecessor, CORE_MATERIALIZER_REVISION)?;
    }
    checked_predecessor_rebuilds(
        crate::graph::segment_graph::PRE_STATIC_SHELL_QUOTING_SEGMENT_SCHEMA_IDENTITY,
        "2026.09.04.1+optional-root-facts",
    )
}

fn checked_predecessor_rebuilds(
    predecessor: &str,
    predecessor_materializer_revision: &str,
) -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    drop(SegmentMaterializer::open(&root)?);

    let record = one_record()?;
    let source = source_state(&record, 0x71);
    let active_head = head(0x72, std::slice::from_ref(&source))?;
    let active_receipt = CoreMaterializationReceipt {
        core_generation_id: active_head.core_generation_id.clone(),
        core_record_contract_fingerprint: active_head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: active_head.source_snapshot_sha256.clone(),
        materializer_revision: predecessor_materializer_revision.to_owned(),
        source_count: active_head.source_count,
        event_count: active_head.event_count,
    };
    let completed = SegmentCompletedControl::current(
        1,
        1,
        Some(active_receipt.clone()),
        Some(super::super::model::canonical_sha256(&(
            BeginIntegrity {
                head: &active_head,
                expected_prior_receipt: None,
            },
            predecessor_materializer_revision,
        ))?),
        Some(active_head),
        None,
        Some("e".repeat(64)),
        predecessor_materializer_revision.to_owned(),
        predecessor.to_owned(),
        crate::graph::GRAPH_SEMANTICS_FINGERPRINT.to_owned(),
        SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        core_record_contract_fingerprint(),
        Default::default(),
        "1".repeat(64),
    );
    let source_control = super::super::model::SourceStateSegment {
        completed: completed.clone(),
        mutations: vec![super::super::model::SourceMutation::Upsert {
            state: source,
            materializer_revision: predecessor_materializer_revision.to_owned(),
        }],
    };
    let empty_control = super::super::model::SourceStateSegment {
        completed,
        mutations: Vec::new(),
    };
    let mut source_json = serde_json::to_value(source_control)?;
    source_json["completed"]["replay_count"] = serde_json::json!(7);
    source_json["completed"]["replay_accumulator_sha256"] = serde_json::json!("legacy-a");
    let mut empty_json = serde_json::to_value(empty_control)?;
    empty_json["completed"]["replay_count"] = serde_json::json!(99);
    empty_json["completed"]["replay_accumulator_sha256"] = serde_json::json!("legacy-b");

    let mut source_reference = super::super::publication::write_source_segment_for_test(
        &root,
        1,
        super::super::model::MATERIALIZER_SOURCE_ROLE,
        0,
        &source_json,
    )?;
    source_reference.ordinal = 0;
    let mut empty_reference = super::super::publication::write_source_segment_for_test(
        &root,
        1,
        super::super::model::MATERIALIZER_SOURCE_ROLE,
        1,
        &empty_json,
    )?;
    empty_reference.ordinal = 1;
    crate::graph::segment::SegmentStore::new(&root).install_manifest_for_test(
        &crate::graph::segment::SegmentManifest {
            schema_version: crate::graph::segment::MANIFEST_SCHEMA_VERSION,
            generation_id: crate::graph::segment::random_generation_id()?,
            prior_generation_id: None,
            graph_generation: 1,
            materializer_identity: predecessor_materializer_revision.to_owned(),
            core_receipt: active_receipt,
            schema_identity: predecessor.to_owned(),
            evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
            ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
            segments: vec![source_reference, empty_reference],
            predecessor_segments: Vec::new(),
        },
    )?;

    let mut materializer =
        SegmentMaterializer::open_for_revision(&root, CORE_MATERIALIZER_REVISION)?;
    let active = materializer
        .active
        .as_ref()
        .ok_or_else(|| io::Error::other("checked predecessor is missing"))?;
    assert!(active.requires_clean_rebuild);
    assert!(active.sources.is_empty());
    assert_eq!(
        super::super::publication::event_index_reader_open_count(active),
        0
    );
    assert!(!materializer.has_queryable_active());
    assert!(
        materializer
            .flat_store()
            .open_active(crate::graph::segment_graph::SegmentGraph::flat_open_policy())
            .is_err()
    );

    let current_head = head(0x73, &[])?;
    let mut session = match materializer.start_core_generation(current_head.clone())? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("predecessor generation unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session,
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        current_head.core_generation_id.clone(),
        0,
        true,
        Vec::new(),
    ))?)?;
    assert!(reconciliations.is_empty());
    let receipt = session.activate()?;
    assert_eq!(receipt.core_generation_id, current_head.core_generation_id);
    assert_eq!(receipt.source_count, 0);
    assert_eq!(receipt.event_count, 0);
    assert_eq!(receipt.materializer_revision, CORE_MATERIALIZER_REVISION);
    assert!(materializer.has_queryable_active());
    let active = materializer
        .active
        .as_ref()
        .ok_or_else(|| io::Error::other("rebuilt generation is missing"))?;
    assert!(!active.requires_clean_rebuild);
    assert!(active.sources.is_empty());
    assert_eq!(active.completed.head.as_ref(), Some(&current_head));
    Ok(())
}

#[test]
fn active_event_merge_opens_each_layer_once_with_more_than_four_indexes() -> TestResult {
    const LAYERS: u32 = 8;
    const RECORDS: u32 = 512;
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    drop(SegmentMaterializer::open(&root)?);

    let record = one_record()?;
    let source = source_state(&record, 0x6d);
    let active_head = head(0x6e, std::slice::from_ref(&source))?;
    let active_receipt = CoreMaterializationReceipt {
        core_generation_id: active_head.core_generation_id.clone(),
        core_record_contract_fingerprint: active_head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: active_head.source_snapshot_sha256.clone(),
        materializer_revision: CORE_MATERIALIZER_REVISION.to_owned(),
        source_count: active_head.source_count,
        event_count: active_head.event_count,
    };
    let completed = SegmentCompletedControl::current(
        1,
        1,
        Some(active_receipt.clone()),
        Some(super::super::model::canonical_sha256(&(
            BeginIntegrity {
                head: &active_head,
                expected_prior_receipt: None,
            },
            CORE_MATERIALIZER_REVISION,
        ))?),
        Some(active_head),
        None,
        Some("e".repeat(64)),
        CORE_MATERIALIZER_REVISION.to_owned(),
        SEGMENT_SCHEMA_IDENTITY.to_owned(),
        crate::graph::GRAPH_SEMANTICS_FINGERPRINT.to_owned(),
        SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        core_record_contract_fingerprint(),
        Default::default(),
        "1".repeat(64),
    );
    let source_control = super::super::model::SourceStateSegment {
        completed,
        mutations: vec![super::super::model::SourceMutation::Upsert {
            state: source.clone(),
            materializer_revision: CORE_MATERIALIZER_REVISION.to_owned(),
        }],
    };
    let mut source_reference = super::super::publication::write_source_segment_for_test(
        &root,
        1,
        super::super::model::MATERIALIZER_SOURCE_ROLE,
        0,
        &source_control,
    )?;
    source_reference.ordinal = 0;
    let index_source = EventIndexSource::new(record.source.clone())?;
    let index_records = (0..RECORDS)
        .map(|index| {
            let record = indexed_record(&record, index)?;
            Ok(IndexedCoreEventState {
                source_storage_key: index_source.storage_key.clone(),
                event_id: record.event_id,
                lineage: IndexedCoreEventLineage {
                    session_id: record.session_id,
                    parent_session_id: record.parent_session_id,
                    root_session_id: record.root_session_id,
                    session_relationship: match record.session_relationship {
                        Some(ProviderNativeSessionRelationship::Root) => {
                            SessionRelationshipKind::Root
                        }
                        Some(ProviderNativeSessionRelationship::Delegated) => {
                            SessionRelationshipKind::Delegated
                        }
                        Some(ProviderNativeSessionRelationship::Forked) => {
                            SessionRelationshipKind::Forked
                        }
                        Some(ProviderNativeSessionRelationship::ResumedFrom) => {
                            SessionRelationshipKind::ResumedFrom
                        }
                        Some(ProviderNativeSessionRelationship::WorkflowChild) => {
                            SessionRelationshipKind::WorkflowChild
                        }
                        None => SessionRelationshipKind::RelatedUnknown,
                    },
                    origin_kind: IndexedCoreEventOriginKind::UniqueToSession,
                    copied_from: None,
                },
                event_sequence: record.event_sequence,
                core_record_sha256: "2".repeat(64),
                core_record_leaf_sha256: "3".repeat(64),
                flat_record_count: 0,
                event_output_root: "4".repeat(64),
                coverage: Default::default(),
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    let mut segments = vec![source_reference];
    for ordinal in 1..=LAYERS {
        segments.push(
            super::super::publication::write_event_index_segment_for_test(
                &root,
                1,
                ordinal,
                vec![index_source.clone()],
                index_records.clone(),
            )?,
        );
    }
    crate::graph::segment::SegmentStore::new(&root).install_manifest_for_test(
        &crate::graph::segment::SegmentManifest {
            schema_version: crate::graph::segment::MANIFEST_SCHEMA_VERSION,
            generation_id: crate::graph::segment::random_generation_id()?,
            prior_generation_id: None,
            graph_generation: 1,
            materializer_identity: CORE_MATERIALIZER_REVISION.to_owned(),
            core_receipt: active_receipt,
            schema_identity: SEGMENT_SCHEMA_IDENTITY.to_owned(),
            evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
            ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
            segments,
            predecessor_segments: Vec::new(),
        },
    )?;

    let mut materializer =
        SegmentMaterializer::open_for_revision(&root, CORE_MATERIALIZER_REVISION)?;
    let active = materializer
        .active
        .as_mut()
        .ok_or_else(|| io::Error::other("active generation is missing"))?;
    super::super::publication::prepare_event_proofs(
        active,
        &root,
        "inspection",
        &|_, _| Ok(true),
        None,
    )?;
    let (states, observed, terminal) = super::super::publication::active_event_page(
        active,
        &root,
        &record.source,
        None,
        usize::try_from(RECORDS)?,
        false,
    )?;
    assert_eq!(states.len(), usize::try_from(RECORDS)?);
    assert_eq!(observed.len(), usize::try_from(RECORDS)?);
    assert!(terminal);
    assert_eq!(
        super::super::publication::event_index_reader_open_count(active),
        usize::try_from(LAYERS)?,
        "each layer should open once while preparing bounded proof pages",
    );
    Ok(())
}

#[test]
fn populated_indexes_prepare_proofs_once_across_repeated_replacement_deletion_and_reopen()
-> TestResult {
    use super::super::publication;
    use crate::graph::segment::{EventIndexEntry, SegmentStore};
    use sha2::{Digest, Sha256};
    const SOURCES: u32 = 72;
    const INDEXES: usize = 6;
    for overlapping in [false, true] {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("graph");
        let mut records = BTreeMap::new();
        for index in 0..SOURCES {
            let source = SourceKey::derive(
                "zed",
                "zed_threads_sqlite",
                "zed-nativepath-sqlite-v0",
                1,
                SourceAnchor::ProviderNative {
                    namespace: "thread-database".to_owned(),
                    key: TypedKey::Utf8(format!("proof-{index}.db")),
                },
            )?;
            let record = CoreRecord::new_selected(
                stable_entity(&source, StableEntityKind::Event, 0x41)?,
                stable_entity(&source, StableEntityKind::Session, 0x31)?,
                source,
                1,
                "message",
                "zed-nativepath-source-backed-v3-neutral-core",
                "original event".to_owned(),
            )?;
            records.insert(record.source.identity().digest(), record);
        }
        let original = records.clone();
        publish_inventory(&root, &records, 1)?;
        // Build six genuinely populated checked indexes from the published
        // records, with both disjoint dictionaries and complete overlap. This
        // exercises the >4-reader case without padding the corpus with empties.
        let mut manifest = SegmentStore::new(&root).load_active()?.ok_or("manifest")?;
        let reference = manifest
            .segments
            .iter()
            .find(|segment| segment.role == ctx_attribution_index::EVENT_STATE_INDEX_ROLE)
            .ok_or("event index")?
            .clone();
        let mut reader = publication::open_event_index_for_test(&root, &reference)?;
        let mut indexed = Vec::new();
        for record in records.values() {
            let source = EventIndexSource::new(record.source.clone())?;
            match reader
                .lookup(&source, record.event_id)?
                .ok_or("published event")?
            {
                EventIndexEntry::State { state, .. } => indexed.push(state),
                EventIndexEntry::Tombstone(_) => return Err("unexpected tombstone".into()),
            }
        }
        drop(reader);
        manifest
            .segments
            .retain(|segment| segment.role != ctx_attribution_index::EVENT_STATE_INDEX_ROLE);
        for index in 0..INDEXES {
            let selected = records
                .values()
                .enumerate()
                .filter(|(ordinal, _)| overlapping || ordinal % INDEXES == index)
                .map(|(_, record)| EventIndexSource::new(record.source.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            let entries = indexed
                .iter()
                .enumerate()
                .filter(|(ordinal, _)| overlapping || ordinal % INDEXES == index)
                .map(|(_, entry)| entry.clone())
                .collect();
            let ordinal = manifest.segments.len() as u32;
            manifest
                .segments
                .push(publication::write_event_index_segment_for_test(
                    &root,
                    manifest.graph_generation,
                    ordinal,
                    selected,
                    entries,
                )?);
        }
        for (ordinal, reference) in manifest.segments.iter_mut().enumerate() {
            reference.ordinal = ordinal as u32;
        }
        SegmentStore::new(&root).install_manifest_for_test(&manifest)?;
        for revision in 2..=4_u8 {
            let mut reopened = SegmentMaterializer::open(&root)?;
            let before = ctx_attribution_index::event_index_reader_work_for_test();
            let active = reopened.active.as_mut().ok_or("active")?;
            publication::prepare_event_proofs(active, &root, "inspection", &|_, _| Ok(true), None)?;
            for _ in 0..3 {
                for record in original.values() {
                    let metadata = publication::lookup_event_metadata(
                        active,
                        &root,
                        &record.source,
                        record.event_id,
                    )?;
                    match records.get(&record.source.identity().digest()) {
                        Some(expected) => {
                            let expected_sha =
                                hex::encode(Sha256::digest(expected.encode_stored()?));
                            assert_eq!(
                                metadata.ok_or("visible event")?.core_record_sha256,
                                expected_sha
                            );
                            let (states, _, terminal) = publication::active_event_page(
                                active,
                                &root,
                                &record.source,
                                None,
                                1,
                                false,
                            )?;
                            assert!(terminal);
                            assert_eq!(states.len(), 1);
                            assert_eq!(states[0].event_id, expected.event_id);
                            assert_eq!(states[0].core_record_sha256, expected_sha);
                        }
                        None => {
                            assert!(metadata.is_none(), "deleted source event remained visible")
                        }
                    }
                }
            }
            let after = ctx_attribution_index::event_index_reader_work_for_test();
            let index_count = SegmentStore::new(&root)
                .load_active()?
                .ok_or("manifest")?
                .segments
                .iter()
                .filter(|segment| segment.role == ctx_attribution_index::EVENT_STATE_INDEX_ROLE)
                .count();
            assert!(index_count >= INDEXES);
            assert_eq!(
                after.0 - before.0,
                index_count as u64,
                "physical reader opens grow with indexes, not sources or queries"
            );
            let base = if overlapping {
                SOURCES as usize * INDEXES
            } else {
                SOURCES as usize
            };
            assert!(
                after.1 - before.1 <= (base + SOURCES as usize * (revision as usize - 2)) as u64,
                "decoder work grows with physical dictionary rows only"
            );
            if revision == 2 {
                assert_eq!(after.1 - before.1, base as u64);
            }
            drop(reopened);
            let remove = records.keys().take(12).copied().collect::<Vec<_>>();
            for source in remove {
                records.remove(&source);
            }
            for record in records.values_mut() {
                // Preserve identities while replacing every surviving record.
                record.content = CoreRecord::new_selected(
                    record.event_id,
                    record.session_id,
                    record.source.clone(),
                    1,
                    "message",
                    "zed-nativepath-source-backed-v3-neutral-core",
                    format!("replacement {revision}"),
                )?
                .content;
            }
            publish_inventory(&root, &records, revision)?;
        }
        // Final publication/reopen and exact receipt no-op are checked by the
        // producer helper as well as the independent proof queries above.
        assert_eq!(publish_inventory(&root, &records, 4)?, (0, 0, 0));
        let changed = records.values_mut().next().ok_or("remaining source")?;
        changed.content = CoreRecord::new_selected(
            changed.event_id,
            changed.session_id,
            changed.source.clone(),
            1,
            "message",
            "zed-nativepath-source-backed-v3-neutral-core",
            "one changed source",
        )?
        .content;
        let prior_indexes = SegmentStore::new(&root)
            .load_active()?
            .ok_or("manifest")?
            .segments
            .iter()
            .filter(|segment| segment.role == ctx_attribution_index::EVENT_STATE_INDEX_ROLE)
            .count();
        let (opens, decodes, writes) = publish_inventory(&root, &records, 5)?;
        assert_eq!(opens, prior_indexes as u64);
        let selected_physical_rows = if overlapping { INDEXES as u64 + 3 } else { 4 };
        assert_eq!(
            decodes, selected_physical_rows,
            "unchanged sources must not enter prior-event decoding"
        );
        assert_eq!(
            writes, selected_physical_rows,
            "unchanged sources must not enter proof scratch"
        );
        assert_eq!(publish_inventory(&root, &records, 5)?, (0, 0, 0));
    }
    Ok(())
}

fn publish_inventory(
    root: &std::path::Path,
    records: &BTreeMap<[u8; 32], CoreRecord>,
    revision: u8,
) -> TestResult<(u64, u64, u64)> {
    use crate::protocol::{CoreEventReplacement, CoreEventTombstone};
    use sha2::{Digest, Sha256};
    let sources = records
        .values()
        .map(|record| CoreSourceState {
            source: record.source.clone(),
            core_record_accumulator: hex::encode(Sha256::digest(record.encode_stored().unwrap())),
            event_count: 1,
        })
        .collect::<Vec<_>>();
    let generation = head(revision, &sources)?;
    let mut materializer = SegmentMaterializer::open(root)?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Current(receipt) => {
            assert_eq!(receipt.source_count as usize, records.len());
            assert_eq!(receipt.event_count as usize, records.len());
            return Ok((0, 0, 0));
        }
        CoreGenerationStart::Started(session) => session,
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        sources.into_iter().map(CoreSourceDelta::Present).collect(),
    ))?)?;
    let before = (
        ctx_attribution_index::event_index_reader_work_for_test().0,
        ctx_attribution_index::event_index_event_decodes_for_test(),
        ctx_attribution_index::materialization::event_proofs::proof_writes_for_test().0,
    );
    session.prepare_prior_event_proofs(None)?;
    let preparation = (
        ctx_attribution_index::event_index_reader_work_for_test().0 - before.0,
        ctx_attribution_index::event_index_event_decodes_for_test() - before.1,
        ctx_attribution_index::materialization::event_proofs::proof_writes_for_test().0 - before.2,
    );
    for reconciliation in reconciliations {
        let source = reconciliation.delta.source();
        let (prior, terminal) = session.event_states(&reconciliation, None)?;
        assert!(terminal);
        let deltas = match (records.get(&source.identity().digest()), prior.as_slice()) {
            (Some(record), []) => vec![CoreEventDelta::Added(record.clone())],
            (Some(record), [prior]) => vec![CoreEventDelta::Replaced(CoreEventReplacement {
                prior_core_record_sha256: prior.core_record_sha256.clone(),
                record: record.clone(),
            })],
            (None, [prior]) => vec![CoreEventDelta::Tombstoned(CoreEventTombstone {
                event_id: prior.event_id,
                prior_core_record_sha256: prior.core_record_sha256.clone(),
            })],
            _ => return Err("unexpected prior records".into()),
        };
        session.ingest_event_pages(vec![CoreEventDeltaPage {
            materialization_id: "0".repeat(64),
            core_generation_id: generation.core_generation_id.clone(),
            reconciliation,
            page_index: 0,
            terminal: true,
            deltas,
        }])?;
    }
    let receipt = session.activate()?;
    assert_eq!(receipt.source_count as usize, records.len());
    assert_eq!(receipt.event_count as usize, records.len());
    drop(materializer);
    let mut reopened = SegmentMaterializer::open(root)?;
    let active = reopened.active.as_mut().ok_or("active")?;
    super::super::publication::prepare_event_proofs(
        active,
        root,
        "inspection",
        &|_, _| Ok(true),
        None,
    )?;
    for record in records.values() {
        let metadata = super::super::publication::lookup_event_metadata(
            active,
            root,
            &record.source,
            record.event_id,
        )?
        .ok_or("event after publication")?;
        assert_eq!(
            metadata.core_record_sha256,
            hex::encode(Sha256::digest(record.encode_stored()?))
        );
    }
    Ok(preparation)
}

#[test]
fn completed_control_accepts_large_event_totals_and_bounds_coverage_by_its_receipt() -> TestResult {
    let mut state = source_state(&one_record()?, 1);
    state.event_count = 220_000_000;
    let head = CoreGenerationHead::new(
        "a".repeat(64),
        1,
        1,
        "b".repeat(64),
        1,
        1,
        "c".repeat(64),
        &[state],
    )
    .unwrap();
    let mut completed = super::super::model::initial_completed_control();
    completed.graph_generation = 1;
    completed.materialization_id = Some("a".repeat(64));
    completed.finish_request_sha256 = Some("b".repeat(64));
    completed.event_count = head.event_count;
    completed.coverage.repository_candidate_events = head.event_count;
    completed.receipt = Some(crate::protocol::CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id.clone(),
        core_record_contract_fingerprint: head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: head.source_snapshot_sha256.clone(),
        materializer_revision: completed.materializer_revision.clone(),
        source_count: head.source_count,
        event_count: head.event_count,
    });
    completed.head = Some(head);
    completed.validate().unwrap();
    completed.coverage.repository_candidate_events += 1;
    assert!(matches!(
        completed.validate(),
        Err(crate::core_materialization::CoreStoreError::Bounds)
    ));
    Ok(())
}
