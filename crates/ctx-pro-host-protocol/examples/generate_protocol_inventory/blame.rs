use super::fixtures::structured_pr_provider_output_citation;
use super::*;

fn resource(kind: ResourceKind, suffix: &str, display: &str) -> ResourceRef {
    ResourceRef {
        id: format!("{}:{suffix}", kind.wire_name()),
        kind,
        display: display.to_owned(),
    }
}

fn canonical_citation(seed: u32, observation_kind: ObservationKind) -> EvidenceCitation {
    EvidenceCitation {
        observation_id: Some(Uuid::from_u128(1_000 + u128::from(seed))),
        observation_seq: Some(u64::from(seed)),
        observation_kind: Some(observation_kind),
        session_id: Some(Uuid::from_u128(2_000 + u128::from(seed))),
        event_id: Some(Uuid::from_u128(3_000 + u128::from(seed))),
        event_seq: Some(u64::from(seed)),
        source_path: Some("fixture/session.jsonl".to_owned()),
        fixture_line: Some(u64::from(seed)),
        source_record_ordinal: Some(u64::from(seed - 1)),
        source_record_subrecord_index: Some(0),
        byte_range: Some(ByteRange {
            start: u64::from(seed) * 100,
            end_exclusive: u64::from(seed) * 100 + 80,
        }),
        source_sha256: Some(format!("{seed:064x}")),
        provider_output: None,
    }
}

fn production_attribution(
    suffix: &str,
    relationship: ProductionRelationship,
    evidence_number: u32,
) -> AgentAttribution {
    let (confidence, state) = match relationship {
        ProductionRelationship::ProducedBy => (FactConfidence::Explicit, FactState::Asserted),
        ProductionRelationship::PossiblyProducedBy => {
            (FactConfidence::Ambiguous, FactState::Ambiguous)
        }
    };
    AgentAttribution {
        id: format!("attribution:{suffix}"),
        relationship,
        producing_session: resource(ResourceKind::Session, suffix, &format!("session-{suffix}")),
        direct_actor: Some(resource(
            ResourceKind::Agent,
            suffix,
            &format!("agent-{suffix}"),
        )),
        owning_root: Some(resource(
            ResourceKind::Session,
            &format!("root-{suffix}"),
            &format!("root-session-{suffix}"),
        )),
        confidence,
        state,
        evidence_numbers: vec![evidence_number],
    }
}

pub(super) fn file_blame_result(
    requested_lines: Option<LineRange>,
    matched_lines: LineRange,
    worktree_status: WorktreeStatus,
    relationship: ProductionRelationship,
    next: Option<BlameContinuation>,
) -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
            requested_lines,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status,
        }),
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: format!("file-match-{}-{}", matched_lines.start, matched_lines.end),
            lines: matched_lines,
            commit: resource(
                ResourceKind::Commit,
                "file-head",
                "0123456789abcdef0123456789abcdef01234567",
            ),
            line_evidence_numbers: vec![1],
            production: vec![production_attribution("file-head", relationship, 2)],
        })],
        evidence: vec![
            NumberedEvidence {
                number: 1,
                citation: canonical_citation(1, ObservationKind::FileTouch),
            },
            NumberedEvidence {
                number: 2,
                citation: canonical_citation(2, ObservationKind::VcsChange),
            },
        ],
        next,
    }
}

pub(super) fn commit_blame_result() -> BlameResult {
    let commit = resource(
        ResourceKind::Commit,
        "commit-query",
        "0123456789abcdef0123456789abcdef01234567",
    );
    let variants = [
        (
            CommitFactType::Produced,
            CommitPredicate::ProducedBy,
            ResourceKind::Session,
            FactConfidence::Explicit,
            FactState::Asserted,
        ),
        (
            CommitFactType::Amended,
            CommitPredicate::AmendedBy,
            ResourceKind::Session,
            FactConfidence::High,
            FactState::Asserted,
        ),
        (
            CommitFactType::CherryPicked,
            CommitPredicate::CherryPickedFrom,
            ResourceKind::Commit,
            FactConfidence::Medium,
            FactState::Asserted,
        ),
        (
            CommitFactType::Reverted,
            CommitPredicate::Reverts,
            ResourceKind::Commit,
            FactConfidence::Low,
            FactState::Contradicted,
        ),
        (
            CommitFactType::Pushed,
            CommitPredicate::PushedBy,
            ResourceKind::Session,
            FactConfidence::Ambiguous,
            FactState::Ambiguous,
        ),
        (
            CommitFactType::Inspected,
            CommitPredicate::InspectedBy,
            ResourceKind::Session,
            FactConfidence::Unknown,
            FactState::Superseded,
        ),
        (
            CommitFactType::Referenced,
            CommitPredicate::ReferencedBy,
            ResourceKind::Session,
            FactConfidence::Explicit,
            FactState::Asserted,
        ),
        (
            CommitFactType::Ambiguous,
            CommitPredicate::PossiblyProducedBy,
            ResourceKind::Session,
            FactConfidence::Ambiguous,
            FactState::Ambiguous,
        ),
    ];
    let matches = variants
        .into_iter()
        .enumerate()
        .map(
            |(index, (fact_type, predicate, object_kind, confidence, state))| {
                let number = u32::try_from(index + 1)
                    .unwrap_or_else(|_| panic!("small commit fixture index"));
                BlameMatch::Commit(CommitBlameMatch {
                    fact_id: format!("commit-fact-{number}"),
                    fact_type,
                    predicate,
                    subject: commit.clone(),
                    object: Some(resource(
                        object_kind,
                        &format!("commit-object-{number}"),
                        &format!("commit-object-{number}"),
                    )),
                    fact_occurred_at_ms: Some(1_753_232_400_000 + i64::from(number)),
                    confidence,
                    state,
                    direct_actor: Some(resource(
                        ResourceKind::Agent,
                        &format!("commit-actor-{number}"),
                        &format!("agent-{number}"),
                    )),
                    owning_root: Some(resource(
                        ResourceKind::Session,
                        &format!("commit-root-{number}"),
                        &format!("root-session-{number}"),
                    )),
                    evidence_numbers: vec![number],
                })
            },
        )
        .collect();
    let evidence = (1..=8)
        .map(|number| NumberedEvidence {
            number,
            citation: canonical_citation(
                number,
                match number % 3 {
                    0 => ObservationKind::Event,
                    1 => ObservationKind::FileTouch,
                    _ => ObservationKind::VcsChange,
                },
            ),
        })
        .collect();
    BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit,
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches,
        evidence,
        next: Some(BlameContinuation {
            cursor: "commit-next".to_owned(),
            reason: ContinuationReason::MoreMatches,
        }),
    }
}

