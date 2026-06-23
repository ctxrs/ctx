use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{json, Value};
use work_record_core::{
    redact_share_safe_markers, Artifact, Event, EventType, Evidence, EvidenceMetadata, FileTouched,
    PullRequest, RedactionState, Run, Session, Summary, VcsChange, VcsWorkspace, WorkContext,
    WorkRecord, WorkRecordArchive, WorkRecordArchiveArtifact,
};

#[derive(Debug, Clone, Serialize)]
pub struct ReportSummary {
    pub record_count: usize,
    pub evidence_count: usize,
    pub linked_pr_count: usize,
    pub tags: Vec<TagCount>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub count: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DashboardReport<'a> {
    pub records: &'a [WorkRecord],
    pub evidence: &'a [Evidence],
    pub archive_artifacts: &'a [WorkRecordArchiveArtifact],
    pub sessions: &'a [Session],
    pub runs: &'a [Run],
    pub events: &'a [Event],
    pub vcs_workspaces: &'a [VcsWorkspace],
    pub vcs_changes: &'a [VcsChange],
    pub pull_requests: &'a [PullRequest],
    pub artifacts: &'a [Artifact],
    pub evidence_metadata: &'a [EvidenceMetadata],
    pub files_touched: &'a [FileTouched],
    pub summaries: &'a [Summary],
}

impl<'a> DashboardReport<'a> {
    pub fn from_records(records: &'a [WorkRecord], evidence: &'a [Evidence]) -> Self {
        Self {
            records,
            evidence,
            archive_artifacts: &[],
            sessions: &[],
            runs: &[],
            events: &[],
            vcs_workspaces: &[],
            vcs_changes: &[],
            pull_requests: &[],
            artifacts: &[],
            evidence_metadata: &[],
            files_touched: &[],
            summaries: &[],
        }
    }

