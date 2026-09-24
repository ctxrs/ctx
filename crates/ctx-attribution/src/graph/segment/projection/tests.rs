use std::collections::{BTreeMap, BTreeSet};

use crate::ingest::{
    CoreProjectionCoverage, PreparedCoreEvidence, PreparedCoreProjectionBatch, PreparedCoreUnit,
    ProducerAuthorityDisposition,
};
use crate::protocol::{
    CORE_REPOSITORY_CONTRACT_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION, CoreRecord,
    CoreSourceState, EvidenceCitation, IDENTITY_VERSION, SourceKey, StableEntityKind,
};

use super::super::model::AuthorityBoundary;
use super::*;

const REPOSITORY: &str = "forge:github.com/ctxrs/ctx";
const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";

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
    .expect("stable entity fixture");
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
                0x21_u8.saturating_add(u8::try_from(index).expect("small fixture")),
            );
            record.session_id = session;
            record.parent_session_id = None;
            record.root_session_id = Some(session);
            record.event_sequence = u64::try_from(index + 1).expect("small fixture");
            record.occurred_at_unix_ms = Some(1_700_000_000_000 + index as i64);
            record
                .validate_contract()
                .expect("modified Core record contract");
            record
        })
        .collect()
}

fn fact(
    record: &CoreRecord,
    fact_type: &str,
    subject: ResourceRef,
    attributes: BTreeMap<String, String>,
) -> Fact {
    Fact::create(
        fact_type,
        subject,
        "produced_by",
        Some(ResourceRef::new(
            ResourceKind::Session,
            record.session_id.to_string(),
        )),
        record.occurred_at_unix_ms.map(|value| value.to_string()),
        Confidence::Verified,
        FactState::Asserted,
        "test.detector",
        "7",
        record.session_id.to_string(),
        record.root_session_id.map(|root| root.to_string()),
        Vec::new(),
        attributes,
    )
}

fn unit(record: &CoreRecord, facts: Vec<Fact>, generation: &str, seed: u8) -> PreparedCoreUnit {
    PreparedCoreUnit {
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
                core_generation_id: generation.to_owned(),
                source: record.source.clone(),
                session_id: record.session_id,
                event_id: record.event_id,
                event_sequence: record.event_sequence,
                byte_range: None,
                evidence_sha256: Some(hex::encode([seed; 32])),
            },
        }),
        coverage: CoreProjectionCoverage::default(),
    }
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
            .map(|(index, (record, facts))| {
                unit(
                    record,
                    facts,
                    &generation,
                    0x41_u8.saturating_add(u8::try_from(index).expect("small fixture")),
                )
            })
            .collect(),
    }
}

fn commit_fact(record: &CoreRecord) -> Fact {
    fact(
        record,
        super::super::GIT_COMMIT_PRODUCED,
        ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
        BTreeMap::new(),
    )
}

fn exact_operation_fact(
    record: &CoreRecord,
    fact_type: &str,
    predicate: &str,
    subject: &str,
    object: ResourceRef,
    operation_kind: &str,
    relation_class: &str,
) -> Fact {
    Fact::create(
        fact_type,
        ResourceRef::in_repository(ResourceKind::Commit, subject, REPOSITORY),
        predicate,
        Some(object),
        record.occurred_at_unix_ms.map(|value| value.to_string()),
        Confidence::Verified,
        FactState::Asserted,
        "core.repository",
        CORE_REPOSITORY_CONTRACT_REVISION.to_string(),
        record.session_id.to_string(),
        record.root_session_id.map(|root| root.to_string()),
        Vec::new(),
        BTreeMap::from([
            ("operation_id".to_owned(), "1".repeat(64)),
            ("receipt_id".to_owned(), "2".repeat(64)),
            ("operation_kind".to_owned(), operation_kind.to_owned()),
            ("relation_class".to_owned(), relation_class.to_owned()),
            ("proof_class".to_owned(), "repository_verified".to_owned()),
            ("operation_state".to_owned(), "asserted".to_owned()),
            ("object_format".to_owned(), "sha1".to_owned()),
            ("outcome_kind".to_owned(), "commit".to_owned()),
            (
                "outcome_capture_revision".to_owned(),
                CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION.to_string(),
            ),
            ("origin_event_sequence".to_owned(), "7".to_owned()),
            ("result_record_sha256".to_owned(), "3".repeat(64)),
        ]),
    )
}

