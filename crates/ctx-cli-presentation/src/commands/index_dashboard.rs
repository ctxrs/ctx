use serde_json::Value;

use crate::progress::{format_bytes, format_count, presentation_snapshot};
use crate::ui::{
    fields, outcome, progress, refresh_progress, section, Document, Field, Line, Outcome,
    OutcomeState, Progress, RenderContext, Span, Token,
};
use ctx_history_refresh::RefreshStatus;

#[derive(Debug, Default)]
pub(super) struct IndexDashboard;

impl IndexDashboard {
    pub(super) fn render(&mut self, readiness: &Value, context: &RenderContext) -> Document {
        render_dashboard(readiness, context, true)
    }

    pub(super) fn render_wait(
        &mut self,
        readiness: &Value,
        context: &RenderContext,
        refresh_convergence_selected: bool,
    ) -> Document {
        render_dashboard(readiness, context, refresh_convergence_selected)
    }
}

fn render_dashboard(
    readiness: &Value,
    context: &RenderContext,
    refresh_convergence_selected: bool,
) -> Document {
    let lexical_status = string_at(readiness, &["lexical", "status"], "unknown");
    let lexical_reason = string_at(readiness, &["lexical", "reason"], "");
    let refresh_status = string_at(readiness, &["refresh", "status"], "unknown");
    if lexical_reason == "generation_not_published" && refresh_status != "pending" {
        return render_missing(context);
    }
    if lexical_status == "unavailable" && refresh_status != "pending" {
        return render_lexical_failure(readiness, context);
    }

    let mut document = if lexical_status == "ready" {
        render_ready(readiness, context)
    } else {
        render_active(readiness, context)
    };
    let refresh_reason = string_at(readiness, &["refresh", "reason"], "");
    let refresh_is_idle = refresh_status == "unavailable"
        && matches!(
            refresh_reason.as_str(),
            "daemon_unavailable" | "refresh_not_observed"
        );
    if lexical_status == "ready"
        && refresh_convergence_selected
        && refresh_status != "ready"
        && !refresh_is_idle
        && bool_at(readiness, &["daemon", "running"])
    {
        append_separated(&mut document, render_refresh(readiness, context));
    }
    append_separated(&mut document, render_semantic(readiness, context));
    append_separated(&mut document, render_health(readiness, context));
    document
}

pub(super) fn render_semantic_disabled_wait(
    readiness: &Value,
    context: &RenderContext,
) -> Document {
    let lexical_searchable = string_at(readiness, &["lexical", "status"], "unknown") == "ready";
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: "Semantic indexing is blocked",
            detail: lexical_searchable.then_some("Keyword search remains available."),
        },
    );
    if lexical_searchable {
        append_separated(
            &mut document,
            section(
                "Keyword search",
                render_searchable_fields(readiness, context),
            ),
        );
    }
    append_separated(
        &mut document,
        section(
            "Semantic search",
            fields(context, &[Field::new("Status", "Off")]),
        ),
    );
    append_action(&mut document, "ctx semantic enable");
    document
}

fn render_missing(context: &RenderContext) -> Document {
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: "Search index is not set up",
            detail: None,
        },
    );
    append_action(&mut document, "ctx setup");
    document
}

fn render_lexical_failure(readiness: &Value, context: &RenderContext) -> Document {
    let reason = humanize(&string_at(
        readiness,
        &["refresh", "reason"],
        &string_at(readiness, &["lexical", "reason"], "unavailable"),
    ));
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: "History refresh is unavailable",
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(context, &[Field::new("Reason", &reason)]));
    if bool_at(readiness, &["initialized"]) {
        append_separated(
            &mut document,
            section(
                "Still searchable",
                render_searchable_fields(readiness, context),
            ),
        );
    }
    append_action(&mut document, "ctx doctor");
    document
}

fn render_ready(readiness: &Value, context: &RenderContext) -> Document {
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: "Your history is searchable",
            detail: None,
        },
    );
    document.push_blank();
    document.append(render_searchable_fields(readiness, context));
    document
}

fn render_searchable_fields(readiness: &Value, context: &RenderContext) -> Document {
    let mut values = Vec::new();
    if let Some(bytes) = u64_at(readiness, &["lexical", "certified_source_bytes"]) {
        values.push(("Processed", format_bytes(bytes)));
    }
    if let Some(sources) = u64_at(readiness, &["lexical", "indexed_sources"]) {
        values.push(("Sources", format_count(sources)));
    }
    if let Some(sessions) = u64_at(readiness, &["lexical", "indexed_sessions"]) {
        values.push(("Sessions", format_count(sessions)));
    }
    if let Some(records) = u64_at(readiness, &["lexical", "indexed_items"]) {
        values.push(("Records", format!("{} searchable", format_count(records))));
    }
    let field_values = values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    fields(context, &field_values)
}

