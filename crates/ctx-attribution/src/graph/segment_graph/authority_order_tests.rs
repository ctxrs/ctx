use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest as _, Sha256};

use super::*;
use crate::envelope::{Confidence, Fact, FactState, ResourceRef};
use crate::graph::git::{GitBlameAuthority, RepositoryWorktreeIdentity};
use crate::graph::segment::{
    EventTombstone, FILE_TOUCHED, FLAT_SERVING_ROLE, FlatSegmentWriter, GIT_COMMIT_PRODUCED,
    MANIFEST_SCHEMA_VERSION, ObservationOrigin, REPOSITORY_LIVE_ACCESS, SegmentManifest,
    SegmentRef, SegmentStore, SegmentWriter, ServingFactState, ServingModelError, ServingRecord,
    project_core_batch, random_generation_id, segment_file_name,
};
use crate::ingest::{
    CoreProjectionCoverage, PreparedCoreEvidence, PreparedCoreProjectionBatch, PreparedCoreUnit,
    ProducerAuthorityDisposition,
};
use crate::protocol::{
    CoreMaterializationReceipt, CoreRecord, CoreSourceState, EvidenceCitation, IDENTITY_VERSION,
    ResourceKind, SourceKey, StableEntityId, StableEntityKind, core_record_contract_fingerprint,
};
use crate::query::{BlameGraph, QueryError, ResourceId, ResourceSelector};

const REPOSITORY: &str = "forge:github.com/ctxrs/ctx";
const WORKTREE: &str = "worktree-1";
const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";
const OTHER_COMMIT: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
const CHUNK_BYTES: u32 = ctx_attribution_index::SEGMENT_CHUNK_BYTES;

fn golden_record() -> CoreRecord {
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
    identity
        .validate_contract()
        .expect("stable entity contract");
    identity
}

fn records(count: usize) -> Vec<CoreRecord> {
    let template = golden_record();
    let source = template.source.clone();
    let session = stable_entity(&source, StableEntityKind::Session, 0x31);
    (0..count)
        .map(|index| {
            let mut record = template.clone();
            record.event_id = stable_entity(
                &source,
                StableEntityKind::Event,
                0x41_u8.saturating_add(u8::try_from(index).expect("small fixture")),
            );
            record.session_id = session;
            record.parent_session_id = None;
            record.root_session_id = Some(session);
            record.event_sequence = u64::try_from(index + 1).expect("small fixture");
            record.occurred_at_unix_ms = Some(1_700_000_000_000 + index as i64);
            record.validate_contract().expect("Core record contract");
            record
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn fact(
    record: &CoreRecord,
    fact_type: &str,
    subject: ResourceRef,
    object: Option<ResourceRef>,
    confidence: Confidence,
    state: FactState,
    attributes: BTreeMap<String, String>,
) -> Fact {
    Fact::create(
        fact_type,
        subject,
        "authorized_by",
        object,
        record.occurred_at_unix_ms.map(|value| value.to_string()),
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

fn batch(records: &[CoreRecord], facts: Vec<Vec<Fact>>) -> PreparedCoreProjectionBatch {
    let generation = "a".repeat(64);
    PreparedCoreProjectionBatch {
        core_generation_id: generation.clone(),
        source: CoreSourceState {
            source: records[0].source.clone(),
            core_record_accumulator: "b".repeat(64),
            event_count: u64::try_from(records.len()).expect("small fixture"),
        },
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
                        evidence_sha256: Some(hex::encode(
                            [0x61_u8.saturating_add(u8::try_from(index).expect("small fixture"));
                                32],
                        )),
                    },
                }),
                coverage: CoreProjectionCoverage::default(),
            })
            .collect(),
    }
}

fn direct_origin() -> BTreeMap<String, String> {
    BTreeMap::from([("observation_origin".to_owned(), "direct".to_owned())])
}

fn copied_origin() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("observation_origin".to_owned(), "copied".to_owned()),
        ("origin_event_id".to_owned(), "origin-event".to_owned()),
        ("origin_session_id".to_owned(), "origin-session".to_owned()),
    ])
}