pub(super) fn pull_request_activity_result() -> BlameResult {
    let pull_request = resource(
        ResourceKind::PullRequest,
        "github-ctxrs-ctx-42",
        "https://github.com/ctxrs/ctx/pull/42",
    );
    let actions = [
        PullRequestAction::Referenced,
        PullRequestAction::Created,
        PullRequestAction::Reviewed,
        PullRequestAction::Commented,
        PullRequestAction::Merged,
        PullRequestAction::Edited,
        PullRequestAction::Closed,
        PullRequestAction::Reopened,
    ];
    let matches = actions
        .into_iter()
        .enumerate()
        .map(|(index, action)| {
            let number =
                u32::try_from(index + 1).unwrap_or_else(|_| panic!("small PR fixture index"));
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request: pull_request.clone(),
                relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
                    fact_id: format!("pr-activity-{number}"),
                    action,
                    session: resource(
                        ResourceKind::Session,
                        &format!("pr-activity-{number}"),
                        &format!("session-{number}"),
                    ),
                    direct_actor: Some(resource(
                        ResourceKind::Agent,
                        &format!("pr-actor-{number}"),
                        &format!("agent-{number}"),
                    )),
                    owning_root: Some(resource(
                        ResourceKind::Session,
                        &format!("pr-root-{number}"),
                        &format!("root-session-{number}"),
                    )),
                    fact_occurred_at_ms: Some(1_753_232_500_000 + i64::from(number)),
                    confidence: FactConfidence::Explicit,
                    state: FactState::Asserted,
                    evidence_numbers: vec![number],
                }),
            })
        })
        .collect();
    let evidence = (1..=8)
        .map(|number| NumberedEvidence {
            number,
            citation: canonical_citation(number, ObservationKind::Event),
        })
        .collect();
    BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request,
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches,
        evidence,
        next: None,
    }
}

pub(super) fn pull_request_membership_result() -> BlameResult {
    let pull_request = resource(
        ResourceKind::PullRequest,
        "github-ctxrs-ctx-42",
        "https://github.com/ctxrs/ctx/pull/42",
    );
    let membership_evidence = NumberedEvidence {
        number: 1,
        citation: structured_pr_provider_output_citation(),
    };
    let contains = PullRequestCommit {
        fact_id: "pr-contains-commit".to_owned(),
        relationship: PullRequestCommitRelationship::ContainsCommit,
        commit: resource(
            ResourceKind::Commit,
            "pr-commit",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        production: vec![production_attribution(
            "pr-commit",
            ProductionRelationship::ProducedBy,
            2,
        )],
        evidence_numbers: vec![1],
    };
    let merged_as = PullRequestCommit {
        fact_id: "pr-merged-as".to_owned(),
        relationship: PullRequestCommitRelationship::MergedAs,
        commit: resource(
            ResourceKind::Commit,
            "pr-merge-commit",
            "89abcdef0123456789abcdef0123456789abcdef",
        ),
        production: vec![production_attribution(
            "pr-merge-commit",
            ProductionRelationship::PossiblyProducedBy,
            3,
        )],
        evidence_numbers: vec![1],
    };
    BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
            pull_request: pull_request.clone(),
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches: vec![
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request: pull_request.clone(),
                relationship: PullRequestBlameRelationship::Commit(contains),
            }),
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request,
                relationship: PullRequestBlameRelationship::Commit(merged_as),
            }),
        ],
        evidence: vec![
            membership_evidence,
            NumberedEvidence {
                number: 2,
                citation: canonical_citation(2, ObservationKind::VcsChange),
            },
            NumberedEvidence {
                number: 3,
                citation: canonical_citation(3, ObservationKind::VcsChange),
            },
        ],
        next: None,
    }
}
