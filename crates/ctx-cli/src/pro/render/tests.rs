use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    RepositoryFileInvocationKind, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
};
use ctx_pro_host_protocol::{
    AgentAttribution, BlameAttribution, BlameContinuation, BlameCoverage, BlameCoverageUnit,
    BlameMatch, BlameOutcome, BlameResult, CommitBlameMatch, CommitFactType, CommitLineage,
    CommitLineageBounds, CommitLineageEdge, CommitLineageOmission, CommitLineageOperationKind,
    CommitLineageProofClass, CommitLineageRelationClass, CommitLineageState,
    CommitLineageTruncationReason, CommitLineageYield, CommitPredicate, ContinuationReason,
    EvidenceCitation, ExactCommitRef, FactConfidence, FactState, FileBlameMatch, GitObjectFormat,
    GitSnapshot, LineRange, NumberedEvidence, ProductionRelationship, PullRequestAction,
    PullRequestActivity, PullRequestBlameMatch, PullRequestBlameRelationship, PullRequestCommit,
    PullRequestCommitRelationship, ResolvedBlameTarget, ResourceKind, ResourceRef,
    ScopedCommitEndpoint, WorktreeStatus, MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
    MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use super::{
    layout::{enum_heading, enum_text},
    BlameEvidenceContext,
};
use crate::pro::evidence_preview::{
    EvidencePreview, EvidencePreviewModel, MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
};
use crate::ui::{ColorMode, Document, Line, RenderContext, StreamKind, TestContext, Token};

fn protocol_snapshot() -> ctx_pro_host_protocol::QuerySnapshotExpectation {
    ctx_pro_host_protocol::QuerySnapshotExpectation::Core {
        receipt: ctx_pro_host_protocol::CoreMaterializationReceiptIdentity {
            core_generation_id: "a".repeat(64),
            materializer_revision: "materializer-v1".to_owned(),
        },
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
    let attribution = if conflicting > 0 {
        BlameAttribution::Conflicting
    } else if evaluated > 0 && proven == evaluated {
        BlameAttribution::Proven
    } else if proven > 0 || possible > 0 {
        BlameAttribution::Possible
    } else {
        BlameAttribution::None
    };
    BlameOutcome {
        attribution,
        coverage: BlameCoverage {
            unit,
            evaluated,
            proven,
            possible,
            conflicting,
            none,
        },
    }
}

fn current(result: BlameResult) -> crate::pro::HostedBlameResult {
    crate::pro::HostedBlameResult {
        result,
        freshness: crate::pro::BlameResultFreshness::Current,
    }
}

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

fn empty_context(result: &BlameResult) -> BlameEvidenceContext {
    if matches!(&result.target, ResolvedBlameTarget::File { .. }) {
        BlameEvidenceContext::for_file(EvidencePreviewModel {
            previews: Vec::new(),
        })
    } else {
        BlameEvidenceContext::not_applicable()
    }
}

fn render_blame_document(result: &BlameResult, context: &RenderContext) -> Document {
    super::render_blame_document(result, context, &empty_context(result))
}

fn render_blame_document_with_evidence_preview(
    result: &BlameResult,
    context: &RenderContext,
    previews: Option<&EvidencePreviewModel>,
) -> Document {
    let evidence_context = previews.map_or_else(
        || empty_context(result),
        |model| BlameEvidenceContext::for_file(model.clone()),
    );
    super::render_blame_document(result, context, &evidence_context)
}

fn print_blame_result_with_evidence_preview(
    result: &BlameResult,
    json_output: bool,
    previews: &EvidencePreviewModel,
    ui: &mut crate::ui::Ui,
) -> anyhow::Result<usize> {
    super::print_blame_result_with_evidence_preview(
        &current(result.clone()),
        json_output,
        previews,
        ui,
    )
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

fn file_preview_result(evidence_count: u32) -> BlameResult {
    let evidence = (1..=evidence_count).map(event_evidence).collect();
    BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: repository(),
            requested_lines: None,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        outcome: outcome(BlameCoverageUnit::CommittedLine, 0, 0, 0, 0),
        matches: Vec::new(),
        evidence,
        next: None,
        lineage: None,
    }
}

fn commit_blame_result(evidence_count: u32) -> BlameResult {
    BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: resource(
                "commit:0123456789abcdef0123456789abcdef01234567",
                ResourceKind::Commit,
                "0123456789abcdef0123456789abcdef01234567",
            ),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 0, 0, 0, 0),
        matches: Vec::new(),
        evidence: (1..=evidence_count).map(event_evidence).collect(),
        next: None,
        lineage: None,
    }
}

