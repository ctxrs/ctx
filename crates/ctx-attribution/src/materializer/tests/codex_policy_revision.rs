//! Same-Core cache transition; predecessor origin state is explicitly seeded.
use super::*;
use crate::graph::segment::{EventIndexEntry, SegmentManifest, SegmentStore};
use crate::graph::segment_state::SegmentCompletedControl;
use crate::materializer::CoreGenerationStart;
use crate::protocol::CoreMaterializationReceipt;
use sha2::Digest;
use std::path::Path;

fn record(parser_revision: &str) -> TestResult<CoreRecord> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "codex-nativepath-jsonl-v0",
        1,
        SourceAnchor::ProviderNative {
            namespace: "session".to_owned(),
            key: TypedKey::utf8("policy-child")?,
        },
    )?;
    let mut record = CoreRecord::new_selected(
        stable_entity(&source, StableEntityKind::Event, 0x41)?,
        stable_entity(&source, StableEntityKind::Session, 0x31)?,
        source.clone(),
        1,
        "message",
        parser_revision,
        "retained child",
    )?;
    record.provider_session_id = Some("policy-child".to_owned());
    record.native_event_id = Some(TypedKey::utf8("event")?);
    record.parent_session_id = Some(stable_entity(&source, StableEntityKind::Session, 0x30)?);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    record.agent_scope = Some(AgentScope::Subagent);
    record.validate_contract()?;
    Ok(record)
}

fn seed_predecessor(
    root: &Path,
    record: &CoreRecord,
    source: &CoreSourceState,
    head: &CoreGenerationHead,
    predecessor_revision: &str,
    prior_origin: IndexedCoreEventOriginKind,
) -> TestResult {
    drop(SegmentMaterializer::open(root)?);
    let receipt = CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id.clone(),
        core_record_contract_fingerprint: head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: head.source_snapshot_sha256.clone(),
        materializer_revision: predecessor_revision.to_owned(),
        source_count: 1,
        event_count: 1,
    };
    let completed = SegmentCompletedControl::current(
        1,
        1,
        Some(receipt.clone()),
        Some("a".repeat(64)),
        Some(head.clone()),
        None,
        Some("b".repeat(64)),
        predecessor_revision.to_owned(),
        SEGMENT_SCHEMA_IDENTITY.to_owned(),
        crate::graph::GRAPH_SEMANTICS_FINGERPRINT.to_owned(),
        SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        core_record_contract_fingerprint(),
        Default::default(),
        "1".repeat(64),
    );
    let control = super::super::model::SourceStateSegment {
        completed,
        mutations: vec![super::super::model::SourceMutation::Upsert {
            state: source.clone(),
            materializer_revision: predecessor_revision.to_owned(),
        }],
    };
    let control_ref = super::super::publication::write_source_segment_for_test(
        root,
        1,
        super::super::model::MATERIALIZER_SOURCE_ROLE,
        0,
        &control,
    )?;
    let index_source = EventIndexSource::new(record.source.clone())?;
    // Synthetic checked predecessor state: old exact admission was
    // UniqueToSession; a then-unknown newer revision was Unknown. No new capture.
    let index_ref = super::super::publication::write_event_index_segment_for_test(
        root,
        1,
        1,
        vec![index_source.clone()],
        vec![IndexedCoreEventState {
            source_storage_key: index_source.storage_key,
            event_id: record.event_id,
            lineage: IndexedCoreEventLineage {
                session_id: record.session_id,
                parent_session_id: record.parent_session_id,
                root_session_id: record.root_session_id,
                session_relationship: if record.session_relationship.is_some() {
                    SessionRelationshipKind::Delegated
                } else {
                    SessionRelationshipKind::RelatedUnknown
                },
                origin_kind: prior_origin,
                copied_from: record.event_copy.as_ref().map(|copy| {
                    crate::graph::segment::IndexedCopiedEventOrigin {
                        ancestor_session_id: copy.ancestor_session_id,
                        ancestor_event_id: copy.ancestor_event_id,
                        proof: crate::protocol::EventCopyProofKind::NativeEventIdentity,
                    }
                }),
            },
            event_sequence: 1,
            core_record_sha256: hex::encode(sha2::Sha256::digest(record.encode_stored()?)),
            core_record_leaf_sha256: "3".repeat(64),
            flat_record_count: 0,
            event_output_root: "4".repeat(64),
            coverage: Default::default(),
        }],
    )?;
    SegmentStore::new(root).install_manifest_for_test(&SegmentManifest {
        schema_version: crate::graph::segment::MANIFEST_SCHEMA_VERSION,
        generation_id: crate::graph::segment::random_generation_id()?,
        prior_generation_id: None,
        graph_generation: 1,
        materializer_identity: predecessor_revision.to_owned(),
        core_receipt: receipt,
        schema_identity: SEGMENT_SCHEMA_IDENTITY.to_owned(),
        evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
        segments: vec![control_ref, index_ref],
        predecessor_segments: Vec::new(),
    })?;
    Ok(())
}

