use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    RepositoryFileObservationKind, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
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

use super::{
    layout::{enum_heading, enum_text},
    print_blame_result, print_blame_result_with_evidence_preview, render_blame_document,
    render_blame_document_with_evidence_preview,
};
use crate::pro::evidence_preview::{
    EvidencePreview, EvidencePreviewKind, EvidencePreviewModel, MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
};
use crate::ui::{ColorMode, Document, Line, RenderContext, StreamKind, TestContext, Token};

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

#[derive(Clone, Default)]
struct SharedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.bytes.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("shared preview writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn render_plain(result: &BlameResult, width: usize) -> String {
    render_blame_document(result, &context(width)).render_plain()
}

fn preview_result(file: bool, evidence_count: u32) -> BlameResult {
    let evidence = (1..=evidence_count).map(event_evidence).collect();
    if file {
        BlameResult {
            target: ResolvedBlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: repository(),
                requested_lines: None,
            },
            git_snapshot: Some(GitSnapshot {
                head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                worktree_status: WorktreeStatus::Clean,
            }),
            matches: Vec::new(),
            evidence,
            next: None,
        }
    } else {
        BlameResult {
            target: ResolvedBlameTarget::Commit {
                commit: resource(
                    "commit:0123456789abcdef0123456789abcdef01234567",
                    ResourceKind::Commit,
                    "0123456789abcdef0123456789abcdef01234567",
                ),
                repository: repository(),
            },
            git_snapshot: None,
            matches: Vec::new(),
            evidence,
            next: None,
        }
    }
}

fn preview(
    result: &BlameResult,
    numbers: Vec<u32>,
    kind: EvidencePreviewKind,
    excerpt: impl Into<String>,
) -> EvidencePreview {
    let citation = result
        .evidence
        .iter()
        .find(|evidence| Some(evidence.number) == numbers.first().copied())
        .unwrap();
    EvidencePreview {
        evidence_numbers: numbers,
        event_id: citation.citation.event_id,
        event_sequence: citation.citation.event_sequence,
        kind,
        excerpt: excerpt.into(),
    }
}

