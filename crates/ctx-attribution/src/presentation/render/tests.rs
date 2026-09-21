use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

use ctx_attribution_model::{
    AgentAttribution, BlameAttribution, BlameContinuation, BlameCoverage, BlameCoverageUnit,
    BlameMatch, BlameOutcome, BlameResult, CommitBlameMatch, CommitFactType, CommitLineage,
    CommitLineageBounds, CommitLineageEdge, CommitLineageOmission, CommitLineageOperationKind,
    CommitLineageProofClass, CommitLineageRelationClass, CommitLineageState,
    CommitLineageTruncationReason, CommitLineageYield, CommitPredicate, ContinuationReason,
    EvidenceCitation, ExactCommitRef, FactConfidence, FactState, FileBlameMatch, GitObjectFormat,
    GitSnapshot, LineRange, MAX_COMMIT_LINEAGE_EXAMINED_EVENTS, MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
    NumberedEvidence, ProductionRelationship, PullRequestAction, PullRequestActivity,
    PullRequestBlameMatch, PullRequestBlameRelationship, PullRequestCommit,
    PullRequestCommitRelationship, ResolvedBlameTarget, ResourceKind, ResourceRef,
    ScopedCommitEndpoint, WorktreeStatus,
};
use ctx_attribution_model::{
    BlameResultFreshness, HostedBlameResult,
    evidence_preview::{
        EvidencePreview, EvidencePreviewModel, MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
        RepositoryFileInvocationKind,
    },
};
use ctx_history_core::{
    EventIdentityInput, NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    SourceKey, TypedKey, derive_event_id, derive_session_id,
};
use ctx_terminal::ui::{ColorMode, Document, Line, RenderContext, StreamKind, TestContext, Token};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use super::{
    BlameEvidenceContext,
    layout::{enum_heading, enum_text},
};

