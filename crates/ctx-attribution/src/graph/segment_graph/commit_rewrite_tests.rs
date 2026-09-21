use std::collections::BTreeMap;

use super::tests::{REPOSITORY, batch, fact, open_graph, records, synthetic_receipt};
use crate::envelope::{
    Confidence as EnvelopeConfidence, Fact as EnvelopeFact, FactState as EnvelopeFactState,
    ResourceRef,
};
use crate::graph::segment::{
    AuthorityBoundary, GIT_COMMIT_CHERRY_PICKED, GIT_COMMIT_PRODUCED, GIT_COMMIT_REFERENCED,
    GIT_COMMIT_REPLACED, GIT_REMOTE_ALIAS, ObservationOrigin, ServingFactState, ServingRecord,
    project_core_batch,
};
use crate::protocol::{
    BlameMatch, BlameRequest, BlameResult, BlameTarget, CORE_REPOSITORY_CONTRACT_REVISION,
    CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION, CommitFactType, CoreMaterializationReceiptIdentity,
    QuerySnapshotExpectation, ResolvedBlameTarget, ResourceKind,
};
use crate::query::commit_lineage::CommitLineageGraph;
use crate::query::{BlameEntry, BlamePage, BlameService, QueryBounds, ResourceSelector};

const REWRITTEN_COMMIT: &str = "1fe2a9ba68125941c92e026f1540b3c1bff84c22";
const SURVIVING_COMMIT: &str = "735ad8b9137e507c69b7d0fb8db90c7dad549e0e";

fn replacement_fact(
    record: &crate::protocol::CoreRecord,
    repository: &str,
    replaced: &str,
    replacement: &str,
) -> EnvelopeFact {
    replacement_fact_with_digest(record, repository, replaced, replacement, &"7a".repeat(32))
}

fn replacement_fact_with_digest(
    record: &crate::protocol::CoreRecord,
    repository: &str,
    replaced: &str,
    replacement: &str,
    result_record_sha256: &str,
) -> EnvelopeFact {
    EnvelopeFact::create(
        GIT_COMMIT_REPLACED,
        ResourceRef::in_repository(ResourceKind::Commit, replacement, repository),
        "replaces",
        Some(ResourceRef::in_repository(
            ResourceKind::Commit,
            replaced,
            repository,
        )),
        record.occurred_at_unix_ms.map(|value| value.to_string()),
        EnvelopeConfidence::Verified,
        EnvelopeFactState::Asserted,
        "core.repository",
        CORE_REPOSITORY_CONTRACT_REVISION.to_string(),
        record.session_id.to_string(),
        record.root_session_id.map(|root| root.to_string()),
        Vec::new(),
        BTreeMap::from([
            ("outcome_kind".to_owned(), "commit".to_owned()),
            (
                "outcome_capture_revision".to_owned(),
                CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION.to_string(),
            ),
            (
                "origin_event_sequence".to_owned(),
                record.event_sequence.to_string(),
            ),
            (
                "result_record_sha256".to_owned(),
                result_record_sha256.to_owned(),
            ),
        ]),
    )
}

fn repository_alias_fact(record: &crate::protocol::CoreRecord, repository: &str) -> EnvelopeFact {
    let remote = repository
        .strip_prefix("forge:")
        .expect("forge repository fixture");
    fact(
        record,
        GIT_REMOTE_ALIAS,
        ResourceRef::in_repository(
            ResourceKind::Remote,
            format!("https://{remote}"),
            repository,
        ),
        "aliases",
        Some(ResourceRef::in_repository(
            ResourceKind::Repository,
            repository,
            repository,
        )),
        record.occurred_at_unix_ms,
        EnvelopeConfidence::Verified,
        EnvelopeFactState::Asserted,
        BTreeMap::new(),
    )
}

fn commit_evidence_fact(
    record: &crate::protocol::CoreRecord,
    repository: &str,
    oid: &str,
    family: &str,
    predicate: &str,
) -> EnvelopeFact {
    fact(
        record,
        family,
        ResourceRef::in_repository(ResourceKind::Commit, oid, repository),
        predicate,
        Some(ResourceRef::new(
            ResourceKind::Session,
            record.session_id.to_string(),
        )),
        record.occurred_at_unix_ms,
        EnvelopeConfidence::Verified,
        EnvelopeFactState::Asserted,
        BTreeMap::new(),
    )
}

fn projected_records(
    records: &[crate::protocol::CoreRecord],
    facts: Vec<Vec<EnvelopeFact>>,
) -> Vec<ServingRecord> {
    project_core_batch(&batch(records, facts))
        .expect("rewrite serving projection")
        .records
}