fn receipt() -> CoreMaterializationReceipt {
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
    publication_generation: u64,
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
        publication_generation,
        generation_id,
        role: FLAT_SERVING_ROLE,
        file_name,
        plaintext_bytes: stats.plaintext_bytes,
        file_sha256,
    }
}

fn publish(
    root: &Path,
    graph_generation: u64,
    prior_generation_id: Option<String>,
    mut segments: Vec<SegmentRef>,
) -> String {
    ctx_history_platform::platform_security::establish_private_data_root(root)
        .expect("private segment fixture root");
    for (ordinal, segment) in segments.iter_mut().enumerate() {
        segment.ordinal = u32::try_from(ordinal).expect("small fixture");
    }
    let generation_id = random_generation_id().expect("manifest generation");
    let manifest = SegmentManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generation_id: generation_id.clone(),
        prior_generation_id,
        graph_generation,
        core_receipt: receipt(),
        materializer_identity: crate::test_support::TEST_MATERIALIZER_REVISION.to_owned(),
        schema_identity: SEGMENT_SCHEMA_IDENTITY.to_owned(),
        evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
        segments,
        predecessor_segments: SegmentStore::new(root)
            .load_active()
            .unwrap()
            .map_or_else(Vec::new, |manifest| manifest.segments),
    };
    let store = SegmentStore::new(root);
    let candidate = store.stage_manifest(&manifest).expect("stage manifest");
    store
        .publish_candidate(candidate)
        .expect("publish manifest");
    generation_id
}

fn open_one(records: Vec<ServingRecord>) -> (tempfile::TempDir, SegmentGraph) {
    let directory = tempfile::tempdir().expect("segment directory");

    let segment = write_segment(directory.path(), 0x51, 1, records, Vec::new());
    publish(directory.path(), 1, None, vec![segment]);
    let graph = SegmentGraph::open(directory.path(), None).expect("segment graph");
    (directory, graph)
}

#[test]
fn copied_file_and_commit_evidence_never_authorizes() {
    let records = records(4);
    let session = |record: &CoreRecord| {
        Some(ResourceRef::new(
            ResourceKind::Session,
            record.session_id.to_string(),
        ))
    };
    let projected = project_core_batch(&batch(
        &records,
        vec![
            vec![fact(
                &records[0],
                FILE_TOUCHED,
                ResourceRef::in_repository(ResourceKind::File, "src/copied.rs", REPOSITORY),
                session(&records[0]),
                Confidence::Verified,
                FactState::Asserted,
                copied_origin(),
            )],
            vec![fact(
                &records[1],
                FILE_TOUCHED,
                ResourceRef::in_repository(ResourceKind::File, "src/direct.rs", REPOSITORY),
                session(&records[1]),
                Confidence::Verified,
                FactState::Asserted,
                direct_origin(),
            )],
            vec![fact(
                &records[2],
                GIT_COMMIT_PRODUCED,
                ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
                session(&records[2]),
                Confidence::Verified,
                FactState::Asserted,
                copied_origin(),
            )],
            vec![fact(
                &records[3],
                GIT_COMMIT_PRODUCED,
                ResourceRef::in_repository(ResourceKind::Commit, OTHER_COMMIT, REPOSITORY),
                session(&records[3]),
                Confidence::Verified,
                FactState::Asserted,
                direct_origin(),
            )],
        ],
    ))
    .expect("authority projection");
    let copied_file = projected
        .records
        .iter()
        .find(|record| record.subject.display().as_deref() == Ok("src/copied.rs"))
        .expect("copied file");
    let direct_file = projected
        .records
        .iter()
        .find(|record| record.subject.display().as_deref() == Ok("src/direct.rs"))
        .expect("direct file");
    let copied_commit = projected
        .records
        .iter()
        .find(|record| record.subject.display().as_deref() == Ok(COMMIT))
        .expect("copied commit");
    let direct_commit = projected
        .records
        .iter()
        .find(|record| record.subject.display().as_deref() == Ok(OTHER_COMMIT))
        .expect("direct commit");
    assert_eq!(copied_file.state, ServingFactState::Ambiguous);
    assert_eq!(copied_commit.state, ServingFactState::Ambiguous);
    let mut forged_copied_file = direct_file.clone();
    forged_copied_file.origin = ObservationOrigin::Copied {
        origin_event_id: "origin-event".to_owned(),
        origin_session_id: "origin-session".to_owned(),
    };
    assert_eq!(
        forged_copied_file.validate(),
        Err(ServingModelError::IneligibleAssertedAuthority)
    );
    let copied_file_id = ResourceId(copied_file.subject.graph_id().expect("file ID"));
    let direct_file_id = ResourceId(direct_file.subject.graph_id().expect("file ID"));
    let copied_commit_id = ResourceId(copied_commit.subject.graph_id().expect("commit ID"));
    let direct_commit_id = ResourceId(direct_commit.subject.graph_id().expect("commit ID"));
    let (_directory, graph) = open_one(projected.records);

    assert_eq!(
        GitBlameAuthority::exact_file_authority(&graph, &copied_file_id),
        Err(QueryError::RepositoryUnavailable)
    );
    GitBlameAuthority::exact_file_authority(&graph, &direct_file_id)
        .expect("direct file authority");
    assert!(
        BlameGraph::production_attribution(&&graph, std::slice::from_ref(&copied_commit_id), 10)
            .expect("copied attribution abstention")
            .is_empty()
    );
    assert!(
        BlameGraph::blame_facts_page(
            &&graph,
            &copied_commit_id,
            crate::query::BlameFactFamily::Commit,
            None,
            10,
        )
        .expect("copied direct-commit blame abstention")
        .items
        .is_empty()
    );
    assert_eq!(
        BlameGraph::production_attribution(&&graph, std::slice::from_ref(&direct_commit_id), 10)
            .expect("direct attribution")
            .len(),
        1
    );
    assert_eq!(
        BlameGraph::blame_facts_page(
            &&graph,
            &direct_commit_id,
            crate::query::BlameFactFamily::Commit,
            None,
            10,
        )
        .expect("direct commit blame")
        .items
        .len(),
        1
    );
}