#[test]
fn page_projection_reuses_exact_flat_evidence_after_one_fact_walk() {
    let records = records(2);
    let prepared = batch(
        &records,
        vec![
            vec![commit_fact(&records[0])],
            vec![
                commit_fact(&records[1]),
                fact(
                    &records[1],
                    super::super::FILE_TOUCHED,
                    ResourceRef::in_repository(
                        ResourceKind::File,
                        "src/projection-reuse.rs",
                        REPOSITORY,
                    ),
                    BTreeMap::new(),
                ),
            ],
        ],
    );
    let units = prepared.units.iter().collect::<Vec<_>>();
    let page = prepare_core_page_projection(&prepared.core_generation_id, &prepared.source, &units)
        .expect("page-wide projection");

    assert_eq!(
        page.projected,
        project_core_batch(&prepared).expect("batch projection")
    );
    assert_eq!(page.metrics.page_traversals, 1);
    assert_eq!(page.metrics.unit_traversals, 2);
    assert_eq!(page.metrics.fact_traversals, 3);
    assert_eq!(page.metrics.record_serializations, 3);
    let expected_payload_bytes = page
        .projected
        .records
        .iter()
        .map(|record| {
            u64::try_from(
                serde_json::to_vec(record)
                    .expect("canonical Flat payload")
                    .len(),
            )
            .expect("portable canonical payload size")
        })
        .sum::<u64>();
    assert_eq!(page.metrics.record_serialized_bytes, expected_payload_bytes);

    for (record, evidence) in page.projected.records.iter().zip(&page.record_evidence) {
        let canonical = serde_json::to_vec(record).expect("canonical Flat payload");
        let writer_work =
            super::super::FlatSegmentWriter::record_work(record).expect("production Flat work");
        assert_eq!(
            usize::try_from(evidence.canonical_payload_bytes).expect("portable payload size"),
            canonical.len()
        );
        assert_eq!(
            evidence.canonical_sha256,
            super::super::canonical_flat_record_sha256(record).expect("production Flat commitment")
        );
        assert_eq!(
            usize::try_from(evidence.flat_frame_bytes).expect("portable frame size"),
            writer_work.frame_bytes
        );
        assert_eq!(
            usize::try_from(evidence.index_associations).expect("portable association count"),
            writer_work.index_associations
        );
    }

    let reconstructed = projected_core_record_evidence(&page.projected.records)
        .expect("authenticated durable reconstruction");
    assert_eq!(reconstructed, page.record_evidence);
}

#[test]
fn projection_preserves_duplicate_support_and_tombstones() {
    let mut records = records(2);
    records[1].occurred_at_unix_ms = records[0].occurred_at_unix_ms;
    let first = commit_fact(&records[0]);
    let second = commit_fact(&records[1]);

    let prepared = batch(&records, vec![vec![first], vec![second]]);
    let projected = project_core_batch(&prepared).expect("pure projection");

    assert_eq!(projected.records.len(), 2);
    assert_eq!(projected.owners.len(), 2);
    assert!(projected.omissions.is_empty());
    assert_eq!(
        projected.records[0].record_id,
        projected.records[1].record_id
    );
    assert!(
        projected
            .records
            .windows(2)
            .all(|pair| pair[0].event_owner.event_sequence < pair[1].event_owner.event_sequence)
    );

    let record = &projected.records[0];
    assert!(record.index_terms.contains(&record.record_id));
    let mut missing_record_id = record.clone();
    missing_record_id
        .index_terms
        .retain(|term| term != &record.record_id);
    assert_eq!(
        missing_record_id.validate_projected(),
        Err(ServingModelError::MissingRecordIdIndexTerm)
    );

    let tombstone = project_tombstone(&projected.owners[0]).expect("owner tombstone");
    assert_eq!(tombstone.event_sequence, projected.owners[0].event_sequence);
    assert_eq!(tombstone.key(), projected.owners[0].key());
}

