use crate::{
    CommitLineageBounds, CommitLineageOmission, ExactCommitRef, GitObjectFormat, SourceKey,
    MAX_COMMIT_LINEAGE_EXAMINED_EVENTS, MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, TypedKey,
};

use super::*;

fn source() -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([3; 32]),
    )
    .unwrap()
}

fn resource(id: &str, kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        kind,
        display: display.to_owned(),
    }
}

fn citation(generation: &str) -> EvidenceCitation {
    let source = source();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", TypedKey::U64(1)).unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(2)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    EvidenceCitation {
        core_generation_id: generation.to_owned(),
        source,
        session_id,
        event_id,
        event_sequence: 7,
        byte_range: None,
        evidence_sha256: Some("e".repeat(64)),
    }
}

fn snapshot(generation: char, materializer_revision: &str) -> QuerySnapshotExpectation {
    QuerySnapshotExpectation::Core {
        receipt: CoreMaterializationReceiptIdentity {
            core_generation_id: generation.to_string().repeat(64),
            materializer_revision: materializer_revision.to_owned(),
        },
    }
}

fn file_request(generation: char) -> BlameRequest {
    BlameRequest {
        target: BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: None,
            lines: Some(LineRange { start: 1, end: 1 }),
        },
        limit: 1,
        cursor: None,
        expected_snapshot: snapshot(generation, "materializer-v1"),
    }
}

fn file_result(result_snapshot: QuerySnapshotExpectation, generations: &[String]) -> BlameResult {
    let evidence = generations
        .iter()
        .enumerate()
        .map(|(index, generation)| NumberedEvidence {
            number: u32::try_from(index + 1).unwrap(),
            citation: citation(generation),
        })
        .collect::<Vec<_>>();
    let line_evidence_numbers = evidence.iter().map(|evidence| evidence.number).collect();
    BlameResult {
        snapshot: result_snapshot,
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(
                "repository:fixture",
                ResourceKind::Repository,
                "fixture/repository",
            ),
            requested_lines: Some(LineRange { start: 1, end: 1 }),
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        outcome: BlameOutcome {
            attribution: BlameAttribution::None,
            coverage: BlameCoverage {
                unit: BlameCoverageUnit::CommittedLine,
                evaluated: 1,
                proven: 0,
                possible: 0,
                conflicting: 0,
                none: 1,
            },
        },
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: "file-match:1".to_owned(),
            lines: LineRange { start: 1, end: 1 },
            commit: resource(
                "commit:0123456",
                ResourceKind::Commit,
                "0123456789abcdef0123456789abcdef01234567",
            ),
            line_evidence_numbers,
            production: Vec::new(),
        })],
        evidence,
        next: None,
        lineage: None,
    }
}

fn empty_file_result(result_snapshot: QuerySnapshotExpectation) -> BlameResult {
    BlameResult {
        snapshot: result_snapshot,
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(
                "repository:fixture",
                ResourceKind::Repository,
                "fixture/repository",
            ),
            requested_lines: Some(LineRange { start: 1, end: 1 }),
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        outcome: BlameOutcome {
            attribution: BlameAttribution::None,
            coverage: BlameCoverage {
                unit: BlameCoverageUnit::CommittedLine,
                evaluated: 0,
                proven: 0,
                possible: 0,
                conflicting: 0,
                none: 0,
            },
        },
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
        lineage: None,
    }
}

fn outcome(
    unit: BlameCoverageUnit,
    proven: u32,
    possible: u32,
    conflicting: u32,
    none: u32,
) -> BlameOutcome {
    let evaluated = proven + possible + conflicting + none;
    let coverage = BlameCoverage {
        unit,
        evaluated,
        proven,
        possible,
        conflicting,
        none,
    };
    BlameOutcome {
        attribution: coverage.aggregate_attribution(),
        coverage,
    }
}