#[test]
fn current_none_result_offers_the_same_safe_core_search_in_human_and_json() {
    let hosted = current(commit_blame_result(0));
    let expected_argv = serde_json::json!([
        "ctx",
        "search",
        "0123456789abcdef0123456789abcdef01234567",
        "--refresh",
        "off"
    ]);
    let json = super::blame_result_json(&hosted, None);
    assert_eq!(json["next_action"]["kind"], "search_core");
    assert_eq!(json["next_action"]["argv"], expected_argv);

    let document = super::render_blame_document(
        &hosted,
        &context(80),
        &BlameEvidenceContext::not_applicable(),
    );
    let human = document.render_plain();
    assert!(human.contains("No producer proven"), "{human}");
    assert!(
        human.contains("ctx search 0123456789abcdef0123456789abcdef01234567 --refresh off"),
        "{human}"
    );
}

fn preview(
    result: &BlameResult,
    numbers: Vec<u32>,
    operation: RepositoryFileInvocationKind,
    excerpt: impl Into<String>,
) -> EvidencePreview {
    let path = match &result.target {
        ResolvedBlameTarget::File { path, .. } => path.clone(),
        ResolvedBlameTarget::Commit { .. } | ResolvedBlameTarget::PullRequest { .. } => {
            "src/lib.rs".to_owned()
        }
    };
    EvidencePreview {
        citation_numbers: numbers,
        operation,
        path,
        prior_path: matches!(operation, RepositoryFileInvocationKind::Rename)
            .then(|| "src/old.rs".to_owned()),
        tool_name: "test_tool".to_owned(),
        event_occurred_at_ms: Some(1_721_000_000_000),
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

fn strip_ansi(rendered: &str) -> String {
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stripped.into_inner()).unwrap()
}

fn single_preview_excerpt_fragments(rendered: &str) -> Vec<&str> {
    let lines = rendered.lines().collect::<Vec<_>>();
    let event_time = lines
        .iter()
        .position(|line| line.trim_start().starts_with("Event time"))
        .unwrap();
    let excerpt_start = event_time
        + if lines[event_time].trim() == "Event time" {
            2
        } else {
            1
        };
    lines[excerpt_start..]
        .iter()
        .map(|line| line.strip_prefix("    ").unwrap())
        .collect()
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
        parent_session: None,
        direct_actor: None,
        owning_root: None,
        fact_occurred_at_ms: None,
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

#[test]
fn direct_commit_blame_shows_its_parent_session() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let mut item = commit_match(
        &commit,
        CommitFactType::Produced,
        CommitPredicate::ProducedBy,
        "worker",
        FactConfidence::Explicit,
        FactState::Asserted,
        1,
    );
    let BlameMatch::Commit(value) = &mut item else {
        unreachable!();
    };
    value.parent_session = Some(resource(
        "session:manager",
        ResourceKind::Session,
        "manager",
    ));
    value.owning_root = Some(resource("run:root", ResourceKind::Run, "root"));
    let result = BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit,
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0),
        matches: vec![item],
        evidence: vec![event_evidence(1)],
        next: None,
        lineage: None,
    };

    let rendered = render_plain(&result, 80);
    assert!(
        rendered.contains("parent        session manager"),
        "{rendered}"
    );
    assert!(rendered.contains("owning root   run root"), "{rendered}");
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
        parent_session: None,
        fact_occurred_at_ms: None,
        confidence,
        state,
        direct_actor: None,
        owning_root: None,
        evidence_numbers: vec![evidence_number],
    })
}

fn exact_commit(digit: char) -> ExactCommitRef {
    let oid = digit.to_string().repeat(40);
    ExactCommitRef {
        resource: resource(&format!("commit:{oid}"), ResourceKind::Commit, &oid),
        logical_repository_id: "ctxrs/ctx".to_owned(),
        object_format: GitObjectFormat::Sha1,
        oid,
    }
}

fn lineage_edge(
    source: ExactCommitRef,
    result: ExactCommitRef,
    state: CommitLineageState,
) -> CommitLineageEdge {
    CommitLineageEdge {
        operation_id: "a".repeat(64),
        kind: CommitLineageOperationKind::Rebase,
        relation_class: CommitLineageRelationClass::Replacement,
        source,
        result,
        actor: resource("session:rebaser", ResourceKind::Session, "rebaser"),
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state,
        observed_at_ms: Some(1_721_000_000_000),
        evidence_numbers: vec![1],
    }
}