fn render_preview_plain(
    result: &BlameResult,
    model: &EvidencePreviewModel,
    width: usize,
) -> String {
    render_blame_document_with_evidence_preview(result, &context(width), Some(model)).render_plain()
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

#[test]
fn wire_enums_are_humanized_in_human_output() {
    assert_eq!(enum_text(CommitPredicate::ReferencedBy), "referenced by");
    assert_eq!(
        enum_heading(PullRequestCommitRelationship::ContainsCommit),
        "Contains commit"
    );
}

#[test]
fn opted_in_file_and_commit_previews_follow_their_numbered_evidence() {
    for file in [true, false] {
        let result = preview_result(file, 1);
        let kind = if file {
            EvidencePreviewKind::File(RepositoryFileObservationKind::Modified)
        } else {
            EvidencePreviewKind::Commit
        };
        let model = EvidencePreviewModel {
            previews: vec![preview(&result, vec![1], kind, "exact target-bearing unit")],
        };
        let rendered = render_preview_plain(&result, &model, 80);
        let evidence = rendered.find("\nEvidence\n").unwrap();
        let preview = rendered
            .find("\nEvidence preview (local history content; explicitly requested)\n")
            .unwrap();

        assert!(evidence < preview, "{rendered}");
        assert!(
            rendered.contains("Evidence preview (local history content; explicitly requested)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("    exact target-bearing unit"),
            "{rendered}"
        );
        let event_command = format!("ctx show event {}", result.evidence[0].citation.event_id);
        assert_eq!(
            rendered[preview..].matches(&event_command).count(),
            1,
            "{rendered}"
        );
        if file {
            assert!(rendered.contains("  [1] Modified file evidence\n"));
        } else {
            assert!(rendered.contains("  [1] Commit evidence\n"));
        }
    }
}

#[test]
fn grouped_preview_heading_wraps_references_and_kind_only_when_required() {
    let result = preview_result(true, 3);
    for reference_count in 1..=3usize {
        let numbers = (1..=u32::try_from(reference_count).unwrap()).collect::<Vec<_>>();
        let references = numbers
            .iter()
            .map(|number| format!("[{number}]"))
            .collect::<Vec<_>>()
            .join(" ");
        let model = EvidencePreviewModel {
            previews: vec![preview(
                &result,
                numbers.clone(),
                EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
                "exact unit",
            )],
        };

        for width in [32, 48, 80, 120] {
            let rendered = render_preview_plain(&result, &model, width);
            let section = rendered.split("\nEvidence preview ").nth(1).unwrap();
            let lines = section.lines().collect::<Vec<_>>();
            let reference_line = lines
                .iter()
                .position(|line| line.trim_start().starts_with("[1]"))
                .unwrap();
            let combined = format!("  {references} Modified file evidence");

            if combined.width() < width {
                assert_eq!(lines[reference_line], combined, "{reference_count}/{width}");
            } else {
                assert_eq!(
                    lines[reference_line],
                    format!("  {references}"),
                    "{reference_count}/{width}"
                );
                assert_eq!(
                    lines[reference_line + 1],
                    "    Modified file evidence",
                    "{reference_count}/{width}"
                );
            }
            assert!(lines[reference_line].width() <= width);
            for number in numbers.iter().map(|number| format!("[{number}]")) {
                assert_eq!(
                    section.matches(&number).count(),
                    1,
                    "reference {number} at {reference_count}/{width}: {section}"
                );
            }
        }
    }
}

#[test]
fn multiline_rename_and_commit_excerpts_preserve_indented_logical_lines() {
    let file_result = preview_result(true, 1);
    let file_model = EvidencePreviewModel {
        previews: vec![preview(
            &file_result,
            vec![1],
            EvidencePreviewKind::File(RepositoryFileObservationKind::Renamed),
            "*** Update File: src/old.rs\n*** Move to: src/lib.rs",
        )],
    };
    let file = render_preview_plain(&file_result, &file_model, 80);
    assert!(file.contains("  [1] Renamed file evidence\n"), "{file}");
    assert!(
        file.contains("    *** Update File: src/old.rs\n    *** Move to: src/lib.rs\n"),
        "{file}"
    );
    assert!(!file.contains("old.rs\\n***"), "{file}");

    let commit_result = preview_result(false, 1);
    let commit_model = EvidencePreviewModel {
        previews: vec![preview(
            &commit_result,
            vec![1],
            EvidencePreviewKind::Commit,
            "commit 0123456789abcdef\nAuthor: Example Agent\n\u{202e}subject",
        )],
    };
    let commit = render_preview_plain(&commit_result, &commit_model, 80);
    assert!(commit.contains("  [1] Commit evidence\n"), "{commit}");
    assert!(
        commit.contains(
            "    commit 0123456789abcdef\n    Author: Example Agent\n    \\u{202e}subject\n"
        ),
        "{commit}"
    );
    assert!(!commit.contains('\u{202e}'));
}

#[test]
fn multibyte_excerpt_limit_is_enforced_in_original_utf8_bytes() {
    let result = preview_result(true, 1);
    for bytes in [511usize, 512, 513] {
        let mut excerpt = "é".repeat(bytes / 2);
        if bytes % 2 == 1 {
            excerpt.push('x');
        }
        assert_eq!(excerpt.len(), bytes);
        let model = EvidencePreviewModel {
            previews: vec![preview(
                &result,
                vec![1],
                EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
                excerpt,
            )],
        };
        let rendered = render_preview_plain(&result, &model, 80);
        if bytes <= MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES {
            assert_eq!(
                rendered.matches('é').count(),
                bytes / 2,
                "{bytes}: {rendered}"
            );
            assert!(
                rendered
                    .split("\nEvidence preview ")
                    .nth(1)
                    .unwrap()
                    .contains("ctx show event "),
                "{bytes}: {rendered}"
            );
        } else {
            assert!(
                rendered.contains("requested but is unavailable"),
                "{bytes}: {rendered}"
            );
            assert!(
                !rendered
                    .split("\nEvidence preview ")
                    .nth(1)
                    .unwrap()
                    .contains("ctx show event "),
                "{bytes}: {rendered}"
            );
        }
    }
}

#[test]
fn rendered_preview_budget_accepts_4096_bytes_and_rejects_4097() {
    let context = context(80);
    let at_limit = Document::from_line(Line::text("x".repeat(4_095)));
    let over_limit = Document::from_line(Line::text("x".repeat(4_096)));
    assert_eq!(super::evidence::MAX_EVIDENCE_PREVIEW_RENDERED_BYTES, 4_096);
    assert_eq!(at_limit.render(&context).len(), 4_096);
    assert_eq!(over_limit.render(&context).len(), 4_097);
    assert!(super::evidence::within_rendered_preview_budget(
        &at_limit, &context
    ));
    assert!(!super::evidence::within_rendered_preview_budget(
        &over_limit,
        &context
    ));
}

#[test]
fn requested_but_unavailable_preview_is_content_free_and_default_output_is_unchanged() {
    let result = preview_result(false, 0);
    let default = render_blame_document(&result, &context(80)).render_plain();
    let requested = render_preview_plain(
        &result,
        &EvidencePreviewModel {
            previews: Vec::new(),
        },
        80,
    );

    assert!(!default.contains("Evidence preview"));
    assert!(requested.starts_with(default.trim_end()), "{requested}");
    assert!(requested.contains("Evidence preview (local history content; explicitly requested)"));
    assert!(requested.contains("Exact cited local-history evidence was requested"));
    assert!(requested.contains("unavailable"));
    assert!(requested.contains("result."));
    assert!(!requested.contains("generation"));
    assert!(!requested.contains("digest"));
}

#[test]
fn default_opt_out_bytes_are_identical_for_every_target_and_supported_width() {
    let results = [
        preview_result(true, 1),
        preview_result(false, 1),
        paginated_pr_result(true),
    ];
    for result in &results {
        for width in [32, 48, 80, 120] {
            for color in [ColorMode::Never, ColorMode::Always] {
                let context = RenderContext::for_test(
                    TestContext::tty(StreamKind::Stdout, width).color(color),
                );
                let default = render_blame_document(result, &context);
                let opt_out = render_blame_document_with_evidence_preview(result, &context, None);
                assert_eq!(
                    default.render(&context),
                    opt_out.render(&context),
                    "target {:?}, width {width}, color {color:?}",
                    result.target
                );
            }
        }
    }
}

#[test]
fn preview_cap_duplicate_grouping_and_aggregate_budget_are_enforced_without_truncation() {
    let result = preview_result(true, 5);
    let exact = "Z".repeat(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES);
    let mut previews = vec![preview(
        &result,
        vec![1, 2],
        EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
        exact.clone(),
    )];
    for number in 3..=5 {
        previews.push(preview(
            &result,
            vec![number],
            EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
            exact.clone(),
        ));
    }
    let model = EvidencePreviewModel { previews };
    let styled_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 32).color(ColorMode::Always));
    let mut section = Document::new();
    super::evidence::render_previews(&mut section, &styled_context, &model);
    let styled = section.render(&styled_context);
    let plain = section.render_plain();

    assert!(styled.len() <= super::evidence::MAX_EVIDENCE_PREVIEW_RENDERED_BYTES);
    assert_eq!(plain.matches("ctx show event ").count(), 3);
    assert!(
        plain.contains("  [1] [2]\n    Modified file evidence\n"),
        "{plain}"
    );
    assert_eq!(
        plain.chars().filter(|character| *character == 'Z').count(),
        3 * MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
        "one or more exact 512-byte excerpts were truncated"
    );
    assert!(
        !plain.contains("  [5] Modified file evidence\n"),
        "fourth preview was rendered"
    );
}

