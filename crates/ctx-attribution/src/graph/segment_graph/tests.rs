use std::collections::BTreeMap;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

use sha2::{Digest as _, Sha256};

use super::*;
use crate::envelope::{
    Confidence as EnvelopeConfidence, Fact as EnvelopeFact, FactState as EnvelopeFactState,
    ResourceRef,
};
use crate::graph::git::{CertifiedLiveRoot, GitBlameAuthority, RepositoryWorktreeIdentity};
use crate::graph::segment::{
    EventTombstone, FILE_TOUCHED, FLAT_SERVING_ROLE, FORGE_CREATE,
    FORGE_PULL_REQUEST_CONTAINS_COMMIT, FactFamily, FlatSegmentWriter, FlatStore,
    GIT_COMMIT_PRODUCED, GIT_COMMIT_REFERENCED, MANIFEST_SCHEMA_VERSION, REPOSITORY_LIVE_ACCESS,
    SegmentManifest, SegmentRef, SegmentStore, SegmentWriter, ServingRecord, ServingResource,
    project_core_batch, random_generation_id, segment_file_name,
};
use crate::ingest::{
    CoreProjectionCoverage, PreparedCoreEvidence, PreparedCoreProjectionBatch, PreparedCoreUnit,
    ProducerAuthorityDisposition,
};
use crate::protocol::{
    CoreMaterializationReceipt, CoreSourceState, EvidenceCitation, IDENTITY_VERSION, ResourceKind,
    SourceKey, StableEntityId, StableEntityKind, core_record_contract_fingerprint,
};
use crate::query::{
    AttributionOutcome, BlameFactFamily, BlameGraph, Citation, Fact, MAX_ATTRIBUTION_CANDIDATES,
    QueryError, ResourceSelector, attribution_outcome,
};

pub(super) const REPOSITORY: &str = "forge:github.com/ctxrs/ctx";
const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";
const REFERENCED_COMMIT: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
const PULL_REQUEST: &str = "github.com/ctxrs/ctx/pull_request/7";
const WORKTREE: &str = "worktree-1";
const CHUNK_BYTES: u32 = ctx_attribution_index::SEGMENT_CHUNK_BYTES;
mod cursor_targets;
#[path = "lookup_tests.rs"]
mod lookup_tests;
mod pinned_generation_tests;

fn golden_record() -> crate::protocol::CoreRecord {
    crate::test_support::core_record()
}

fn stable_entity(source: &SourceKey, kind: StableEntityKind, byte: u8) -> StableEntityId {
    let mut uuid_bytes = [byte; 16];
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let identity: StableEntityId = serde_json::from_value(serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": kind,
        "digest": vec![byte; 32],
        "source_digest": source.identity().digest(),
        "source_descriptor_digest": source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))
    .expect("stable entity");
    identity.validate_contract().expect("identity contract");
    identity
}

fn indexed_event(source: &SourceKey, index: usize) -> StableEntityId {
    let digest: [u8; 32] = Sha256::digest(format!("segment-test-event-{index}")).into();
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let identity: StableEntityId = serde_json::from_value(serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": StableEntityKind::Event,
        "digest": digest,
        "source_digest": source.identity().digest(),
        "source_descriptor_digest": source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))
    .expect("indexed event identity");
    identity.validate_contract().expect("identity contract");
    identity
}