fn complete_lineage_result() -> BlameResult {
    let requested = exact_commit('3');
    let source = exact_commit('1');
    let commit = requested.resource.clone();
    BlameResult {
        snapshot: protocol_snapshot(),
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
                "rebaser",
                FactConfidence::Explicit,
                FactState::Asserted,
                1,
            ),
            commit_match(
                &commit,
                CommitFactType::Referenced,
                CommitPredicate::ReferencedBy,
                "observer",
                FactConfidence::Explicit,
                FactState::Asserted,
                2,
            ),
        ],
        evidence: (1..=2).map(event_evidence).collect(),
        next: None,
        lineage: Some(CommitLineage {
            requested: requested.clone(),
            edges: vec![lineage_edge(
                source.clone(),
                requested.clone(),
                CommitLineageState::Asserted,
            )],
            yielded_by: Vec::new(),
            origin: Some(source),
            endpoint: Some(ScopedCommitEndpoint::CurrentAtRef {
                commit: requested,
                scope: resource(
                    "branch:refs/heads/main",
                    ResourceKind::Branch,
                    "refs/heads/main",
                ),
                observation_id: "observation:main-1".to_owned(),
                observed_at_ms: 1_721_000_001_000,
                evidence_numbers: vec![2],
            }),
            complete: true,
            ambiguous: false,
            bounds: CommitLineageBounds {
                returned_events: 1,
                returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
                examined_events: 2,
                examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
                omission: CommitLineageOmission::Exact(0),
                truncation_reason: None,
            },
        }),
    }
}

fn plural_lineage_result() -> BlameResult {
    let mut result = complete_lineage_result();
    let lineage = result.lineage.as_mut().unwrap();
    lineage.edges.push(lineage_edge(
        exact_commit('2'),
        exact_commit('4'),
        CommitLineageState::Asserted,
    ));
    lineage.origin = None;
    result
}

fn partial_lineage_result(omission: CommitLineageOmission) -> BlameResult {
    let requested = exact_commit('3');
    let source = exact_commit('1');
    BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: requested.resource.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: vec![event_evidence(1)],
        next: None,
        lineage: Some(CommitLineage {
            requested: requested.clone(),
            edges: vec![lineage_edge(
                source,
                requested,
                CommitLineageState::Ambiguous,
            )],
            yielded_by: Vec::new(),
            origin: None,
            endpoint: None,
            complete: false,
            ambiguous: true,
            bounds: CommitLineageBounds {
                returned_events: 1,
                returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
                examined_events: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
                examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
                omission,
                truncation_reason: Some(CommitLineageTruncationReason::ExaminedEventLimit),
            },
        }),
    }
}

#[test]
fn commit_renderer_keeps_production_grouping_golden() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let result = BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 1, 1, 0, 1),
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
        lineage: None,
    };
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_commit.golden.txt")
    );
}

#[test]
fn commit_lineage_complete_human_output_is_exact_and_deduplicates_yield_actor() {
    let result = complete_lineage_result();
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert_eq!(
        rendered,
        include_str!("../../../testdata/pro/blame_commit_lineage_complete.golden.txt")
    );
    assert_eq!(rendered.matches("session rebaser").count(), 1, "{rendered}");
    assert!(!rendered.contains("Produced by"), "{rendered}");
    assert!(!rendered.contains("created"), "{rendered}");
    assert!(!rendered.contains("implemented by"), "{rendered}");
}

#[test]
fn plural_mappings_render_as_one_deterministic_operation_with_copyable_ids() {
    let result = plural_lineage_result();
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert_eq!(
        rendered,
        include_str!("../../../testdata/pro/blame_commit_lineage_plural.golden.txt")
    );
    assert_eq!(rendered.matches("Rebase · replacement").count(), 1);
    assert_eq!(rendered.matches(&"a".repeat(64)).count(), 1);
    assert!(rendered.contains(&"1".repeat(40)), "{rendered}");
    assert!(rendered.contains(&"2".repeat(40)), "{rendered}");
    assert!(rendered.contains(&"3".repeat(40)), "{rendered}");
    assert!(rendered.contains(&"4".repeat(40)), "{rendered}");
    assert!(rendered.contains("1 operation"), "{rendered}");
    assert!(rendered.contains("2 mappings"), "{rendered}");

    for width in [32, 48, 80, 120] {
        let width_rendered = render_plain(&result, width);
        for id in [
            "a".repeat(64),
            "1".repeat(40),
            "2".repeat(40),
            "3".repeat(40),
            "4".repeat(40),
        ] {
            assert!(
                width_rendered.contains(&id),
                "width {width}: {width_rendered}"
            );
        }
    }

    let mut reversed = result;
    reversed.lineage.as_mut().unwrap().edges.reverse();
    assert_eq!(render_plain(&reversed, 80), rendered);
}

