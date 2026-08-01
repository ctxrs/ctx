use std::io::Write as _;

use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
};
use ctx_pro_host_protocol::{
    AgentAttribution, BlameContinuation, BlameMatch, BlameResult, CommitBlameMatch, CommitFactType,
    CommitPredicate, ContinuationReason, EvidenceCitation, FactConfidence, FactState,
    FileBlameMatch, GitSnapshot, LineRange, NumberedEvidence, ProductionRelationship,
    PullRequestAction, PullRequestActivity, PullRequestBlameMatch, PullRequestBlameRelationship,
    PullRequestCommit, PullRequestCommitRelationship, ResolvedBlameTarget, ResourceKind,
    ResourceRef, WorktreeStatus,
};
use unicode_width::UnicodeWidthStr as _;

use super::render_blame_document;
use crate::ui::{ColorMode, Document, RenderContext, StreamKind, TestContext, Token};

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

fn render_plain(result: &BlameResult, width: usize) -> String {
    render_blame_document(result, &context(width)).render_plain()
}

fn resource(id: &str, kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        kind,
        display: display.to_owned(),
    }
}

fn repository() -> ResourceRef {
    resource(
        "repository:ctxrs/ctx",
        ResourceKind::Repository,
        "ctxrs/ctx",
    )
}

fn event_evidence(number: u32) -> NumberedEvidence {
    let source = SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([number as u8; 32]),
    )
    .unwrap();
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
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(u64::from(number)))
            .unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    NumberedEvidence {
        number,
        citation: EvidenceCitation {
            core_generation_id: "a".repeat(64),
            source,
            session_id,
            event_id,
            event_sequence: u64::from(number),
            byte_range: None,
            evidence_sha256: None,
        },
    }
}

fn attribution(
    id: &str,
    relationship: ProductionRelationship,
    session: &str,
    evidence_number: u32,
) -> AgentAttribution {
    let ambiguous = relationship == ProductionRelationship::PossiblyProducedBy;
    AgentAttribution {
        id: id.to_owned(),
        relationship,
        producing_session: resource(
            &format!("session:{session}"),
            ResourceKind::Session,
            session,
        ),
        direct_actor: None,
        owning_root: None,
        confidence: if ambiguous {
            FactConfidence::Ambiguous
        } else {
            FactConfidence::Explicit
        },
        state: if ambiguous {
            FactState::Ambiguous
        } else {
            FactState::Asserted
        },
        evidence_numbers: vec![evidence_number],
    }
}

fn commit_match(
    commit: &ResourceRef,
    fact_type: CommitFactType,
    predicate: CommitPredicate,
    object: &str,
    confidence: FactConfidence,
    state: FactState,
    evidence_number: u32,
) -> BlameMatch {
    BlameMatch::Commit(CommitBlameMatch {
        fact_id: format!("fact:{evidence_number}"),
        fact_type,
        predicate,
        subject: commit.clone(),
        object: Some(resource(
            &format!("session:{object}"),
            ResourceKind::Session,
            object,
        )),
        fact_occurred_at_ms: None,
        confidence,
        state,
        direct_actor: None,
        owning_root: None,
        evidence_numbers: vec![evidence_number],
    })
}

#[test]
fn commit_renderer_keeps_production_grouping_golden() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![
            commit_match(
                &commit,
                CommitFactType::Produced,
                CommitPredicate::ProducedBy,
                "producer",
                FactConfidence::Explicit,
                FactState::Asserted,
                1,
            ),
            commit_match(
                &commit,
                CommitFactType::Ambiguous,
                CommitPredicate::PossiblyProducedBy,
                "possible",
                FactConfidence::Ambiguous,
                FactState::Ambiguous,
                2,
            ),
            commit_match(
                &commit,
                CommitFactType::Referenced,
                CommitPredicate::ReferencedBy,
                "observer",
                FactConfidence::Explicit,
                FactState::Asserted,
                3,
            ),
        ],
        evidence: (1..=3).map(event_evidence).collect(),
        next: None,
    };
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_commit.golden.txt")
    );
}

