use std::io::{self, Write};

use anyhow::Result;
use ctx_pro_host_protocol::{
    AgentAttribution, BlameMatch, BlameResult, CommitBlameMatch, ContinuationReason,
    EvidenceCitation, FactState, FileBlameMatch, LineRange, PullRequestBlameMatch,
    PullRequestBlameRelationship, ResolvedBlameTarget, ResourceRef, WorktreeStatus,
};
use serde_json::Value;

#[must_use]
pub(crate) fn blame_result_json(result: &BlameResult) -> Value {
    serde_json::to_value(result).unwrap_or(Value::Null)
}

pub(crate) fn print_blame_result(result: &BlameResult, json_output: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer_pretty(&mut output, result)?;
        writeln!(output)?;
    } else {
        write!(output, "{}", render_blame_text(result))?;
    }
    Ok(())
}

pub(crate) fn render_blame_text(result: &BlameResult) -> String {
    let mut out = String::new();
    render_target(&mut out, result);
    match &result.target {
        ResolvedBlameTarget::File { .. } => render_file_matches(&mut out, &result.matches),
        ResolvedBlameTarget::Commit { .. } => render_commit_matches(&mut out, &result.matches),
        ResolvedBlameTarget::PullRequest { .. } => {
            render_pull_request_matches(&mut out, &result.matches)
        }
    }
    render_evidence(&mut out, result);
    render_continuation(&mut out, result);
    out
}

fn render_target(out: &mut String, result: &BlameResult) {
    match &result.target {
        ResolvedBlameTarget::File {
            path,
            repository,
            requested_lines,
        } => {
            out.push_str(path);
            if let Some(lines) = requested_lines {
                out.push(':');
                out.push_str(&line_range_text(lines));
            }
            out.push('\n');
            out.push_str(&format!("repository  {}\n", repository.display));
            if let Some(snapshot) = &result.git_snapshot {
                out.push_str(&format!("snapshot    HEAD {}\n", snapshot.head_oid));
                match snapshot.worktree_status {
                    WorktreeStatus::Clean => out.push_str("worktree    clean\n"),
                    WorktreeStatus::Differs => {
                        out.push_str("worktree    differs (ranges are committed HEAD lines)\n");
                    }
                }
            }
        }
        ResolvedBlameTarget::Commit { commit, repository } => {
            out.push_str(&format!("commit      {}\n", commit.display));
            out.push_str(&format!("repository  {}\n", repository.display));
        }
        ResolvedBlameTarget::PullRequest {
            selector: _,
            pull_request,
            repository,
        } => {
            out.push_str(&format!("PR          {}\n", pull_request.display));
            out.push_str(&format!("repository  {}\n", repository.display));
        }
    }
}

fn render_file_matches(out: &mut String, matches: &[BlameMatch]) {
    if matches.is_empty() {
        out.push_str("\nNo committed line matches were returned.\n");
        return;
    }
    for value in matches.iter().filter_map(|value| match value {
        BlameMatch::File(value) => Some(value),
        BlameMatch::Commit(_) | BlameMatch::PullRequest(_) => None,
    }) {
        render_file_match(out, value);
    }
}

fn render_file_match(out: &mut String, value: &FileBlameMatch) {
    out.push_str(&format!("\nLines {}\n", line_range_text(&value.lines)));
    out.push_str(&format!("  commit    {}\n", value.commit.display));
    out.push_str(&format!(
        "  evidence  {}\n",
        evidence_numbers_text(&value.line_evidence_numbers)
    ));
    if value.production.is_empty() {
        out.push_str("\n  Agent production  not proven\n");
        return;
    }
    for attribution in &value.production {
        out.push('\n');
        out.push_str(match attribution.relationship {
            ctx_pro_host_protocol::ProductionRelationship::ProducedBy => "  Produced by\n",
            ctx_pro_host_protocol::ProductionRelationship::PossiblyProducedBy => {
                "  Possible producer\n"
            }
        });
        render_attribution(out, attribution, "    ");
    }
}