fn live_root_fact(
    record: &CoreRecord,
    path: &str,
    confidence: Confidence,
    state: FactState,
    mut origin: BTreeMap<String, String>,
) -> Fact {
    origin.extend([
        ("local_root".to_owned(), path.to_owned()),
        ("locator_fingerprint".to_owned(), "f".repeat(64)),
        ("locator_fingerprint_revision".to_owned(), "1".to_owned()),
        (
            "observed_at_unix_ms".to_owned(),
            record.occurred_at_unix_ms.expect("timestamp").to_string(),
        ),
    ]);
    fact(
        record,
        REPOSITORY_LIVE_ACCESS,
        ResourceRef::in_worktree(ResourceKind::Worktree, WORKTREE, REPOSITORY, WORKTREE),
        Some(ResourceRef::in_repository(
            ResourceKind::Repository,
            REPOSITORY,
            REPOSITORY,
        )),
        confidence,
        state,
        origin,
    )
}

#[test]
fn live_root_rejects_ambiguous_superseded_contradicted_copied_and_unverified_records() {
    let records = records(6);
    let facts = vec![
        live_root_fact(
            &records[0],
            "/tmp/ambiguous",
            Confidence::Verified,
            FactState::Ambiguous,
            direct_origin(),
        ),
        live_root_fact(
            &records[1],
            "/tmp/superseded",
            Confidence::Verified,
            FactState::Superseded,
            direct_origin(),
        ),
        live_root_fact(
            &records[2],
            "/tmp/contradicted",
            Confidence::Verified,
            FactState::Contradicted,
            direct_origin(),
        ),
        live_root_fact(
            &records[3],
            "/tmp/copied",
            Confidence::Verified,
            FactState::Asserted,
            copied_origin(),
        ),
        live_root_fact(
            &records[4],
            "/tmp/high",
            Confidence::High,
            FactState::Asserted,
            direct_origin(),
        ),
        live_root_fact(
            &records[5],
            "/tmp/direct",
            Confidence::Verified,
            FactState::Asserted,
            direct_origin(),
        ),
    ];
    let projected = project_core_batch(&batch(
        &records,
        facts.into_iter().map(|fact| vec![fact]).collect(),
    ))
    .expect("live root projection");
    assert_eq!(projected.records.len(), 6);
    let (_directory, graph) = open_one(projected.records);
    let identity = RepositoryWorktreeIdentity {
        repository_id: REPOSITORY.to_owned(),
        worktree_id: Some(WORKTREE.to_owned()),
    };
    let roots = GitBlameAuthority::certified_live_root_candidates(&graph, &identity)
        .expect("live root candidates");
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, Path::new("/tmp/direct"));
    GitBlameAuthority::revalidate_certified_live_root(&graph, &roots[0])
        .expect("direct root revalidation");
}