pub(super) fn records(count: usize) -> Vec<crate::protocol::CoreRecord> {
    let template = golden_record();
    let source = template.source.clone();
    let session = stable_entity(&source, StableEntityKind::Session, 0x31);
    let mut records = (0..count)
        .map(|index| {
            let mut record = template.clone();
            record.event_id = indexed_event(&source, index);
            record.session_id = session;
            record.parent_session_id = None;
            record.root_session_id = Some(session);
            record
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_id.digest());
    for (index, record) in records.iter_mut().enumerate() {
        record.event_sequence = u64::try_from(index + 1).expect("bounded fixture");
        record.occurred_at_unix_ms =
            Some(1_700_000_000_000 + i64::try_from(index).expect("bounded fixture timestamp"));
        record.validate_contract().expect("Core record contract");
    }
    records
}

#[allow(clippy::too_many_arguments)]
pub(super) fn fact(
    record: &crate::protocol::CoreRecord,
    fact_type: &str,
    subject: ResourceRef,
    predicate: &str,
    object: Option<ResourceRef>,
    occurred_at: Option<i64>,
    confidence: EnvelopeConfidence,
    state: EnvelopeFactState,
    mut attributes: BTreeMap<String, String>,
) -> EnvelopeFact {
    attributes
        .entry("observation_origin".to_owned())
        .or_insert_with(|| "direct".to_owned());
    EnvelopeFact::create(
        fact_type,
        subject,
        predicate,
        object,
        occurred_at.map(|value| value.to_string()),
        confidence,
        state,
        "test.detector",
        "7",
        record.session_id.to_string(),
        record.root_session_id.map(|root| root.to_string()),
        Vec::new(),
        attributes,
    )
}

pub(super) fn batch(
    records: &[crate::protocol::CoreRecord],
    facts: Vec<Vec<EnvelopeFact>>,
) -> PreparedCoreProjectionBatch {
    let generation = "a".repeat(64);
    let source = CoreSourceState {
        source: records[0].source.clone(),
        core_record_accumulator: "b".repeat(64),
        event_count: u64::try_from(records.len()).expect("small fixture"),
    };
    PreparedCoreProjectionBatch {
        core_generation_id: generation.clone(),
        source,
        units: records
            .iter()
            .zip(facts)
            .enumerate()
            .map(|(index, (record, facts))| PreparedCoreUnit {
                origin_event_id: record.event_id.to_string(),
                producer_authority_disposition: ProducerAuthorityDisposition::EligibleUnique,
                stable_entities: [
                    Some(record.event_id),
                    Some(record.session_id),
                    record.root_session_id,
                ]
                .into_iter()
                .flatten()
                .collect(),
                facts,
                evidence: Some(PreparedCoreEvidence {
                    citation: EvidenceCitation {
                        core_generation_id: generation.clone(),
                        source: record.source.clone(),
                        session_id: record.session_id,
                        event_id: record.event_id,
                        event_sequence: record.event_sequence,
                        byte_range: None,
                        evidence_sha256: Some(hex::encode(Sha256::digest(format!(
                            "segment-test-evidence-{index}"
                        )))),
                    },
                }),
                coverage: CoreProjectionCoverage::default(),
            })
            .collect(),
    }
}

fn representative_projection() -> (
    Vec<crate::protocol::CoreRecord>,
    PreparedCoreProjectionBatch,
    Vec<ServingRecord>,
) {
    let records = records(8);
    let session = || ResourceRef::new(ResourceKind::Session, records[0].session_id.to_string());
    let touched = |record: &crate::protocol::CoreRecord| {
        fact(
            record,
            FILE_TOUCHED,
            ResourceRef::in_repository(ResourceKind::File, "src/lib.rs", REPOSITORY),
            "touched_by",
            Some(session()),
            Some(1_700_000_000_100),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )
    };
    let produced = |record: &crate::protocol::CoreRecord| {
        fact(
            record,
            GIT_COMMIT_PRODUCED,
            ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
            "produced_by",
            Some(session()),
            Some(1_700_000_001_000),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )
    };
    let facts = vec![
        vec![touched(&records[0])],
        vec![touched(&records[1])],
        vec![produced(&records[2])],
        vec![produced(&records[3])],
        vec![fact(
            &records[4],
            FORGE_CREATE,
            ResourceRef::in_repository(ResourceKind::PullRequest, PULL_REQUEST, REPOSITORY),
            "created_by",
            Some(session()),
            Some(1_700_000_002_000),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )],
        vec![fact(
            &records[5],
            FORGE_PULL_REQUEST_CONTAINS_COMMIT,
            ResourceRef::in_repository(ResourceKind::PullRequest, PULL_REQUEST, REPOSITORY),
            "contains_commit",
            Some(ResourceRef::in_repository(
                ResourceKind::Commit,
                COMMIT,
                REPOSITORY,
            )),
            Some(1_700_000_002_100),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )],
        vec![fact(
            &records[6],
            GIT_COMMIT_REFERENCED,
            ResourceRef::in_repository(ResourceKind::Commit, REFERENCED_COMMIT, REPOSITORY),
            "referenced_by",
            Some(session()),
            Some(1_700_000_003_000),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Ambiguous,
            BTreeMap::from([
                ("observation_origin".to_owned(), "copied".to_owned()),
                ("origin_event_id".to_owned(), "origin-event".to_owned()),
                ("origin_session_id".to_owned(), "origin-session".to_owned()),
            ]),
        )],
        vec![live_root_fact(
            &records[7],
            "/tmp/ctx-fixture",
            '4',
            1_700_000_004_000,
        )],
    ];
    let batch = batch(&records, facts);
    let projected = project_core_batch(&batch).expect("serving projection");
    (records, batch, projected.records)
}

fn record_local_attribution_projection() -> (Vec<ServingRecord>, ResourceId, ResourceId, ResourceId)
{
    let mut records = records(1);
    let source = records[0].source.clone();
    let worker = records[0].session_id;
    let manager = stable_entity(&source, StableEntityKind::Session, 0x32);
    let root = stable_entity(&source, StableEntityKind::Session, 0x33);

    records[0].parent_session_id = Some(manager);
    records[0].root_session_id = Some(root);
    records[0].session_relationship = None;
    records[0]
        .validate_contract()
        .expect("record-local Core lineage");

    let session = |identity: StableEntityId| {
        ResourceRef::in_repository(ResourceKind::Session, identity.to_string(), REPOSITORY)
    };
    let prepared = batch(
        &records,
        vec![vec![fact(
            &records[0],
            GIT_COMMIT_PRODUCED,
            ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
            "produced_by",
            Some(session(worker)),
            Some(1_700_000_001_000),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )]],
    );
    let projected = project_core_batch(&prepared).expect("record-local attribution projection");
    let produced = projected
        .records
        .iter()
        .find(|record| record.fact_family.as_str() == GIT_COMMIT_PRODUCED)
        .expect("produced commit attribution record");
    let commit = ResourceId(produced.subject.graph_id().expect("commit graph ID"));
    let producing_session = produced
        .object
        .as_ref()
        .and_then(|object| object.graph_id().ok())
        .map(ResourceId)
        .expect("direct producing session graph ID");
    let owning_root = produced
        .scope
        .as_ref()
        .and_then(|scope| scope.graph_id().ok())
        .map(ResourceId)
        .expect("record-local root run graph ID");
    (projected.records, commit, producing_session, owning_root)
}

fn referenced_commit_projection(
    count: usize,
    distinct_facts: bool,
) -> (
    Vec<crate::protocol::CoreRecord>,
    PreparedCoreProjectionBatch,
    Vec<ServingRecord>,
) {
    let records = records(count);
    let session = || ResourceRef::new(ResourceKind::Session, records[0].session_id.to_string());
    let facts = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            vec![fact(
                record,
                GIT_COMMIT_REFERENCED,
                ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
                "referenced_by",
                Some(session()),
                Some(if distinct_facts {
                    1_700_100_000_000 + i64::try_from(index).expect("bounded fixture")
                } else {
                    1_700_100_000_000
                }),
                EnvelopeConfidence::Verified,
                EnvelopeFactState::Asserted,
                BTreeMap::new(),
            )]
        })
        .collect::<Vec<_>>();
    let batch = batch(&records, facts);
    let projected = project_core_batch(&batch).expect("referenced commit projection");
    (records, batch, projected.records)
}