fn origin(root: &Path, record: &CoreRecord) -> TestResult<IndexedCoreEventOriginKind> {
    let manifest = SegmentStore::new(root)
        .load_active()?
        .ok_or_else(|| io::Error::other("manifest"))?;
    let reference = manifest
        .segments
        .iter()
        .find(|r| r.role == crate::graph::segment::EVENT_STATE_INDEX_ROLE)
        .ok_or_else(|| io::Error::other("event index"))?;
    let mut reader = super::super::publication::open_event_index_for_test(root, reference)?;
    match reader.lookup(
        &EventIndexSource::new(record.source.clone())?,
        record.event_id,
    )? {
        Some(EventIndexEntry::State { state, .. }) => {
            assert_eq!(state.lineage.session_id, record.session_id);
            assert_eq!(state.lineage.parent_session_id, record.parent_session_id);
            assert_eq!(state.lineage.root_session_id, record.root_session_id);
            assert_eq!(
                state
                    .lineage
                    .copied_from
                    .as_ref()
                    .map(|copy| (copy.ancestor_session_id, copy.ancestor_event_id)),
                record
                    .event_copy
                    .as_ref()
                    .map(|copy| (copy.ancestor_session_id, copy.ancestor_event_id)),
            );
            Ok(state.lineage.origin_kind)
        }
        _ => Err(io::Error::other("event missing").into()),
    }
}

fn materialize(
    root: &Path,
    record: &CoreRecord,
    source: &CoreSourceState,
    head: &CoreGenerationHead,
) -> TestResult<CoreMaterializationReceipt> {
    let mut materializer =
        SegmentMaterializer::open_for_revision(root, CORE_MATERIALIZER_REVISION)?;
    let mut session = match materializer.start_core_generation(head.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("old policy must not be current on same Core").into());
        }
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        head.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source.clone())],
    ))?)?;
    assert_eq!(
        reconciliations.len(),
        1,
        "same source accumulator must be rederived after policy change"
    );
    let (states, terminal) = session.event_states(&reconciliations[0], None)?;
    assert!(terminal);
    let delta = match states.as_slice() {
        [] => CoreEventDelta::Added(record.clone()),
        [prior] => {
            assert!(
                prior.requires_replacement,
                "policy revision must rederive unchanged bytes"
            );
            assert_eq!(
                prior.core_record_sha256,
                hex::encode(sha2::Sha256::digest(record.encode_stored()?))
            );
            CoreEventDelta::Replaced(crate::protocol::CoreEventReplacement {
                prior_core_record_sha256: prior.core_record_sha256.clone(),
                record: record.clone(),
            })
        }
        _ => return Err(io::Error::other("unexpected prior event count").into()),
    };
    session.ingest_event_pages(vec![CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: head.core_generation_id.clone(),
        reconciliation: reconciliations[0].clone(),
        page_index: 0,
        terminal: true,
        deltas: vec![delta],
    }])?;
    Ok(session.activate()?)
}

#[test]
fn codex_admission_revision_rederives_cached_same_core_like_cold_and_then_noops() -> TestResult {
    assert_policy_transition(
        "codex-nativepath-core-activity-v10-item-completed-plan",
        "2026.09.04.2+bounded-shell-quotes",
        IndexedCoreEventOriginKind::UniqueToSession,
        IndexedCoreEventOriginKind::Unknown,
    )
}

#[test]
fn codex_v14_admission_rederives_v11_and_v14_under_the_released_v11_policy() -> TestResult {
    for (parser_revision, prior_origin, expected_origin) in [
        (
            "codex-nativepath-core-activity-v11-item-call-identity",
            IndexedCoreEventOriginKind::UniqueToSession,
            IndexedCoreEventOriginKind::UniqueToSession,
        ),
        (
            "codex-nativepath-core-activity-v14-literal-patch-file-facts",
            IndexedCoreEventOriginKind::Unknown,
            IndexedCoreEventOriginKind::UniqueToSession,
        ),
    ] {
        assert_policy_transition(
            parser_revision,
            "2026.09.06.1+codex-item-call-identity",
            prior_origin,
            expected_origin,
        )?;
    }
    Ok(())
}