fn manifest_reference(publication_generation: u64, byte: u8) -> SegmentRef {
    let generation_id = format!("{byte:02x}").repeat(32);
    SegmentRef {
        ordinal: 0,
        publication_generation,
        generation_id: generation_id.clone(),
        role: FLAT_SERVING_ROLE,
        file_name: segment_file_name(&generation_id, FLAT_SERVING_ROLE),
        plaintext_bytes: 0,
        file_sha256: "d".repeat(64),
    }
}

fn test_manifest(mut segments: Vec<SegmentRef>) -> SegmentManifest {
    for (ordinal, segment) in segments.iter_mut().enumerate() {
        segment.ordinal = u32::try_from(ordinal).expect("small fixture");
    }
    SegmentManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generation_id: "9".repeat(64),
        prior_generation_id: Some("8".repeat(64)),
        graph_generation: 2,
        core_receipt: receipt(),
        materializer_identity: crate::test_support::TEST_MATERIALIZER_REVISION.to_owned(),
        schema_identity: SEGMENT_SCHEMA_IDENTITY.to_owned(),
        evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
        segments,
        predecessor_segments: Vec::new(),
    }
}

#[test]
fn manifest_and_open_validation_reject_reversed_replacement_deletion_layers() {
    let reversed = test_manifest(vec![
        manifest_reference(1, 0x71),
        manifest_reference(2, 0x72),
    ]);
    assert!(reversed.validate().is_err());
    assert!(
        SegmentGraph::flat_open_policy()
            .validate_manifest_for_test(&reversed)
            .is_err()
    );

    let grouped = test_manifest(vec![
        manifest_reference(2, 0x73),
        manifest_reference(2, 0x74),
        manifest_reference(1, 0x75),
    ]);
    grouped.validate().expect("same-layer chunks are valid");
    SegmentGraph::flat_open_policy()
        .validate_manifest_for_test(&grouped)
        .expect("open accepts newest-first layers");
}

#[test]
fn same_publication_chunks_apply_records_before_layer_tombstones() {
    let records = records(1);
    let old = fact(
        &records[0],
        FILE_TOUCHED,
        ResourceRef::in_repository(ResourceKind::File, "src/old.rs", REPOSITORY),
        None,
        Confidence::Verified,
        FactState::Asserted,
        direct_origin(),
    );
    let new = fact(
        &records[0],
        FILE_TOUCHED,
        ResourceRef::in_repository(ResourceKind::File, "src/new.rs", REPOSITORY),
        None,
        Confidence::Verified,
        FactState::Asserted,
        direct_origin(),
    );
    let old = project_core_batch(&batch(&records, vec![vec![old]])).expect("old projection");
    let new = project_core_batch(&batch(&records, vec![vec![new]])).expect("new projection");
    let tombstone = new.owners[0].tombstone();
    let directory = tempfile::tempdir().expect("segment directory");

    let oldest = write_segment(directory.path(), 0x61, 1, old.records, Vec::new());
    let first_generation = publish(directory.path(), 1, None, vec![oldest.clone()]);
    let deletion_chunk = write_segment(directory.path(), 0x62, 2, Vec::new(), vec![tombstone]);
    let replacement_chunk = write_segment(directory.path(), 0x63, 2, new.records, Vec::new());
    publish(
        directory.path(),
        2,
        Some(first_generation),
        vec![deletion_chunk, replacement_chunk, oldest],
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