pub(super) fn synthetic_receipt() -> CoreMaterializationReceipt {
    CoreMaterializationReceipt {
        core_generation_id: "a".repeat(64),
        core_record_contract_fingerprint: core_record_contract_fingerprint(),
        source_snapshot_sha256: "c".repeat(64),
        materializer_revision: crate::test_support::TEST_MATERIALIZER_REVISION.to_owned(),
        source_count: 1,
        event_count: 1,
    }
}

fn write_segment(
    root: &Path,
    generation_byte: u8,
    records: Vec<ServingRecord>,
    tombstones: Vec<EventTombstone>,
) -> SegmentRef {
    let generation = [generation_byte; 32];
    let generation_id = hex::encode(generation);
    let file_name = segment_file_name(&generation_id, FLAT_SERVING_ROLE);
    let path = root.join(&file_name);
    let writer = SegmentWriter::create(&path, generation, FLAT_SERVING_ROLE, CHUNK_BYTES)
        .expect("segment writer");
    let stats = FlatSegmentWriter::write(writer, records, tombstones).expect("Flat segment");
    let file_sha256 = hex::encode(Sha256::digest(std::fs::read(&path).expect("segment bytes")));
    SegmentRef {
        ordinal: 0,
        publication_generation: 1,
        generation_id,
        role: FLAT_SERVING_ROLE,
        file_name,
        plaintext_bytes: stats.plaintext_bytes,
        file_sha256,
    }
}

fn publish(
    root: &Path,
    receipt: CoreMaterializationReceipt,
    mut segments: Vec<SegmentRef>,
    identity_override: Option<(&str, &str, &str)>,
) {
    ctx_history_platform::platform_security::establish_private_data_root(root)
        .expect("private segment fixture root");
    for (ordinal, segment) in segments.iter_mut().enumerate() {
        segment.ordinal = u32::try_from(ordinal).expect("small fixture");
    }
    let store = SegmentStore::new(root);
    let active = store.load_active().expect("load active manifest");
    let graph_generation = active
        .as_ref()
        .map_or(Some(1), |manifest| manifest.graph_generation.checked_add(1))
        .expect("graph generation");
    let (schema_identity, evidence_identity, ordering_identity) = identity_override.unwrap_or((
        SEGMENT_SCHEMA_IDENTITY,
        SEGMENT_EVIDENCE_IDENTITY,
        SEGMENT_ORDERING_IDENTITY,
    ));
    let manifest = SegmentManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generation_id: random_generation_id().expect("manifest generation"),
        prior_generation_id: active
            .as_ref()
            .map(|manifest| manifest.generation_id.clone()),
        graph_generation,
        materializer_identity: receipt.materializer_revision.clone(),
        core_receipt: receipt,
        schema_identity: schema_identity.to_owned(),
        evidence_identity: evidence_identity.to_owned(),
        ordering_identity: ordering_identity.to_owned(),
        segments,
        predecessor_segments: active.map_or_else(Vec::new, |manifest| manifest.segments),
    };
    let candidate = store.stage_manifest(&manifest).expect("stage manifest");
    store
        .publish_candidate(candidate)
        .expect("publish manifest");
}

fn publish_synthetic(root: &Path, segments: Vec<SegmentRef>) {
    publish(root, synthetic_receipt(), segments, None);
}