#[test]
fn ultra_narrow_contexts_preserve_grouped_references_and_full_event_command_atoms() {
    let result = preview_result(true, 3);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1, 2, 3],
            EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
            "exact unit",
        )],
    };
    let event_command = format!(
        "    ctx show event {}",
        result.evidence[0].citation.event_id
    );

    for width in [1, 2, 8, 16] {
        for color in [ColorMode::Never, ColorMode::Always] {
            let context =
                RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color));
            let mut section = Document::new();
            super::evidence::render_previews(&mut section, &context, &model);
            let rendered = section.render(&context);
            assert!(
                rendered.len() <= super::evidence::MAX_EVIDENCE_PREVIEW_RENDERED_BYTES,
                "{width}/{color:?}: {}",
                rendered.len()
            );
            let mut stripped = anstream::StripStream::new(Vec::new());
            stripped.write_all(rendered.as_bytes()).unwrap();
            let stripped = String::from_utf8(stripped.into_inner()).unwrap();
            assert_eq!(stripped, section.render_plain());
            assert!(
                stripped.contains("  [1] [2] [3]\n    Modified file evidence\n"),
                "{width}/{color:?}: {stripped}"
            );
            assert_eq!(stripped.matches(&event_command).count(), 1);
            for reference in ["[1]", "[2]", "[3]"] {
                assert_eq!(stripped.matches(reference).count(), 1);
            }
        }
    }
}