fn production(id: &str, producer: &str, relationship: ProductionRelationship) -> AgentAttribution {
    let (confidence, state) = match relationship {
        ProductionRelationship::ProducedBy => (FactConfidence::Explicit, FactState::Asserted),
        ProductionRelationship::PossiblyProducedBy => {
            (FactConfidence::Ambiguous, FactState::Ambiguous)
        }
    };
    AgentAttribution {
        id: id.to_owned(),
        relationship,
        producing_session: resource(producer, ResourceKind::Session, producer),
        parent_session: None,
        direct_actor: None,
        owning_root: None,
        fact_occurred_at_ms: None,
        confidence,
        state,
        evidence_numbers: vec![1],
    }
}

fn file_match(id: &str, start: u32, end: u32, production: Vec<AgentAttribution>) -> BlameMatch {
    BlameMatch::File(FileBlameMatch {
        id: id.to_owned(),
        lines: LineRange { start, end },
        commit: resource(
            "commit:0123456",
            ResourceKind::Commit,
            "0123456789abcdef0123456789abcdef01234567",
        ),
        line_evidence_numbers: vec![1],
        production,
    })
}

fn commit_match(
    fact_id: &str,
    fact_type: CommitFactType,
    state: FactState,
    object: &str,
) -> BlameMatch {
    let predicate = match fact_type {
        CommitFactType::Produced => CommitPredicate::ProducedBy,
        CommitFactType::Ambiguous => CommitPredicate::PossiblyProducedBy,
        CommitFactType::Amended => CommitPredicate::AmendedBy,
        CommitFactType::CherryPicked => CommitPredicate::CherryPickedFrom,
        CommitFactType::Reverted => CommitPredicate::Reverts,
        CommitFactType::Pushed => CommitPredicate::PushedBy,
        CommitFactType::Inspected => CommitPredicate::InspectedBy,
        CommitFactType::Referenced => CommitPredicate::ReferencedBy,
    };
    BlameMatch::Commit(CommitBlameMatch {
        fact_id: fact_id.to_owned(),
        fact_type,
        predicate,
        subject: resource(
            "commit:fixture",
            ResourceKind::Commit,
            "0123456789abcdef0123456789abcdef01234567",
        ),
        object: Some(resource(object, ResourceKind::Session, object)),
        parent_session: None,
        fact_occurred_at_ms: None,
        confidence: if state == FactState::Ambiguous {
            FactConfidence::Ambiguous
        } else {
            FactConfidence::Explicit
        },
        state,
        direct_actor: None,
        owning_root: None,
        evidence_numbers: vec![1],
    })
}

fn commit_result(matches: Vec<BlameMatch>, outcome: BlameOutcome) -> BlameResult {
    BlameResult {
        snapshot: snapshot('a', "materializer-v1"),
        target: ResolvedBlameTarget::Commit {
            commit: resource(
                "commit:fixture",
                ResourceKind::Commit,
                "0123456789abcdef0123456789abcdef01234567",
            ),
            repository: resource(
                "repository:fixture",
                ResourceKind::Repository,
                "fixture/repository",
            ),
        },
        git_snapshot: None,
        outcome,
        matches,
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: citation(&"a".repeat(64)),
        }],
        next: None,
        lineage: None,
    }
}

fn pull_request_activity(fact_id: &str, state: FactState) -> BlameMatch {
    BlameMatch::PullRequest(PullRequestBlameMatch {
        pull_request: resource("pr:7", ResourceKind::PullRequest, "7"),
        relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
            fact_id: fact_id.to_owned(),
            action: PullRequestAction::Reviewed,
            session: resource(
                "session:reviewer",
                ResourceKind::Session,
                "session:reviewer",
            ),
            direct_actor: None,
            owning_root: None,
            fact_occurred_at_ms: None,
            confidence: if state == FactState::Ambiguous {
                FactConfidence::Ambiguous
            } else {
                FactConfidence::Explicit
            },
            state,
            evidence_numbers: vec![1],
        }),
    })
}

