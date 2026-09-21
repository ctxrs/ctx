use chrono::{DateTime, SecondsFormat, Utc};
use ctx_attribution_model::{
    AgentAttribution, BlameMatch, CommitBlameMatch, CommitFactType, CommitLineage,
    CommitLineageOmission, CommitLineageOperationKind, CommitLineageProofClass,
    CommitLineageRelationClass, CommitLineageState, CommitLineageTruncationReason, CommitPredicate,
    ContinuationReason, ExactCommitRef, FactConfidence, FactState, FileBlameMatch, GitObjectFormat,
    NumberedEvidence, ProductionRelationship, PullRequestAction, PullRequestBlameMatch,
    PullRequestBlameRelationship, PullRequestCommitRelationship, QuerySnapshotExpectation,
    ResolvedBlameTarget, ResourceRef, ScopedCommitEndpoint, StableEntityId, WorktreeStatus,
};
use ctx_attribution_model::{
    BlameResultFreshness,
    evidence_preview::{EvidencePreviewModel, RepositoryFileInvocationKind},
};
use serde_json::Value;

use crate::presentation::{
    BlameOutput, blame_summary,
    render::{BlameEvidenceContext, blame_result_json_with_context, successful_next_action},
};

/// Produces the structured MCP value and its text projection from one validated result.
pub fn render_blame_tool<T: BlameOutput>(
    output: &T,
    previews: Option<&EvidencePreviewModel>,
) -> (Value, String) {
    let evidence_context = BlameEvidenceContext::for_result(output.result(), previews);
    let structured = blame_result_json_with_context(output, &evidence_context);
    let text = render_blame_text(output, &evidence_context);
    (structured, text)
}

fn render_blame_text<T: BlameOutput>(
    output: &T,
    evidence_context: &BlameEvidenceContext,
) -> String {
    let result = output.result();
    let mut out = String::from("ctx blame\n");
    out.push_str(&format!(
        "outcome: {}\n",
        blame_summary::outcome_heading(result.outcome.attribution)
    ));
    out.push_str(&format!(
        "page coverage: {}\n",
        blame_summary::coverage_text(&result.outcome.coverage, "")
    ));
    if let Some(freshness) = output.freshness() {
        push_text(&mut out, "freshness", freshness_text(freshness), "");
    }
    match &result.snapshot {
        QuerySnapshotExpectation::Core { receipt } => {
            push_text(&mut out, "snapshot.kind", "core", "");
            push_text(
                &mut out,
                "snapshot.receipt.core_generation_id",
                &receipt.core_generation_id,
                "",
            );
            push_text(
                &mut out,
                "snapshot.receipt.materializer_revision",
                &receipt.materializer_revision,
                "",
            );
        }
    }
    match &result.target {
        ResolvedBlameTarget::File {
            path,
            repository,
            requested_lines,
        } => {
            push_text(&mut out, "target.kind", "file", "");
            push_text(&mut out, "target.path", path, "");
            push_resource(&mut out, "target.repository", repository, "");
            if let Some(lines) = requested_lines {
                push_number(&mut out, "target.lines.start", lines.start, "");
                push_number(&mut out, "target.lines.end", lines.end, "");
            }
        }
        ResolvedBlameTarget::Commit { commit, repository } => {
            push_text(&mut out, "target.kind", "commit", "");
            push_resource(&mut out, "target.commit", commit, "");
            push_resource(&mut out, "target.repository", repository, "");
        }
        ResolvedBlameTarget::PullRequest {
            selector,
            pull_request,
            repository,
        } => {
            push_text(&mut out, "target.kind", "pull_request", "");
            push_text(&mut out, "target.selector", selector, "");
            push_resource(&mut out, "target.pull_request", pull_request, "");
            push_resource(&mut out, "target.repository", repository, "");
        }
    }
    if let Some(snapshot) = &result.git_snapshot {
        push_text(&mut out, "git_snapshot.head_oid", &snapshot.head_oid, "");
        push_text(
            &mut out,
            "git_snapshot.worktree_status",
            worktree_status_text(snapshot.worktree_status),
            "",
        );
    }
    if let Some(lineage) = &result.lineage {
        out.push('\n');
        render_lineage(&mut out, lineage);
    }

    out.push_str(&format!("matches: {}\n", result.matches.len()));
    for (index, matched) in result.matches.iter().enumerate() {
        render_match(&mut out, index + 1, matched);
    }

    out.push_str(&format!("\nevidence: {}\n", result.evidence.len()));
    for evidence in &result.evidence {
        render_evidence(&mut out, evidence);
    }
    render_evidence_context(&mut out, evidence_context);

    if let Some(next) = &result.next {
        push_text(
            &mut out,
            "next.reason",
            continuation_reason_text(next.reason),
            "",
        );
        push_text(&mut out, "next.cursor", &next.cursor, "");
    }
    if let Some(action) = successful_next_action(output) {
        let command = action
            .argv
            .iter()
            .map(|argument| crate::presentation::shell_quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" ");
        push_text(&mut out, "next action", &command, "");
    }
    out
}