fn render_attribution(out: &mut String, value: &AgentAttribution, indent: &str) {
    out.push_str(&format!(
        "{indent}session       {}\n",
        value.producing_session.display
    ));
    if let Some(actor) = &value.direct_actor {
        out.push_str(&format!(
            "{indent}direct actor  {} {}\n",
            actor.kind.wire_name(),
            actor.display
        ));
    }
    if let Some(root) = &value.owning_root {
        out.push_str(&format!(
            "{indent}owning root   {} {}\n",
            root.kind.wire_name(),
            root.display
        ));
    }
    out.push_str(&format!(
        "{indent}confidence    {}\n",
        enum_text(value.confidence)
    ));
    out.push_str(&format!(
        "{indent}state         {}\n",
        enum_text(value.state)
    ));
    out.push_str(&format!(
        "{indent}evidence      {}\n",
        evidence_numbers_text(&value.evidence_numbers)
    ));
}

fn render_commit_matches(out: &mut String, matches: &[BlameMatch]) {
    let commits = matches
        .iter()
        .filter_map(|value| match value {
            BlameMatch::Commit(value) => Some(value),
            BlameMatch::File(_) | BlameMatch::PullRequest(_) => None,
        })
        .collect::<Vec<_>>();
    if commits.is_empty() {
        out.push_str("\nNo cited agent attribution was found for this commit.\n");
        return;
    }
    let (produced, remaining): (Vec<_>, Vec<_>) = commits.into_iter().partition(|value| {
        value.predicate == ctx_pro_host_protocol::CommitPredicate::ProducedBy
            && value.state == FactState::Asserted
    });
    let (possible, also_recorded): (Vec<_>, Vec<_>) = remaining.into_iter().partition(|value| {
        value.predicate == ctx_pro_host_protocol::CommitPredicate::PossiblyProducedBy
    });
    if !produced.is_empty() {
        out.push_str("\nProduced by\n");
        for value in produced {
            render_commit_match(out, value, false);
        }
    }
    if !possible.is_empty() {
        out.push_str("\nPossible producers\n");
        for value in possible {
            render_commit_match(out, value, false);
        }
    }
    if !also_recorded.is_empty() {
        out.push_str("\nAlso recorded\n");
        for value in also_recorded {
            render_commit_match(out, value, true);
        }
    }
    if matches.iter().any(|value| {
        matches!(
            value,
            BlameMatch::Commit(CommitBlameMatch {
                state: FactState::Ambiguous,
                ..
            })
        )
    }) && !matches.iter().any(|value| {
        matches!(
            value,
            BlameMatch::Commit(CommitBlameMatch {
                predicate: ctx_pro_host_protocol::CommitPredicate::ProducedBy,
                state: FactState::Asserted,
                ..
            })
        )
    }) {
        out.push_str("\nNo producing session is asserted.\n");
    }
}

fn render_commit_match(out: &mut String, value: &CommitBlameMatch, show_predicate: bool) {
    out.push('\n');
    if show_predicate {
        out.push_str(&format!("  {}\n", enum_text(value.predicate)));
    }
    out.push_str(&format!(
        "    {} {}",
        value.subject.kind.wire_name(),
        value.subject.display
    ));
    match &value.object {
        Some(object) => out.push_str(&format!(
            " -> {} {}\n",
            object.kind.wire_name(),
            object.display
        )),
        None => out.push_str(" -> source commit not resolved\n"),
    }
    if let Some(actor) = &value.direct_actor {
        render_resource_line(out, "direct actor", actor);
    }
    if let Some(root) = &value.owning_root {
        render_resource_line(out, "owning root", root);
    }
    if let Some(time) = value.fact_occurred_at_ms {
        out.push_str(&format!("  fact occurred  {time}\n"));
    }
    out.push_str(&format!(
        "  confidence     {}\n",
        enum_text(value.confidence)
    ));
    out.push_str(&format!("  state          {}\n", enum_text(value.state)));
    out.push_str(&format!(
        "  evidence       {}\n",
        evidence_numbers_text(&value.evidence_numbers)
    ));
}