#[test]
fn commit_lineage_partial_human_output_is_exact_and_abstains() {
    let result = partial_lineage_result(CommitLineageOmission::AtLeast(2));
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert_eq!(
        rendered,
        include_str!("../../../testdata/pro/blame_commit_lineage_partial.golden.txt")
    );
    assert!(!rendered.contains("operation yielded"), "{rendered}");
    assert!(
        rendered.contains("operation yield is ambiguous"),
        "{rendered}"
    );
}

#[test]
fn contradicted_lineage_never_affirms_an_operation_yield() {
    let mut result = partial_lineage_result(CommitLineageOmission::AtLeast(1));
    result.lineage.as_mut().unwrap().edges[0].state = CommitLineageState::Contradicted;
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert!(!rendered.contains("operation yielded"), "{rendered}");
    assert!(
        rendered.contains("operation yield is contradicted"),
        "{rendered}"
    );
}

#[test]
fn commit_lineage_omission_counts_are_only_shown_when_supported() {
    for (omission, expected, rejected) in [
        (
            CommitLineageOmission::Exact(2),
            "More proven lineage may be omitted: 2 operation events.",
            "at least 2",
        ),
        (
            CommitLineageOmission::AtLeast(2),
            "More proven lineage may be omitted: at least 2 operation events.",
            "omitted: 2 operation events",
        ),
        (
            CommitLineageOmission::Unknown,
            "More proven lineage may be omitted.",
            "omitted:",
        ),
    ] {
        let result = partial_lineage_result(omission);
        result.validate().unwrap();
        let rendered = render_plain(&result, 80);
        assert!(rendered.contains(expected), "{rendered}");
        assert!(!rendered.contains(rejected), "{rendered}");
    }
}

#[test]
fn commit_lineage_json_is_the_unmodified_protocol_value() {
    let result = complete_lineage_result();
    let rendered = super::blame_result_json(&result, None);
    assert_eq!(
        rendered["lineage"],
        serde_json::to_value(result.lineage.as_ref().unwrap()).unwrap()
    );
    assert_eq!(rendered["matches"].as_array().map(Vec::len), Some(2));
    assert_eq!(rendered["next"], serde_json::Value::Null);
}

#[test]
fn commit_lineage_keeps_paginated_production_for_a_different_exact_object() {
    let mut result = complete_lineage_result();
    let source = exact_commit('1').resource;
    result.matches.push(commit_match(
        &source,
        CommitFactType::Produced,
        CommitPredicate::ProducedBy,
        "source-producer",
        FactConfidence::Explicit,
        FactState::Asserted,
        1,
    ));
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert!(rendered.contains("Also recorded"), "{rendered}");
    assert!(rendered.contains("  Produced by"), "{rendered}");
    assert!(rendered.contains("session source-producer"), "{rendered}");
}

#[test]
fn standalone_yield_is_rendered_only_as_a_yield_record() {
    let mut result = complete_lineage_result();
    let lineage = result.lineage.as_mut().unwrap();
    lineage.edges.clear();
    lineage.yielded_by = vec![CommitLineageYield {
        yield_id: "yield:requested".to_owned(),
        operation_id: "b".repeat(64),
        logical_repository_id: lineage.requested.logical_repository_id.clone(),
        actor: resource("session:rebaser", ResourceKind::Session, "rebaser"),
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state: CommitLineageState::Asserted,
        observed_at_ms: Some(1_721_000_000_000),
        evidence_numbers: vec![1],
    }];
    lineage.origin = Some(lineage.requested.clone());
    lineage.bounds.examined_events = 1;
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert!(
        rendered.contains("Yield operation · 1 yield record"),
        "{rendered}"
    );
    assert!(!rendered.contains("Rebase · replacement"), "{rendered}");
    assert_eq!(rendered.matches("session rebaser").count(), 1, "{rendered}");
}