fn render_lineage(out: &mut String, lineage: &CommitLineage) {
    push_bool(out, "lineage.complete", lineage.complete, "");
    push_bool(out, "lineage.ambiguous", lineage.ambiguous, "");
    push_exact_commit(out, "lineage.requested", &lineage.requested, "");

    out.push_str(&format!("lineage.edges: {}\n", lineage.edges.len()));
    for (index, edge) in lineage.edges.iter().enumerate() {
        let prefix = format!("lineage.edge.{}", index + 1);
        push_text(
            out,
            &format!("{prefix}.operation_id"),
            &edge.operation_id,
            "",
        );
        push_text(
            out,
            &format!("{prefix}.kind"),
            lineage_operation_text(edge.kind),
            "",
        );
        push_text(
            out,
            &format!("{prefix}.relation_class"),
            lineage_relation_text(edge.relation_class),
            "",
        );
        push_text(
            out,
            &format!("{prefix}.proof_class"),
            lineage_proof_text(edge.proof_class),
            "",
        );
        push_text(
            out,
            &format!("{prefix}.state"),
            lineage_state_text(edge.state),
            "",
        );
        if let Some(observed_at_ms) = edge.observed_at_ms {
            push_number(out, &format!("{prefix}.observed_at_ms"), observed_at_ms, "");
        }
        push_exact_commit(out, &format!("{prefix}.source"), &edge.source, "");
        push_exact_commit(out, &format!("{prefix}.result"), &edge.result, "");
        push_resource(out, &format!("{prefix}.actor"), &edge.actor, "");
        push_number_list(
            out,
            &format!("{prefix}.evidence_numbers"),
            &edge.evidence_numbers,
            "",
        );
    }

    out.push_str(&format!(
        "lineage.yielded_by: {}\n",
        lineage.yielded_by.len()
    ));
    for (index, yielded) in lineage.yielded_by.iter().enumerate() {
        let prefix = format!("lineage.yield.{}", index + 1);
        push_text(
            out,
            &format!("{prefix}.operation_id"),
            &yielded.operation_id,
            "",
        );
        push_text(out, &format!("{prefix}.yield_id"), &yielded.yield_id, "");
        push_text(
            out,
            &format!("{prefix}.logical_repository_id"),
            &yielded.logical_repository_id,
            "",
        );
        push_text(
            out,
            &format!("{prefix}.proof_class"),
            lineage_proof_text(yielded.proof_class),
            "",
        );
        push_text(
            out,
            &format!("{prefix}.state"),
            lineage_state_text(yielded.state),
            "",
        );
        if let Some(observed_at_ms) = yielded.observed_at_ms {
            push_number(out, &format!("{prefix}.observed_at_ms"), observed_at_ms, "");
        }
        push_resource(out, &format!("{prefix}.actor"), &yielded.actor, "");
        push_number_list(
            out,
            &format!("{prefix}.evidence_numbers"),
            &yielded.evidence_numbers,
            "",
        );
    }

    if let Some(origin) = &lineage.origin {
        push_exact_commit(out, "lineage.origin", origin, "");
    }
    if let Some(endpoint) = &lineage.endpoint {
        let (kind, commit, scope, observation_id, observed_at_ms, evidence_numbers) = match endpoint
        {
            ScopedCommitEndpoint::CurrentAtRef {
                commit,
                scope,
                observation_id,
                observed_at_ms,
                evidence_numbers,
            } => (
                "current_at_ref",
                commit,
                scope,
                observation_id,
                observed_at_ms,
                evidence_numbers,
            ),
            ScopedCommitEndpoint::CurrentForPr {
                commit,
                scope,
                observation_id,
                observed_at_ms,
                evidence_numbers,
            } => (
                "current_for_pr",
                commit,
                scope,
                observation_id,
                observed_at_ms,
                evidence_numbers,
            ),
        };
        push_text(out, "lineage.endpoint.kind", kind, "");
        push_text(out, "lineage.endpoint.observation_id", observation_id, "");
        push_number(out, "lineage.endpoint.observed_at_ms", *observed_at_ms, "");
        push_exact_commit(out, "lineage.endpoint.commit", commit, "");
        push_resource(out, "lineage.endpoint.scope", scope, "");
        push_number_list(
            out,
            "lineage.endpoint.evidence_numbers",
            evidence_numbers,
            "",
        );
    }

    let bounds = &lineage.bounds;
    push_number(
        out,
        "lineage.bounds.returned_events",
        bounds.returned_events,
        "",
    );
    push_number(
        out,
        "lineage.bounds.returned_event_limit",
        bounds.returned_event_limit,
        "",
    );
    push_number(
        out,
        "lineage.bounds.examined_events",
        bounds.examined_events,
        "",
    );
    push_number(
        out,
        "lineage.bounds.examined_event_limit",
        bounds.examined_event_limit,
        "",
    );
    if let Some(reason) = bounds.truncation_reason {
        push_text(
            out,
            "lineage.bounds.truncation_reason",
            lineage_truncation_text(reason),
            "",
        );
    }
    match bounds.omission {
        CommitLineageOmission::Exact(count) => {
            push_text(out, "lineage.bounds.omission.kind", "exact", "");
            push_number(out, "lineage.bounds.omission.count", count, "");
        }
        CommitLineageOmission::AtLeast(count) => {
            push_text(out, "lineage.bounds.omission.kind", "at_least", "");
            push_number(out, "lineage.bounds.omission.count", count, "");
        }
        CommitLineageOmission::Unknown => {
            push_text(out, "lineage.bounds.omission.kind", "unknown", "");
        }
    }
}