fn pin_graph(root: &Path) -> SegmentGraph {
    let pinned = FlatStore::new(root)
        .open_active(SegmentGraph::flat_open_policy())
        .expect("pin active Flat generation");
    SegmentGraph::from_pinned(pinned, None)
}

#[cfg(unix)]
fn assert_pin_rejected(root: &Path) {
    assert!(
        FlatStore::new(root)
            .open_active(SegmentGraph::flat_open_policy())
            .is_err()
    );
}

pub(super) fn open_graph(
    root: &Path,
    receipt: CoreMaterializationReceipt,
    records: Vec<ServingRecord>,
) -> SegmentGraph {
    let segment = write_segment(root, 0x51, records, Vec::new());
    publish(root, receipt, vec![segment], None);
    pin_graph(root)
}

fn normalize_fact(mut fact: Fact) -> Fact {
    fact.citations
        .sort_by_key(|citation| citation.evidence().event_sequence);
    fact
}

fn served_commit_facts(graph: &SegmentGraph, target: &ResourceId) -> Vec<Fact> {
    BlameGraph::blame_facts_page(&graph, target, BlameFactFamily::Commit, None, 100)
        .expect("served commit facts")
        .items
        .into_iter()
        .map(|(fact, _)| fact)
        .collect()
}

#[test]
fn checked_in_segment_contract_fingerprints_recompute() {
    assert_eq!(
        SEGMENT_SCHEMA_IDENTITY,
        format!(
            "sha256:{}",
            hex::encode(Sha256::digest(SEGMENT_SCHEMA_CONTRACT))
        )
    );
    assert_eq!(
        SEGMENT_ORDERING_IDENTITY,
        format!(
            "sha256:{}",
            hex::encode(Sha256::digest(SEGMENT_ORDERING_CONTRACT))
        )
    );
    assert_eq!(crate::query::BLAME_ORDERING_VERSION, 1);
}