#[test]
fn nullable_line_ranges_require_explicit_exact_attributes() {
    let records = records(1);
    let without = fact(
        &records[0],
        super::super::FILE_TOUCHED,
        ResourceRef::in_repository(ResourceKind::File, "src/without.rs", REPOSITORY),
        BTreeMap::new(),
    );
    let with = fact(
        &records[0],
        super::super::FILE_TOUCHED,
        ResourceRef::in_repository(ResourceKind::File, "src/with.rs", REPOSITORY),
        BTreeMap::from([
            ("start_line".to_owned(), "7".to_owned()),
            ("end_line_inclusive".to_owned(), "11".to_owned()),
        ]),
    );

    let prepared = batch(&records, vec![vec![without, with]]);
    let projected = project_core_batch(&prepared).expect("line projection");

    assert_eq!(projected.records.len(), 2);
    assert_eq!(
        projected
            .records
            .iter()
            .find(|record| record.subject.display().as_deref() == Ok("src/without.rs"))
            .expect("without-line record")
            .line_range,
        None
    );
    assert_eq!(
        projected
            .records
            .iter()
            .find(|record| record.subject.display().as_deref() == Ok("src/with.rs"))
            .expect("with-line record")
            .line_range,
        Some(LineRange {
            start_line: 7,
            end_line_inclusive: 11,
        })
    );
}

#[test]
fn unsupported_and_repositoryless_facts_are_explicit_omissions() {
    let records = records(1);
    let unsupported = fact(
        &records[0],
        "git.commit.attempted",
        ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
        BTreeMap::new(),
    );
    let unscoped = fact(
        &records[0],
        super::super::GIT_COMMIT_REFERENCED,
        ResourceRef::new(ResourceKind::Commit, COMMIT),
        BTreeMap::new(),
    );
    let prepared = batch(&records, vec![vec![unsupported, unscoped]]);
    let projected = project_core_batch(&prepared).expect("omission projection");
    assert!(projected.records.is_empty());
    assert_eq!(projected.owners.len(), 1);
    assert_eq!(projected.omissions.len(), 2);
    assert_eq!(
        projected
            .omissions
            .iter()
            .map(|omission| omission.reason)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ProjectionOmissionReason::MissingRepository,
            ProjectionOmissionReason::UnsupportedFactFamily,
        ])
    );
}

#[test]
fn oversized_flat_fact_does_not_discard_neighboring_attribution() {
    let records = records(2);
    let oversized = fact(
        &records[0],
        super::super::GIT_COMMIT_PRODUCED,
        ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
        (0..127)
            .map(|index| (format!("detail_{index:03}"), "\"".repeat(4_096)))
            .collect(),
    );
    let oversized_id = oversized.fact_id.clone();
    let ordinary = commit_fact(&records[1]);
    let prepared = batch(&records, vec![vec![oversized], vec![ordinary]]);
    let oversized_projection = project_fact(
        &prepared.source,
        &prepared.units[0],
        &prepared.units[0].facts[0],
        FactFamily::new(super::super::GIT_COMMIT_PRODUCED).unwrap(),
        REPOSITORY,
        &event_owner(
            &prepared.core_generation_id,
            &prepared.source,
            &prepared.units[0],
        )
        .unwrap()
        .unwrap(),
        &collect_stable_entities(prepared.units.iter()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        super::super::FlatSegmentWriter::record_work(&oversized_projection),
        Err(super::super::FlatSegmentError::Bound("record bytes"))
    ));
    let units = prepared.units.iter().collect::<Vec<_>>();
    let page = prepare_core_page_projection(&prepared.core_generation_id, &prepared.source, &units)
        .expect("one oversized fact must not fail the page");

    assert_eq!(page.projected.records.len(), 1);
    assert_eq!(
        page.projected.records[0].event_owner.event_id,
        records[1].event_id.to_string()
    );
    assert_eq!(page.record_evidence.len(), 1);
    super::super::FlatSegmentWriter::record_work(&page.projected.records[0])
        .expect("surviving attribution can be written");
    assert_eq!(page.metrics.record_serializations, 1);
    assert_eq!(page.projected.omissions.len(), 1);
    let omission = &page.projected.omissions[0];
    assert_eq!(
        omission.reason,
        ProjectionOmissionReason::OversizedFlatRecord
    );
    assert_eq!(
        omission.source_id,
        core_source_storage_id(&prepared.source.source)
    );
    assert_eq!(omission.event_id, records[0].event_id.to_string());
    assert_eq!(omission.fact_id, oversized_id);
    assert_eq!(
        page.record_evidence,
        projected_core_record_evidence(&page.projected.records).unwrap()
    );
}