fn push_exact_commit(out: &mut String, label: &str, commit: &ExactCommitRef, indent: &str) {
    push_resource(out, &format!("{label}.resource"), &commit.resource, indent);
    push_text(
        out,
        &format!("{label}.logical_repository_id"),
        &commit.logical_repository_id,
        indent,
    );
    push_text(
        out,
        &format!("{label}.object_format"),
        object_format_text(commit.object_format),
        indent,
    );
    push_text(out, &format!("{label}.oid"), &commit.oid, indent);
}

fn render_match(out: &mut String, index: usize, matched: &BlameMatch) {
    out.push_str(&format!("\nmatch {index}\n"));
    match matched {
        BlameMatch::File(value) => {
            push_text(out, "kind", "file", "  ");
            render_file_match(out, value);
        }
        BlameMatch::Commit(value) => {
            push_text(out, "kind", "commit", "  ");
            render_commit_match(out, value);
        }
        BlameMatch::PullRequest(value) => {
            push_text(out, "kind", "pull_request", "  ");
            render_pull_request_match(out, value);
        }
    }
}

fn render_file_match(out: &mut String, value: &FileBlameMatch) {
    push_text(out, "id", &value.id, "  ");
    push_number(out, "lines.start", value.lines.start, "  ");
    push_number(out, "lines.end", value.lines.end, "  ");
    push_resource(out, "commit", &value.commit, "  ");
    push_number_list(
        out,
        "line_evidence_numbers",
        &value.line_evidence_numbers,
        "  ",
    );
    for (index, attribution) in value.production.iter().enumerate() {
        out.push_str(&format!("  production {}\n", index + 1));
        render_attribution(out, attribution, "    ");
    }
}

fn render_commit_match(out: &mut String, value: &CommitBlameMatch) {
    push_text(out, "fact_id", &value.fact_id, "  ");
    push_text(out, "fact_type", commit_fact_text(value.fact_type), "  ");
    push_text(
        out,
        "predicate",
        commit_predicate_text(value.predicate),
        "  ",
    );
    push_optional_timestamp(out, "fact_observed_at", value.fact_occurred_at_ms, "  ");
    push_text(out, "confidence", confidence_text(value.confidence), "  ");
    push_text(out, "state", fact_state_text(value.state), "  ");
    push_resource(out, "subject", &value.subject, "  ");
    push_optional_resource(out, "object", value.object.as_ref(), "  ");
    push_optional_resource(out, "parent_session", value.parent_session.as_ref(), "  ");
    push_optional_resource(out, "direct_actor", value.direct_actor.as_ref(), "  ");
    push_optional_resource(out, "owning_root", value.owning_root.as_ref(), "  ");
    push_number_list(out, "evidence_numbers", &value.evidence_numbers, "  ");
}