#[test]
fn non_asserted_standalone_yields_never_use_affirmative_wording() {
    for (state, expected) in [
        (
            CommitLineageState::Ambiguous,
            "operation yield is ambiguous",
        ),
        (
            CommitLineageState::Contradicted,
            "operation yield is contradicted",
        ),
    ] {
        let mut result = complete_lineage_result();
        let lineage = result.lineage.as_mut().unwrap();
        lineage.edges.clear();
        lineage.yielded_by = vec![CommitLineageYield {
            yield_id: "yield:requested".to_owned(),
            operation_id: "b".repeat(64),
            logical_repository_id: lineage.requested.logical_repository_id.clone(),
            actor: resource("session:rebaser", ResourceKind::Session, "rebaser"),
            proof_class: CommitLineageProofClass::RepositoryVerified,
            state,
            observed_at_ms: Some(1_721_000_000_000),
            evidence_numbers: vec![1],
        }];
        lineage.origin = None;
        lineage.endpoint = None;
        lineage.ambiguous = true;
        lineage.bounds.examined_events = 1;
        result.validate().unwrap();
        let rendered = render_plain(&result, 80);
        assert!(!rendered.contains("operation yielded"), "{rendered}");
        assert!(rendered.contains(expected), "{rendered}");
    }
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
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::PullRequest {
            selector: "https://gitlab.example.com/ctxrs/ctx/-/merge_requests/42".to_owned(),
            pull_request: pull_request.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::PullRequestRelationship, 2, 0, 0, 0),
        matches: vec![
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request: pull_request.clone(),
                relationship: PullRequestBlameRelationship::Commit(PullRequestCommit {
                    fact_id: "fact:membership".to_owned(),
                    relationship: PullRequestCommitRelationship::ContainsCommit,
                    commit: resource("commit:deadbeef", ResourceKind::Commit, "deadbeef"),
                    fact_occurred_at_ms: Some(1_721_000_000_500),
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
        lineage: None,
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
            fact_occurred_at_ms: None,
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
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request: pull_request.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: if commit_page {
            outcome(BlameCoverageUnit::PullRequestRelationship, 0, 0, 0, 1)
        } else {
            outcome(BlameCoverageUnit::PullRequestRelationship, 1, 0, 0, 0)
        },
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
        lineage: None,
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

fn paginated_file_result() -> BlameResult {
    BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: repository(),
            requested_lines: Some(LineRange { start: 42, end: 60 }),
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "deadbeef".to_owned(),
            worktree_status: WorktreeStatus::Differs,
        }),
        outcome: outcome(BlameCoverageUnit::CommittedLine, 0, 0, 0, 9),
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
        lineage: None,
    }
}

#[test]
fn file_continuation_uses_committed_window_golden() {
    let result = paginated_file_result();
    result.validate().unwrap();
    assert_eq!(
        render_plain(&result, 80),
        include_str!("../../../testdata/pro/blame_file_continuation.golden.txt")
    );
}

#[test]
fn unavailable_file_context_keeps_the_plain_continuation_command() {
    let result = paginated_file_result();
    result.validate().unwrap();
    assert_eq!(
        render_preview_plain(
            &result,
            &EvidencePreviewModel {
                previews: Vec::new(),
            },
            80,
        ),
        include_str!("../../../testdata/pro/blame_file_continuation.golden.txt")
    );
}

#[test]
fn empty_commit_page_has_a_concise_golden() {
    let result = BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: resource("commit:abcdef", ResourceKind::Commit, "abcdef"),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 0, 0, 0, 0),
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
        lineage: None,
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
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 0, 1, 0, 0),
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
        lineage: None,
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
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0),
        matches: vec![produced],
        evidence: vec![evidence],
        next: None,
        lineage: None,
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
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::File {
            path: "src/long/authored/path/to/the/implementation.rs".to_owned(),
            repository: repository(),
            requested_lines: None,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "deadbeef".to_owned(),
            worktree_status: WorktreeStatus::Differs,
        }),
        outcome: outcome(BlameCoverageUnit::CommittedLine, 0, 0, 20, 0),
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
        lineage: None,
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
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0),
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
        lineage: None,
    };
    result.validate().unwrap();
    let rendered = render_plain(&result, 120);
    assert!(rendered.contains(&expected_citation), "{rendered}");
}

#[test]
fn styled_output_strips_to_plain_and_plain_bytes_ignore_color() {
    let commit = resource("commit:abcdef", ResourceKind::Commit, "abcdef");
    let result = BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: repository(),
        },
        git_snapshot: None,
        outcome: outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0),
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
        lineage: None,
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
fn default_file_context_follows_its_numbered_evidence() {
    let result = file_preview_result(1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
            "exact target-bearing unit",
        )],
    };
    let rendered = render_preview_plain(&result, &model, 80);
    let evidence = rendered.find("\nEvidence\n").unwrap();
    let preview = rendered
        .find("\nEvidence context (local history content)\n")
        .unwrap();

    assert!(evidence < preview, "{rendered}");
    assert!(
        rendered.contains("Evidence context (local history content)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("    exact target-bearing unit"),
        "{rendered}"
    );
    let event_command = format!("ctx show event {}", result.evidence[0].citation.event_id);
    assert_eq!(
        rendered[preview..].matches(&event_command).count(),
        0,
        "{rendered}"
    );
    assert!(rendered.contains("  [1] Modify file request via test_tool\n"));
    assert!(rendered.contains("Path        src/lib.rs"), "{rendered}");
    assert!(rendered.contains("Event time  2024-"), "{rendered}");
}