#[test]
fn pull_request_renderer_preserves_proof_edges_and_continuation_golden() {
    let pull_request = resource(
        "pull_request:ctxrs/ctx#42",
        ResourceKind::PullRequest,
        "ctxrs/ctx#42",
    );
    let mut producer = attribution(
        "fact:producer",
        ProductionRelationship::ProducedBy,
        "producer",
        2,
    );
    producer.direct_actor = Some(resource("agent:codex", ResourceKind::Agent, "codex"));
    producer.owning_root = Some(resource("run:root", ResourceKind::Run, "root"));
    let result = BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "https://gitlab.example.com/ctxrs/ctx/-/merge_requests/42".to_owned(),
            pull_request: pull_request.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request: pull_request.clone(),
                relationship: PullRequestBlameRelationship::Commit(PullRequestCommit {
                    fact_id: "fact:membership".to_owned(),
                    relationship: PullRequestCommitRelationship::ContainsCommit,
                    commit: resource("commit:deadbeef", ResourceKind::Commit, "deadbeef"),
                    production: vec![
                        producer,
                        attribution(
                            "fact:possible",
                            ProductionRelationship::PossiblyProducedBy,
                            "possible",
                            3,
                        ),
                    ],
                    evidence_numbers: vec![1],
                }),
            }),
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request,
                relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
                    fact_id: "fact:reviewed".to_owned(),
                    action: PullRequestAction::Reviewed,
                    session: resource("session:reviewer", ResourceKind::Session, "reviewer"),
                    direct_actor: Some(resource(
                        "agent:review-agent",
                        ResourceKind::Agent,
                        "review-agent",
                    )),
                    owning_root: Some(resource(
                        "run:review-root",
                        ResourceKind::Run,
                        "review-root",
                    )),
                    fact_occurred_at_ms: Some(1_721_000_000_000),
                    confidence: FactConfidence::Explicit,
                    state: FactState::Asserted,
                    evidence_numbers: vec![4],
                }),
            }),
        ],
        evidence: (1..=4).map(event_evidence).collect(),
        next: Some(BlameContinuation {
            cursor: "next-page".to_owned(),
            reason: ContinuationReason::MoreMatches,
        }),
    };
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_pr.golden.txt")
    );
}

fn paginated_pr_result(commit_page: bool) -> BlameResult {
    let pull_request = resource(
        "pull_request:ctxrs/ctx#42",
        ResourceKind::PullRequest,
        "ctxrs/ctx#42",
    );
    let relationship = if commit_page {
        PullRequestBlameRelationship::Commit(PullRequestCommit {
            fact_id: "fact:membership".to_owned(),
            relationship: PullRequestCommitRelationship::ContainsCommit,
            commit: resource("commit:deadbeef", ResourceKind::Commit, "deadbeef"),
            production: Vec::new(),
            evidence_numbers: vec![1],
        })
    } else {
        PullRequestBlameRelationship::Activity(PullRequestActivity {
            fact_id: "fact:reviewed".to_owned(),
            action: PullRequestAction::Reviewed,
            session: resource("session:reviewer", ResourceKind::Session, "reviewer"),
            direct_actor: None,
            owning_root: None,
            fact_occurred_at_ms: None,
            confidence: FactConfidence::Explicit,
            state: FactState::Asserted,
            evidence_numbers: vec![1],
        })
    };
    BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request: pull_request.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![BlameMatch::PullRequest(PullRequestBlameMatch {
            pull_request,
            relationship,
        })],
        evidence: vec![event_evidence(1)],
        next: Some(BlameContinuation {
            cursor: if commit_page {
                "activity-page".to_owned()
            } else {
                "commit-page".to_owned()
            },
            reason: ContinuationReason::MoreMatches,
        }),
    }
}

#[test]
fn pull_request_commit_only_page_scopes_missing_activity_golden() {
    let result = paginated_pr_result(true);
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_pr_commit_only_page.golden.txt")
    );
}

#[test]
fn pull_request_activity_only_page_scopes_missing_commits_golden() {
    let result = paginated_pr_result(false);
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_pr_activity_only_page.golden.txt")
    );
}

#[test]
fn file_continuation_uses_committed_window_golden() {
    let result = BlameResult {
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: repository(),
            requested_lines: Some(LineRange { start: 42, end: 60 }),
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "deadbeef".to_owned(),
            worktree_status: WorktreeStatus::Differs,
        }),
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: "file:42-50".to_owned(),
            lines: LineRange { start: 42, end: 50 },
            commit: resource("commit:deadbeef", ResourceKind::Commit, "deadbeef"),
            line_evidence_numbers: vec![1],
            production: Vec::new(),
        })],
        evidence: vec![event_evidence(1)],
        next: Some(BlameContinuation {
            cursor: "more-lines".to_owned(),
            reason: ContinuationReason::MoreCommittedLines,
        }),
    };
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_file_continuation.golden.txt")
    );
}

#[test]
fn empty_commit_page_has_a_concise_golden() {
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: resource("commit:abcdef", ResourceKind::Commit, "abcdef"),
            repository: repository(),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_empty.golden.txt")
    );
}

#[test]
fn ambiguous_commit_never_implies_an_asserted_producer_golden() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![commit_match(
            &commit,
            CommitFactType::Ambiguous,
            CommitPredicate::PossiblyProducedBy,
            "possible",
            FactConfidence::Ambiguous,
            FactState::Ambiguous,
            1,
        )],
        evidence: vec![event_evidence(1)],
        next: None,
    };
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_commit_ambiguous.golden.txt")
    );
}

#[test]
fn narrow_commit_uses_label_children_without_truncating_ids_golden() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let session_id = "session:018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2";
    let evidence = event_evidence(1);
    let event_id = evidence.citation.event_id.to_string();
    let mut produced = commit_match(
        &commit,
        CommitFactType::Produced,
        CommitPredicate::ProducedBy,
        "session-producer",
        FactConfidence::Explicit,
        FactState::Asserted,
        1,
    );
    if let BlameMatch::Commit(value) = &mut produced {
        value.object = Some(resource(
            session_id,
            ResourceKind::Session,
            "session-producer",
        ));
    }
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![produced],
        evidence: vec![evidence],
        next: None,
    };
    result.validate().unwrap();
    let rendered = render_plain(&result, 32);
    assert_eq!(
        rendered,
        include_str!("../../../testdata/pro/blame_commit_narrow.golden.txt")
    );
    assert!(rendered.contains(session_id));
    assert!(rendered.contains(&event_id));
}