    pub fn from_archive(archive: &'a WorkRecordArchive) -> Self {
        Self {
            records: &archive.records,
            evidence: &archive.evidence,
            archive_artifacts: &archive.artifacts,
            evidence_metadata: &archive.evidence_metadata,
            ..Self::from_records(&archive.records, &archive.evidence)
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceReport {
    pub schema_version: u32,
    pub share_safe: bool,
    pub summary: ReportSummary,
    pub privacy: PrivacySummary,
    pub records: Vec<EvidenceRecordReport>,
    pub commands: Vec<EvidenceCommandReport>,
    pub pull_requests: Vec<SafePullRequest>,
    pub evidence_metadata: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivacySummary {
    pub default_redacted: bool,
    pub raw_transcripts_withheld: usize,
    pub redacted_previews: usize,
    pub withheld_links: usize,
    pub local_paths_redacted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceRecordReport {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub pr_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceCommandReport {
    pub id: String,
    pub record_id: Option<String>,
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: i64,
    pub started_at: String,
    pub output_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SafePullRequest {
    pub url: String,
    pub title: Option<String>,
    pub state: Option<String>,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardExportData {
    pub schema_version: u32,
    pub product: &'static str,
    pub share_safe: bool,
    pub summary: ReportSummary,
    pub privacy: PrivacySummary,
    pub views: Vec<&'static str>,
    pub records: Vec<DashboardRecord>,
    pub commands: Vec<EvidenceCommandReport>,
    pub sessions: Vec<Value>,
    pub runs: Vec<Value>,
    pub events: Vec<Value>,
    pub vcs_workspaces: Vec<Value>,
    pub vcs_changes: Vec<Value>,
    pub pull_requests: Vec<SafePullRequest>,
    pub artifacts: Vec<Value>,
    pub evidence_metadata: Vec<Value>,
    pub files_touched: Vec<Value>,
    pub summaries: Vec<Value>,
    pub status: DashboardStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardRecord {
    pub id: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub kind: String,
    pub workspace: Option<String>,
    pub pr_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardStatus {
    pub export_mode: &'static str,
    pub local_only: bool,
    pub javascript_app: &'static str,
    pub data_contract: &'static str,
    pub search_command: &'static str,
}

pub fn summarize(records: &[WorkRecord], evidence: &[Evidence]) -> ReportSummary {
    let mut tag_counts = std::collections::BTreeMap::<String, usize>::new();
    for record in records {
        for tag in &record.tags {
            *tag_counts
                .entry(redact_share_safe_markers(tag))
                .or_default() += 1;
        }
    }

    ReportSummary {
        record_count: records.len(),
        evidence_count: evidence.len(),
        linked_pr_count: records
            .iter()
            .filter(|record| record.pr_url.is_some())
            .count(),
        tags: tag_counts
            .into_iter()
            .map(|(tag, count)| TagCount { tag, count })
            .collect(),
    }
}

pub fn render_text(records: &[WorkRecord], evidence: &[Evidence]) -> String {
    let summary = summarize(records, evidence);
    let mut out = String::new();
    out.push_str("Work Recorder Report\n");
    out.push_str(&format!("records: {}\n", summary.record_count));
    out.push_str(&format!("evidence: {}\n", summary.evidence_count));
    out.push_str(&format!("linked_prs: {}\n", summary.linked_pr_count));
    if !summary.tags.is_empty() {
        out.push_str("tags:\n");
        for tag in summary.tags {
            out.push_str(&format!("  {}: {}\n", tag.tag, tag.count));
        }
    }
    out
}

pub fn render_json(records: &[WorkRecord], evidence: &[Evidence]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&summarize(records, evidence))
}

pub fn render_dashboard_html(records: &[WorkRecord], evidence: &[Evidence]) -> String {
    render_dashboard_html_report(&DashboardReport::from_records(records, evidence))
}

pub fn render_dashboard_html_archive(archive: &WorkRecordArchive) -> String {
    render_dashboard_html_report(&DashboardReport::from_archive(archive))
}

pub fn render_dashboard_html_report(report: &DashboardReport<'_>) -> String {
    render_dashboard_spa_html(report)
}

pub fn dashboard_static_assets() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            "assets/dashboard-DnBIV6pA.js",
            include_bytes!("../../../apps/work-recorder-dashboard/dist/assets/dashboard-DnBIV6pA.js"),
        ),
        (
            "assets/styles-CVYnGeXJ.js",
            include_bytes!("../../../apps/work-recorder-dashboard/dist/assets/styles-CVYnGeXJ.js"),
        ),
        (
            "assets/styles-Dr8iICqu.css",
            include_bytes!("../../../apps/work-recorder-dashboard/dist/assets/styles-Dr8iICqu.css"),
        ),
    ]
}

pub fn dashboard_export_data(report: &DashboardReport<'_>) -> DashboardExportData {
    DashboardExportData {
        schema_version: 1,
        product: "ctx Work Recorder",
        share_safe: true,
        summary: summarize(report.records, report.evidence),
        privacy: privacy_summary(report),
        views: vec![
            "Overview",
            "Workspace / Repo",
            "Provider Coverage",
            "Session Detail",
            "PR / Evidence",
            "Search / Explore",
            "Settings / Status",
            "Transcript, Messages, and Tool Calls",
            "Artifacts",
        ],
        records: report
            .records
            .iter()
            .map(|record| DashboardRecord {
                id: record.id.to_string(),
                title: redact_share_safe_markers(&record.title),
                body: redact_share_safe_markers(&record.body),
                tags: record
                    .tags
                    .iter()
                    .map(|tag| redact_share_safe_markers(tag))
                    .collect(),
                kind: redact_share_safe_markers(&record.kind),
                workspace: record
                    .workspace
                    .as_deref()
                    .map(safe_workspace_label),
                pr_url: record.pr_url.as_deref().and_then(safe_external_url),
                created_at: record.created_at.to_rfc3339(),
                updated_at: record.updated_at.to_rfc3339(),
            })
            .collect(),
        commands: evidence_report(report).commands,
        sessions: report
            .sessions
            .iter()
            .map(|session| {
                json!({
                    "id": session.id.to_string(),
                    "work_record_id": session.work_record_id.map(|id| id.to_string()),
                    "parent_session_id": session.parent_session_id.map(|id| id.to_string()),
                    "root_session_id": session.root_session_id.map(|id| id.to_string()),
                    "provider": session.provider.as_str(),
                    "external_session_id": session.external_session_id.as_deref().map(redact_share_safe_markers),
                    "external_agent_id": session.external_agent_id.as_deref().map(redact_share_safe_markers),
                    "agent_type": session.agent_type.as_str(),
                    "role_hint": session.role_hint.as_deref().map(redact_share_safe_markers),
                    "is_primary": session.is_primary,
                    "status": session.status.as_str(),
                    "fidelity": session.sync.fidelity.as_str(),
                    "transcript_blob_id": session.transcript_blob_id.map(|id| id.to_string()),
                    "started_at": session.started_at.to_rfc3339(),
                    "ended_at": session.ended_at.map(|time| time.to_rfc3339()),
                })
            })
            .collect(),
        runs: report
            .runs
            .iter()
            .map(|run| {
                json!({
                    "id": run.id.to_string(),
                    "work_record_id": run.work_record_id.map(|id| id.to_string()),
                    "session_id": run.session_id.map(|id| id.to_string()),
                    "run_type": run.run_type.as_str(),
                    "status": run.status.as_str(),
                    "started_at": run.started_at.to_rfc3339(),
                    "ended_at": run.ended_at.map(|time| time.to_rfc3339()),
                    "exit_code": run.exit_code,
                    "cwd": run.cwd.as_deref().map(safe_workspace_label),
                    "command_preview": run.command_preview.as_deref().map(redact_share_safe_markers),
                })
            })
            .collect(),
        events: report
            .events
            .iter()
            .map(|event| {
                json!({
                    "id": event.id.to_string(),
                    "seq": event.seq,
                    "work_record_id": event.work_record_id.map(|id| id.to_string()),
                    "session_id": event.session_id.map(|id| id.to_string()),
                    "run_id": event.run_id.map(|id| id.to_string()),
                    "event_type": event.event_type.as_str(),
                    "role": event.role.map(|role| role.as_str()),
                    "occurred_at": event.occurred_at.to_rfc3339(),
                    "preview": event_preview(event),
                    "payload_blob_id": event.payload_blob_id.map(|id| id.to_string()),
                    "redaction_state": event.redaction_state.as_str(),
                    "fidelity": event.sync.fidelity.as_str(),
                })
            })
            .collect(),
        vcs_workspaces: report
            .vcs_workspaces
            .iter()
            .map(|workspace| {
                json!({
                    "id": workspace.id.to_string(),
                    "kind": workspace.kind.as_str(),
                    "repo": safe_repo_label(workspace),
                    "root": safe_workspace_label(&workspace.root_path),
                    "host": workspace.host.as_str(),
                    "owner": workspace.owner.as_deref().map(redact_share_safe_markers),
                    "name": workspace.name.as_deref().map(redact_share_safe_markers),
                    "monorepo_subpath": workspace.monorepo_subpath.as_deref().map(redact_share_safe_markers),
                })
            })
            .collect(),
        vcs_changes: report
            .vcs_changes
            .iter()
            .map(|change| {
                json!({
                    "id": change.id.to_string(),
                    "vcs_workspace_id": change.vcs_workspace_id.to_string(),
                    "kind": change.kind.as_str(),
                    "change_id": redact_share_safe_markers(&change.change_id),
                    "branch_or_bookmark": change.branch_or_bookmark.as_deref().map(redact_share_safe_markers),
                    "tree_hash": change.tree_hash.as_deref().map(redact_share_safe_markers),
                    "author_time": change.author_time.map(|time| time.to_rfc3339()),
                })
            })
            .collect(),
        pull_requests: evidence_report(report).pull_requests,
        artifacts: report
            .artifacts
            .iter()
            .map(|artifact| {
                json!({
                    "id": artifact.id.to_string(),
                    "kind": artifact.kind.as_str(),
                    "byte_size": artifact.byte_size,
                    "media_type": artifact.media_type.as_deref().map(redact_share_safe_markers),
                    "redaction_state": artifact.redaction_state.as_str(),
                    "preview": safe_artifact_preview(artifact.redaction_state, artifact.preview_text.as_deref()),
                })
            })
            .chain(report.archive_artifacts.iter().map(|artifact| {
                json!({
                    "id": artifact.id.to_string(),
                    "evidence_id": artifact.evidence_id.to_string(),
                    "kind": artifact.kind.as_str(),
                    "byte_size": artifact.byte_size,
                    "media_type": artifact.media_type.as_deref().map(redact_share_safe_markers),
                    "redaction_state": artifact.redaction_state.as_str(),
                    "preview": safe_artifact_preview(artifact.redaction_state, artifact.preview_text.as_deref()),
                })
            }))
            .collect(),
        evidence_metadata: evidence_metadata_values(report),
        files_touched: report
            .files_touched
            .iter()
            .map(|file| {
                json!({
                    "id": file.id.to_string(),
                    "work_record_id": file.work_record_id.map(|id| id.to_string()),
                    "path": share_safe_relative_path(&file.path),
                    "change_kind": file.change_kind.map(|kind| kind.as_str()),
                    "old_path": file.old_path.as_deref().map(share_safe_relative_path),
                    "line_count_delta": file.line_count_delta,
                    "confidence": file.confidence.as_str(),
                })
            })
            .collect(),
        summaries: report
            .summaries
            .iter()
            .map(|summary| {
                json!({
                    "id": summary.id.to_string(),
                    "work_record_id": summary.work_record_id.map(|id| id.to_string()),
                    "session_id": summary.session_id.map(|id| id.to_string()),
                    "kind": summary.kind.as_str(),
                    "model_or_source": summary.model_or_source.as_deref().map(redact_share_safe_markers),
                    "text": redact_share_safe_markers(&summary.text),
                })
            })
            .collect(),
        status: DashboardStatus {
            export_mode: "Static local export",
            local_only: true,
            javascript_app: "React/Vite",
            data_contract: "Work Recorder dashboard export v1",
            search_command: "ctx search <query> --json",
        },
    }
}

fn render_dashboard_spa_html(report: &DashboardReport<'_>) -> String {
    let data = dashboard_export_data(report);
    let data_json = serde_json::to_string(&data)
        .expect("dashboard export data must serialize")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    include_str!("../../../apps/work-recorder-dashboard/dist/index.html")
        .replace("__CTX_DASHBOARD_DATA__", &data_json)
}

pub fn render_evidence_report_json(report: &DashboardReport<'_>) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&evidence_report(report))
}

pub fn render_evidence_report_markdown(report: &DashboardReport<'_>) -> String {
    let report = evidence_report(report);
    let mut out = String::new();
    out.push_str("# Work Recorder Evidence Report\n\n");
    out.push_str("Share-safe: yes\n\n");
    out.push_str("## Summary\n\n");
    out.push_str(&format!("- Records: {}\n", report.summary.record_count));
    out.push_str(&format!("- Commands: {}\n", report.commands.len()));
    out.push_str(&format!(
        "- Linked PR URLs: {}\n",
        report.summary.linked_pr_count
    ));
    out.push_str(&format!(
        "- Typed PR records: {}\n",
        report.pull_requests.len()
    ));
    out.push_str(&format!(
        "- Raw transcripts withheld: {}\n\n",
        report.privacy.raw_transcripts_withheld
    ));

    out.push_str("## Records\n\n");
    for record in &report.records {
        out.push_str(&format!("- `{}` {}\n", record.id, record.title));
        if !record.summary.is_empty() {
            out.push_str(&format!("  {}\n", record.summary));
        }
        if let Some(pr_url) = &record.pr_url {
            out.push_str(&format!("  PR: {pr_url}\n"));
        }
    }

    out.push_str("\n## Commands\n\n");
    for command in &report.commands {
        out.push_str(&format!(
            "- `{}` exit {} in {}ms\n",
            command.command, command.exit_code, command.duration_ms
        ));
        if let Some(preview) = &command.output_preview {
            out.push_str("  ```text\n");
            out.push_str(preview);
            out.push_str("\n  ```\n");
        }
    }
    out
}

pub fn context_markdown(context: &WorkContext) -> String {
    let mut out = String::new();
    out.push_str("# Work Context\n\n");
    if let Some(query) = &context.query {
        out.push_str(&format!(
            "query: `{}`\n\n",
            redact_share_safe_markers(query)
        ));
    }
    for record in &context.records {
        out.push_str(&format!(
            "## {}\n",
            redact_share_safe_markers(&record.title)
        ));
        out.push_str(&format!("id: `{}`\n", record.id));
        if !record.tags.is_empty() {
            out.push_str(&format!(
                "tags: {}\n",
                redact_share_safe_markers(&record.tags.join(", "))
            ));
        }
        out.push('\n');
        out.push_str(&redact_share_safe_markers(&record.body));
        out.push_str("\n\n");
    }
    if !context.evidence.is_empty() {
        out.push_str("## Evidence\n");
        for evidence in &context.evidence {
            out.push_str(&format!(
                "- `{}` exited {} in {}ms\n",
                redact_share_safe_markers(&evidence.command),
                evidence.exit_code,
                evidence.duration_ms
            ));
        }
    }
    out
}

pub fn archive_json(archive: &WorkRecordArchive) -> serde_json::Result<String> {
    serde_json::to_string_pretty(archive)
}

#[allow(dead_code)]
fn metric(out: &mut String, value: usize, label: &str) {
    out.push_str("<div class=\"metric\"><strong>");
    out.push_str(&value.to_string());
    out.push_str("</strong><span>");
    push_escaped(out, label);
    out.push_str("</span></div>");
}

#[allow(dead_code)]
fn render_record(out: &mut String, record: &WorkRecord) {
    out.push_str("<article class=\"record\" id=\"record-");
    out.push_str(&record.id.to_string());
    out.push_str("\"><h3>");
    push_escaped(out, &redact_share_safe_markers(&record.title));
    out.push_str("</h3><div class=\"meta\"><span class=\"pill\">");
    push_escaped(out, &redact_share_safe_markers(&record.kind));
    out.push_str("</span><span>");
    push_escaped(out, &record.created_at.to_rfc3339());
    out.push_str("</span>");
    if let Some(workspace) = &record.workspace {
        out.push_str("<span>");
        push_escaped(out, &safe_workspace_label(workspace));
        out.push_str("</span>");
    }
    out.push_str("</div>");

    if !record.tags.is_empty() {
        out.push_str("<div class=\"meta\">");
        for tag in &record.tags {
            out.push_str("<span class=\"pill\">#");
            push_escaped(out, &redact_share_safe_markers(tag));
            out.push_str("</span>");
        }
        out.push_str("</div>");
    }

    if !record.body.is_empty() {
        out.push_str("<div class=\"body\">");
        push_escaped(out, &redact_share_safe_markers(&record.body));
        out.push_str("</div>");
    }

    if let Some(pr_url) = &record.pr_url {
        out.push_str("<div class=\"meta\">PR: ");
        if let Some(safe_url) = safe_external_url(pr_url) {
            out.push_str("<a class=\"pr\" rel=\"noreferrer\" href=\"");
            push_attr_escaped(out, &safe_url);
            out.push_str("\">");
            push_escaped(out, &safe_url);
            out.push_str("</a>");
        } else {
            out.push_str("<span class=\"pr\">");
            push_escaped(out, "link withheld");
            out.push_str("</span>");
        }
        out.push_str("</div>");
    }

    out.push_str("</article>\n");
}

#[allow(dead_code)]
fn render_evidence(out: &mut String, evidence: &Evidence) {
    out.push_str("<article class=\"evidence\"><div><code>");
    push_escaped(out, &redact_share_safe_markers(&evidence.command));
    out.push_str("</code></div><div class=\"meta\"><span class=\"");
    out.push_str(if evidence.exit_code == 0 {
        "status-ok"
    } else {
        "status-fail"
    });
    out.push_str("\">exit ");
    out.push_str(&evidence.exit_code.to_string());
    out.push_str("</span><span>");
    out.push_str(&evidence.duration_ms.to_string());
    out.push_str("ms</span><span>");
    push_escaped(out, &evidence.started_at.to_rfc3339());
    out.push_str("</span></div>");
    if let Some(preview) = evidence_preview(evidence) {
        out.push_str("<pre class=\"preview\">");
        push_escaped(out, &redact_share_safe_markers(preview));
        out.push_str("</pre>");
    }
    out.push_str("</article>\n");
}

#[allow(dead_code)]
fn render_sessions_runs(out: &mut String, report: &DashboardReport<'_>) {
    out.push_str("<section><h2>Sessions and Runs</h2>");
    if report.sessions.is_empty() && report.runs.is_empty() {
        out.push_str("<div class=\"empty\">No session or run metadata is available in this export.</div></section>");
        return;
    }
    out.push_str("<div class=\"panel\"><table class=\"table\"><thead><tr><th>Type</th><th>Status</th><th>Details</th></tr></thead><tbody>");
    for session in report.sessions.iter().take(12) {
        out.push_str("<tr><td>session</td><td>");
        push_escaped(out, session.status.as_str());
        out.push_str("</td><td>");
        push_escaped(out, session.provider.as_str());
        if let Some(role) = &session.role_hint {
            out.push_str(" / ");
            push_escaped(out, &redact_share_safe_markers(role));
        }
        out.push_str("</td></tr>");
    }
    for run in report.runs.iter().take(16) {
        out.push_str("<tr><td>run</td><td>");
        push_escaped(out, run.status.as_str());
        out.push_str("</td><td>");
        if let Some(command) = &run.command_preview {
            push_escaped(out, &redact_share_safe_markers(command));
        } else {
            push_escaped(out, run.run_type.as_str());
        }
        if let Some(exit_code) = run.exit_code {
            out.push_str(" exit ");
            out.push_str(&exit_code.to_string());
        }
        out.push_str("</td></tr>");
    }
    out.push_str("</tbody></table></div></section>");
}

#[allow(dead_code)]
fn render_summaries(out: &mut String, report: &DashboardReport<'_>) {
    if report.summaries.is_empty() {
        return;
    }
    out.push_str("<section><h2>Summaries</h2>");
    for summary in report.summaries.iter().take(8) {
        out.push_str("<article class=\"panel\"><div class=\"meta\"><span class=\"pill\">");
        push_escaped(out, summary.kind.as_str());
        out.push_str("</span>");
        if let Some(source) = &summary.model_or_source {
            out.push_str("<span>");
            push_escaped(out, &redact_share_safe_markers(source));
            out.push_str("</span>");
        }
        out.push_str("</div><div class=\"body\">");
        push_escaped(out, &redact_share_safe_markers(&summary.text));
        out.push_str("</div></article>");
    }
    out.push_str("</section>");
}

#[allow(dead_code)]
fn render_timeline(out: &mut String, report: &DashboardReport<'_>) {
    out.push_str("<section><h2>Timeline</h2>");
    if report.events.is_empty() && report.runs.is_empty() {
        out.push_str(
            "<div class=\"empty\">No timeline events are available in this export.</div></section>",
        );
        return;
    }
    out.push_str("<div class=\"panel timeline\">");
    for run in report.runs.iter().take(6) {
        out.push_str("<div class=\"timeline-item\"><strong>");
        push_escaped(out, run.run_type.as_str());
        out.push_str("</strong><div class=\"meta\"><span>");
        push_escaped(out, &run.started_at.to_rfc3339());
        out.push_str("</span><span>");
        push_escaped(out, run.status.as_str());
        out.push_str("</span></div>");
        if let Some(command) = &run.command_preview {
            out.push_str("<div class=\"body\">");
            push_escaped(out, &redact_share_safe_markers(command));
            out.push_str("</div>");
        }
        out.push_str("</div>");
    }
    for event in report.events.iter().take(10) {
        out.push_str("<div class=\"timeline-item\"><strong>");
        push_escaped(out, event.event_type.as_str());
        out.push_str("</strong><div class=\"meta\"><span>#");
        out.push_str(&event.seq.to_string());
        out.push_str("</span><span>");
        push_escaped(out, &event.occurred_at.to_rfc3339());
        out.push_str("</span></div>");
        if let Some(preview) = event_preview(event) {
            out.push_str("<div class=\"body\">");
            push_escaped(out, &preview);
            out.push_str("</div>");
        }
        out.push_str("</div>");
    }
    out.push_str("</div></section>");
}

#[allow(dead_code)]
fn render_transcript_views(out: &mut String, report: &DashboardReport<'_>) {
    let transcript_like = report
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type,
                EventType::Message | EventType::ToolCall | EventType::ToolOutput
            )
        })
        .take(12)
        .collect::<Vec<_>>();
    out.push_str("<section><h2>Transcript, Messages, and Tool Calls</h2>");
    if transcript_like.is_empty() {
        out.push_str("<div class=\"empty\">No redacted transcript events are available. Raw transcript blobs remain withheld.</div></section>");
        return;
    }
    for event in transcript_like {
        out.push_str("<article class=\"panel\"><div class=\"meta\"><span class=\"pill\">");
        push_escaped(out, event.event_type.as_str());
        out.push_str("</span>");
        if let Some(role) = event.role {
            out.push_str("<span>");
            push_escaped(out, role.as_str());
            out.push_str("</span>");
        }
        out.push_str("<span>");
        push_escaped(out, event.redaction_state.as_str());
        out.push_str("</span></div>");
        if let Some(preview) = event_preview(event) {
            out.push_str("<div class=\"body\">");
            push_escaped(out, &preview);
            out.push_str("</div>");
        }
        out.push_str("</article>");
    }
    out.push_str("</section>");
}

#[allow(dead_code)]
fn render_files_touched(out: &mut String, report: &DashboardReport<'_>) {
    out.push_str("<section><h2>Files Touched</h2>");
    if report.files_touched.is_empty() {
        out.push_str("<div class=\"empty\">No file touch metadata is available in this export.</div></section>");
        return;
    }
    out.push_str("<div class=\"panel\"><table class=\"table\"><thead><tr><th>Path</th><th>Change</th><th>Delta</th></tr></thead><tbody>");
    for file in report.files_touched.iter().take(25) {
        out.push_str("<tr><td><code>");
        push_escaped(out, &share_safe_relative_path(&file.path));
        out.push_str("</code></td><td>");
        if let Some(kind) = file.change_kind {
            push_escaped(out, kind.as_str());
        } else {
            out.push_str("unknown");
        }
        out.push_str("</td><td>");
        if let Some(delta) = file.line_count_delta {
            out.push_str(&delta.to_string());
        }
        out.push_str("</td></tr>");
    }
    out.push_str("</tbody></table></div></section>");
}

#[allow(dead_code)]
fn render_evidence_metadata(out: &mut String, report: &DashboardReport<'_>) {
    if report.evidence_metadata.is_empty() {
        return;
    }
    out.push_str("<section><h2>Evidence Status</h2><div class=\"panel\"><table class=\"table\"><thead><tr><th>Kind</th><th>Status</th><th>Freshness</th></tr></thead><tbody>");
    for evidence in report.evidence_metadata.iter().take(16) {
        out.push_str("<tr><td>");
        push_escaped(out, evidence.kind.as_str());
        out.push_str("</td><td>");
        push_escaped(out, evidence.status.as_str());
        out.push_str("</td><td>");
        push_escaped(out, evidence.freshness.as_str());
        out.push_str("</td></tr>");
    }
    out.push_str("</tbody></table></div></section>");
}

#[allow(dead_code)]
fn render_vcs(out: &mut String, report: &DashboardReport<'_>) {
    out.push_str("<section><h2>Git and jj State</h2>");
    if report.vcs_workspaces.is_empty() && report.vcs_changes.is_empty() {
        out.push_str(
            "<div class=\"empty\">No Git or jj state is available in this export.</div></section>",
        );
        return;
    }
    out.push_str("<div class=\"panel\">");
    for workspace in report.vcs_workspaces.iter().take(8) {
        out.push_str("<div class=\"meta\"><span class=\"pill\">");
        push_escaped(out, workspace.kind.as_str());
        out.push_str("</span><span>");
        if let Some(owner) = &workspace.owner {
            push_escaped(out, owner);
            out.push('/');
        }
        if let Some(name) = &workspace.name {
            push_escaped(out, name);
        } else {
            push_escaped(out, &safe_workspace_label(&workspace.root_path));
        }
        out.push_str("</span></div>");
    }
    for change in report.vcs_changes.iter().take(12) {
        out.push_str("<div class=\"body\"><code>");
        push_escaped(out, change.kind.as_str());
        out.push_str("</code> ");
        push_escaped(out, &redact_share_safe_markers(&change.change_id));
        if let Some(branch) = &change.branch_or_bookmark {
            out.push_str(" on ");
            push_escaped(out, &redact_share_safe_markers(branch));
        }
        out.push_str("</div>");
    }
    out.push_str("</div></section>");
}

#[allow(dead_code)]
fn render_pr_links(out: &mut String, report: &DashboardReport<'_>) {
    let mut urls = BTreeSet::<String>::new();
    for record in report.records {
        if let Some(url) = &record.pr_url {
            urls.insert(url.clone());
        }
    }
    for pr in report.pull_requests {
        urls.insert(pr.url.clone());
    }
    out.push_str("<section><h2>PR Links</h2>");
    if urls.is_empty() {
        out.push_str("<div class=\"empty\">No pull request links are available in this export.</div></section>");
        return;
    }
    out.push_str("<div class=\"panel\">");
    for url in urls {
        if let Some(safe_url) = safe_external_url(&url) {
            out.push_str("<div><a class=\"pr\" rel=\"noreferrer\" href=\"");
            push_attr_escaped(out, &safe_url);
            out.push_str("\">");
            push_escaped(out, &safe_url);
            out.push_str("</a></div>");
        } else {
            out.push_str("<div class=\"status-note\">link withheld</div>");
        }
    }
    out.push_str("</div></section>");
}

#[allow(dead_code)]
fn render_artifacts(out: &mut String, report: &DashboardReport<'_>) {
    out.push_str("<section><h2>Artifacts</h2>");
    if report.artifacts.is_empty() && report.archive_artifacts.is_empty() {
        out.push_str(
            "<div class=\"empty\">No artifacts are available in this export.</div></section>",
        );
        return;
    }
    for artifact in report.artifacts.iter().take(12) {
        out.push_str("<article class=\"panel\"><div class=\"meta\"><span class=\"pill\">");
        push_escaped(out, artifact.kind.as_str());
        out.push_str("</span><span>");
        out.push_str(&artifact.byte_size.to_string());
        out.push_str(" bytes</span><span>");
        push_escaped(out, artifact.redaction_state.as_str());
        out.push_str("</span></div>");
        if let Some(preview) =
            safe_artifact_preview(artifact.redaction_state, artifact.preview_text.as_deref())
        {
            out.push_str("<pre class=\"preview\">");
            push_escaped(out, &preview);
            out.push_str("</pre>");
        }
        out.push_str("</article>");
    }
    for artifact in report.archive_artifacts.iter().take(12) {
        out.push_str("<article class=\"panel\"><div class=\"meta\"><span class=\"pill\">");
        push_escaped(out, artifact.kind.as_str());
        out.push_str("</span><span>");
        out.push_str(&artifact.byte_size.to_string());
        out.push_str(" bytes</span><span>");
        push_escaped(out, artifact.redaction_state.as_str());
        out.push_str("</span></div>");
        if let Some(preview) =
            safe_artifact_preview(artifact.redaction_state, artifact.preview_text.as_deref())
        {
            out.push_str("<pre class=\"preview\">");
            push_escaped(out, &preview);
            out.push_str("</pre>");
        }
        out.push_str("</article>");
    }
    out.push_str("</section>");
}

#[allow(dead_code)]
fn render_privacy(out: &mut String, privacy: &PrivacySummary) {
    out.push_str("<section><h2>Redaction and Privacy</h2><div class=\"panel\">");
    out.push_str(
        "<div class=\"tag\"><span>Default output</span><strong>redacted/share-safe</strong></div>",
    );
    out.push_str("<div class=\"tag\"><span>Raw transcripts withheld</span><strong>");
    out.push_str(&privacy.raw_transcripts_withheld.to_string());
    out.push_str("</strong></div><div class=\"tag\"><span>Redacted previews</span><strong>");
    out.push_str(&privacy.redacted_previews.to_string());
    out.push_str("</strong></div><div class=\"tag\"><span>Withheld links</span><strong>");
    out.push_str(&privacy.withheld_links.to_string());
    out.push_str("</strong></div></div></section>");
}

#[allow(dead_code)]
fn render_publish_preview(
    out: &mut String,
    report: &DashboardReport<'_>,
    privacy: &PrivacySummary,
) {
    out.push_str("<section><h2>Share and Publish Preview</h2><div class=\"panel\">");
    out.push_str("This export is prepared for local review with redacted summaries, command previews, safe PR links, and raw transcript content withheld by default.");
    out.push_str("<div class=\"meta\"><span class=\"pill\">records ");
    out.push_str(&report.records.len().to_string());
    out.push_str("</span><span class=\"pill\">commands ");
    out.push_str(&report.evidence.len().to_string());
    out.push_str("</span><span class=\"pill\">withheld ");
    out.push_str(&privacy.raw_transcripts_withheld.to_string());
    out.push_str("</span></div></div></section>");
}

fn evidence_report(report: &DashboardReport<'_>) -> EvidenceReport {
    EvidenceReport {
        schema_version: 1,
        share_safe: true,
        summary: summarize(report.records, report.evidence),
        privacy: privacy_summary(report),
        records: report
            .records
            .iter()
            .map(|record| EvidenceRecordReport {
                id: record.id.to_string(),
                title: redact_share_safe_markers(&record.title),
                summary: redact_share_safe_markers(&record.body),
                tags: record
                    .tags
                    .iter()
                    .map(|tag| redact_share_safe_markers(tag))
                    .collect(),
                pr_url: record.pr_url.as_deref().and_then(safe_external_url),
            })
            .collect(),
        commands: report
            .evidence
            .iter()
            .map(|evidence| EvidenceCommandReport {
                id: evidence.id.to_string(),
                record_id: evidence.record_id.map(|id| id.to_string()),
                command: redact_share_safe_markers(&evidence.command),
                exit_code: evidence.exit_code,
                duration_ms: evidence.duration_ms,
                started_at: evidence.started_at.to_rfc3339(),
                output_preview: evidence_preview(evidence)
                    .map(|preview| redact_share_safe_markers(&truncate_chars(preview, 900))),
            })
            .collect(),
        pull_requests: report
            .pull_requests
            .iter()
            .filter_map(|pr| {
                Some(SafePullRequest {
                    url: safe_external_url(&pr.url)?,
                    title: pr.title.as_deref().map(redact_share_safe_markers),
                    state: pr.state.as_deref().map(redact_share_safe_markers),
                    head_ref: pr.head_ref.as_deref().map(redact_share_safe_markers),
                    base_ref: pr.base_ref.as_deref().map(redact_share_safe_markers),
                })
            })
            .collect(),
        evidence_metadata: evidence_metadata_values(report),
    }
}

fn evidence_metadata_values(report: &DashboardReport<'_>) -> Vec<Value> {
    report
        .evidence_metadata
        .iter()
        .map(|evidence| {
            json!({
                "id": evidence.id.to_string(),
                "work_record_id": evidence.work_record_id.to_string(),
                "kind": evidence.kind.as_str(),
                "status": evidence.status.as_str(),
                "freshness": evidence.freshness.as_str(),
                "stale_reason": evidence.stale_reason.as_deref().map(redact_share_safe_markers),
                "observed_tree_hash": evidence.observed_tree_hash.as_deref().map(redact_share_safe_markers),
                "observed_head_sha": evidence.observed_head_sha.as_deref().map(redact_share_safe_markers),
                "metadata": redact_metadata_value(&evidence.sync.metadata),
            })
        })
        .collect()
}

fn privacy_summary(report: &DashboardReport<'_>) -> PrivacySummary {
    let raw_transcripts_withheld = report
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind.as_str() == "transcript")
        .count()
        + report
            .archive_artifacts
            .iter()
            .filter(|artifact| artifact.kind.as_str() == "transcript")
            .count();
    let redacted_previews = report
        .events
        .iter()
        .filter(|event| event.redaction_state != RedactionState::Raw)
        .count()
        + report
            .artifacts
            .iter()
            .filter(|artifact| artifact.redaction_state != RedactionState::Raw)
            .count()
        + report
            .archive_artifacts
            .iter()
            .filter(|artifact| artifact.redaction_state != RedactionState::Raw)
            .count();
    let withheld_links = report
        .records
        .iter()
        .filter_map(|record| record.pr_url.as_deref())
        .chain(report.pull_requests.iter().map(|pr| pr.url.as_str()))
        .filter(|url| safe_external_url(url).is_none())
        .count();
    PrivacySummary {
        default_redacted: true,
        raw_transcripts_withheld,
        redacted_previews,
        withheld_links,
        local_paths_redacted: true,
    }
}

fn redact_metadata_value(value: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(redact_share_safe_markers(value)),
        Value::Array(values) => Value::Array(values.iter().map(redact_metadata_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redact_metadata_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn evidence_preview(evidence: &Evidence) -> Option<&str> {
    if !evidence.stdout.is_empty() {
        Some(&evidence.stdout)
    } else if !evidence.stderr.is_empty() {
        Some(&evidence.stderr)
    } else {
        None
    }
}

fn event_preview(event: &Event) -> Option<String> {
    if event.redaction_state == RedactionState::Raw {
        return Some("raw event payload withheld".to_owned());
    }
    for key in [
        "summary", "preview", "text", "message", "command", "output", "name",
    ] {
        if let Some(value) = event.payload.get(key).and_then(|value| value.as_str()) {
            return Some(redact_share_safe_markers(&truncate_chars(value, 900)));
        }
    }
    if event.payload.is_object() || event.payload.is_array() {
        return Some(redact_share_safe_markers(&truncate_chars(
            &event.payload.to_string(),
            900,
        )));
    }
    None
}

fn safe_artifact_preview(
    redaction_state: RedactionState,
    preview_text: Option<&str>,
) -> Option<String> {
    if redaction_state == RedactionState::Raw {
        return Some("raw artifact content withheld".to_owned());
    }
    preview_text.map(|preview| redact_share_safe_markers(&truncate_chars(preview, 900)))
}

fn share_safe_relative_path(value: &str) -> String {
    let redacted = redact_share_safe_markers(value);
    if redacted.contains("[REDACTED_PATH]") {
        value
            .rsplit(['/', '\\'])
            .next()
            .filter(|segment| !segment.is_empty())
            .map(|segment| format!("[REDACTED_PATH]/{segment}"))
            .unwrap_or_else(|| "[REDACTED_PATH]".to_owned())
    } else {
        redacted
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        out.push_str("\n[truncated]");
    }
    out
}

fn safe_external_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.starts_with("https://")
        && !trimmed.contains('@')
        && !trimmed.contains('?')
        && !trimmed.contains('#')
    {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn safe_workspace_label(value: &str) -> String {
    let trimmed = value.trim_end_matches('/');
    let name = trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or("local workspace");
    format!("workspace: {name}")
}

fn safe_repo_label(workspace: &VcsWorkspace) -> String {
    match (&workspace.owner, &workspace.name) {
        (Some(owner), Some(name)) => format!(
            "{}/{}",
            redact_share_safe_markers(owner),
            redact_share_safe_markers(name)
        ),
        (_, Some(name)) => redact_share_safe_markers(name),
        _ => safe_workspace_label(&workspace.root_path),
    }
}

#[allow(dead_code)]
fn push_escaped(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

#[allow(dead_code)]
fn push_attr_escaped(out: &mut String, value: &str) {
    push_escaped(out, value);
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;
    use work_record_core::{
        AgentType, ArtifactKind, CaptureProvider, Confidence, EntityTimestamps, EventRole,
        EventType, EvidenceFreshness, EvidenceKind, EvidenceStatus, FileChangeKind,
        PullRequestLinkSource, PullRequestProvider, RunStatus, RunType, SessionStatus, SummaryKind,
        SyncMetadata, VcsChangeKind, VcsHost, VcsKind,
    };

    use super::*;

    #[test]
    fn summarizes_records() {
        let mut record = WorkRecord::new("One", "Body", vec!["cli".into()], "task", None);
        record.pr_url = Some("https://github.com/ctxrs/ctx/pull/1".into());
        let evidence = Evidence::new(
            Some(Uuid::new_v4()),
            "cargo test",
            0,
            String::new(),
            String::new(),
            Utc::now(),
            1,
        );

        let summary = summarize(&[record], &[evidence]);
        assert_eq!(summary.record_count, 1);
        assert_eq!(summary.linked_pr_count, 1);
        assert_eq!(
            summary.tags[0],
            TagCount {
                tag: "cli".into(),
                count: 1
            }
        );
    }

    #[test]
    fn renders_dashboard_html_with_escaped_content() {
        let mut record = WorkRecord::new(
            "Ship <dashboard> token=ghp_1234567890abcdef",
            "body with <script>alert(1)</script> password=hunter2 cwd=/tmp/work",
            vec!["report".into(), "secret=shhh".into()],
            "task",
            Some("/tmp/work".into()),
        );
        record.pr_url = Some("https://token@example.test/ctx/pull/1".into());
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test <unsafe> token=secret",
            1,
            "stdout <ok> password=hunter2".into(),
            String::new(),
            Utc::now(),
            25,
        );

        let html = render_dashboard_html(&[record], &[evidence]);
        let data = dashboard_data_from_html(&html);
        let rendered = data.to_string();

        assert!(html.contains("ctx-dashboard-data"));
        assert_eq!(data["product"], "ctx Work Recorder");
        assert_eq!(data["status"]["javascript_app"], "React/Vite");
        assert!(rendered.contains("Ship <dashboard> token=[REDACTED_SECRET]"));
        assert!(rendered.contains("<script>alert(1)</script>"));
        assert!(rendered.contains("workspace: work"));
        assert!(!rendered.contains("/tmp/work"));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("ghp_123456"));
        assert!(!rendered.contains("secret=shhh"));
        assert!(rendered.contains("password=[REDACTED_SECRET]"));
        assert!(rendered.contains("[REDACTED_PATH]"));
        assert!(rendered.contains("cargo test <unsafe> token=[REDACTED_SECRET]"));
        assert!(!rendered.contains("token=secret"));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains("href=\"javascript:alert(1)\""));
        assert!(!rendered.contains("https://token@example.test"));
    }

    #[test]
    fn context_markdown_redacts_share_unsafe_fields() {
        let mut record = WorkRecord::new(
            "Deploy token=ghp_1234567890abcdef",
            "body password=hunter2 in /home/daddy/code/project",
            vec!["secret=shhh".into()],
            "task",
            None,
        );
        record.pr_url = Some("https://github.com/ctxrs/ctx/pull/1".into());
        let evidence = Evidence::new(
            Some(record.id),
            "gh token=secret",
            0,
            String::new(),
            String::new(),
            Utc::now(),
            1,
        );
        let context = WorkContext {
            query: Some("password=hunter2".into()),
            records: vec![record],
            evidence: vec![evidence],
        };

        let markdown = context_markdown(&context);

        assert!(markdown.contains("password=[REDACTED_SECRET]"));
        assert!(markdown.contains("token=[REDACTED_SECRET]"));
        assert!(markdown.contains("[REDACTED_PATH]"));
        assert!(!markdown.contains("hunter2"));
        assert!(!markdown.contains("ghp_123456"));
        assert!(!markdown.contains("/home/daddy/code/project"));
        assert!(!markdown.contains("secret=shhh"));
    }

    #[test]
    fn rich_dashboard_fixture_is_not_sparse_and_stays_share_safe() {
        let fixture = rich_fixture();
        let report = fixture.report();
        assert_rich_fixture_not_sparse(&report);

        let html = render_dashboard_html_report(&report);
        let data = dashboard_data_from_html(&html);
        let rendered = data.to_string();

        for section in [
            "Overview",
            "Workspace / Repo",
            "Session Detail",
            "PR / Evidence",
            "Search / Explore",
            "Settings / Status",
            "Transcript, Messages, and Tool Calls",
            "Artifacts",
        ] {
            assert!(rendered.contains(section), "missing section {section}");
        }
        assert!(rendered.contains("raw artifact content withheld"));
        assert!(rendered.contains("raw event payload withheld"));
        assert!(rendered.contains("cargo test -p work-record-report token=[REDACTED_SECRET]"));
        assert!(rendered.contains("password=[REDACTED_SECRET]"));
        assert!(rendered.contains("[REDACTED_PATH]/lib.rs"));
        assert!(!rendered.contains("ghp_123456"));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("/home/daddy/code/private"));
        assert!(!rendered.contains("raw transcript secret"));
    }

    fn dashboard_data_from_html(html: &str) -> serde_json::Value {
        let marker = "<script id=\"ctx-dashboard-data\" type=\"application/json\">";
        let start = html.find(marker).expect("dashboard data script") + marker.len();
        let tail = &html[start..];
        let end = tail.find("</script>").expect("dashboard data script end");
        serde_json::from_str(&tail[..end]).expect("dashboard data json")
    }

    #[test]
    fn evidence_reports_are_deterministic_redacted_review_primitives() {
        let fixture = rich_fixture();
        let report = fixture.report();

        let markdown = render_evidence_report_markdown(&report);
        let json = render_evidence_report_json(&report).unwrap();

        assert!(markdown.contains("# Work Recorder Evidence Report"));
        assert!(markdown.contains("Share-safe: yes"));
        assert!(markdown.contains("cargo test -p work-record-report token=[REDACTED_SECRET]"));
        assert!(json.contains("\"share_safe\": true"));
        assert!(json.contains("\"raw_transcripts_withheld\": 1"));
        assert!(!markdown.contains("ghp_123456"));
        assert!(!json.contains("hunter2"));
        assert!(!json.contains("/home/daddy/code/private"));
    }

    struct RichFixture {
        records: Vec<WorkRecord>,
        evidence: Vec<Evidence>,
        archive_artifacts: Vec<WorkRecordArchiveArtifact>,
        sessions: Vec<Session>,
        runs: Vec<Run>,
        events: Vec<Event>,
        vcs_workspaces: Vec<VcsWorkspace>,
        vcs_changes: Vec<VcsChange>,
        pull_requests: Vec<PullRequest>,
        artifacts: Vec<Artifact>,
        evidence_metadata: Vec<EvidenceMetadata>,
        files_touched: Vec<FileTouched>,
        summaries: Vec<Summary>,
    }

    impl RichFixture {
        fn report(&self) -> DashboardReport<'_> {
            DashboardReport {
                records: &self.records,
                evidence: &self.evidence,
                archive_artifacts: &self.archive_artifacts,
                sessions: &self.sessions,
                runs: &self.runs,
                events: &self.events,
                vcs_workspaces: &self.vcs_workspaces,
                vcs_changes: &self.vcs_changes,
                pull_requests: &self.pull_requests,
                artifacts: &self.artifacts,
                evidence_metadata: &self.evidence_metadata,
                files_touched: &self.files_touched,
                summaries: &self.summaries,
            }
        }
    }

    fn assert_rich_fixture_not_sparse(report: &DashboardReport<'_>) {
        assert!(report.records.len() >= 2);
        assert!(report.evidence.len() >= 2);
        assert!(!report.sessions.is_empty());
        assert!(!report.runs.is_empty());
        assert!(report.events.len() >= 3);
        assert!(!report.vcs_workspaces.is_empty());
        assert!(!report.vcs_changes.is_empty());
        assert!(!report.pull_requests.is_empty());
        assert!(!report.artifacts.is_empty());
        assert!(!report.files_touched.is_empty());
    }

    fn rich_fixture() -> RichFixture {
        let t0 = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 6, 23, 12, 5, 0).unwrap();
        let timestamps = EntityTimestamps {
            created_at: t0,
            updated_at: t1,
        };
        let sync = SyncMetadata::default();
        let record_id = id("018f45d0-0000-7000-8000-000000000001");
        let second_record_id = id("018f45d0-0000-7000-8000-000000000002");
        let session_id = id("018f45d0-0000-7000-8000-000000000010");
        let run_id = id("018f45d0-0000-7000-8000-000000000020");
        let event_id = id("018f45d0-0000-7000-8000-000000000030");
        let workspace_id = id("018f45d0-0000-7000-8000-000000000040");
        let change_id = id("018f45d0-0000-7000-8000-000000000050");
        let pr_id = id("018f45d0-0000-7000-8000-000000000060");
        let artifact_id = id("018f45d0-0000-7000-8000-000000000070");
        let evidence_id = id("018f45d0-0000-7000-8000-000000000080");

        let mut record = WorkRecord::new(
            "Finish dashboard token=ghp_1234567890abcdef",
            "Built report v2 from /home/daddy/code/private with password=hunter2",
            vec!["dashboard".into(), "review".into()],
            "task",
            Some("/home/daddy/code/private".into()),
        );
        record.id = record_id;
        record.created_at = t0;
        record.updated_at = t1;
        record.pr_url = Some("https://github.com/ctxrs/ctx/pull/42".into());

        let mut second = WorkRecord::new(
            "Add evidence report",
            "Markdown and JSON review primitives",
            vec!["evidence".into()],
            "task",
            None,
        );
        second.id = second_record_id;
        second.created_at = t0;
        second.updated_at = t1;

        let evidence = vec![
            Evidence {
                id: evidence_id,
                record_id: Some(record_id),
                command: "cargo test -p work-record-report token=ghp_1234567890abcdef".into(),
                exit_code: 0,
                stdout: "ok password=hunter2".into(),
                stderr: String::new(),
                started_at: t1,
                duration_ms: 321,
            },
            Evidence {
                id: id("018f45d0-0000-7000-8000-000000000081"),
                record_id: Some(second_record_id),
                command: "cargo fmt -p work-record-report".into(),
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                started_at: t1,
                duration_ms: 12,
            },
        ];

        RichFixture {
            records: vec![record, second],
            evidence,
            archive_artifacts: vec![],
            sessions: vec![Session {
                id: session_id,
                work_record_id: Some(record_id),
                parent_session_id: None,
                root_session_id: Some(session_id),
                capture_source_id: None,
                provider: CaptureProvider::Codex,
                external_session_id: Some("codex-session".into()),
                external_agent_id: Some("agent-1".into()),
                agent_type: AgentType::Implementer,
                role_hint: Some("implementation worker".into()),
                is_primary: false,
                status: SessionStatus::Completed,
                transcript_blob_id: Some(artifact_id),
                started_at: t0,
                ended_at: Some(t1),
                timestamps: timestamps.clone(),
                sync: sync.clone(),
            }],
            runs: vec![Run {
                id: run_id,
                work_record_id: Some(record_id),
                session_id: Some(session_id),
                run_type: RunType::Command,
                status: RunStatus::Succeeded,
                started_at: t0,
                ended_at: Some(t1),
                exit_code: Some(0),
                cwd: Some("/home/daddy/code/private".into()),
                command_preview: Some(
                    "cargo test -p work-record-report token=ghp_1234567890abcdef".into(),
                ),
                input_blob_id: None,
                output_blob_id: Some(artifact_id),
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            events: vec![
                Event {
                    id: event_id,
                    seq: 1,
                    work_record_id: Some(record_id),
                    session_id: Some(session_id),
                    run_id: None,
                    event_type: EventType::Message,
                    role: Some(EventRole::Assistant),
                    occurred_at: t0,
                    capture_source_id: None,
                    payload: json!({"text": "Implemented dashboard password=hunter2"}),
                    payload_blob_id: None,
                    dedupe_key: Some("message-1".into()),
                    redaction_state: RedactionState::Redacted,
                    sync: sync.clone(),
                },
                Event {
                    id: id("018f45d0-0000-7000-8000-000000000031"),
                    seq: 2,
                    work_record_id: Some(record_id),
                    session_id: Some(session_id),
                    run_id: Some(run_id),
                    event_type: EventType::ToolCall,
                    role: Some(EventRole::Assistant),
                    occurred_at: t0,
                    capture_source_id: None,
                    payload: json!({"name": "exec_command", "command": "cargo test"}),
                    payload_blob_id: None,
                    dedupe_key: Some("tool-1".into()),
                    redaction_state: RedactionState::SafePreview,
                    sync: sync.clone(),
                },
                Event {
                    id: id("018f45d0-0000-7000-8000-000000000032"),
                    seq: 3,
                    work_record_id: Some(record_id),
                    session_id: Some(session_id),
                    run_id: Some(run_id),
                    event_type: EventType::ToolOutput,
                    role: Some(EventRole::Tool),
                    occurred_at: t1,
                    capture_source_id: None,
                    payload: json!({"text": "raw transcript secret"}),
                    payload_blob_id: Some(artifact_id),
                    dedupe_key: Some("tool-2".into()),
                    redaction_state: RedactionState::Raw,
                    sync: sync.clone(),
                },
            ],
            vcs_workspaces: vec![VcsWorkspace {
                id: workspace_id,
                kind: VcsKind::Git,
                root_path: "/home/daddy/code/private".into(),
                repo_fingerprint: "ctxrs/ctx".into(),
                primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
                host: VcsHost::Github,
                owner: Some("ctxrs".into()),
                name: Some("ctx".into()),
                monorepo_subpath: Some("crates/work-record-report".into()),
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            vcs_changes: vec![VcsChange {
                id: change_id,
                vcs_workspace_id: workspace_id,
                kind: VcsChangeKind::GitBranch,
                change_id: "abc123".into(),
                parent_change_ids: vec!["def456".into()],
                branch_or_bookmark: Some("ctx/wr-finished-dashboard-v2".into()),
                tree_hash: Some("tree123".into()),
                author_time: Some(t0),
                confidence: Confidence::Explicit,
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            pull_requests: vec![PullRequest {
                id: pr_id,
                vcs_workspace_id: Some(workspace_id),
                provider: PullRequestProvider::Github,
                url: "https://github.com/ctxrs/ctx/pull/42".into(),
                number: Some(42),
                owner: Some("ctxrs".into()),
                repo: Some("ctx".into()),
                title: Some("Dashboard v2".into()),
                state: Some("open".into()),
                head_ref: Some("ctx/wr-finished-dashboard-v2".into()),
                base_ref: Some("main".into()),
                head_sha: Some("abc123".into()),
                confidence: Confidence::High,
                link_source: PullRequestLinkSource::Explicit,
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            artifacts: vec![Artifact {
                id: artifact_id,
                kind: ArtifactKind::Transcript,
                blob_hash: "sha256:abc".into(),
                blob_path: "/home/daddy/code/private/transcript.jsonl".into(),
                byte_size: 2048,
                media_type: Some("application/jsonl".into()),
                preview_text: Some("raw transcript secret".into()),
                redaction_state: RedactionState::Raw,
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            evidence_metadata: vec![EvidenceMetadata {
                id: id("018f45d0-0000-7000-8000-000000000090"),
                work_record_id: record_id,
                vcs_change_id: Some(change_id),
                kind: EvidenceKind::Test,
                status: EvidenceStatus::Passed,
                freshness: EvidenceFreshness::Fresh,
                command_run_id: Some(run_id),
                artifact_id: Some(artifact_id),
                observed_tree_hash: Some("tree123".into()),
                observed_head_sha: Some("abc123".into()),
                started_at: Some(t0),
                ended_at: Some(t1),
                stale_reason: None,
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            files_touched: vec![FileTouched {
                id: id("018f45d0-0000-7000-8000-0000000000a0"),
                work_record_id: Some(record_id),
                run_id: Some(run_id),
                event_id: Some(event_id),
                vcs_workspace_id: Some(workspace_id),
                path: "/home/daddy/code/private/crates/work-record-report/src/lib.rs".into(),
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: Some(420),
                confidence: Confidence::High,
                timestamps: timestamps.clone(),
                source_id: None,
                sync: sync.clone(),
            }],
            summaries: vec![Summary {
                id: id("018f45d0-0000-7000-8000-0000000000b0"),
                work_record_id: Some(record_id),
                session_id: Some(session_id),
                kind: SummaryKind::CtxGenerated,
                model_or_source: Some("test-fixture".into()),
                text: "Dashboard v2 summary".into(),
                citations: vec![],
                timestamps,
                source_id: None,
                sync,
            }],
        }
    }

    fn id(value: &str) -> Uuid {
        Uuid::parse_str(value).unwrap()
    }
}