#[test]
fn plain_flat_attribution_uses_only_record_local_session_identity() {
    let (records, commit, expected_session, expected_root) = record_local_attribution_projection();
    assert_eq!(records.len(), 1);
    let directory = tempfile::tempdir().expect("plain lineage segment directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), records);

    let attributions =
        BlameGraph::production_attribution(&&graph, std::slice::from_ref(&commit), 100)
            .expect("plain Flat record-local attribution");
    assert_eq!(attributions.len(), 1);
    assert_eq!(attributions[0].producing_session, expected_session);
    assert_eq!(attributions[0].parent_session, None);
    assert_eq!(attributions[0].root_run.as_ref(), Some(&expected_root));
    assert_eq!(attributions[0].fact_occurred_at_ms, Some(1_700_000_001_000));
}

#[test]
fn plain_flat_conflicting_producers_return_five_exact_ordered_candidates() {
    let mut core_records = records(7);
    let source = core_records[0].source.clone();
    for (index, record) in core_records.iter_mut().enumerate() {
        let byte = 0x40_u8
            .checked_add(u8::try_from(index).expect("small producer fixture"))
            .expect("producer identity byte");
        let session = stable_entity(&source, StableEntityKind::Session, byte);
        record.session_id = session;
        record.parent_session_id = None;
        record.root_session_id = Some(session);
        record.validate_contract().expect("producer Core record");
    }
    let facts = core_records
        .iter()
        .map(|record| {
            vec![fact(
                record,
                GIT_COMMIT_PRODUCED,
                ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
                "produced_by",
                Some(ResourceRef::new(
                    ResourceKind::Session,
                    record.session_id.to_string(),
                )),
                record.occurred_at_unix_ms,
                EnvelopeConfidence::Verified,
                EnvelopeFactState::Asserted,
                BTreeMap::new(),
            )]
        })
        .collect::<Vec<_>>();
    let projected =
        project_core_batch(&batch(&core_records, facts)).expect("conflicting producer projection");
    let commit = projected
        .records
        .iter()
        .find(|record| record.fact_family.as_str() == GIT_COMMIT_PRODUCED)
        .and_then(|record| record.subject.graph_id().ok())
        .map(ResourceId)
        .expect("conflicting producer commit ID");
    let directory = tempfile::tempdir().expect("conflicting producer segment directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), projected.records);

    let attributions = BlameGraph::production_attribution(&&graph, &[commit], 100)
        .expect("conflicting producer candidates");
    assert_eq!(
        attribution_outcome(&attributions),
        AttributionOutcome::Conflicting
    );
    assert_eq!(attributions.len(), MAX_ATTRIBUTION_CANDIDATES);
    assert!(attributions.windows(2).all(|pair| {
        (pair[0].producing_session.clone(), pair[0].fact_id.clone())
            < (pair[1].producing_session.clone(), pair[1].fact_id.clone())
    }));
    assert!(attributions.iter().all(|candidate| {
        !candidate.citations.is_empty() && candidate.citations.iter().all(Citation::is_exact)
    }));
}

#[test]
fn operation_identity_survives_projection_serving_owner_dedup_and_tombstones() {
    let records = records(2);
    let session = || ResourceRef::new(ResourceKind::Session, records[0].session_id.to_string());
    let operation = |record: &crate::protocol::CoreRecord, occurred_at, identity: char| {
        fact(
            record,
            GIT_COMMIT_PRODUCED,
            ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
            "produced_by",
            Some(session()),
            Some(occurred_at),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )
        .with_operation_identity_sha256(identity.to_string().repeat(64))
    };
    let repeated = operation(&records[0], 1_700_200_000_000, '1');
    let distinct_operation = operation(&records[0], 1_700_200_000_000, '2');
    let distinct_timestamp = operation(&records[0], 1_700_200_000_001, '1');
    let expected_ids = [
        repeated.fact_id.clone(),
        distinct_operation.fact_id.clone(),
        distinct_timestamp.fact_id.clone(),
    ]
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(expected_ids.len(), 3);

    let prepared = batch(
        &records,
        vec![
            vec![
                repeated.clone(),
                repeated.clone(),
                distinct_operation,
                distinct_timestamp,
            ],
            vec![repeated.clone()],
        ],
    );
    let projected = project_core_batch(&prepared).expect("operation-aware projection");
    assert_eq!(projected.records.len(), 5);
    assert_eq!(
        projected
            .records
            .iter()
            .map(|record| record.record_id.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        expected_ids
    );
    let removed_owner = projected
        .owners
        .iter()
        .find(|owner| owner.event_id == records[0].event_id.to_string())
        .expect("first event owner")
        .clone();
    let target = ResourceId(
        projected.records[0]
            .subject
            .graph_id()
            .expect("commit graph ID"),
    );

    let directory = tempfile::tempdir().expect("operation identity segment directory");

    let oldest = write_segment(directory.path(), 0x75, projected.records, Vec::new());
    publish(
        directory.path(),
        synthetic_receipt(),
        vec![oldest.clone()],
        None,
    );

    for _ in 0..2 {
        let graph =
            SegmentGraph::open(directory.path(), None).expect("reopen operation identity graph");
        let served = served_commit_facts(&graph, &target);
        assert_eq!(served.len(), 3);
        assert_eq!(
            served
                .iter()
                .map(|fact| fact.id.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            expected_ids
        );
        assert_eq!(
            served
                .iter()
                .find(|fact| fact.id == repeated.fact_id)
                .expect("repeated logical operation")
                .citations
                .len(),
            2
        );
    }

    let mut tombstone = write_segment(
        directory.path(),
        0x76,
        Vec::new(),
        vec![removed_owner.tombstone()],
    );
    tombstone.publication_generation = 2;
    publish(
        directory.path(),
        synthetic_receipt(),
        vec![tombstone, oldest],
        None,
    );
    let graph = SegmentGraph::open(directory.path(), None).expect("reopen tombstoned graph");
    let served = served_commit_facts(&graph, &target);
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].id, repeated.fact_id);
    assert_eq!(served[0].citations.len(), 1);
    assert_eq!(
        served[0].citations[0].evidence().event_id,
        records[1].event_id
    );
}

#[test]
fn physical_256_and_257_observations_deduplicate_to_one_logical_fact() {
    for count in [256, 257] {
        let (_records, _batch, projected) = referenced_commit_projection(count, false);
        let target = ResourceId(projected[0].subject.graph_id().expect("commit graph ID"));
        let directory = tempfile::tempdir().expect("segment directory");

        let graph = open_graph(directory.path(), synthetic_receipt(), projected);
        let page =
            BlameGraph::blame_facts_page(&&graph, &target, BlameFactFamily::Commit, None, 500)
                .expect("deduplicated logical page");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].0.citations.len(), 32);
        assert!(page.next_cursor.is_none());
    }
}

#[test]
fn sixteen_replacement_layers_short_circuit_and_seventeen_fail_closed() {
    const MAX_LAYERS: u8 = 16;

    let (_records, _batch, projected) = referenced_commit_projection(256, false);
    let tombstones = projected
        .iter()
        .map(|record| record.event_owner.tombstone())
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("query-work directory");

    let receipt = synthetic_receipt();
    let mut references = Vec::new();
    for generation in 1..=MAX_LAYERS {
        let mut segment = write_segment(
            directory.path(),
            0x80_u8.checked_add(generation).expect("generation byte"),
            projected.clone(),
            if generation == MAX_LAYERS {
                tombstones.clone()
            } else {
                Vec::new()
            },
        );
        segment.publication_generation = u64::from(generation);
        references.insert(0, segment);
        publish(directory.path(), receipt.clone(), references.clone(), None);
    }
    let graph = SegmentGraph::open(directory.path(), None).expect("bounded segment graph");

    let records = graph
        .merged_records(
            GIT_COMMIT_REFERENCED,
            super::merge::Lookup::Exact {
                repository: None,
                term: COMMIT,
            },
        )
        .expect("shadowed replacement layers remain within query work");
    assert_eq!(records.len(), 256);

    let mut seventeenth = write_segment(
        directory.path(),
        0x80_u8
            .checked_add(MAX_LAYERS + 1)
            .expect("generation byte"),
        projected,
        Vec::new(),
    );
    seventeenth.publication_generation = u64::from(MAX_LAYERS + 1);
    references.insert(0, seventeenth);
    publish(directory.path(), receipt, references, None);
    assert!(matches!(
        SegmentGraph::open(directory.path(), None),
        Err(SegmentGraphError::Corrupt("Flat layer bound"))
    ));
}

