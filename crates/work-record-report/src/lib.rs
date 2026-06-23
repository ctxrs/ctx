use serde::Serialize;
use work_record_core::{
    redact_share_safe_markers, Evidence, WorkContext, WorkRecord, WorkRecordArchive,
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
    let summary = summarize(records, evidence);
    let failing_evidence_count = evidence.iter().filter(|item| item.exit_code != 0).count();
    let recent_records = records.iter().take(25).collect::<Vec<_>>();
    let recent_evidence = evidence.iter().take(25).collect::<Vec<_>>();

    let mut out = String::new();
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>ctx Work Records</title>\n");
    out.push_str("<style>\n");
    out.push_str(
        r#":root{color-scheme:light;--bg:#f7f8fa;--ink:#18202b;--muted:#647084;--line:#d9dee7;--panel:#ffffff;--accent:#1f6feb;--ok:#0f7b45;--warn:#b42318}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.5 system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}main{max-width:1120px;margin:0 auto;padding:32px 20px 48px}.top{display:flex;justify-content:space-between;gap:24px;align-items:flex-start;border-bottom:1px solid var(--line);padding-bottom:20px}.eyebrow{margin:0 0 8px;color:var(--muted);font-size:12px;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:0;font-size:34px;line-height:1.1;letter-spacing:0}h2{margin:0 0 14px;font-size:18px;letter-spacing:0}.privacy{max-width:410px;background:#eef6ff;border:1px solid #c8dcf8;border-radius:8px;padding:12px 14px;color:#234466}.grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin:24px 0}.metric{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:14px}.metric strong{display:block;font-size:28px;line-height:1}.metric span{display:block;margin-top:6px;color:var(--muted)}section{margin-top:28px}.layout{display:grid;grid-template-columns:minmax(0,2fr) minmax(280px,1fr);gap:18px}.record,.evidence,.cue{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:14px;margin-bottom:12px}.record h3{margin:0 0 6px;font-size:16px}.meta{display:flex;flex-wrap:wrap;gap:8px;margin:8px 0;color:var(--muted);font-size:12px}.pill{display:inline-flex;border:1px solid var(--line);border-radius:999px;padding:2px 8px;background:#fbfcfe;color:#354052}.body{white-space:pre-wrap;overflow-wrap:anywhere;color:#2f3a4a}.pr{color:var(--accent);overflow-wrap:anywhere}.empty{color:var(--muted);border:1px dashed var(--line);border-radius:8px;padding:16px;background:#fff}.evidence code,.cue code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px}.status-ok{color:var(--ok);font-weight:700}.status-fail{color:var(--warn);font-weight:700}.preview{margin-top:8px;background:#111827;color:#f9fafb;border-radius:6px;padding:10px;max-height:180px;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere}.tags{display:flex;flex-wrap:wrap;gap:8px}.tag{display:flex;justify-content:space-between;gap:16px;border-bottom:1px solid var(--line);padding:7px 0}.footer{margin-top:32px;color:var(--muted);font-size:12px}@media (max-width:760px){main{padding:22px 14px 36px}.top,.layout{display:block}.privacy{margin-top:16px}.grid{grid-template-columns:repeat(2,minmax(0,1fr))}h1{font-size:28px}}"#,
    );
    out.push_str("\n</style>\n</head>\n<body>\n<main>\n");

    out.push_str("<div class=\"top\"><div><p class=\"eyebrow\">Local Work Recorder</p><h1>Work Records</h1></div>");
    out.push_str("<div class=\"privacy\">Static local export. No hosted sync, tracking, JavaScript, or remote assets are included. Review this file before sharing because records and evidence may contain private code, paths, command output, or PR links.</div></div>\n");

    out.push_str("<div class=\"grid\">");
    metric(&mut out, summary.record_count, "records");
    metric(&mut out, summary.evidence_count, "evidence items");
    metric(&mut out, summary.linked_pr_count, "PR links");
    metric(&mut out, failing_evidence_count, "failed evidence");
    out.push_str("</div>\n");

    out.push_str("<div class=\"layout\"><div>");
    out.push_str("<section><h2>Recent Records</h2>\n");
    if recent_records.is_empty() {
        out.push_str("<div class=\"empty\">No Work Records found in the local store.</div>\n");
    } else {
        for record in recent_records {
            render_record(&mut out, record);
        }
    }
    out.push_str("</section>\n</div><aside>");

    out.push_str("<section><h2>Evidence Previews</h2>\n");
    if recent_evidence.is_empty() {
        out.push_str("<div class=\"empty\">No evidence has been captured yet.</div>\n");
    } else {
        for item in recent_evidence {
            render_evidence(&mut out, item);
        }
    }
    out.push_str("</section>\n");

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

fn evidence_preview(evidence: &Evidence) -> Option<&str> {
    if !evidence.stdout.is_empty() {
        Some(&evidence.stdout)
    } else if !evidence.stderr.is_empty() {
        Some(&evidence.stderr)
    } else {
        None
    }
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
    use chrono::Utc;
    use uuid::Uuid;

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
}