fn render_pull_request_match(out: &mut String, value: &PullRequestBlameMatch) {
    push_resource(out, "pull_request", &value.pull_request, "  ");
    match &value.relationship {
        PullRequestBlameRelationship::Activity(value) => {
            push_text(out, "relationship.kind", "activity", "  ");
            push_text(out, "fact_id", &value.fact_id, "  ");
            push_text(out, "action", pull_request_action_text(value.action), "  ");
            push_optional_timestamp(out, "fact_observed_at", value.fact_occurred_at_ms, "  ");
            push_text(out, "confidence", confidence_text(value.confidence), "  ");
            push_text(out, "state", fact_state_text(value.state), "  ");
            push_resource(out, "session", &value.session, "  ");
            push_optional_resource(out, "direct_actor", value.direct_actor.as_ref(), "  ");
            push_optional_resource(out, "owning_root", value.owning_root.as_ref(), "  ");
            push_number_list(out, "evidence_numbers", &value.evidence_numbers, "  ");
        }
        PullRequestBlameRelationship::Commit(value) => {
            push_text(out, "relationship.kind", "commit", "  ");
            push_text(out, "fact_id", &value.fact_id, "  ");
            push_text(
                out,
                "relationship",
                pull_request_commit_relationship_text(value.relationship),
                "  ",
            );
            push_resource(out, "commit", &value.commit, "  ");
            push_optional_timestamp(out, "fact_observed_at", value.fact_occurred_at_ms, "  ");
            push_number_list(out, "evidence_numbers", &value.evidence_numbers, "  ");
            for (index, attribution) in value.production.iter().enumerate() {
                out.push_str(&format!("  production {}\n", index + 1));
                render_attribution(out, attribution, "    ");
            }
        }
    }
}

fn render_attribution(out: &mut String, value: &AgentAttribution, indent: &str) {
    push_text(out, "id", &value.id, indent);
    push_text(
        out,
        "relationship",
        production_relationship_text(value.relationship),
        indent,
    );
    push_resource(out, "producing_session", &value.producing_session, indent);
    push_optional_resource(out, "parent_session", value.parent_session.as_ref(), indent);
    push_optional_resource(out, "direct_actor", value.direct_actor.as_ref(), indent);
    push_optional_resource(out, "owning_root", value.owning_root.as_ref(), indent);
    push_optional_timestamp(out, "fact_observed_at", value.fact_occurred_at_ms, indent);
    push_text(out, "confidence", confidence_text(value.confidence), indent);
    push_text(out, "state", fact_state_text(value.state), indent);
    push_number_list(out, "evidence_numbers", &value.evidence_numbers, indent);
}

fn render_evidence(out: &mut String, value: &NumberedEvidence) {
    out.push_str(&format!("evidence {}\n", value.number));
    let citation = &value.citation;
    push_text(
        out,
        "core_generation_id",
        &citation.core_generation_id,
        "  ",
    );
    let source = serde_json::to_value(&citation.source)
        .and_then(|value| serde_json::to_string(&value))
        .unwrap_or_else(|_| "[unrenderable structured value]".to_owned());
    push_preescaped(out, "source", &source, "  ");
    push_preescaped(
        out,
        "session_id",
        &stable_entity_text(&citation.session_id),
        "  ",
    );
    push_preescaped(
        out,
        "event_id",
        &stable_entity_text(&citation.event_id),
        "  ",
    );
    push_number(out, "event_sequence", citation.event_sequence, "  ");
    if let Some(digest) = &citation.evidence_sha256 {
        push_text(out, "evidence_sha256", digest, "  ");
    }
    if let Some(range) = &citation.byte_range {
        push_number(out, "byte_range.start", range.start, "  ");
        push_number(out, "byte_range.end_exclusive", range.end_exclusive, "  ");
    }
}