#[test]
fn duplicate_citations_split_across_segments_are_not_dropped() {
    let (_records, _batch, projected) = referenced_commit_projection(257, false);
    let target = ResourceId(projected[0].subject.graph_id().expect("commit graph ID"));
    let expected_directory = tempfile::tempdir().expect("expected segment directory");
    let split_directory = tempfile::tempdir().expect("split segment directory");

    let expected = open_graph(
        expected_directory.path(),
        synthetic_receipt(),
        projected.clone(),
    );

    let mut oldest_records = projected;
    let newest_records = oldest_records.split_off(128);
    let mut newest = write_segment(split_directory.path(), 0x6c, newest_records, Vec::new());
    newest.publication_generation = 2;
    let oldest = write_segment(split_directory.path(), 0x6d, oldest_records, Vec::new());
    publish(
        split_directory.path(),
        synthetic_receipt(),
        Vec::new(),
        None,
    );
    publish(
        split_directory.path(),
        synthetic_receipt(),
        vec![newest, oldest],
        None,
    );
    let split = SegmentGraph::open(split_directory.path(), None).expect("split segment graph");
    let page = |graph: &SegmentGraph| {
        BlameGraph::blame_facts_page(&graph, &target, BlameFactFamily::Commit, None, 500)
            .expect("duplicate citation page")
    };
    assert_eq!(
        normalize_fact(page(&split).items.remove(0).0),
        normalize_fact(page(&expected).items.remove(0).0)
    );
}

#[test]
fn current_records_precede_same_segment_tombstones_when_merging() {
    let records = records(1);
    let old_batch = batch(
        &records,
        vec![vec![fact(
            &records[0],
            FILE_TOUCHED,
            ResourceRef::in_repository(ResourceKind::File, "src/old.rs", REPOSITORY),
            "touched_by",
            None,
            Some(1),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )]],
    );
    let new_batch = batch(
        &records,
        vec![vec![fact(
            &records[0],
            FILE_TOUCHED,
            ResourceRef::in_repository(ResourceKind::File, "src/new.rs", REPOSITORY),
            "touched_by",
            None,
            Some(2),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )]],
    );
    let old = project_core_batch(&old_batch).expect("old projection");
    let new = project_core_batch(&new_batch).expect("new projection");
    let tombstone = new.owners[0].tombstone();
    let directory = tempfile::tempdir().expect("segment directory");

    let mut newest = write_segment(directory.path(), 0x61, new.records, vec![tombstone]);
    newest.publication_generation = 2;
    let oldest = write_segment(directory.path(), 0x62, old.records, Vec::new());
    publish(directory.path(), synthetic_receipt(), Vec::new(), None);
    publish(
        directory.path(),
        synthetic_receipt(),
        vec![newest, oldest],
        None,
    );
    let graph = SegmentGraph::open(directory.path(), None).expect("segment graph");
    let resolve = |path: &str| {
        BlameGraph::resolve(
            &&graph,
            &ResourceSelector {
                kind: ResourceKind::File,
                value: path.to_owned(),
                repository: Some(REPOSITORY.to_owned()),
            },
            2,
        )
        .expect("resolve file")
    };
    assert_eq!(resolve("src/new.rs").len(), 1);
    assert!(resolve("src/old.rs").is_empty());
}

#[test]
fn same_layer_tombstone_chunk_does_not_hide_replacement_in_another_chunk() {
    let records = records(1);
    let old_batch = batch(
        &records,
        vec![vec![fact(
            &records[0],
            FILE_TOUCHED,
            ResourceRef::in_repository(ResourceKind::File, "src/old-split.rs", REPOSITORY),
            "touched_by",
            None,
            Some(1),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )]],
    );
    let new_batch = batch(
        &records,
        vec![vec![fact(
            &records[0],
            FILE_TOUCHED,
            ResourceRef::in_repository(ResourceKind::File, "src/new-split.rs", REPOSITORY),
            "touched_by",
            None,
            Some(2),
            EnvelopeConfidence::Verified,
            EnvelopeFactState::Asserted,
            BTreeMap::new(),
        )]],
    );
    let old = project_core_batch(&old_batch).expect("old split projection");
    let new = project_core_batch(&new_batch).expect("new split projection");
    let tombstone = new.owners[0].tombstone();
    let directory = tempfile::tempdir().expect("segment directory");

    let mut tombstone_chunk = write_segment(directory.path(), 0x6f, Vec::new(), vec![tombstone]);
    tombstone_chunk.publication_generation = 2;
    let mut replacement_chunk = write_segment(directory.path(), 0x70, new.records, Vec::new());
    replacement_chunk.publication_generation = 2;
    let old_chunk = write_segment(directory.path(), 0x71, old.records, Vec::new());
    publish(directory.path(), synthetic_receipt(), Vec::new(), None);
    publish(
        directory.path(),
        synthetic_receipt(),
        vec![tombstone_chunk, replacement_chunk, old_chunk],
        None,
    );
    let graph = SegmentGraph::open(directory.path(), None).expect("layered segment graph");
    let resolve = |path: &str| {
        BlameGraph::resolve(
            &&graph,
            &ResourceSelector {
                kind: ResourceKind::File,
                value: path.to_owned(),
                repository: Some(REPOSITORY.to_owned()),
            },
            2,
        )
        .expect("resolve split file")
    };
    assert_eq!(resolve("src/new-split.rs").len(), 1);
    assert!(resolve("src/old-split.rs").is_empty());
}