fn render_active(readiness: &Value, context: &RenderContext) -> Document {
    let mut document = render_refresh_progress(readiness, context);
    let current = if bool_at(readiness, &["initialized"]) {
        u64_at(readiness, &["lexical", "indexed_items"])
            .map(|count| format!("{} searchable records", format_count(count)))
            .unwrap_or_else(|| "searchable generation published".to_owned())
    } else {
        "not published".to_owned()
    };
    document.push_blank();
    document.append(fields(context, &[Field::new("Current index", &current)]));
    document
}

fn render_refresh(readiness: &Value, context: &RenderContext) -> Document {
    section("Refresh", render_refresh_progress(readiness, context))
}

fn render_refresh_progress(readiness: &Value, context: &RenderContext) -> Document {
    RefreshStatus::parse_schema_v1(readiness["refresh"].clone())
        .and_then(|status| presentation_snapshot(&status))
        .map(|snapshot| refresh_progress(context, &snapshot))
        .unwrap_or_else(|_| fields(context, &[Field::new("Refresh", "status unavailable")]))
}

fn render_semantic(readiness: &Value, context: &RenderContext) -> Document {
    if !bool_at(readiness, &["semantic", "enabled"]) {
        return fields(context, &[Field::new("Semantic search", "Off")]);
    }

    let semantic_status = string_at(readiness, &["semantic", "status"], "unknown");
    let embedded = u64_at(readiness, &["semantic", "coverage", "embedded_items"]).unwrap_or(0);
    let candidates = u64_at(readiness, &["semantic", "coverage", "candidate_items"]);
    let searchable = u64_at(readiness, &["semantic", "coverage", "searchable_items"]);
    if semantic_status == "ready" {
        return fields(context, &[Field::new("Semantic search", "On")]);
    }

    if let Some(reason) = semantic_failure_reason(readiness) {
        let reason = humanize(&reason);
        let mut document = outcome(
            context,
            Outcome {
                state: OutcomeState::Error,
                title: "Semantic search needs attention",
                detail: None,
            },
        );
        document.push_blank();
        document.append(fields(context, &[Field::new("Reason", &reason)]));
        append_action(&mut document, "ctx doctor");
        return document;
    }

    let total = candidates
        .or(searchable)
        .filter(|total| *total > 0)
        .map(|total| total.max(embedded));
    let mut document = progress(
        context,
        Progress {
            label: "Semantic search",
            current: embedded,
            total,
            detail: None,
        },
    );
    let embedded_value = total
        .map(|total| {
            format!(
                "{} / {} records",
                format_count(embedded),
                format_count(total)
            )
        })
        .unwrap_or_else(|| format!("{} records", format_count(embedded)));
    document.push_blank();
    document.append(fields(context, &[Field::new("Embedded", &embedded_value)]));
    document
}

fn semantic_failure_reason(readiness: &Value) -> Option<String> {
    let semantic_status = string_at(readiness, &["semantic", "status"], "unknown");
    if matches!(
        semantic_status.as_str(),
        "failed" | "stale_lock" | "unavailable" | "blocked"
    ) {
        return Some(string_at(
            readiness,
            &["semantic", "reason"],
            &semantic_status,
        ));
    }

    let job_status = string_at(
        readiness,
        &["daemon", "jobs", "semantic_index", "status"],
        "unknown",
    );
    let reason = string_at(
        readiness,
        &["daemon", "jobs", "semantic_index", "reason"],
        &job_status,
    );
    if matches!(
        job_status.as_str(),
        "failed" | "stale_lock" | "unavailable" | "blocked"
    ) || (job_status == "skipped" && reason == "model_cache_missing")
    {
        Some(reason)
    } else {
        None
    }
}

fn render_health(readiness: &Value, context: &RenderContext) -> Document {
    if semantic_failure_reason(readiness).is_some() {
        return Document::new();
    }
    let lexical_ready = string_at(readiness, &["lexical", "status"], "unknown") == "ready"
        && string_at(readiness, &["refresh", "status"], "unknown") == "ready";
    let semantic_status = string_at(readiness, &["semantic", "status"], "unknown");
    let semantic_ready =
        !bool_at(readiness, &["semantic", "enabled"]) || semantic_status == "ready";
    if (lexical_ready && semantic_ready) || bool_at(readiness, &["daemon", "running"]) {
        return Document::new();
    }

    outcome(
        context,
        Outcome {
            state: OutcomeState::Warning,
            title: "Background indexing stopped",
            detail: Some("Run `ctx doctor` for details."),
        },
    )
}

fn append_action(document: &mut Document, command: &str) {
    let action = Document::from_line(
        Line::new()
            .with(Span::text("  "))
            .with(Span::new(command, Token::Command)),
    );
    document.push_blank();
    document.append(section("Next", action));
}

fn append_separated(document: &mut Document, other: Document) {
    if other.is_empty() {
        return;
    }
    if !document.is_empty() {
        document.push_blank();
    }
    document.append(other);
}