#[test]
fn ultra_narrow_actual_render_budget_omits_only_complete_preview_items() {
    let result = preview_result(true, 3);
    let model = EvidencePreviewModel {
        previews: (1..=3)
            .map(|number| {
                preview(
                    &result,
                    vec![number],
                    EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
                    "Z".repeat(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES),
                )
            })
            .collect(),
    };

    for width in [1, 2, 8, 16] {
        for color in [ColorMode::Never, ColorMode::Always] {
            let context =
                RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color));
            let mut section = Document::new();
            super::evidence::render_previews(&mut section, &context, &model);
            let rendered = section.render(&context);
            assert!(
                rendered.len() <= super::evidence::MAX_EVIDENCE_PREVIEW_RENDERED_BYTES,
                "{width}/{color:?}: {}",
                rendered.len()
            );
            let mut stripped = anstream::StripStream::new(Vec::new());
            stripped.write_all(rendered.as_bytes()).unwrap();
            let stripped = String::from_utf8(stripped.into_inner()).unwrap();
            let admitted = stripped.matches("ctx show event ").count();
            assert_eq!(
                stripped.matches('Z').count(),
                admitted * MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
                "partial excerpt at {width}/{color:?}: {stripped}"
            );
            for evidence in &result.evidence {
                let command = format!("ctx show event {}", evidence.citation.event_id);
                assert!(stripped.matches(&command).count() <= 1);
                let reference = format!("[{}]", evidence.number);
                assert!(stripped.matches(&reference).count() <= 1);
            }
            if width == 1 && color == ColorMode::Always {
                assert!(admitted < model.previews.len(), "{stripped}");
            }
        }
    }
}

#[test]
fn sanitizer_expansion_omits_the_complete_item_instead_of_truncating_it() {
    let result = preview_result(true, 1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
            "\0".repeat(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES),
        )],
    };
    let rendered = render_preview_plain(&result, &model, 80);

    assert!(
        rendered.contains("requested but is unavailable"),
        "{rendered}"
    );
    assert!(
        !rendered
            .split("\nEvidence preview ")
            .nth(1)
            .unwrap()
            .contains("ctx show event "),
        "{rendered}"
    );
    assert!(!rendered.contains("\\u{0000}"), "{rendered}");
}