#[test]
fn identity_and_checked_file_corruption_fail_closed() {
    let (_, _, projected) = representative_projection();
    let receipt = synthetic_receipt();

    let wrong = tempfile::tempdir().expect("wrong identity directory");
    let segment = write_segment(wrong.path(), 0x71, projected.clone(), Vec::new());
    publish(
        wrong.path(),
        receipt.clone(),
        vec![segment],
        Some((
            // Pre-exact-operation-lineage Flat manifests must fail closed
            // rather than reinterpret their prior authority semantics.
            "sha256:4989bd0320bc9a913253d803f4796b4555126586d13728fd7259937df15c2f6d",
            SEGMENT_EVIDENCE_IDENTITY,
            SEGMENT_ORDERING_IDENTITY,
        )),
    );
    assert!(matches!(
        SegmentGraph::open(wrong.path(), None),
        Err(SegmentGraphError::Identity("schema"))
    ));

    let corrupt_segment = tempfile::tempdir().expect("corrupt segment directory");
    let segment = write_segment(corrupt_segment.path(), 0x72, projected.clone(), Vec::new());
    let segment_path = corrupt_segment.path().join(&segment.file_name);
    publish(corrupt_segment.path(), receipt.clone(), vec![segment], None);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(segment_path)
        .expect("open segment for corruption");
    file.seek(SeekFrom::Start(80)).expect("seek segment");
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)
        .expect("read segment segment_bytes");
    byte[0] ^= 0x80;
    file.seek(SeekFrom::Start(80)).expect("rewind segment");
    file.write_all(&byte).expect("corrupt segment");
    drop(file);
    // This corruption lands in the first checked data chunk, which is
    // consumed while opening the Flat header. Unrelated chunks are verified
    // lazily when a bounded query reads them.
    let Err(segment_error) = SegmentGraph::open(corrupt_segment.path(), None) else {
        panic!("corrupt segment opened");
    };
    assert!(
        !segment_error
            .to_string()
            .contains("changed repeatedly while opening")
    );

    let corrupt_manifest = tempfile::tempdir().expect("corrupt manifest directory");
    let segment = write_segment(corrupt_manifest.path(), 0x73, projected, Vec::new());
    publish(corrupt_manifest.path(), receipt, vec![segment], None);
    let manifest_path = SegmentStore::new(corrupt_manifest.path()).active_manifest_path();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(manifest_path)
        .expect("open manifest for corruption");
    file.seek(SeekFrom::Start(120)).expect("seek manifest");
    file.write_all(&[0xff]).expect("corrupt manifest");
    drop(file);
    let Err(manifest_error) = SegmentGraph::open(corrupt_manifest.path(), None) else {
        panic!("corrupt manifest opened");
    };
    assert!(
        !manifest_error
            .to_string()
            .contains("changed repeatedly while opening")
    );
}

#[test]
fn unopened_segment_data_is_checked_on_the_bounded_query_that_reads_it() {
    let (_, _, projected) = referenced_commit_projection(1_000, true);
    let directory = tempfile::tempdir().expect("lazy authentication directory");

    let segment = write_segment(directory.path(), 0x7b, projected, Vec::new());
    let segment_path = directory.path().join(&segment.file_name);
    publish(directory.path(), synthetic_receipt(), vec![segment], None);

    // Offset 20,000 is beyond the checked container header and first
    // 16-KiB plaintext chunk, but well before this fixture's trailing FST.
    // Opening must not scan it; the query that reaches the affected record
    // chunk must still fail closed through that chunk's AES-GCM tag.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(segment_path)
        .expect("open segment for lazy corruption");
    file.seek(SeekFrom::Start(20_000))
        .expect("seek lazy segment corruption");
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)
        .expect("read lazy segment segment_bytes");
    byte[0] ^= 0x80;
    file.seek(SeekFrom::Start(20_000))
        .expect("rewind lazy segment corruption");
    file.write_all(&byte)
        .expect("write lazy segment corruption");
    drop(file);

    let graph = SegmentGraph::open(directory.path(), None)
        .expect("unrelated segment_bytes is not scanned at open");
    assert!(
        BlameGraph::resolve(
            &&graph,
            &ResourceSelector {
                kind: ResourceKind::Commit,
                value: COMMIT.to_owned(),
                repository: Some(REPOSITORY.to_owned()),
            },
            2,
        )
        .is_err()
    );
}