#[test]
fn grouped_preview_heading_wraps_references_and_kind_only_when_required() {
    let result = file_preview_result(3);
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
                RepositoryFileInvocationKind::Modify,
                "exact unit",
            )],
        };

        for width in [32, 48, 80, 120] {
            let rendered = render_preview_plain(&result, &model, width);
            let section = &rendered[rendered.find("Evidence context").unwrap()..];
            let lines = section.lines().collect::<Vec<_>>();
            let reference_line = lines
                .iter()
                .position(|line| line.trim_start().starts_with("[1]"))
                .unwrap();
            let combined = format!("  {references} Modify file request via test_tool");

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
                    "    Modify file request via test_tool",
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
fn multiline_rename_excerpt_preserves_indented_logical_lines() {
    let file_result = file_preview_result(1);
    let file_model = EvidencePreviewModel {
        previews: vec![preview(
            &file_result,
            vec![1],
            RepositoryFileInvocationKind::Rename,
            "*** Update File: src/old.rs\n*** Move to: src/lib.rs",
        )],
    };
    let file = render_preview_plain(&file_result, &file_model, 80);
    assert!(
        file.contains("  [1] Rename file request via test_tool\n"),
        "{file}"
    );
    assert!(
        file.contains("Path        src/old.rs → src/lib.rs"),
        "{file}"
    );
    assert!(
        file.contains("    *** Update File: src/old.rs\n    *** Move to: src/lib.rs\n"),
        "{file}"
    );
    assert!(!file.contains("old.rs\\n***"), "{file}");
}

#[test]
fn missing_preview_time_is_quiet_and_typed_values_use_global_sanitization() {
    let result = file_preview_result(1);
    let mut item = preview(
        &result,
        vec![1],
        RepositoryFileInvocationKind::Modify,
        "history\u{202e}\u{1b}",
    );
    item.event_occurred_at_ms = None;
    item.path = "src/\u{202e}lib.rs\u{1b}".to_owned();
    item.tool_name = "edit\u{202e}\u{1b}".to_owned();
    let rendered = render_preview_plain(
        &result,
        &EvidencePreviewModel {
            previews: vec![item],
        },
        120,
    );

    assert!(!rendered.contains("Event time"), "{rendered}");
    assert!(rendered.contains("src/\u{202e}lib.rs\\x1b"), "{rendered}");
    assert!(rendered.contains("edit\u{202e}\\x1b"), "{rendered}");
    assert!(rendered.contains("history\\u{202e}\\x1b"), "{rendered}");
}

#[test]
fn preview_preserves_sanitized_whitespace_and_control_escapes_exactly() {
    let result = file_preview_result(1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
            "modified: src/a  b.rs\n 2 files changed, 3 insertions(+), 1 deletion(-)  \n\tstatus:\0  keep\tgap\u{202e}\u{1b}  ",
        )],
    };
    let rendered = render_preview_plain(&result, &model, 120);

    assert_eq!(
        single_preview_excerpt_fragments(&rendered),
        [
            "modified: src/a  b.rs",
            " 2 files changed, 3 insertions(+), 1 deletion(-)  ",
            "\\tstatus:\\u{0000}  keep\\tgap\\u{202e}\\x1b  ",
        ]
    );
    assert!(!rendered.contains('\0'));
    assert!(!rendered.contains('\u{202e}'));
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn evidence_renderer_escapes_strict_format_controls_and_preserves_text_shaping() {
    const CONTROLS: &str = "\u{2028}\u{2029}\u{2061}\u{2062}\u{2063}\u{2064}";
    const PRESERVED: &str = "می\u{200c}روم 👩\u{200d}💻 ✈\u{fe0f} e\u{0301} مرحبا";
    let result = file_preview_result(1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
            format!("modified: src/lib.rs {CONTROLS} {PRESERVED}"),
        )],
    };
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let rendered =
        render_blame_document_with_evidence_preview(&result, &context, Some(&model)).render_plain();

    assert!(
        rendered.contains(
            "modified: src/lib.rs \\u{2028}\\u{2029}\\u{2061}\\u{2062}\\u{2063}\\u{2064}"
        ),
        "{rendered}"
    );
    assert!(rendered.contains(PRESERVED), "{rendered}");
    assert!(!CONTROLS.chars().any(|control| rendered.contains(control)));
    for preserved_escape in ["\\u{200c}", "\\u{200d}", "\\u{fe0f}", "\\u{0301}"] {
        assert!(!rendered.contains(preserved_escape), "{rendered}");
    }
}

