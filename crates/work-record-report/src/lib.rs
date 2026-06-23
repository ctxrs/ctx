use std::collections::BTreeSet;

use serde::Serialize;
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

pub fn summarize(records: &[WorkRecord], evidence: &[Evidence]) -> ReportSummary {
    let mut tag_counts = std::collections::BTreeMap::<String, usize>::new();
    for record in records {
        for tag in &record.tags {
            *tag_counts.entry(tag.clone()).or_default() += 1;
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
    let summary = summarize(report.records, report.evidence);
    let failing_evidence_count = report
        .evidence
        .iter()
        .filter(|item| item.exit_code != 0)
        .count();
    let recent_records = report.records.iter().take(25).collect::<Vec<_>>();
    let recent_evidence = report.evidence.iter().take(25).collect::<Vec<_>>();
    let privacy = privacy_summary(report);

    let mut out = String::new();
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>ctx Work Records</title>\n");
    out.push_str("<style>\n");
    out.push_str(
        r#":root{color-scheme:light;--bg:#f7f8fa;--ink:#18202b;--muted:#647084;--line:#d9dee7;--panel:#ffffff;--accent:#1f6feb;--ok:#0f7b45;--warn:#b42318;--note:#72560a}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.5 system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}main{max-width:1180px;margin:0 auto;padding:32px 20px 48px}.top{display:flex;justify-content:space-between;gap:24px;align-items:flex-start;border-bottom:1px solid var(--line);padding-bottom:20px}.eyebrow{margin:0 0 8px;color:var(--muted);font-size:12px;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:0;font-size:34px;line-height:1.1;letter-spacing:0}h2{margin:0 0 14px;font-size:18px;letter-spacing:0}h3{margin:0 0 6px;font-size:16px}.privacy{max-width:440px;background:#eef6ff;border:1px solid #c8dcf8;border-radius:8px;padding:12px 14px;color:#234466}.grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin:24px 0}.metric{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:14px}.metric strong{display:block;font-size:28px;line-height:1}.metric span{display:block;margin-top:6px;color:var(--muted)}section{margin-top:28px}.layout{display:grid;grid-template-columns:minmax(0,2fr) minmax(300px,1fr);gap:18px}.record,.evidence,.cue,.panel{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:14px;margin-bottom:12px}.meta{display:flex;flex-wrap:wrap;gap:8px;margin:8px 0;color:var(--muted);font-size:12px}.pill{display:inline-flex;border:1px solid var(--line);border-radius:999px;padding:2px 8px;background:#fbfcfe;color:#354052}.body{white-space:pre-wrap;overflow-wrap:anywhere;color:#2f3a4a}.pr{color:var(--accent);overflow-wrap:anywhere}.empty{color:var(--muted);border:1px dashed var(--line);border-radius:8px;padding:16px;background:#fff}.evidence code,.cue code,.panel code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px}.status-ok{color:var(--ok);font-weight:700}.status-fail{color:var(--warn);font-weight:700}.status-note{color:var(--note);font-weight:700}.preview{margin-top:8px;background:#111827;color:#f9fafb;border-radius:6px;padding:10px;max-height:180px;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere}.timeline{border-left:2px solid var(--line);padding-left:14px}.timeline-item{position:relative;margin:0 0 12px}.timeline-item:before{content:"";position:absolute;left:-20px;top:5px;width:10px;height:10px;border-radius:50%;background:var(--accent)}.table{width:100%;border-collapse:collapse}.table th,.table td{text-align:left;border-bottom:1px solid var(--line);padding:7px 6px;vertical-align:top}.table th{color:var(--muted);font-size:12px}.tag{display:flex;justify-content:space-between;gap:16px;border-bottom:1px solid var(--line);padding:7px 0}.footer{margin-top:32px;color:var(--muted);font-size:12px}@media (max-width:760px){main{padding:22px 14px 36px}.top,.layout{display:block}.privacy{margin-top:16px}.grid{grid-template-columns:repeat(2,minmax(0,1fr))}h1{font-size:28px}.table{display:block;overflow:auto}}"#,
    );
    out.push_str("\n</style>\n</head>\n<body>\n<main>\n");

    out.push_str("<div class=\"top\"><div><p class=\"eyebrow\">Local Work Recorder</p><h1>Work Records</h1></div>");
    out.push_str("<div class=\"privacy\">Static local export. No hosted sync, tracking, JavaScript, or remote assets are included. Review this file before sharing because records and evidence may contain private code, paths, command output, or PR links.</div></div>\n");

    out.push_str("<div class=\"grid\">");
    metric(&mut out, summary.record_count, "records");
    metric(&mut out, summary.evidence_count, "evidence items");
    metric(
        &mut out,
        summary.linked_pr_count + report.pull_requests.len(),
        "PR links",
    );
    metric(&mut out, failing_evidence_count, "failed evidence");
    out.push_str("</div>\n");

    render_publish_preview(&mut out, report, &privacy);

    out.push_str("<div class=\"layout\"><div>");
    render_summaries(&mut out, report);
    out.push_str("<section><h2>Recent Records</h2>\n");
    if recent_records.is_empty() {
        out.push_str("<div class=\"empty\">No Work Records found in the local store.</div>\n");
    } else {
        for record in recent_records {
            render_record(&mut out, record);
        }
    }
    out.push_str("</section>\n</div><aside>");

    render_sessions_runs(&mut out, report);
    render_timeline(&mut out, report);
    render_transcript_views(&mut out, report);
    out.push_str("<section><h2>Evidence Previews</h2>\n");
    if recent_evidence.is_empty() {
        out.push_str("<div class=\"empty\">No evidence has been captured yet.</div>\n");
    } else {
        for item in recent_evidence {
            render_evidence(&mut out, item);
        }
    }
    out.push_str("</section>\n");
    render_evidence_metadata(&mut out, report);
    render_files_touched(&mut out, report);
    render_vcs(&mut out, report);
    render_pr_links(&mut out, report);
    render_artifacts(&mut out, report);
    render_privacy(&mut out, &privacy);

    out.push_str("<section><h2>Capture and Search Cues</h2><div class=\"cue\">");
    out.push_str("Use <code>ctx search &lt;query&gt; --json</code> for exact matches, ");
    out.push_str("<code>ctx context &lt;query&gt;</code> for handoff context, and ");
    out.push_str(
        "<code>ctx evidence run --record &lt;id&gt; ...</code> to attach fresh local evidence.",
    );
    out.push_str("</div></section>\n");

    if !summary.tags.is_empty() {
        out.push_str("<section><h2>Tags</h2><div class=\"record\">");
        for tag in summary.tags {
            out.push_str("<div class=\"tag\"><span>");
            push_escaped(&mut out, &redact_share_safe_markers(&tag.tag));
            out.push_str("</span><strong>");
            out.push_str(&tag.count.to_string());
            out.push_str("</strong></div>");
        }
        out.push_str("</div></section>\n");
    }

    out.push_str("</aside></div>");
    out.push_str("<div class=\"footer\">Generated by <code>ctx dashboard export</code> from local Work Recorder data.</div>");
    out.push_str("\n</main>\n</body>\n</html>\n");
    out
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
        "- Pull requests: {}\n",
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

fn metric(out: &mut String, value: usize, label: &str) {
    out.push_str("<div class=\"metric\"><strong>");
    out.push_str(&value.to_string());
    out.push_str("</strong><span>");
    push_escaped(out, label);
    out.push_str("</span></div>");
}

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
    }
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
    if redacted.contains("[local-path]") {
        value
            .rsplit(['/', '\\'])
            .next()
            .filter(|segment| !segment.is_empty())
            .map(|segment| format!("[local-path]/{segment}"))
            .unwrap_or_else(|| "[local-path]".to_owned())
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

        assert!(html.contains("Local Work Recorder"));
        assert!(html.contains("ctx dashboard export"));
        assert!(html.contains("Ship &lt;dashboard&gt; token=[redacted]"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("workspace: work"));
        assert!(!html.contains("/tmp/work"));
        assert!(!html.contains("hunter2"));
        assert!(!html.contains("ghp_123456"));
        assert!(!html.contains("secret=shhh"));
        assert!(html.contains("password=[redacted]"));
        assert!(html.contains("[local-path]"));
        assert!(html.contains("cargo test &lt;unsafe&gt; token=[redacted]"));
        assert!(!html.contains("token=secret"));
        assert!(html.contains("password=[redacted]"));
        assert!(!html.contains("hunter2"));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains("href=\"javascript:alert(1)\""));
        assert!(!html.contains("https://token@example.test"));
        assert!(html.contains("link withheld"));
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

        assert!(markdown.contains("password=[redacted]"));
        assert!(markdown.contains("token=[redacted]"));
        assert!(markdown.contains("[local-path]"));
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

        for section in [
            "Share and Publish Preview",
            "Summaries",
            "Sessions and Runs",
            "Timeline",
            "Transcript, Messages, and Tool Calls",
            "Evidence Previews",
            "Evidence Status",
            "Files Touched",
            "Git and jj State",
            "PR Links",
            "Artifacts",
            "Redaction and Privacy",
        ] {
            assert!(html.contains(section), "missing section {section}");
        }
        assert!(!html.contains("No session or run metadata"));
        assert!(!html.contains("No timeline events"));
        assert!(!html.contains("No file touch metadata"));
        assert!(!html.contains("No Git or jj state"));
        assert!(!html.contains("No artifacts"));
        assert!(html.contains("raw artifact content withheld"));
        assert!(html.contains("raw event payload withheld"));
        assert!(html.contains("cargo test -p work-record-report token=[redacted]"));
        assert!(html.contains("password=[redacted]"));
        assert!(html.contains("[local-path]/lib.rs"));
        assert!(!html.contains("ghp_123456"));
        assert!(!html.contains("hunter2"));
        assert!(!html.contains("/home/daddy/code/private"));
        assert!(!html.contains("raw transcript secret"));
    }

    #[test]
    fn evidence_reports_are_deterministic_redacted_review_primitives() {
        let fixture = rich_fixture();
        let report = fixture.report();

        let markdown = render_evidence_report_markdown(&report);
        let json = render_evidence_report_json(&report).unwrap();

        assert!(markdown.contains("# Work Recorder Evidence Report"));
        assert!(markdown.contains("Share-safe: yes"));
        assert!(markdown.contains("cargo test -p work-record-report token=[redacted]"));
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