fn pull_request_commit(fact_id: &str, production: Vec<AgentAttribution>) -> BlameMatch {
    BlameMatch::PullRequest(PullRequestBlameMatch {
        pull_request: resource("pr:7", ResourceKind::PullRequest, "7"),
        relationship: PullRequestBlameRelationship::Commit(PullRequestCommit {
            fact_id: fact_id.to_owned(),
            relationship: PullRequestCommitRelationship::ContainsCommit,
            commit: resource(
                "commit:fixture",
                ResourceKind::Commit,
                "0123456789abcdef0123456789abcdef01234567",
            ),
            fact_occurred_at_ms: None,
            production,
            evidence_numbers: vec![1],
        }),
    })
}

fn pull_request_result(matches: Vec<BlameMatch>, outcome: BlameOutcome) -> BlameResult {
    BlameResult {
        snapshot: snapshot('a', "materializer-v1"),
        target: ResolvedBlameTarget::PullRequest {
            selector: "7".to_owned(),
            pull_request: resource("pr:7", ResourceKind::PullRequest, "7"),
            repository: resource(
                "repository:fixture",
                ResourceKind::Repository,
                "fixture/repository",
            ),
        },
        git_snapshot: None,
        outcome,
        matches,
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: citation(&"a".repeat(64)),
        }],
        next: None,
        lineage: None,
    }
}

#[test]
fn non_commit_blame_result_rejects_commit_lineage() {
    let mut result = empty_file_result(snapshot('a', "materializer-v1"));
    let oid = "1".repeat(40);
    result.lineage = Some(CommitLineage {
        requested: ExactCommitRef {
            resource: resource("commit:one", ResourceKind::Commit, &oid),
            logical_repository_id: "ctxrs/ctx".to_owned(),
            object_format: GitObjectFormat::Sha1,
            oid,
        },
        edges: Vec::new(),
        yielded_by: Vec::new(),
        origin: None,
        endpoint: None,
        complete: true,
        ambiguous: false,
        bounds: CommitLineageBounds {
            returned_events: 0,
            returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            examined_events: 0,
            examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            omission: CommitLineageOmission::Exact(0),
            truncation_reason: None,
        },
    });

    let error = result.validate().unwrap_err();
    assert_eq!(error.class, ErrorClass::Corrupt);
    assert!(error.message.contains("only valid for commit blame"));
}

#[test]
fn every_citation_generation_matches_the_expected_request_snapshot() {
    let request = file_request('a');
    let matching = file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64), "a".repeat(64)],
    );
    matching.validate_for_request(&request).unwrap();

    let mismatch = file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64), "b".repeat(64)],
    );
    let error = mismatch.validate_for_request(&request).unwrap_err();
    assert_eq!(error.class, ErrorClass::Corrupt);
    assert_eq!(
        error.message,
        "blame evidence generation does not match the requested Core snapshot"
    );
}

#[test]
fn result_snapshot_must_exactly_match_the_request_even_when_the_result_is_empty() {
    let request = file_request('a');
    empty_file_result(snapshot('a', "materializer-v1"))
        .validate_for_request(&request)
        .unwrap();

    let mismatch = empty_file_result(snapshot('a', "materializer-v2"));
    let error = mismatch.validate_for_request(&request).unwrap_err();
    assert_eq!(error.class, ErrorClass::Corrupt);
    assert_eq!(
        error.message,
        "blame result snapshot does not match the requested Core snapshot"
    );
}

#[test]
fn missing_or_malformed_result_snapshot_fails_deserialization() {
    let matching = empty_file_result(snapshot('a', "materializer-v1"));
    let mut missing = serde_json::to_value(&matching).unwrap();
    missing.as_object_mut().unwrap().remove("snapshot");
    assert!(serde_json::from_value::<BlameResult>(missing).is_err());

    let mut malformed = serde_json::to_value(matching).unwrap();
    malformed["snapshot"]["receipt"]["materializer_revision"] =
        serde_json::Value::String(String::new());
    assert!(serde_json::from_value::<BlameResult>(malformed).is_err());

    let mut missing_outcome =
        serde_json::to_value(empty_file_result(snapshot('a', "materializer-v1"))).unwrap();
    missing_outcome.as_object_mut().unwrap().remove("outcome");
    assert!(serde_json::from_value::<BlameResult>(missing_outcome).is_err());
}