#[test]
fn origin_defaults_conservatively_and_only_typed_attributes_raise_authority() {
    let records = records(1);
    let unspecified = commit_fact(&records[0]);
    let mut later_attributes = BTreeMap::from([
        ("outcome_capture_revision".to_owned(), "1".to_owned()),
        ("origin_event_sequence".to_owned(), "9".to_owned()),
        ("result_record_sha256".to_owned(), "e".repeat(64)),
    ]);
    let later = fact(
        &records[0],
        super::super::GIT_COMMIT_PRODUCED,
        ResourceRef::in_repository(
            ResourceKind::Commit,
            "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
            REPOSITORY,
        ),
        std::mem::take(&mut later_attributes),
    );
    let projected = project_core_batch(&batch(&records, vec![vec![unspecified, later]]))
        .expect("origin projection");
    assert!(projected.records.iter().any(|record| {
        record.subject.display().as_deref() == Ok(COMMIT)
            && record.origin == ObservationOrigin::Unspecified
            && record.state == ServingFactState::Ambiguous
            && !record.grants_authority(AuthorityBoundary::Ownership)
    }));
    assert!(projected.records.iter().any(|record| {
        record.origin
            == ObservationOrigin::Later {
                origin_event_sequence: 9,
            }
            && record.state == ServingFactState::Ambiguous
            && !record.grants_authority(AuthorityBoundary::Ownership)
    }));
}

#[test]
fn only_complete_exact_commit_operation_attributes_preserve_asserted_flat_authority() {
    const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let records = records(1);
    let session = ResourceRef::new(ResourceKind::Session, records[0].session_id.to_string());
    let replacement = exact_operation_fact(
        &records[0],
        super::super::GIT_COMMIT_REPLACED,
        "replaces",
        COMMIT,
        ResourceRef::in_repository(ResourceKind::Commit, SOURCE, REPOSITORY),
        "rebase",
        "replacement",
    );
    let produced = exact_operation_fact(
        &records[0],
        super::super::GIT_COMMIT_PRODUCED,
        "produced_by",
        COMMIT,
        session,
        "rebase",
        "replacement",
    );
    let cherry_pick = exact_operation_fact(
        &records[0],
        super::super::GIT_COMMIT_CHERRY_PICKED,
        "cherry_picked_from",
        COMMIT,
        ResourceRef::in_repository(ResourceKind::Commit, SOURCE, REPOSITORY),
        "cherry_pick",
        "derivation",
    );
    let mut missing_proof = replacement.clone();
    missing_proof.attributes.remove("proof_class");
    missing_proof.fact_id.push_str("-missing-proof");
    let mut wrong_format = cherry_pick.clone();
    wrong_format
        .attributes
        .insert("object_format".to_owned(), "sha256".to_owned());
    wrong_format.fact_id.push_str("-wrong-format");
    let mut uppercase_operation_id = replacement.clone();
    uppercase_operation_id
        .attributes
        .insert("operation_id".to_owned(), "A".repeat(64));
    uppercase_operation_id
        .fact_id
        .push_str("-uppercase-operation-id");
    let heuristic_cherry_pick = fact(
        &records[0],
        super::super::GIT_COMMIT_CHERRY_PICKED,
        ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
        BTreeMap::from([
            ("observation_origin".to_owned(), "direct".to_owned()),
            ("operation_id".to_owned(), "1".repeat(64)),
        ]),
    );

    let projected = project_core_batch(&batch(
        &records,
        vec![vec![
            replacement,
            produced,
            cherry_pick,
            missing_proof,
            wrong_format,
            uppercase_operation_id,
            heuristic_cherry_pick,
        ]],
    ))
    .expect("exact operation Flat projection");
    assert_eq!(projected.records.len(), 7);
    let operation_id = "1".repeat(64);
    let authoritative = projected
        .records
        .iter()
        .filter(|record| record.grants_authority(AuthorityBoundary::Ownership))
        .collect::<Vec<_>>();
    assert_eq!(authoritative.len(), 3);
    assert!(authoritative.iter().all(|record| {
        record.state == ServingFactState::Asserted
            && record.index_terms.iter().any(|term| term == &operation_id)
            && record.event_owner.event_id == records[0].event_id.to_string()
            && record.event_owner.direct_session_id == records[0].session_id.to_string()
            && record.event_owner.root_session_id
                == records[0].root_session_id.map(|root| root.to_string())
            && record.citations.len() == 1
    }));
    for suffix in ["-missing-proof", "-wrong-format", "-uppercase-operation-id"] {
        assert!(projected.records.iter().any(|record| {
            record.record_id.ends_with(suffix)
                && !record.grants_authority(AuthorityBoundary::Ownership)
                && !record.index_terms.iter().any(|term| term == &operation_id)
        }));
    }
    assert!(projected.records.iter().any(|record| {
        record.fact_family.as_str() == super::super::GIT_COMMIT_CHERRY_PICKED
            && record.detector_id == "test.detector"
            && record.origin == ObservationOrigin::Direct
            && record.state == ServingFactState::Asserted
            && !record.grants_authority(AuthorityBoundary::Ownership)
            && !record.index_terms.iter().any(|term| term == &operation_id)
    }));
}