fn render_resource_line(out: &mut String, label: &str, resource: &ResourceRef) {
    out.push_str(&format!(
        "  {label:<13} {} {}\n",
        resource.kind.wire_name(),
        resource.display
    ));
}

fn render_pull_request_matches(out: &mut String, matches: &[BlameMatch]) {
    let pull_requests = matches.iter().filter_map(|value| match value {
        BlameMatch::PullRequest(value) => Some(value),
        BlameMatch::File(_) | BlameMatch::Commit(_) => None,
    });
    let (commits, activities): (Vec<_>, Vec<_>) = pull_requests
        .partition(|value| matches!(value.relationship, PullRequestBlameRelationship::Commit(_)));

    out.push_str("\nCode produced\n");
    if commits.is_empty() {
        out.push_str("  No associated commits on this page.\n");
    } else {
        for value in commits {
            render_pull_request_commit(out, value);
        }
    }

    out.push_str("\nPR activity\n");
    if activities.is_empty() {
        out.push_str("  No cited activity on this page.\n");
    } else {
        for value in activities {
            render_pull_request_activity(out, value);
        }
    }
}

fn render_pull_request_commit(out: &mut String, value: &PullRequestBlameMatch) {
    let PullRequestBlameRelationship::Commit(commit) = &value.relationship else {
        return;
    };
    out.push_str(&format!(
        "  {}  commit {}\n",
        enum_text(commit.relationship),
        commit.commit.display
    ));
    out.push_str(&format!(
        "    membership evidence  {}\n",
        evidence_numbers_text(&commit.evidence_numbers)
    ));
    if commit.production.is_empty() {
        out.push_str("    agent production     not proven\n");
    } else {
        for attribution in &commit.production {
            out.push_str(match attribution.relationship {
                ctx_pro_host_protocol::ProductionRelationship::ProducedBy => "    produced by\n",
                ctx_pro_host_protocol::ProductionRelationship::PossiblyProducedBy => {
                    "    possible producer\n"
                }
            });
            render_attribution(out, attribution, "    ");
        }
    }
}

fn render_pull_request_activity(out: &mut String, value: &PullRequestBlameMatch) {
    let PullRequestBlameRelationship::Activity(activity) = &value.relationship else {
        return;
    };
    out.push_str(&format!(
        "  {:<10} session {}  {}\n",
        enum_text(activity.action),
        activity.session.display,
        evidence_numbers_text(&activity.evidence_numbers)
    ));
    out.push_str(&format!(
        "    confidence {}  state {}\n",
        enum_text(activity.confidence),
        enum_text(activity.state)
    ));
    if let Some(actor) = &activity.direct_actor {
        render_resource_line(out, "direct actor", actor);
    }
    if let Some(root) = &activity.owning_root {
        render_resource_line(out, "owning root", root);
    }
    if let Some(time) = activity.fact_occurred_at_ms {
        out.push_str(&format!("    fact occurred {time}\n"));
    }
}

fn render_evidence(out: &mut String, result: &BlameResult) {
    if result.evidence.is_empty() {
        return;
    }
    out.push_str("\nEvidence\n");
    for evidence in &result.evidence {
        out.push_str(&format!(
            "  [{}] {}\n",
            evidence.number,
            citation_text(&evidence.citation)
        ));
    }
}

fn citation_text(citation: &EvidenceCitation) -> String {
    if let Some(event_id) = citation.event_id {
        return format!("ctx show event {event_id}");
    }
    if let Some(session_id) = citation.session_id {
        return format!("ctx show session {session_id}");
    }
    if let Some(path) = &citation.source_path {
        if let Some(range) = &citation.byte_range {
            return format!("{path}:{}-{}", range.start, range.end_exclusive);
        }
        if let Some(line) = citation.fixture_line {
            return format!("{path}:{line}");
        }
        return path.clone();
    }
    if let Some(observation_id) = citation.observation_id {
        return format!("observation {observation_id}");
    }
    "canonical evidence".to_owned()
}