fn live_root_fact(
    record: &crate::protocol::CoreRecord,
    path: &str,
    fingerprint_byte: char,
    observed_at: i64,
) -> EnvelopeFact {
    live_root_fact_for_worktree(record, WORKTREE, path, fingerprint_byte, observed_at)
}

fn live_root_fact_for_worktree(
    record: &crate::protocol::CoreRecord,
    worktree: &str,
    path: &str,
    fingerprint_byte: char,
    observed_at: i64,
) -> EnvelopeFact {
    fact(
        record,
        REPOSITORY_LIVE_ACCESS,
        ResourceRef::in_worktree(ResourceKind::Worktree, worktree, REPOSITORY, worktree),
        "authorized_by_core",
        Some(ResourceRef::in_repository(
            ResourceKind::Repository,
            REPOSITORY,
            REPOSITORY,
        )),
        Some(observed_at),
        EnvelopeConfidence::Verified,
        EnvelopeFactState::Asserted,
        BTreeMap::from([
            ("local_root".to_owned(), path.to_owned()),
            (
                "locator_fingerprint".to_owned(),
                fingerprint_byte.to_string().repeat(64),
            ),
            ("locator_fingerprint_revision".to_owned(), "1".to_owned()),
            ("observed_at_unix_ms".to_owned(), observed_at.to_string()),
        ]),
    )
}

#[test]
fn repository_scoped_file_authority_keeps_latest_root_per_worktree() {
    let records = records(3);
    let projection = batch(
        &records,
        vec![
            vec![live_root_fact_for_worktree(
                &records[0],
                "worktree-a",
                "/tmp/a-old",
                '1',
                10,
            )],
            vec![live_root_fact_for_worktree(
                &records[1],
                "worktree-a",
                "/tmp/a-new",
                '2',
                20,
            )],
            vec![live_root_fact_for_worktree(
                &records[2],
                "worktree-b",
                "/tmp/b",
                '3',
                15,
            )],
        ],
    );
    let projected = project_core_batch(&projection).expect("live root projection");
    let directory = tempfile::tempdir().expect("segment directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), projected.records);
    let repository = RepositoryWorktreeIdentity {
        repository_id: REPOSITORY.to_owned(),
        worktree_id: None,
    };
    let roots = GitBlameAuthority::certified_live_root_candidates(&graph, &repository)
        .expect("repository roots");
    assert_eq!(roots.len(), 2);
    assert!(
        roots
            .iter()
            .any(|root| root.path == Path::new("/tmp/a-new"))
    );
    assert!(roots.iter().any(|root| root.path == Path::new("/tmp/b")));
    assert!(
        !roots
            .iter()
            .any(|root| root.path == Path::new("/tmp/a-old"))
    );

    let worktree = RepositoryWorktreeIdentity {
        repository_id: REPOSITORY.to_owned(),
        worktree_id: Some("worktree-a".to_owned()),
    };
    let roots = GitBlameAuthority::certified_live_root_candidates(&graph, &worktree)
        .expect("worktree roots");
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, Path::new("/tmp/a-new"));
}

#[test]
fn live_root_authority_rejects_stale_and_ambiguous_latest_roots() {
    let records = records(3);
    let projection = batch(
        &records,
        vec![
            vec![live_root_fact(&records[0], "/tmp/old", '1', 10)],
            vec![live_root_fact(&records[1], "/tmp/new-a", '2', 20)],
            vec![live_root_fact(&records[2], "/tmp/new-b", '3', 20)],
        ],
    );
    let projected = project_core_batch(&projection).expect("live root projection");
    let old = projected
        .records
        .iter()
        .find(|record| {
            record.attributes.get("local_root")
                == Some(&crate::graph::segment::AttributeValue::String(
                    "/tmp/old".to_owned(),
                ))
        })
        .expect("old root");
    let stale = CertifiedLiveRoot {
        worktree_resource_id: old.subject.graph_id().expect("worktree id"),
        repository_resource_id: old
            .object
            .as_ref()
            .expect("repository")
            .graph_id()
            .expect("repository id"),
        path: "/tmp/old".into(),
        security_geometry_fingerprint: "1".repeat(64),
        observed_at_unix_ms: 10,
    };
    let directory = tempfile::tempdir().expect("segment directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), projected.records);
    let identity = RepositoryWorktreeIdentity {
        repository_id: REPOSITORY.to_owned(),
        worktree_id: Some(WORKTREE.to_owned()),
    };
    let roots =
        GitBlameAuthority::certified_live_root_candidates(&graph, &identity).expect("latest roots");
    assert_eq!(roots.len(), 2);
    assert_eq!(
        GitBlameAuthority::revalidate_certified_live_root(&graph, &stale),
        Err(QueryError::RepositoryUnavailable)
    );
}