#[test]
fn explicit_producer_disposition_keeps_context_but_filters_ineligible_authority() {
    const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let records = records(3);
    let produced = |record: &CoreRecord| {
        exact_operation_fact(
            record,
            super::super::GIT_COMMIT_PRODUCED,
            "produced_by",
            COMMIT,
            ResourceRef::new(ResourceKind::Session, record.session_id.to_string()),
            "rebase",
            "replacement",
        )
    };
    let contextual_file = |record: &CoreRecord, path: &str| {
        fact(
            record,
            super::super::FILE_TOUCHED,
            ResourceRef::in_repository(ResourceKind::File, path, REPOSITORY),
            BTreeMap::new(),
        )
    };
    let copied_cherry_pick = exact_operation_fact(
        &records[1],
        super::super::GIT_COMMIT_CHERRY_PICKED,
        "cherry_picked_from",
        COMMIT,
        ResourceRef::in_repository(ResourceKind::Commit, SOURCE, REPOSITORY),
        "cherry_pick",
        "derivation",
    );
    let ambiguous = Fact::create(
        super::super::GIT_COMMIT_AMBIGUOUS,
        ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
        "possibly_produced_by",
        Some(ResourceRef::new(
            ResourceKind::Session,
            records[2].session_id.to_string(),
        )),
        records[2]
            .occurred_at_unix_ms
            .map(|value| value.to_string()),
        Confidence::Ambiguous,
        FactState::Ambiguous,
        "test.detector",
        "7",
        records[2].session_id.to_string(),
        records[2].root_session_id.map(|root| root.to_string()),
        Vec::new(),
        BTreeMap::new(),
    );
    let unknown_merge = fact(
        &records[2],
        super::super::FORGE_MERGE,
        ResourceRef::in_repository(ResourceKind::PullRequest, "224", REPOSITORY),
        BTreeMap::new(),
    );
    let mut prepared = batch(
        &records,
        vec![
            vec![
                produced(&records[0]),
                contextual_file(&records[0], "src/original.rs"),
            ],
            vec![
                produced(&records[1]),
                copied_cherry_pick,
                contextual_file(&records[1], "src/copied.rs"),
            ],
            vec![
                ambiguous,
                unknown_merge,
                contextual_file(&records[2], "src/unknown.rs"),
            ],
        ],
    );
    prepared.units[0].producer_authority_disposition = ProducerAuthorityDisposition::EligibleUnique;
    prepared.units[1].producer_authority_disposition =
        ProducerAuthorityDisposition::IneligibleCopied;
    prepared.units[2].producer_authority_disposition = ProducerAuthorityDisposition::AbstainUnknown;
    let units = prepared.units.iter().collect::<Vec<_>>();
    let projected =
        prepare_core_page_projection(&prepared.core_generation_id, &prepared.source, &units)
            .expect("explicit producer-disposition projection");

    assert_eq!(projected.projected.owners.len(), 3);
    assert_eq!(
        projected
            .projected
            .records
            .iter()
            .filter(|record| record.grants_authority(AuthorityBoundary::Ownership))
            .count(),
        1
    );
    assert!(projected.projected.records.iter().any(|record| {
        record.event_owner.event_id == records[0].event_id.to_string()
            && record.fact_family.as_str() == super::super::GIT_COMMIT_PRODUCED
    }));
    for (record, path) in records
        .iter()
        .zip(["src/original.rs", "src/copied.rs", "src/unknown.rs"])
    {
        assert!(projected.projected.records.iter().any(|projected| {
            projected.event_owner.event_id == record.event_id.to_string()
                && projected.subject.display().as_deref() == Ok(path)
        }));
    }
    assert_eq!(
        projected
            .projected
            .omissions
            .iter()
            .filter(|omission| {
                omission.reason == ProjectionOmissionReason::IneligibleProducerAuthority
            })
            .count(),
        4
    );
    assert_eq!(
        projected.metrics.record_serializations,
        u64::try_from(projected.projected.records.len()).expect("bounded records"),
        "producer-disposition filtering must serialize each retained record exactly once"
    );
    assert!(projected.projected.records.iter().all(|record| {
        record.event_owner.event_id == records[0].event_id.to_string()
            || (!record.grants_authority(AuthorityBoundary::Ownership)
                && record.fact_family.as_str() != super::super::GIT_COMMIT_AMBIGUOUS)
    }));
}