#[test]
fn preview_wraps_long_family_emoji_path_only_at_grapheme_boundaries() {
    let result = file_preview_result(1);
    let family = "👨‍👩‍👧‍👦";
    let combining = "e\u{0301}";
    let excerpt = format!("src/{}  /{}.rs", family.repeat(16), combining.repeat(12));
    assert!(excerpt.len() <= MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
            &excerpt,
        )],
    };
    let mut grapheme_boundaries = excerpt
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    grapheme_boundaries.push(excerpt.len());

    for width in [1, 2, 8, 16, 32, 48, 80, 120] {
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
            let stripped = strip_ansi(&rendered);
            let fragments = single_preview_excerpt_fragments(&stripped);
            assert_eq!(fragments.concat(), excerpt, "{width}/{color:?}");
            let mut consumed = 0usize;
            for fragment in fragments {
                consumed = consumed.saturating_add(fragment.len());
                assert!(
                    grapheme_boundaries.contains(&consumed),
                    "split grapheme at byte {consumed} for {width}/{color:?}"
                );
            }
        }
    }
}

#[test]
fn multibyte_excerpt_limit_is_enforced_in_original_utf8_bytes() {
    let result = file_preview_result(1);
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
                RepositoryFileInvocationKind::Modify,
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
                rendered.contains("Modify file request via test_tool"),
                "{bytes}: {rendered}"
            );
        } else {
            assert!(
                !rendered.contains("Evidence context"),
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
fn unavailable_context_is_omitted_and_default_output_is_unchanged() {
    let result = file_preview_result(0);
    let default = render_blame_document(&result, &context(80)).render_plain();
    let requested = render_preview_plain(
        &result,
        &EvidencePreviewModel {
            previews: Vec::new(),
        },
        80,
    );

    assert!(!default.contains("Evidence context"));
    assert_eq!(requested, default);
}

#[test]
fn absent_context_preserves_base_bytes_for_every_target_and_supported_width() {
    let results = [
        file_preview_result(1),
        commit_blame_result(1),
        paginated_pr_result(true),
    ];
    for result in &results {
        for width in [32, 48, 80, 120] {
            for color in [ColorMode::Never, ColorMode::Always] {
                let context = RenderContext::for_test(
                    TestContext::tty(StreamKind::Stdout, width).color(color),
                );
                let default = render_blame_document(result, &context);
                let absent = render_blame_document_with_evidence_preview(result, &context, None);
                assert_eq!(
                    default.render(&context),
                    absent.render(&context),
                    "target {:?}, width {width}, color {color:?}",
                    result.target
                );
            }
        }
    }
}

#[test]
fn preview_cap_duplicate_grouping_and_aggregate_budget_are_enforced_without_truncation() {
    let result = file_preview_result(5);
    let exact = "X".repeat(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES);
    let mut previews = vec![preview(
        &result,
        vec![1, 2],
        RepositoryFileInvocationKind::Modify,
        exact.clone(),
    )];
    for number in 3..=5 {
        previews.push(preview(
            &result,
            vec![number],
            RepositoryFileInvocationKind::Modify,
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
    assert_eq!(
        plain.matches("Modify file request via test_tool").count(),
        3
    );
    assert!(
        plain.contains("  [1] [2]\n    Modify file request via test_tool\n"),
        "{plain}"
    );
    assert_eq!(
        plain.chars().filter(|character| *character == 'X').count(),
        3 * MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
        "one or more exact 512-byte excerpts were truncated"
    );
    assert!(
        !plain.contains("  [5] Modify file request via test_tool\n"),
        "fourth preview was rendered"
    );
}

#[test]
fn ultra_narrow_contexts_preserve_grouped_references_and_exact_excerpt_atoms() {
    let result = file_preview_result(3);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1, 2, 3],
            RepositoryFileInvocationKind::Modify,
            "exact unit",
        )],
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
            assert_eq!(stripped, section.render_plain());
            assert!(
                stripped.contains("  [1] [2] [3]\n    Modify file request via test_tool\n"),
                "{width}/{color:?}: {stripped}"
            );
            assert!(!stripped.contains("ctx show event "));
            for reference in ["[1]", "[2]", "[3]"] {
                assert_eq!(stripped.matches(reference).count(), 1);
            }
        }
    }
}

