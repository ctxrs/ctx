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
        usize::try_from(LAYERS * 2)?,
        "each layer should open once per bounded 256-entry frontier, not once per event",
    );
    Ok(())
}