fn stable_entity_text(value: &StableEntityId) -> String {
    serde_json::to_value(value)
        .and_then(|value| serde_json::to_string(&value))
        .unwrap_or_else(|_| "[unrenderable structured value]".to_owned())
}

fn render_evidence_context(out: &mut String, context: &BlameEvidenceContext) {
    out.push_str("\nevidence_context\n");
    push_text(out, "status", context.status_text(), "  ");
    let previews = &context.model().previews;
    out.push_str(&format!("  items: {}\n", previews.len()));
    for (index, preview) in previews.iter().enumerate() {
        out.push_str(&format!("  item {}\n", index + 1));
        push_number_list(out, "citation_numbers", &preview.citation_numbers, "    ");
        push_text(
            out,
            "operation",
            preview_operation_text(preview.operation),
            "    ",
        );
        push_text(out, "path", &preview.path, "    ");
        if let Some(prior_path) = &preview.prior_path {
            push_text(out, "prior_path", prior_path, "    ");
        }
        push_text(out, "tool_name", &preview.tool_name, "    ");
        push_optional_timestamp(out, "event_time", preview.event_occurred_at_ms, "    ");
        push_text(out, "excerpt", &preview.excerpt, "    ");
    }
}

fn push_optional_resource(
    out: &mut String,
    label: &str,
    value: Option<&ResourceRef>,
    indent: &str,
) {
    if let Some(value) = value {
        push_resource(out, label, value, indent);
    }
}

fn push_resource(out: &mut String, label: &str, value: &ResourceRef, indent: &str) {
    push_text(out, &format!("{label}.id"), &value.id, indent);
    push_text(
        out,
        &format!("{label}.kind"),
        value.kind.wire_name(),
        indent,
    );
    push_text(out, &format!("{label}.display"), &value.display, indent);
}

fn push_number_list(out: &mut String, label: &str, values: &[u32], indent: &str) {
    let rendered = values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    push_preescaped(out, label, &rendered, indent);
}

fn push_optional_timestamp(out: &mut String, label: &str, unix_ms: Option<i64>, indent: &str) {
    if let Some(unix_ms) = unix_ms {
        push_timestamp(out, label, unix_ms, indent);
    }
}

fn push_timestamp(out: &mut String, label: &str, unix_ms: i64, indent: &str) {
    if let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(unix_ms) {
        push_preescaped(
            out,
            label,
            &timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            indent,
        );
    }
}

fn push_text(out: &mut String, label: &str, value: &str, indent: &str) {
    push_preescaped(out, label, &escape_controls(value), indent);
}

fn push_preescaped(out: &mut String, label: &str, value: &str, indent: &str) {
    out.push_str(&format!("{indent}{label}: {value}\n"));
}

fn push_number(out: &mut String, label: &str, value: impl std::fmt::Display, indent: &str) {
    out.push_str(&format!("{indent}{label}: {value}\n"));
}

fn push_bool(out: &mut String, label: &str, value: bool, indent: &str) {
    out.push_str(&format!("{indent}{label}: {value}\n"));
}

const fn freshness_text(value: BlameResultFreshness) -> &'static str {
    match value {
        BlameResultFreshness::Current => "current",
        BlameResultFreshness::StaleCommitted => "stale_committed",
    }
}

const fn worktree_status_text(value: WorktreeStatus) -> &'static str {
    match value {
        WorktreeStatus::Clean => "clean",
        WorktreeStatus::Differs => "differs",
    }
}

const fn continuation_reason_text(value: ContinuationReason) -> &'static str {
    match value {
        ContinuationReason::MoreMatches => "more_matches",
        ContinuationReason::MoreCommittedLines => "more_committed_lines",
    }
}

const fn object_format_text(value: GitObjectFormat) -> &'static str {
    match value {
        GitObjectFormat::Sha1 => "sha1",
        GitObjectFormat::Sha256 => "sha256",
    }
}

const fn lineage_operation_text(value: CommitLineageOperationKind) -> &'static str {
    match value {
        CommitLineageOperationKind::Amend => "amend",
        CommitLineageOperationKind::Rebase => "rebase",
        CommitLineageOperationKind::CherryPick => "cherry_pick",
    }
}

const fn lineage_relation_text(value: CommitLineageRelationClass) -> &'static str {
    match value {
        CommitLineageRelationClass::Replacement => "replacement",
        CommitLineageRelationClass::Derivation => "derivation",
    }
}