#[test]
fn preview_is_safe_and_stable_at_supported_widths_and_across_color() {
    let result = preview_result(true, 1);
    let family = "👨‍👩‍👧‍👦";
    let persian = "می‌روم";
    let combining = "e\u{0301}";
    let excerpt = format!("{family} {persian} {combining} bad\u{202e}name\u{1b}\tend");
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            EvidencePreviewKind::File(RepositoryFileObservationKind::Modified),
            excerpt,
        )],
    };

    for width in [32, 48, 80, 120] {
        let plain_context = context(width);
        let styled_context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Always),
        );
        let plain_document =
            render_blame_document_with_evidence_preview(&result, &plain_context, Some(&model));
        let styled_document =
            render_blame_document_with_evidence_preview(&result, &styled_context, Some(&model));
        let plain = plain_document.render_plain();
        let styled = styled_document.render(&styled_context);
        let mut stripped = anstream::StripStream::new(Vec::new());
        stripped.write_all(styled.as_bytes()).unwrap();
        let stripped = String::from_utf8(stripped.into_inner()).unwrap();

        assert_eq!(stripped, plain, "width {width}");
        for legitimate in [family, persian, combining] {
            assert!(plain.contains(legitimate), "width {width}: {plain}");
        }
        for visible in ["\\u{202e}", "\\x1b", "\\t"] {
            assert!(plain.contains(visible), "width {width}: {plain}");
        }
        assert!(!plain.contains('\u{202e}'));
        assert!(!plain.contains('\u{1b}'));
        let event_id = result.evidence[0].citation.event_id.to_string();
        let event_command = format!("ctx show event {event_id}");
        let preview_section = plain.split("\nEvidence preview ").nth(1).unwrap();
        assert_eq!(
            preview_section.matches(&event_command).count(),
            1,
            "width {width}"
        );
        for line in preview_section.lines() {
            assert!(
                line.width() < width || line.contains(&event_id),
                "width {width} overflow: {line:?}"
            );
        }
    }
}

#[test]
fn preview_bytes_are_accounted_and_json_bytes_remain_exactly_unchanged() {
    let result = preview_result(false, 1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            EvidencePreviewKind::Commit,
            "exact commit evidence",
        )],
    };
    let default_bytes =
        crate::ui::canonical_human_output_bytes(|context| render_blame_document(&result, context));

    for color in [ColorMode::Never, ColorMode::Always] {
        let writer = SharedWriter::default();
        let captured = writer.clone();
        let stdout_context =
            RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(color));
        let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
        let mut ui =
            crate::ui::Ui::with_writers(writer, stdout_context, io::sink(), stderr_context);
        let measured =
            print_blame_result_with_evidence_preview(&result, false, &model, &mut ui).unwrap();
        ui.flush().unwrap();
        assert!(captured.text().contains("Evidence preview"));
        assert!(measured > default_bytes);
        assert_eq!(
            measured,
            crate::ui::canonical_human_output_bytes(|context| {
                render_blame_document_with_evidence_preview(&result, context, Some(&model))
            })
        );
    }

    let writer = SharedWriter::default();
    let captured = writer.clone();
    let pipe = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let mut ui = crate::ui::Ui::with_writers(writer, pipe, io::sink(), pipe);
    let measured =
        print_blame_result_with_evidence_preview(&result, true, &model, &mut ui).unwrap();
    ui.flush().unwrap();
    let mut expected = serde_json::to_string_pretty(&result).unwrap();
    expected.push('\n');
    assert_eq!(captured.text(), expected);
    assert_eq!(measured, expected.len());

    let mut default_ui = crate::ui::Ui::with_writers(io::sink(), pipe, io::sink(), pipe);
    assert_eq!(
        print_blame_result(&result, true, &mut default_ui).unwrap(),
        expected.len()
    );
}