#[test]
fn many_attributions_keep_two_space_ancestry_at_reference_widths() {
    let attributions = (2..=9)
        .map(|number| {
            attribution(
                &format!("fact:producer:{number}"),
                if number % 3 == 0 {
                    ProductionRelationship::PossiblyProducedBy
                } else {
                    ProductionRelationship::ProducedBy
                },
                &format!("producer-{number}"),
                number,
            )
        })
        .collect::<Vec<_>>();
    let result = BlameResult {
        target: ResolvedBlameTarget::File {
            path: "src/long/authored/path/to/the/implementation.rs".to_owned(),
            repository: repository(),
            requested_lines: None,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "deadbeef".to_owned(),
            worktree_status: WorktreeStatus::Differs,
        }),
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: "file:1".to_owned(),
            lines: LineRange { start: 1, end: 20 },
            commit: resource("commit:deadbeef", ResourceKind::Commit, "deadbeef"),
            line_evidence_numbers: vec![1],
            production: attributions,
        })],
        evidence: (1..=9).map(event_evidence).collect(),
        next: Some(BlameContinuation {
            cursor: "next-attribution-page".to_owned(),
            reason: ContinuationReason::MoreCommittedLines,
        }),
    };
    result.validate().unwrap();

    for width in [32, 48, 80, 120] {
        let rendered = render_plain(&result, width);
        assert!(!rendered.contains('\t'));
        assert_eq!(rendered.matches("  Produced by\n").count(), 1);
        assert_eq!(rendered.matches("  Possible producers\n").count(), 1);
        assert!(rendered.contains("    session producer-2\n"));
        assert!(rendered.contains("      state         asserted\n"));
        assert!(rendered.contains("      state         ambiguous\n"));

        let available = width - 1;
        for line in rendered.lines() {
            if line.width() > available {
                let value = line.trim_start();
                assert!(
                    value.starts_with("ctx show ")
                        || value.starts_with("ctx blame ")
                        || value == "src/long/authored/path/to/the/implementation.rs",
                    "unexpected overflow at width {width}: {line:?}"
                );
            }
        }
    }
}

#[test]
fn labels_wider_than_the_configured_column_stack_or_align_by_actual_width() {
    let narrow = context(32);
    let mut narrow_document = Document::new();
    super::layout::push_field(
        &mut narrow_document,
        &narrow,
        6,
        "occurred",
        10,
        "2024-07-14T23:33:20.000Z",
        Token::Text,
        true,
    );
    assert_eq!(
        narrow_document.render_plain(),
        "      occurred\n        2024-07-14T23:33:20.000Z\n"
    );

    let wide = context(48);
    let mut wide_document = Document::new();
    super::layout::push_field(
        &mut wide_document,
        &wide,
        6,
        "direct actor",
        10,
        "agent codex",
        Token::Text,
        true,
    );
    assert_eq!(
        wide_document.render_plain(),
        "      direct actor  agent codex\n"
    );
}

#[test]
fn core_evidence_keeps_generation_event_source_and_sequence() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let evidence = event_evidence(1);
    let expected_citation = format!(
        "ctx show event {} · Core {} · source {} · sequence {}",
        evidence.citation.event_id,
        &evidence.citation.core_generation_id[..12],
        evidence.citation.source.identity(),
        evidence.citation.event_sequence,
    );
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![commit_match(
            &commit,
            CommitFactType::Produced,
            CommitPredicate::ProducedBy,
            "producer",
            FactConfidence::Explicit,
            FactState::Asserted,
            1,
        )],
        evidence: vec![evidence],
        next: None,
    };
    result.validate().unwrap();
    let rendered = render_plain(&result, 120);
    assert!(rendered.contains(&expected_citation), "{rendered}");
}

#[test]
fn styled_output_strips_to_plain_and_plain_bytes_ignore_color() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: vec![commit_match(
            &commit,
            CommitFactType::Produced,
            CommitPredicate::ProducedBy,
            "producer",
            FactConfidence::Explicit,
            FactState::Asserted,
            1,
        )],
        evidence: vec![event_evidence(1)],
        next: None,
    };
    let styled_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always));
    let plain_document = render_blame_document(&result, &context(80));
    let styled_document = render_blame_document(&result, &styled_context);
    let styled = styled_document.render(&styled_context);
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(styled.as_bytes()).unwrap();
    let stripped = String::from_utf8(stripped.into_inner()).unwrap();

    assert_eq!(stripped, plain_document.render_plain());
    assert_eq!(
        styled_document.render_plain().len(),
        plain_document.render_plain().len()
    );
}