#[test]
fn malformed_or_missing_citation_generation_fails_typed_response_validation() {
    let request = file_request('a');
    let malformed = file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64), "not-a-generation".to_owned()],
    );
    assert_eq!(
        malformed.validate_for_request(&request).unwrap_err().class,
        ErrorClass::Corrupt
    );

    let mut missing = serde_json::to_value(file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64)],
    ))
    .unwrap();
    missing["evidence"][0]["citation"]
        .as_object_mut()
        .unwrap()
        .remove("core_generation_id");
    assert!(serde_json::from_value::<BlameResult>(missing).is_err());
}

#[test]
fn outcome_is_derived_from_the_returned_file_production_evidence() {
    let request = file_request('a');
    let base = file_result(snapshot('a', "materializer-v1"), &["a".repeat(64)]);
    base.validate_for_request(&request).unwrap();

    for dishonest in [
        outcome(BlameCoverageUnit::CommittedLine, 1, 0, 0, 0),
        outcome(BlameCoverageUnit::CommittedLine, 0, 1, 0, 0),
        outcome(BlameCoverageUnit::CommittedLine, 0, 0, 1, 0),
    ] {
        let mut result = base.clone();
        result.outcome = dishonest;
        let error = result.validate_for_request(&request).unwrap_err();
        assert_eq!(error.class, ErrorClass::Corrupt);
        assert_eq!(
            error.message,
            "blame outcome must exactly match the returned page evidence"
        );
    }

    for (production, expected) in [
        (
            vec![production(
                "produced-a",
                "session:a",
                ProductionRelationship::ProducedBy,
            )],
            outcome(BlameCoverageUnit::CommittedLine, 1, 0, 0, 0),
        ),
        (
            vec![production(
                "possible-a",
                "session:a",
                ProductionRelationship::PossiblyProducedBy,
            )],
            outcome(BlameCoverageUnit::CommittedLine, 0, 1, 0, 0),
        ),
        (
            vec![
                production(
                    "produced-a",
                    "session:a",
                    ProductionRelationship::ProducedBy,
                ),
                production(
                    "produced-b",
                    "session:b",
                    ProductionRelationship::ProducedBy,
                ),
            ],
            outcome(BlameCoverageUnit::CommittedLine, 0, 0, 1, 0),
        ),
    ] {
        let mut result = base.clone();
        let BlameMatch::File(file) = &mut result.matches[0] else {
            panic!("fixture must contain a file match");
        };
        file.production = production;
        result.outcome = expected;
        result.validate_for_request(&request).unwrap();
    }

    let mut mixed = base.clone();
    let BlameMatch::File(file) = &mut mixed.matches[0] else {
        panic!("fixture must contain a file match");
    };
    file.production = vec![production(
        "produced-a",
        "session:a",
        ProductionRelationship::ProducedBy,
    )];
    mixed.outcome = outcome(BlameCoverageUnit::CommittedLine, 1, 0, 0, 0);
    mixed.outcome.attribution = BlameAttribution::Possible;
    assert_eq!(mixed.validate().unwrap_err().class, ErrorClass::Corrupt);
}