fn protocol_snapshot() -> ctx_attribution_model::QuerySnapshotExpectation {
    ctx_attribution_model::QuerySnapshotExpectation::Core {
        receipt: ctx_attribution_model::CoreMaterializationReceiptIdentity {
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

fn current(result: BlameResult) -> HostedBlameResult {
    HostedBlameResult {
        result,
        freshness: BlameResultFreshness::Current,
    }
}

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

fn context_with_time_zone(width: usize, time_zone: &'static str) -> RenderContext {
    RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, width)
            .color(ColorMode::Never)
            .time_zone(time_zone),
    )
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
    ui: &mut ctx_terminal::ui::Ui,
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

#[test]
fn mcp_text_projects_typed_results_context_and_actions() {
    let result = complete_lineage_result();
    result.validate().unwrap();
    let hosted = current(result);
    let (structured, text) = crate::presentation::mcp_text::render_blame_tool(&hosted, None);
    assert_eq!(structured["freshness"]["state"], "current");
    for expected in [
        "outcome: Possible producer found",
        "freshness: current",
        "target.kind: commit",
        "lineage.edge.1.kind: rebase",
        "lineage.endpoint.kind: current_at_ref",
        "kind: commit",
        "fact_type: git.commit.produced",
        "evidence 1",
        "evidence_context\n  status: not_applicable",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
    assert!(
        text.contains(concat!(
            "  session_id: {\"contract_version\":1,",
            "\"digest\":[20,233,132,64,252,140,9,212,155,238,244,10,172,159,215,72,148,36,194,153,183,46,47,160,66,26,40,211,99,151,183,38],",
            "\"entity_kind\":\"Session\",",
            "\"source_descriptor_digest\":[23,168,222,127,54,125,141,151,103,203,145,153,85,93,70,237,181,77,75,93,137,229,125,218,133,48,69,51,99,42,13,249],",
            "\"source_digest\":[15,152,156,194,251,27,110,175,132,3,7,141,118,34,66,75,140,119,65,8,238,211,232,170,39,177,14,113,119,20,117,203],",
            "\"uuid\":\"14e98440-fc8c-89d4-9bee-f40aac9fd748\"}\n",
            "  event_id: {\"contract_version\":1,",
            "\"digest\":[140,215,101,172,104,163,58,44,46,4,102,154,8,190,99,112,51,35,26,106,17,243,1,160,121,192,57,11,135,121,127,197],",
            "\"entity_kind\":\"Event\",",
            "\"source_descriptor_digest\":[23,168,222,127,54,125,141,151,103,203,145,153,85,93,70,237,181,77,75,93,137,229,125,218,133,48,69,51,99,42,13,249],",
            "\"source_digest\":[15,152,156,194,251,27,110,175,132,3,7,141,118,34,66,75,140,119,65,8,238,211,232,170,39,177,14,113,119,20,117,203],",
            "\"uuid\":\"8cd765ac-68a3-8a2c-ae04-669a08be6370\"}\n",
        )),
        "{text}"
    );

    let result = file_preview_result(1);
    let model = EvidencePreviewModel {
        previews: vec![preview(
            &result,
            vec![1],
            RepositoryFileInvocationKind::Rename,
            "line one\nline two",
        )],
    };
    let hosted = current(result);
    let (_, text) = crate::presentation::mcp_text::render_blame_tool(&hosted, Some(&model));

    assert!(text.contains("status: available"), "{text}");
    assert!(text.contains("operation: rename"), "{text}");
    assert!(text.contains("prior_path: src/old.rs"), "{text}");
    assert!(text.contains("excerpt: line one\\nline two"), "{text}");
    assert!(
        text.contains("next action: ctx search src/lib.rs --refresh off"),
        "{text}"
    );

    for (result, expected) in [
        (paginated_pr_result(true), "relationship: contains_commit"),
        (paginated_pr_result(false), "action: reviewed"),
    ] {
        result.validate().unwrap();
        let (_, text) = crate::presentation::mcp_text::render_blame_tool(&result, None);
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
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
        outcome: outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 1),
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
        outcome: outcome(BlameCoverageUnit::CommitFact, 0, 0, 0, 0),
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
        include_str!("../../../testdata/blame/blame_commit.golden.txt")
    );
}

#[test]
fn commit_lineage_complete_human_output_is_exact_and_deduplicates_yield_actor() {
    let result = complete_lineage_result();
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert_eq!(
        rendered,
        include_str!("../../../testdata/blame/blame_commit_lineage_complete.golden.txt")
    );
    assert_eq!(rendered.matches("session rebaser").count(), 1, "{rendered}");
    assert!(!rendered.contains("Produced by"), "{rendered}");
    assert!(!rendered.contains("created"), "{rendered}");
    assert!(!rendered.contains("implemented by"), "{rendered}");
}

#[test]
fn human_timestamps_use_the_render_context_time_zone() {
    let result = complete_lineage_result();
    result.validate().unwrap();
    let context = context_with_time_zone(80, "America/New_York");
    let rendered = render_blame_document(&result, &context).render_plain();

    assert!(
        rendered.contains("observed      2024-07-14 19:33:20 EDT"),
        "{rendered}"
    );
    assert!(
        rendered.contains("observed      2024-07-14 19:33:21 EDT"),
        "{rendered}"
    );
    assert!(!rendered.contains("2024-07-14T23:33:20.000Z"), "{rendered}");
}

#[test]
fn plural_mappings_render_as_one_deterministic_operation_with_copyable_ids() {
    let result = plural_lineage_result();
    result.validate().unwrap();
    let rendered = render_plain(&result, 80);
    assert_eq!(
        rendered,
        include_str!("../../../testdata/blame/blame_commit_lineage_plural.golden.txt")
    );
    assert_eq!(rendered.matches("Rebase · replacement").count(), 1);
    assert_eq!(rendered.matches(&"a".repeat(64)).count(), 1);
    assert!(rendered.contains(&"1".repeat(40)), "{rendered}");
    assert!(rendered.contains(&"2".repeat(40)), "{rendered}");
    assert!(rendered.contains(&"3".repeat(40)), "{rendered}");
    assert!(rendered.contains(&"4".repeat(40)), "{rendered}");
    assert!(rendered.contains("1 operation"), "{rendered}");
    assert!(rendered.contains("2 mappings"), "{rendered}");
    assert!(
        rendered.contains("operation yielded 2 mappings"),
        "{rendered}"
    );
    assert!(!rendered.contains("2 mapped commits"), "{rendered}");

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
        include_str!("../../../testdata/blame/blame_commit_lineage_partial.golden.txt")
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
        3,
    ));
    result.evidence.push(event_evidence(3));
    result.outcome = outcome(BlameCoverageUnit::CommitFact, 2, 0, 0, 1);
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
        include_str!("../../../testdata/blame/blame_pr.golden.txt")
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

mod additional;

#[test]
fn stale_positive_keeps_results_and_offers_manual_completion() {
    let mut result = commit_blame_result(0);
    result.outcome = outcome(BlameCoverageUnit::CommitFact, 1, 0, 0, 0);
    let output = HostedBlameResult {
        result,
        freshness: BlameResultFreshness::StaleCommitted,
    };
    let json = super::blame_result_json(&output, None);
    assert_eq!(json["freshness"]["state"], "stale_committed");
    assert_eq!(
        json["next_action"],
        serde_json::json!({"kind":"import_all", "argv":["ctx", "import", "--all"]})
    );
}