fn humanize(value: &str) -> String {
    value.replace('_', " ")
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn string_at(value: &Value, path: &[&str], default: &str) -> String {
    value_at(value, path)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    value_at(value, path)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn u64_at(value: &Value, path: &[&str]) -> Option<u64> {
    value_at(value, path).and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn readiness() -> Value {
        json!({
            "initialized": true,
            "lexical": {
                "status": "ready",
                "indexed_items": 854466,
                "indexed_sessions": 3486,
                "indexed_sources": 12,
                "certified_source_bytes": 10700000000_u64,
            },
            "refresh": {
                "status": "pending",
                "reason": "core_refresh_pending",
                "request_id": "logical-request",
                "request_state": "running",
                "logical_request_id": "logical-request",
                "logical_phase": "direct",
                "physical_attempt_id": "physical-attempt",
                "physical_attempt_state": "running",
                "progress_owner_request_id": "physical-attempt",
                "progress_owner_attempt_state": "running",
                "progress": {
                    "phase": "scanning_provider_sources",
                    "completed_sources": 7,
                    "total_sources": 12,
                    "total_sources_known": true,
                    "current_source": "~/.local/share/opencode/opencode.db",
                    "completed_records": 1234,
                    "completed_bytes": 4 * 1024 * 1024,
                    "providers": ["opencode"],
                    "processed_sessions": 18,
                    "processed_messages": 1234,
                    "processed_tool_calls": 91,
                    "processed_bytes": 4 * 1024 * 1024,
                    "elapsed_millis": 2_000,
                },
            },
            "semantic": {
                "status": "disabled",
                "enabled": false,
                "coverage": {},
            },
            "daemon": {
                "status": "running",
                "running": true,
                "jobs": {"semantic_index": {"status": "disabled"}},
            },
        })
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn searchable_generation_and_refresh_progress_are_separate_truths() {
        let rendered = render_dashboard(&readiness(), &context(80), true).render_plain();
        assert!(rendered.starts_with("✓ Your history is searchable"));
        assert!(rendered.contains("3,486"));
        assert!(rendered.contains("854,466 searchable"));
        assert!(rendered.contains("Refresh"));
        assert!(rendered.contains("Agent histories  OpenCode"));
        assert!(rendered.contains("Sessions         18"));
        assert!(rendered.contains("Messages         1,234"));
        assert!(rendered.contains("Tool calls       91"));
        assert!(rendered.contains("Data scanned     4.0 MiB"));
        assert!(!rendered.contains("7 / 12"));
        assert!(!rendered.contains("~/.local/share/opencode/opencode.db"));
        assert!(!rendered.contains("inventory"));
        assert!(!rendered.contains("history file"));
    }

    #[test]
    fn first_publication_uses_authoritative_refresh_progress() {
        let mut value = readiness();
        value["initialized"] = json!(false);
        value["lexical"] = json!({
            "status": "pending",
            "reason": "generation_not_published",
        });
        let rendered = render_dashboard(&value, &context(80), true).render_plain();
        assert!(rendered.starts_with("Indexing your agent history"));
        assert!(rendered.contains("Current index  not published"));
        assert!(!rendered.contains("ctx setup"));
    }

    #[test]
    fn stopped_daemon_does_not_present_a_stale_refresh_eta() {
        let mut value = readiness();
        value["daemon"]["running"] = json!(false);

        let rendered = render_dashboard(&value, &context(80), true).render_plain();

        assert!(rendered.contains("Background indexing stopped"));
        assert!(!rendered.contains("\nRefresh\n"), "{rendered}");
        assert!(!rendered.contains("Remaining"), "{rendered}");
    }

    #[test]
    fn semantic_only_wait_omits_unselected_refresh_convergence() {
        let rendered = render_dashboard(&readiness(), &context(80), false).render_plain();

        assert!(rendered.contains("Your history is searchable"));
        assert!(rendered.contains("Semantic search"));
        assert!(!rendered.contains("\nRefresh\n"), "{rendered}");
        assert!(!rendered.contains("Remaining"), "{rendered}");
    }

    #[test]
    fn missing_unrequested_generation_points_to_setup() {
        let mut value = readiness();
        value["initialized"] = json!(false);
        value["lexical"] = json!({
            "status": "unavailable",
            "reason": "generation_not_published",
        });
        value["refresh"] = json!({
            "status": "unavailable",
            "reason": "daemon_unavailable",
        });
        let rendered = render_dashboard(&value, &context(80), true).render_plain();
        assert!(rendered.starts_with("✗ Search index is not set up"));
        assert!(rendered.contains("ctx setup"));
    }

    #[test]
    fn dashboard_fits_supported_widths() {
        for width in [32, 48, 80, 120] {
            let context = context(width);
            assert_fits(&render_dashboard(&readiness(), &context, true), &context);
        }
    }

    #[test]
    fn styled_rendering_strips_to_plain_bytes() {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always),
        );
        let document = render_dashboard(&readiness(), &context, true);
        let styled = document.render(&context);
        assert!(styled.contains("\u{1b}["));
        assert_eq!(strip_ansi(&styled), document.render_plain());
    }
}