fn render_continuation(out: &mut String, result: &BlameResult) {
    let Some(next) = &result.next else {
        return;
    };
    match next.reason {
        ContinuationReason::MoreMatches => {
            out.push_str(&format!(
                "\n{} matches shown; more matches are available.\n",
                result.matches.len()
            ));
        }
        ContinuationReason::MoreCommittedLines => {
            let window = emitted_file_window(&result.matches)
                .map(|lines| format!(" for committed lines {}", line_range_text(&lines)))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n{} matches shown{window}; more committed lines are available.\n",
                result.matches.len(),
            ));
        }
    }
    out.push_str("continue  ");
    out.push_str(&continuation_command(result, &next.cursor));
    out.push('\n');
}

fn continuation_command(result: &BlameResult, cursor: &str) -> String {
    let (mut command, repository) = match &result.target {
        ResolvedBlameTarget::File {
            path,
            repository,
            requested_lines,
        } => {
            let mut command = format!("ctx blame file {}", shell_display(path));
            if let Some(lines) = requested_lines {
                command.push_str(" --lines ");
                command.push_str(&line_range_argument(lines));
            }
            (command, repository)
        }
        ResolvedBlameTarget::Commit { commit, repository } => (
            format!("ctx blame commit {}", shell_display(&commit.display)),
            repository,
        ),
        ResolvedBlameTarget::PullRequest {
            selector,
            pull_request: _,
            repository,
        } => (
            format!("ctx blame pr {}", shell_display(selector)),
            repository,
        ),
    };
    command.push_str(" --repository ");
    command.push_str(&shell_display(&repository.display));
    command.push_str(" --cursor ");
    command.push_str(&shell_display(cursor));
    command
}