#[test]
fn file_coverage_is_line_weighted_and_ranges_are_ordered_without_overlap() {
    let mut valid = file_result(snapshot('a', "materializer-v1"), &["a".repeat(64)]);
    let ResolvedBlameTarget::File {
        requested_lines, ..
    } = &mut valid.target
    else {
        panic!("fixture must resolve a file target");
    };
    *requested_lines = Some(LineRange { start: 1, end: 10 });
    valid.matches = vec![
        file_match(
            "proven",
            1,
            2,
            vec![production(
                "produced-a",
                "session:a",
                ProductionRelationship::ProducedBy,
            )],
        ),
        file_match(
            "possible",
            3,
            5,
            vec![production(
                "possible-b",
                "session:b",
                ProductionRelationship::PossiblyProducedBy,
            )],
        ),
        file_match(
            "conflicting",
            6,
            6,
            vec![
                production(
                    "produced-c",
                    "session:c",
                    ProductionRelationship::ProducedBy,
                ),
                production(
                    "produced-d",
                    "session:d",
                    ProductionRelationship::ProducedBy,
                ),
            ],
        ),
        file_match("none", 7, 10, Vec::new()),
    ];
    valid.outcome = outcome(BlameCoverageUnit::CommittedLine, 2, 3, 1, 4);
    valid.validate().unwrap();

    let mut overlapping = valid.clone();
    let BlameMatch::File(second) = &mut overlapping.matches[1] else {
        panic!("fixture must contain a file match");
    };
    second.lines.start = 2;
    assert_eq!(
        overlapping.validate().unwrap_err().message,
        "file blame matches must have ordered, non-overlapping line ranges"
    );

    let mut out_of_order = valid;
    out_of_order.matches.swap(0, 1);
    assert_eq!(
        out_of_order.validate().unwrap_err().message,
        "file blame matches must have ordered, non-overlapping line ranges"
    );
}

#[test]
fn commit_coverage_detects_page_wide_producer_conflicts_and_duplicate_fact_units() {
    commit_result(
        vec![commit_match(
            "produced-a",
            CommitFactType::Produced,
            FactState::Asserted,
            "session:a",
        )],
        outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0),
    )
    .validate()
    .unwrap();

    commit_result(
        vec![
            commit_match(
                "produced-a",
                CommitFactType::Produced,
                FactState::Asserted,
                "session:a",
            ),
            commit_match(
                "produced-a-again",
                CommitFactType::Produced,
                FactState::Asserted,
                "session:a",
            ),
        ],
        outcome(BlameCoverageUnit::CommitFact, 2, 0, 0, 0),
    )
    .validate()
    .unwrap();

    let conflicting = commit_result(
        vec![
            commit_match(
                "produced-a",
                CommitFactType::Produced,
                FactState::Asserted,
                "session:a",
            ),
            commit_match(
                "produced-b",
                CommitFactType::Produced,
                FactState::Asserted,
                "session:b",
            ),
            commit_match(
                "possible-c",
                CommitFactType::Ambiguous,
                FactState::Ambiguous,
                "session:c",
            ),
            commit_match(
                "inspected-d",
                CommitFactType::Inspected,
                FactState::Asserted,
                "session:d",
            ),
        ],
        outcome(BlameCoverageUnit::CommitFact, 0, 1, 2, 1),
    );
    conflicting.validate().unwrap();

    let duplicate = commit_result(
        vec![
            commit_match(
                "duplicate",
                CommitFactType::Produced,
                FactState::Asserted,
                "session:a",
            ),
            commit_match(
                "duplicate",
                CommitFactType::Ambiguous,
                FactState::Ambiguous,
                "session:b",
            ),
        ],
        outcome(BlameCoverageUnit::CommitFact, 1, 1, 0, 0),
    );
    assert_eq!(
        duplicate.validate().unwrap_err().message,
        "commit blame page contains duplicate fact units"
    );
}