const fn lineage_proof_text(value: CommitLineageProofClass) -> &'static str {
    match value {
        CommitLineageProofClass::RecordExact => "record_exact",
        CommitLineageProofClass::RepositoryVerified => "repository_verified",
        CommitLineageProofClass::ForgeVerified => "forge_verified",
    }
}

const fn lineage_state_text(value: CommitLineageState) -> &'static str {
    match value {
        CommitLineageState::Asserted => "asserted",
        CommitLineageState::Ambiguous => "ambiguous",
        CommitLineageState::Contradicted => "contradicted",
    }
}

const fn lineage_truncation_text(value: CommitLineageTruncationReason) -> &'static str {
    match value {
        CommitLineageTruncationReason::ReturnedEventLimit => "returned_event_limit",
        CommitLineageTruncationReason::ExaminedEventLimit => "examined_event_limit",
        CommitLineageTruncationReason::EvidenceGap => "evidence_gap",
    }
}

const fn confidence_text(value: FactConfidence) -> &'static str {
    match value {
        FactConfidence::Explicit => "explicit",
        FactConfidence::High => "high",
        FactConfidence::Medium => "medium",
        FactConfidence::Low => "low",
        FactConfidence::Ambiguous => "ambiguous",
        FactConfidence::Unknown => "unknown",
    }
}

const fn fact_state_text(value: FactState) -> &'static str {
    match value {
        FactState::Asserted => "asserted",
        FactState::Ambiguous => "ambiguous",
        FactState::Contradicted => "contradicted",
        FactState::Superseded => "superseded",
    }
}

const fn production_relationship_text(value: ProductionRelationship) -> &'static str {
    match value {
        ProductionRelationship::ProducedBy => "produced_by",
        ProductionRelationship::PossiblyProducedBy => "possibly_produced_by",
    }
}

const fn commit_fact_text(value: CommitFactType) -> &'static str {
    match value {
        CommitFactType::Produced => "git.commit.produced",
        CommitFactType::Amended => "git.commit.amended",
        CommitFactType::CherryPicked => "git.commit.cherry_picked",
        CommitFactType::Reverted => "git.commit.reverted",
        CommitFactType::Pushed => "git.commit.pushed",
        CommitFactType::Inspected => "git.commit.inspected",
        CommitFactType::Referenced => "git.commit.referenced",
        CommitFactType::Ambiguous => "git.commit.ambiguous",
    }
}

const fn commit_predicate_text(value: CommitPredicate) -> &'static str {
    match value {
        CommitPredicate::ProducedBy => "produced_by",
        CommitPredicate::PossiblyProducedBy => "possibly_produced_by",
        CommitPredicate::AmendedBy => "amended_by",
        CommitPredicate::CherryPickedFrom => "cherry_picked_from",
        CommitPredicate::Reverts => "reverts",
        CommitPredicate::PushedBy => "pushed_by",
        CommitPredicate::InspectedBy => "inspected_by",
        CommitPredicate::ReferencedBy => "referenced_by",
    }
}

const fn pull_request_action_text(value: PullRequestAction) -> &'static str {
    match value {
        PullRequestAction::Referenced => "referenced",
        PullRequestAction::Created => "created",
        PullRequestAction::Reviewed => "reviewed",
        PullRequestAction::Commented => "commented",
        PullRequestAction::Merged => "merged",
        PullRequestAction::Edited => "edited",
        PullRequestAction::Closed => "closed",
        PullRequestAction::Reopened => "reopened",
    }
}

const fn pull_request_commit_relationship_text(
    value: PullRequestCommitRelationship,
) -> &'static str {
    match value {
        PullRequestCommitRelationship::ContainsCommit => "contains_commit",
        PullRequestCommitRelationship::MergedAs => "merged_as",
    }
}

const fn preview_operation_text(value: RepositoryFileInvocationKind) -> &'static str {
    match value {
        RepositoryFileInvocationKind::Read => "read",
        RepositoryFileInvocationKind::Create => "create",
        RepositoryFileInvocationKind::Modify => "modify",
        RepositoryFileInvocationKind::Delete => "delete",
        RepositoryFileInvocationKind::Rename => "rename",
        RepositoryFileInvocationKind::Write => "write",
    }
}

fn escape_controls(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => escaped.extend(character.escape_unicode()),
            character => escaped.push(character),
        }
    }
    escaped
}