#[test]
fn copied_file_and_commit_facts_are_retained_without_authority() {
    let records = records(1);
    let copied = BTreeMap::from([
        ("observation_origin".to_owned(), "copied".to_owned()),
        ("origin_event_id".to_owned(), "origin-event".to_owned()),
        ("origin_session_id".to_owned(), "origin-session".to_owned()),
    ]);
    let direct = BTreeMap::from([("observation_origin".to_owned(), "direct".to_owned())]);
    let projected = project_core_batch(&batch(
        &records,
        vec![vec![
            fact(
                &records[0],
                super::super::FILE_TOUCHED,
                ResourceRef::in_repository(ResourceKind::File, "src/copied.rs", REPOSITORY),
                copied.clone(),
            ),
            fact(
                &records[0],
                super::super::GIT_COMMIT_PRODUCED,
                ResourceRef::in_repository(ResourceKind::Commit, COMMIT, REPOSITORY),
                copied,
            ),
            fact(
                &records[0],
                super::super::FILE_TOUCHED,
                ResourceRef::in_repository(ResourceKind::File, "src/direct.rs", REPOSITORY),
                direct.clone(),
            ),
            fact(
                &records[0],
                super::super::GIT_COMMIT_PRODUCED,
                ResourceRef::in_repository(
                    ResourceKind::Commit,
                    "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                    REPOSITORY,
                ),
                direct,
            ),
        ]],
    ))
    .expect("authority projection");
    assert_eq!(projected.records.len(), 4);

    let copied_file = projected
        .records
        .iter()
        .find(|record| record.subject.display().as_deref() == Ok("src/copied.rs"))
        .expect("copied file evidence");
    assert_eq!(copied_file.state, ServingFactState::Ambiguous);
    assert!(!copied_file.grants_authority(AuthorityBoundary::FileIdentity));

    let copied_commit = projected
        .records
        .iter()
        .find(|record| record.subject.display().as_deref() == Ok(COMMIT))
        .expect("copied commit evidence");
    assert_eq!(copied_commit.state, ServingFactState::Ambiguous);
    assert!(!copied_commit.grants_authority(AuthorityBoundary::Ownership));

    assert!(projected.records.iter().any(|record| {
        record.subject.display().as_deref() == Ok("src/direct.rs")
            && record.grants_authority(AuthorityBoundary::FileIdentity)
    }));
    assert!(projected.records.iter().any(|record| {
        record.subject.display().as_deref() == Ok("abcdefabcdefabcdefabcdefabcdefabcdefabcd")
            && record.grants_authority(AuthorityBoundary::Ownership)
    }));
}