#[test]
fn commit_producer_facts_require_session_objects_and_exact_semantics() {
    let invalid_cases = [
        (
            "produced repository object",
            CommitFactType::Produced,
            FactState::Asserted,
            FactConfidence::Explicit,
            ResourceKind::Repository,
            "resource reference has an unexpected kind",
        ),
        (
            "produced ambiguous state",
            CommitFactType::Produced,
            FactState::Ambiguous,
            FactConfidence::Explicit,
            ResourceKind::Session,
            "asserted production has inconsistent state or confidence",
        ),
        (
            "produced ambiguous confidence",
            CommitFactType::Produced,
            FactState::Asserted,
            FactConfidence::Ambiguous,
            ResourceKind::Session,
            "asserted production has inconsistent state or confidence",
        ),
        (
            "possible commit object",
            CommitFactType::Ambiguous,
            FactState::Ambiguous,
            FactConfidence::Ambiguous,
            ResourceKind::Commit,
            "resource reference has an unexpected kind",
        ),
        (
            "possible asserted state",
            CommitFactType::Ambiguous,
            FactState::Asserted,
            FactConfidence::Ambiguous,
            ResourceKind::Session,
            "possible production must preserve ambiguous state and confidence",
        ),
        (
            "possible explicit confidence",
            CommitFactType::Ambiguous,
            FactState::Ambiguous,
            FactConfidence::Explicit,
            ResourceKind::Session,
            "possible production must preserve ambiguous state and confidence",
        ),
    ];

    for (name, fact_type, state, confidence, object_kind, expected_message) in invalid_cases {
        let mut blame_match = commit_match(name, fact_type, state, "session:producer");
        let BlameMatch::Commit(fact) = &mut blame_match else {
            panic!("commit fixture must contain a commit match");
        };
        fact.confidence = confidence;
        fact.object.as_mut().expect("commit producer object").kind = object_kind;
        let invalid = commit_result(
            vec![blame_match],
            outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0),
        );
        assert_eq!(
            invalid.validate().unwrap_err().message,
            expected_message,
            "{name}"
        );
    }
}

#[test]
fn pull_request_coverage_derives_activity_and_commit_production_without_duplicate_units() {
    let valid = pull_request_result(
        vec![
            pull_request_activity("activity-asserted", FactState::Asserted),
            pull_request_activity("activity-ambiguous", FactState::Ambiguous),
            pull_request_activity("activity-superseded", FactState::Superseded),
            pull_request_commit(
                "commit-proven",
                vec![production(
                    "produced-a",
                    "session:a",
                    ProductionRelationship::ProducedBy,
                )],
            ),
            pull_request_commit(
                "commit-possible",
                vec![production(
                    "possible-b",
                    "session:b",
                    ProductionRelationship::PossiblyProducedBy,
                )],
            ),
            pull_request_commit(
                "commit-conflicting",
                vec![
                    production(
                        "produced-c",
                        "session:c",
                        ProductionRelationship::ProducedBy,
                    ),
                    production(
                        "produced-d",
                        "session:d",
                        ProductionRelationship::ProducedBy,
                    ),
                ],
            ),
            pull_request_commit("commit-none", Vec::new()),
        ],
        outcome(BlameCoverageUnit::PullRequestRelationship, 2, 2, 1, 2),
    );
    valid.validate().unwrap();

    let duplicate = pull_request_result(
        vec![
            pull_request_activity("duplicate", FactState::Asserted),
            pull_request_commit(
                "duplicate",
                vec![production(
                    "produced-a",
                    "session:a",
                    ProductionRelationship::ProducedBy,
                )],
            ),
        ],
        outcome(BlameCoverageUnit::PullRequestRelationship, 2, 0, 0, 0),
    );
    assert_eq!(
        duplicate.validate().unwrap_err().message,
        "pull request blame page contains duplicate fact units"
    );
}

#[test]
fn outcome_rejects_wrong_count_sum_unit_page_scope_and_unknown_fields() {
    let base = file_result(snapshot('a', "materializer-v1"), &["a".repeat(64)]);

    let mut wrong_sum = base.clone();
    wrong_sum.outcome.coverage.evaluated = 2;
    assert_eq!(wrong_sum.validate().unwrap_err().class, ErrorClass::Corrupt);

    let mut wrong_unit = base.clone();
    wrong_unit.outcome.coverage.unit = BlameCoverageUnit::CommitFact;
    assert_eq!(
        wrong_unit.validate().unwrap_err().class,
        ErrorClass::Corrupt
    );

    let mut wrong_page = base.clone();
    wrong_page.outcome.coverage.evaluated = 2;
    wrong_page.outcome.coverage.none = 2;
    assert_eq!(
        wrong_page.validate().unwrap_err().class,
        ErrorClass::Corrupt
    );

    let mut unknown = serde_json::to_value(base).unwrap();
    unknown["outcome"]["partial"] = serde_json::json!(false);
    assert!(serde_json::from_value::<BlameResult>(unknown).is_err());
}