#[test]
fn codex_v15_admission_replays_abstentions_and_preserves_released_and_copied_core() -> TestResult {
    for revision in [
        "codex-nativepath-core-activity-v11-item-call-identity",
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "codex-nativepath-core-activity-v15-revert-lineage",
    ] {
        let record = record(revision)?;
        let prior = if revision == "codex-nativepath-core-activity-v15-revert-lineage" {
            IndexedCoreEventOriginKind::Unknown
        } else {
            IndexedCoreEventOriginKind::UniqueToSession
        };
        assert_record_transition(
            record.clone(),
            "2026.09.15.2+native-provider-compatibility",
            prior,
            IndexedCoreEventOriginKind::UniqueToSession,
        )?;
        let mut copied = record.clone();
        copied.event_copy = Some(crate::protocol::ProviderNativeEventCopy {
            ancestor_session_id: record.parent_session_id.unwrap(),
            ancestor_event_id: stable_entity(&record.source, StableEntityKind::Event, 0x42)?,
            proof: crate::protocol::ProviderNativeCopyProof::NativeEventIdentity,
        });
        assert_record_transition(
            copied,
            "2026.09.15.2+native-provider-compatibility",
            IndexedCoreEventOriginKind::CopiedFromAncestor,
            IndexedCoreEventOriginKind::CopiedFromAncestor,
        )?;
        let mut unknown = record;
        unknown.parser_revision.push_str("-unknown");
        assert_record_transition(
            unknown,
            "2026.09.15.2+native-provider-compatibility",
            IndexedCoreEventOriginKind::Unknown,
            IndexedCoreEventOriginKind::Unknown,
        )?;
    }
    Ok(())
}

fn assert_policy_transition(
    parser_revision: &str,
    predecessor_revision: &str,
    prior_origin: IndexedCoreEventOriginKind,
    expected_origin: IndexedCoreEventOriginKind,
) -> TestResult {
    let record = record(parser_revision)?;
    assert_record_transition(record, predecessor_revision, prior_origin, expected_origin)
}

pub(super) fn assert_record_transition(
    record: CoreRecord,
    predecessor_revision: &str,
    prior_origin: IndexedCoreEventOriginKind,
    expected_origin: IndexedCoreEventOriginKind,
) -> TestResult {
    let temporary = tempfile::tempdir()?;
    let original = record.encode_stored()?;
    assert_eq!(
        crate::core_materialization::producer_authority_disposition(&record)
            .permits_positive_authority(),
        expected_origin == IndexedCoreEventOriginKind::UniqueToSession
    );
    let source = source_state(&record, 0x61);
    let generation = head(0x61, std::slice::from_ref(&source))?;
    let warm = temporary.path().join("warm");
    let cold = temporary.path().join("cold");
    seed_predecessor(
        &warm,
        &record,
        &source,
        &generation,
        predecessor_revision,
        prior_origin,
    )?;
    assert_eq!(origin(&warm, &record)?, prior_origin);
    let warm_receipt = materialize(&warm, &record, &source, &generation)?;
    let cold_receipt = materialize(&cold, &record, &source, &generation)?;
    assert_eq!(warm_receipt, cold_receipt);
    assert_eq!(
        warm_receipt.core_generation_id,
        generation.core_generation_id
    );
    assert_eq!(
        warm_receipt.materializer_revision,
        CORE_MATERIALIZER_REVISION
    );
    assert_eq!(origin(&warm, &record)?, expected_origin);
    assert_eq!(origin(&warm, &record)?, origin(&cold, &record)?);
    assert_eq!(original, record.encode_stored()?);
    for root in [&warm, &cold] {
        let before = SegmentStore::new(root).load_active()?.unwrap();
        let mut reopened =
            SegmentMaterializer::open_for_revision(root, CORE_MATERIALIZER_REVISION)?;
        assert!(
            matches!(reopened.start_core_generation(generation.clone())?, CoreGenerationStart::Current(receipt) if receipt == warm_receipt)
        );
        drop(reopened);
        assert_eq!(
            before.generation_id,
            SegmentStore::new(root)
                .load_active()?
                .unwrap()
                .generation_id
        );
    }
    Ok(())
}