fn commit_target(oid: &str) -> BlameTarget {
    BlameTarget::Commit {
        oid: oid.to_owned(),
        repository: Some(REPOSITORY.to_owned()),
    }
}

fn old_commit_answers(include_replacements: bool) -> (BlamePage, BlameResult) {
    let records = records(7);
    let alternate = "835ad8b9137e507c69b7d0fb8db90c7dad549e0e";
    let unrelated = "935ad8b9137e507c69b7d0fb8db90c7dad549e0e";
    let other = "a35ad8b9137e507c69b7d0fb8db90c7dad549e0e";
    let mut facts = vec![
        vec![
            repository_alias_fact(&records[0], REPOSITORY),
            commit_evidence_fact(
                &records[0],
                REPOSITORY,
                REWRITTEN_COMMIT,
                GIT_COMMIT_PRODUCED,
                "produced_by",
            ),
        ],
        vec![commit_evidence_fact(
            &records[1],
            REPOSITORY,
            REWRITTEN_COMMIT,
            GIT_COMMIT_REFERENCED,
            "referenced_by",
        )],
        vec![commit_evidence_fact(
            &records[2],
            REPOSITORY,
            SURVIVING_COMMIT,
            GIT_COMMIT_PRODUCED,
            "produced_by",
        )],
        vec![commit_evidence_fact(
            &records[3],
            REPOSITORY,
            unrelated,
            GIT_COMMIT_REFERENCED,
            "referenced_by",
        )],
        vec![commit_evidence_fact(
            &records[4],
            REPOSITORY,
            alternate,
            GIT_COMMIT_REFERENCED,
            "referenced_by",
        )],
        vec![commit_evidence_fact(
            &records[5],
            REPOSITORY,
            SURVIVING_COMMIT,
            GIT_COMMIT_REFERENCED,
            "referenced_by",
        )],
        vec![commit_evidence_fact(
            &records[6],
            REPOSITORY,
            other,
            GIT_COMMIT_REFERENCED,
            "referenced_by",
        )],
    ];
    if include_replacements {
        facts[3].push(replacement_fact(
            &records[3],
            REPOSITORY,
            REWRITTEN_COMMIT,
            SURVIVING_COMMIT,
        ));
        facts[4].push(replacement_fact(
            &records[4],
            REPOSITORY,
            REWRITTEN_COMMIT,
            alternate,
        ));
        facts[5].push(replacement_fact(
            &records[5],
            REPOSITORY,
            SURVIVING_COMMIT,
            REWRITTEN_COMMIT,
        ));
        facts[6].push(replacement_fact(
            &records[6],
            REPOSITORY,
            alternate,
            unrelated,
        ));
    }

    let directory = tempfile::tempdir().expect("old commit answer directory");

    let graph = open_graph(
        directory.path(),
        synthetic_receipt(),
        projected_records(&records, facts),
    );
    let target = commit_target(REWRITTEN_COMMIT);
    let page = BlameService::new(&graph, QueryBounds::default())
        .expect("old commit blame service")
        .execute(&target, None)
        .expect("old commit blame page");
    let snapshot = QuerySnapshotExpectation::Core {
        receipt: CoreMaterializationReceiptIdentity::from_receipt(graph.completed_receipt())
            .expect("completed old commit receipt"),
    };
    let wire = graph
        .blame_request(&BlameRequest {
            target,
            limit: 100,
            cursor: None,
            expected_snapshot: snapshot,
        })
        .expect("wire old commit blame");
    (page, wire)
}

#[test]
fn arbitrary_replacement_records_cannot_change_any_old_commit_answer_field() {
    let (baseline_page, baseline_wire) = old_commit_answers(false);
    let (mutated_page, mutated_wire) = old_commit_answers(true);

    assert_eq!(mutated_page, baseline_page);
    assert_eq!(mutated_wire, baseline_wire);
    let ResolvedBlameTarget::Commit { commit, .. } = &mutated_page.target else {
        panic!("commit target");
    };
    assert_eq!(commit.display, REWRITTEN_COMMIT);
    assert!(mutated_page.entries.iter().all(|entry| {
        matches!(entry, BlameEntry::Commit(entry) if entry.subject.display == REWRITTEN_COMMIT)
    }));
}