#[test]
fn shared_admission_keeps_complete_items_identical_across_human_widths() {
    let result = file_preview_result(3);
    let model = EvidencePreviewModel {
        previews: (1..=3)
            .map(|number| {
                preview(
                    &result,
                    vec![number],
                    RepositoryFileInvocationKind::Modify,
                    "X".repeat(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES),
                )
            })
            .collect(),
    };

    let shared_item_count = super::evidence::admitted_previews(&model).previews.len();
    assert!(shared_item_count > 0);
    for width in [1, 2, 8, 16, 32, 48, 80, 120] {
        for color in [ColorMode::Never, ColorMode::Always] {
            let context =
                RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color));
            let mut section = Document::new();
            super::evidence::render_previews(&mut section, &context, &model);
            let rendered = section.render(&context);
            if width >= 32 {
                assert!(
                    rendered.len() <= super::evidence::MAX_EVIDENCE_PREVIEW_RENDERED_BYTES,
                    "{width}/{color:?}: {}",
                    rendered.len()
                );
            }
            let stripped = strip_ansi(&rendered);
            let admitted = stripped
                .matches("Modify file request via test_tool")
                .count();
            assert_eq!(admitted, shared_item_count, "{width}/{color:?}: {stripped}");
            assert_eq!(
                stripped.matches('X').count(),
                admitted * MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
                "partial excerpt at {width}/{color:?}: {stripped}"
            );
            for evidence in &result.evidence {
                let reference = format!("[{}]", evidence.number);
                assert!(stripped.matches(&reference).count() <= 1);
            }
        }
    }

    let canonical_bytes = crate::ui::canonical_human_output_bytes(|context| {
        let mut section = Document::new();
        super::evidence::render_previews(&mut section, context, &model);
        section
    });
    assert!(
        canonical_bytes <= super::evidence::MAX_EVIDENCE_PREVIEW_RENDERED_BYTES,
        "canonical: {canonical_bytes}"
    );
}

#[test]
fn sanitizer_expansion_omits_the_complete_item_instead_of_truncating_it() {
    let result = file_preview_result(1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
            "\0".repeat(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES),
        )],
    };
    let rendered = render_preview_plain(&result, &model, 80);

    assert!(!rendered.contains("Evidence context"), "{rendered}");
    assert!(!rendered.contains("\\u{0000}"), "{rendered}");
}

#[test]
fn preview_is_safe_and_stable_at_supported_widths_and_across_color() {
    let result = file_preview_result(1);
    let family = "👨‍👩‍👧‍👦";
    let persian = "می‌روم";
    let combining = "e\u{0301}";
    let excerpt = format!("{family} {persian} {combining} bad\u{202e}name\u{1b}\tend");
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
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
        let preview_section = &plain[plain.find("Evidence context").unwrap()..];
        assert!(!preview_section.contains("ctx show event "));
        for line in preview_section.lines() {
            if line.trim() == "Modify file request via test_tool" {
                continue;
            }
            assert!(line.width() < width, "width {width} overflow: {line:?}");
        }
    }
}

#[test]
fn evidence_context_bytes_are_accounted_and_json_is_status_bearing() {
    let result = file_preview_result(1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Modify,
            "modified: src/lib.rs",
        )],
    };
    let default_bytes =
        crate::ui::canonical_human_output_bytes(|context| render_blame_document(&result, context));
    let hosted = current(result.clone());
    let evidence_context = BlameEvidenceContext::for_file(model.clone());

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
        assert!(captured
            .text()
            .contains("Evidence context (local history content)"));
        assert!(measured > default_bytes);
        assert_eq!(
            measured,
            crate::ui::canonical_human_output_bytes(|context| {
                super::render_blame_document(&hosted, context, &evidence_context)
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
    let hosted = current(result.clone());
    let expected_value = super::blame_result_json(&hosted, Some(&model));
    let mut helper_fields = expected_value.clone();
    helper_fields
        .as_object_mut()
        .unwrap()
        .remove("evidence_context");
    let mut expected_helper_fields = serde_json::to_value(&result).unwrap();
    expected_helper_fields.as_object_mut().unwrap().insert(
        "freshness".to_owned(),
        serde_json::json!({"state": "current"}),
    );
    expected_helper_fields.as_object_mut().unwrap().insert(
        "next_action".to_owned(),
        expected_value["next_action"].clone(),
    );
    assert_eq!(helper_fields, expected_helper_fields);
    assert_eq!(expected_value["evidence_context"]["status"], "available");
    assert_eq!(
        expected_value["evidence_context"]["items"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let mut expected = serde_json::to_string_pretty(&expected_value).unwrap();
    expected.push('\n');
    assert_eq!(captured.text(), expected);
    assert_eq!(measured, expected.len());
    assert!(!captured.text().contains('\u{1b}'));

    let unavailable = EvidencePreviewModel {
        previews: Vec::new(),
    };
    let unavailable_value = super::blame_result_json(&hosted, Some(&unavailable));
    assert_eq!(
        unavailable_value["evidence_context"]["status"],
        "unavailable"
    );
    assert_eq!(
        unavailable_value["evidence_context"]["items"],
        serde_json::json!([])
    );
}