fn evidence_numbers_text(numbers: &[u32]) -> String {
    numbers
        .iter()
        .map(|number| format!("[{number}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn line_range_text(lines: &LineRange) -> String {
    if lines.start == lines.end {
        lines.start.to_string()
    } else {
        format!("{}-{}", lines.start, lines.end)
    }
}

fn line_range_argument(lines: &LineRange) -> String {
    if lines.start == lines.end {
        lines.start.to_string()
    } else {
        format!("{}:{}", lines.start, lines.end)
    }
}

fn emitted_file_window(matches: &[BlameMatch]) -> Option<LineRange> {
    let mut ranges = matches.iter().filter_map(|value| match value {
        BlameMatch::File(value) => Some(&value.lines),
        BlameMatch::Commit(_) | BlameMatch::PullRequest(_) => None,
    });
    let first = ranges.next()?;
    let mut start = first.start;
    let mut end = first.end;
    for range in ranges {
        start = start.min(range.start);
        end = end.max(range.end);
    }
    Some(LineRange { start, end })
}

fn enum_text<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn shell_display(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:@".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use ctx_pro_host_protocol::{
        CommitFactType, CommitPredicate, FactConfidence, NumberedEvidence, ProductionRelationship,
        PullRequestAction, PullRequestActivity, PullRequestCommit, PullRequestCommitRelationship,
    };
    use uuid::Uuid;

    use super::*;

    fn resource(id: &str, kind: ctx_pro_host_protocol::ResourceKind, display: &str) -> ResourceRef {
        ResourceRef {
            id: id.to_owned(),
            kind,
            display: display.to_owned(),
        }
    }

    fn event_evidence(number: u32) -> NumberedEvidence {
        NumberedEvidence {
            number,
            citation: EvidenceCitation {
                observation_id: None,
                observation_seq: None,
                observation_kind: None,
                session_id: None,
                event_id: Some(Uuid::from_u128(u128::from(number))),
                event_seq: Some(u64::from(number)),
                source_locator: None,
                source_path: None,
                fixture_line: None,
                source_record_ordinal: None,
                source_record_subrecord_index: None,
                byte_range: None,
                source_sha256: None,
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
                ctx_pro_host_protocol::ResourceKind::Session,
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

    fn repository() -> ResourceRef {
        resource(
            "repository:ctxrs/ctx",
            ctx_pro_host_protocol::ResourceKind::Repository,
            "ctxrs/ctx",
        )
    }

    #[test]
    fn commit_renderer_keeps_production_grouping_golden() {
        let commit = resource(
            "commit:abcdef",
            ctx_pro_host_protocol::ResourceKind::Commit,
            "abcdef",
        );
        let match_for = |fact_id: &str,
                         fact_type: CommitFactType,
                         predicate: CommitPredicate,
                         object: &str,
                         confidence: FactConfidence,
                         state: FactState,
                         evidence_number: u32| {
            BlameMatch::Commit(CommitBlameMatch {
                fact_id: fact_id.to_owned(),
                fact_type,
                predicate,
                subject: commit.clone(),
                object: Some(resource(
                    &format!("session:{object}"),
                    ctx_pro_host_protocol::ResourceKind::Session,
                    object,
                )),
                fact_occurred_at_ms: None,
                confidence,
                state,
                direct_actor: None,
                owning_root: None,
                evidence_numbers: vec![evidence_number],
            })
        };
        let result = BlameResult {
            target: ResolvedBlameTarget::Commit {
                commit: commit.clone(),
                repository: repository(),
            },
            git_snapshot: None,
            matches: vec![
                match_for(
                    "fact:produced",
                    CommitFactType::Produced,
                    CommitPredicate::ProducedBy,
                    "producer",
                    FactConfidence::Explicit,
                    FactState::Asserted,
                    1,
                ),
                match_for(
                    "fact:possible",
                    CommitFactType::Ambiguous,
                    CommitPredicate::PossiblyProducedBy,
                    "possible",
                    FactConfidence::Ambiguous,
                    FactState::Ambiguous,
                    2,
                ),
                match_for(
                    "fact:referenced",
                    CommitFactType::Referenced,
                    CommitPredicate::ReferencedBy,
                    "observer",
                    FactConfidence::Explicit,
                    FactState::Asserted,
                    3,
                ),
            ],
            evidence: vec![event_evidence(1), event_evidence(2), event_evidence(3)],
            next: None,
        };
        result.validate().unwrap();
        assert_eq!(
            render_blame_text(&result),
            include_str!("../../testdata/pro/blame_commit.golden.txt")
        );
    }

    #[test]
    fn pull_request_renderer_labels_proof_edges_and_uses_valid_continuation_golden() {
        let pull_request = resource(
            "pull_request:ctxrs/ctx#42",
            ctx_pro_host_protocol::ResourceKind::PullRequest,
            "ctxrs/ctx#42",
        );
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
                        commit: resource(
                            "commit:deadbeef",
                            ctx_pro_host_protocol::ResourceKind::Commit,
                            "deadbeef",
                        ),
                        production: vec![
                            attribution(
                                "fact:producer",
                                ProductionRelationship::ProducedBy,
                                "producer",
                                2,
                            ),
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
                        session: resource(
                            "session:reviewer",
                            ctx_pro_host_protocol::ResourceKind::Session,
                            "reviewer",
                        ),
                        direct_actor: None,
                        owning_root: None,
                        fact_occurred_at_ms: None,
                        confidence: FactConfidence::Explicit,
                        state: FactState::Asserted,
                        evidence_numbers: vec![4],
                    }),
                }),
            ],
            evidence: (1..=4).map(event_evidence).collect(),
            next: Some(ctx_pro_host_protocol::BlameContinuation {
                cursor: "next-page".to_owned(),
                reason: ContinuationReason::MoreMatches,
            }),
        };
        result.validate().unwrap();
        assert_eq!(
            render_blame_text(&result),
            include_str!("../../testdata/pro/blame_pr.golden.txt")
        );
    }

    #[test]
    fn pull_request_commit_only_page_scopes_missing_activity_golden() {
        let pull_request = resource(
            "pull_request:ctxrs/ctx#42",
            ctx_pro_host_protocol::ResourceKind::PullRequest,
            "ctxrs/ctx#42",
        );
        let result = BlameResult {
            target: ResolvedBlameTarget::PullRequest {
                selector: "42".to_owned(),
                pull_request: pull_request.clone(),
                repository: repository(),
            },
            git_snapshot: None,
            matches: vec![BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request,
                relationship: PullRequestBlameRelationship::Commit(PullRequestCommit {
                    fact_id: "fact:membership".to_owned(),
                    relationship: PullRequestCommitRelationship::ContainsCommit,
                    commit: resource(
                        "commit:deadbeef",
                        ctx_pro_host_protocol::ResourceKind::Commit,
                        "deadbeef",
                    ),
                    production: Vec::new(),
                    evidence_numbers: vec![1],
                }),
            })],
            evidence: vec![event_evidence(1)],
            next: Some(ctx_pro_host_protocol::BlameContinuation {
                cursor: "activity-page".to_owned(),
                reason: ContinuationReason::MoreMatches,
            }),
        };
        result.validate().unwrap();
        assert_eq!(
            render_blame_text(&result),
            include_str!("../../testdata/pro/blame_pr_commit_only_page.golden.txt")
        );
    }

    #[test]
    fn pull_request_activity_only_page_scopes_missing_commits_golden() {
        let pull_request = resource(
            "pull_request:ctxrs/ctx#42",
            ctx_pro_host_protocol::ResourceKind::PullRequest,
            "ctxrs/ctx#42",
        );
        let result = BlameResult {
            target: ResolvedBlameTarget::PullRequest {
                selector: "42".to_owned(),
                pull_request: pull_request.clone(),
                repository: repository(),
            },
            git_snapshot: None,
            matches: vec![BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request,
                relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
                    fact_id: "fact:reviewed".to_owned(),
                    action: PullRequestAction::Reviewed,
                    session: resource(
                        "session:reviewer",
                        ctx_pro_host_protocol::ResourceKind::Session,
                        "reviewer",
                    ),
                    direct_actor: None,
                    owning_root: None,
                    fact_occurred_at_ms: None,
                    confidence: FactConfidence::Explicit,
                    state: FactState::Asserted,
                    evidence_numbers: vec![1],
                }),
            })],
            evidence: vec![event_evidence(1)],
            next: Some(ctx_pro_host_protocol::BlameContinuation {
                cursor: "commit-page".to_owned(),
                reason: ContinuationReason::MoreMatches,
            }),
        };
        result.validate().unwrap();
        assert_eq!(
            render_blame_text(&result),
            include_str!("../../testdata/pro/blame_pr_activity_only_page.golden.txt")
        );
    }

    #[test]
    fn file_continuation_uses_colon_range_and_committed_window_golden() {
        let result = BlameResult {
            target: ResolvedBlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: repository(),
                requested_lines: Some(LineRange { start: 42, end: 60 }),
            },
            git_snapshot: Some(ctx_pro_host_protocol::GitSnapshot {
                head_oid: "deadbeef".to_owned(),
                worktree_status: WorktreeStatus::Differs,
            }),
            matches: vec![BlameMatch::File(FileBlameMatch {
                id: "file:42-50".to_owned(),
                lines: LineRange { start: 42, end: 50 },
                commit: resource(
                    "commit:deadbeef",
                    ctx_pro_host_protocol::ResourceKind::Commit,
                    "deadbeef",
                ),
                line_evidence_numbers: vec![1],
                production: Vec::new(),
            })],
            evidence: vec![event_evidence(1)],
            next: Some(ctx_pro_host_protocol::BlameContinuation {
                cursor: "more-lines".to_owned(),
                reason: ContinuationReason::MoreCommittedLines,
            }),
        };
        result.validate().unwrap();
        assert_eq!(
            render_blame_text(&result),
            include_str!("../../testdata/pro/blame_file_continuation.golden.txt")
        );
    }
}