#[test]
fn successor_producer_is_never_reported_as_old_commit_producer() {
    let records = records(3);
    let projected = projected_records(
        &records,
        vec![
            vec![
                repository_alias_fact(&records[0], REPOSITORY),
                commit_evidence_fact(
                    &records[0],
                    REPOSITORY,
                    REWRITTEN_COMMIT,
                    GIT_COMMIT_REFERENCED,
                    "referenced_by",
                ),
            ],
            vec![replacement_fact(
                &records[1],
                REPOSITORY,
                REWRITTEN_COMMIT,
                SURVIVING_COMMIT,
            )],
            vec![commit_evidence_fact(
                &records[2],
                REPOSITORY,
                SURVIVING_COMMIT,
                GIT_COMMIT_PRODUCED,
                "produced_by",
            )],
        ],
    );
    let directory = tempfile::tempdir().expect("successor producer directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), projected);
    let page = BlameService::new(&graph, QueryBounds::default())
        .expect("successor producer service")
        .execute(&commit_target(REWRITTEN_COMMIT), None)
        .expect("old commit direct facts");

    let ResolvedBlameTarget::Commit { commit, .. } = &page.target else {
        panic!("commit target");
    };
    assert_eq!(commit.display, REWRITTEN_COMMIT);
    assert_eq!(page.entries.len(), 1);
    let BlameEntry::Commit(entry) = &page.entries[0] else {
        panic!("old commit reference");
    };
    assert_eq!(entry.subject.display, REWRITTEN_COMMIT);
    assert_eq!(entry.fact.fact_type, GIT_COMMIT_REFERENCED);
    assert_ne!(entry.fact.fact_type, GIT_COMMIT_PRODUCED);
    let snapshot = QuerySnapshotExpectation::Core {
        receipt: CoreMaterializationReceiptIdentity::from_receipt(graph.completed_receipt())
            .expect("completed successor producer receipt"),
    };
    let wire = graph
        .blame_request(&BlameRequest {
            target: commit_target(REWRITTEN_COMMIT),
            limit: 100,
            cursor: None,
            expected_snapshot: snapshot,
        })
        .expect("wire old commit direct facts");
    assert_eq!(wire.matches.len(), 1);
    assert!(wire.matches.iter().all(|item| {
        matches!(item, BlameMatch::Commit(item) if item.subject.display == REWRITTEN_COMMIT)
    }));
    assert!(wire.matches.iter().all(|item| {
        matches!(item, BlameMatch::Commit(item) if item.fact_type == CommitFactType::Referenced)
    }));
}

#[test]
fn malformed_later_replacement_is_retained_without_ownership_authority() {
    let records = records(1);
    let projected = projected_records(
        &records,
        vec![vec![replacement_fact_with_digest(
            &records[0],
            REPOSITORY,
            REWRITTEN_COMMIT,
            SURVIVING_COMMIT,
            &"7A".repeat(32),
        )]],
    );
    let replacement = projected
        .iter()
        .find(|record| record.fact_family.as_str() == GIT_COMMIT_REPLACED)
        .expect("one malformed replacement observation");
    assert_eq!(replacement.state, ServingFactState::Ambiguous);
    assert!(matches!(
        replacement.origin,
        ObservationOrigin::Later { .. }
    ));
    assert!(!replacement.grants_authority(AuthorityBoundary::Ownership));
}

#[test]
fn heuristic_cherry_pick_lookalike_is_not_served_as_operation_lineage() {
    let records = records(1);
    let operation_id = "1".repeat(64);
    let receipt_id = "2".repeat(64);
    let projected = projected_records(
        &records,
        vec![vec![
            repository_alias_fact(&records[0], REPOSITORY),
            fact(
                &records[0],
                GIT_COMMIT_CHERRY_PICKED,
                ResourceRef::in_repository(ResourceKind::Commit, SURVIVING_COMMIT, REPOSITORY),
                "cherry_picked_from",
                Some(ResourceRef::in_repository(
                    ResourceKind::Commit,
                    REWRITTEN_COMMIT,
                    REPOSITORY,
                )),
                records[0].occurred_at_unix_ms,
                EnvelopeConfidence::Verified,
                EnvelopeFactState::Asserted,
                BTreeMap::from([
                    ("operation_id".to_owned(), operation_id),
                    ("receipt_id".to_owned(), receipt_id),
                    ("operation_kind".to_owned(), "cherry_pick".to_owned()),
                    ("relation_class".to_owned(), "derivation".to_owned()),
                    ("proof_class".to_owned(), "repository_verified".to_owned()),
                    ("operation_state".to_owned(), "asserted".to_owned()),
                    ("object_format".to_owned(), "sha1".to_owned()),
                    ("outcome_kind".to_owned(), "commit".to_owned()),
                    (
                        "outcome_capture_revision".to_owned(),
                        CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION.to_string(),
                    ),
                    (
                        "origin_event_sequence".to_owned(),
                        records[0].event_sequence.to_string(),
                    ),
                    ("result_record_sha256".to_owned(), "3".repeat(64)),
                ]),
            ),
        ]],
    );
    let lookalike = projected
        .iter()
        .find(|record| record.fact_family.as_str() == GIT_COMMIT_CHERRY_PICKED)
        .expect("heuristic cherry-pick record");
    // Generic direct-observation authority is intentionally insufficient for
    // exact operation lineage; only the typed Core-operation verifier admits it.
    assert!(!lookalike.grants_authority(AuthorityBoundary::Ownership));
    assert_eq!(lookalike.verified_commit_operation_id(), None);

    let directory = tempfile::tempdir().expect("heuristic lineage directory");
    let graph = open_graph(directory.path(), synthetic_receipt(), projected);
    let repository = graph
        .resolve_resources(
            &ResourceSelector {
                kind: ResourceKind::Repository,
                value: REPOSITORY.to_owned(),
                repository: None,
            },
            1,
        )
        .expect("resolve repository")
        .pop()
        .expect("repository resource");
    let commit = graph
        .resolve_resources(
            &ResourceSelector {
                kind: ResourceKind::Commit,
                value: SURVIVING_COMMIT.to_owned(),
                repository: Some(REPOSITORY.to_owned()),
            },
            1,
        )
        .expect("resolve commit")
        .pop()
        .expect("commit resource");
    let adjacent = graph
        .operation_ids_for_commit(&commit, &repository, &Default::default(), 10)
        .expect("query exact operation IDs");
    assert!(adjacent.operation_ids.is_empty());
    assert!(!adjacent.has_more);
}

#[test]
fn verified_operation_scope_requires_matching_optional_root() {
    // Form an exact verified operation with the existing projection/publication
    // harness, then vary only the redundant owner/scope values at query time.
    for (root_present, scope_present, valid) in [
        (false, false, true),
        (true, true, true),
        (false, true, false),
        (true, false, false),
    ] {
        let records = records(1);
        let operation_id = "1".repeat(64);
        let mut operation =
            replacement_fact(&records[0], REPOSITORY, REWRITTEN_COMMIT, SURVIVING_COMMIT);
        operation.attributes.extend(BTreeMap::from([
            ("operation_id".to_owned(), operation_id.clone()),
            ("receipt_id".to_owned(), "2".repeat(64)),
            ("operation_kind".to_owned(), "amend".to_owned()),
            ("relation_class".to_owned(), "replacement".to_owned()),
            ("proof_class".to_owned(), "repository_verified".to_owned()),
            ("operation_state".to_owned(), "asserted".to_owned()),
            ("object_format".to_owned(), "sha1".to_owned()),
        ]));
        operation = operation.with_operation_identity_sha256("4".repeat(64));
        let mut projected = projected_records(
            &records,
            vec![vec![
                operation,
                repository_alias_fact(&records[0], REPOSITORY),
            ]],
        );
        let operation = projected
            .iter_mut()
            .find(|record| record.fact_family.as_str() == GIT_COMMIT_REPLACED)
            .expect("verified operation record");
        assert_eq!(
            operation.verified_commit_operation_id(),
            Some(operation_id.as_str())
        );
        let expected_root = root_present.then(|| records[0].root_session_id.unwrap().to_string());
        operation.event_owner.root_session_id = expected_root.clone();
        if !scope_present {
            operation.scope = None;
        }
        let directory = tempfile::tempdir().expect("operation scope directory");
        let graph = open_graph(directory.path(), synthetic_receipt(), projected);
        let repository = graph
            .resolve_resources(
                &ResourceSelector {
                    kind: ResourceKind::Repository,
                    value: REPOSITORY.to_owned(),
                    repository: None,
                },
                1,
            )
            .expect("resolve operation repository")
            .pop()
            .expect("repository");
        let answer = graph.operation_facts_for_id(&operation_id, &repository, 10);
        if valid {
            let page = answer.expect("matching optional root is valid");
            assert_eq!(page.facts.len(), 1);
            assert!(!page.has_more);
            assert_eq!(
                page.facts[0].metadata.evidence_identity.root_session_id,
                expected_root
            );
            assert_eq!(
                page.facts[0].metadata.evidence_identity.direct_session_id,
                records[0].session_id.to_string()
            );
        } else {
            assert!(
                answer.is_err(),
                "mixed owner/scope must fail query validation"
            );
        }
    }
}
