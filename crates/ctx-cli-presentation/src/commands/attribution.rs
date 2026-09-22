use crate::ui::{fields, section, Document, Field, RenderContext};
use serde_json::Value;

/// Displays the runtime-owned projection. Unknown or absent fields stay unknown.
pub fn append_attribution(context: &RenderContext, document: &mut Document, report: &Value) {
    let Some(attribution) = report.get("attribution") else {
        return;
    };
    let state = match attribution["currentness"].as_str() {
        Some("current") => match attribution["materialized_coverage"].as_str() {
            Some("empty") => "ready; no indexed evidence",
            Some("abstained") => "ready; evidence did not support attribution",
            _ => "ready",
        },
        Some("not_materialized") => "not indexed",
        Some("partial") => "partially indexed",
        Some("stale") => "index is behind current history",
        Some("needs_rebuild") => "index needs rebuilding",
        _ => "unavailable",
    };
    let diagnostic = attribution
        .get("diagnostic")
        .or_else(|| attribution.get("error"));
    let detail = diagnostic.and_then(|diagnostic| diagnostic["message"].as_str());
    let action = diagnostic
        .and_then(|diagnostic| diagnostic["next_action"]["argv"].as_array())
        .and_then(|argv| argv.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .map(|argv| {
            argv.into_iter()
                .map(crate::transcript::shell_quote_arg)
                .collect::<Vec<_>>()
                .join(" ")
        });
    let mut rows = vec![Field::new("Index", state)];
    let progress = attribution
        .get("progress")
        .filter(|value| value.is_object());
    let work = progress
        .and_then(|progress| progress["phase"].as_str())
        .map(|phase| match phase {
            "preparing" => "preparing index",
            "indexing" => "indexing history",
            "publishing" => "publishing index",
            "complete" => "finishing",
            _ => "running",
        });
    let counts = progress.and_then(|progress| {
        let completed = progress["completed_sources"].as_u64()?;
        let total = progress["total_sources"].as_u64()?;
        let changes = progress["applied_changes"].as_u64()?;
        Some(format!(
            "{completed}/{total} sources; {changes} changes applied"
        ))
    });
    if let Some(work) = work {
        rows.push(Field::new("Activity", work));
    }
    if let Some(counts) = &counts {
        rows.push(Field::new("Progress", counts));
    }
    let indexing_disabled = attribution["indexing_enabled"] == false;
    if indexing_disabled {
        rows.push(Field::new("Indexing", "disabled by blame.enabled = false"));
    }
    if let Some(detail) = detail {
        rows.push(Field::new("Detail", detail));
    }
    if !indexing_disabled {
        if let Some(action) = &action {
            rows.push(Field::new("Complete", action));
        }
    }
    document.push_blank();
    document.append(section("Blame", fields(context, &rows)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{StreamKind, TestContext};
    use serde_json::json;

    #[test]
    fn current_empty_is_terminal_and_pending_shows_runtime_completion_command() {
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
        for coverage in ["empty", "abstained"] {
            let mut doc = Document::new();
            append_attribution(
                &context,
                &mut doc,
                &json!({"attribution":{"currentness":"current","materialized_coverage":coverage}}),
            );
            assert!(doc.render_plain().contains("ready;"));
            assert!(!doc.render_plain().contains("Complete"));
        }
        let mut doc = Document::new();
        append_attribution(
            &context,
            &mut doc,
            &json!({"attribution":{"currentness":"not_materialized","diagnostic":{"message":"Indexing is required.","next_action":{"kind":"import_all","argv":["ctx","import","--all"]}}}}),
        );
        assert!(doc.render_plain().contains("ctx import --all"));
        assert!(!doc.render_plain().contains("index watch"));
    }

    #[test]
    fn active_work_and_disabled_indexing_do_not_replace_committed_readiness() {
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
        let mut doc = Document::new();
        append_attribution(
            &context,
            &mut doc,
            &json!({"attribution": {
                "currentness": "stale", "indexing_enabled": false,
                "diagnostic": {"next_action": {"argv": ["ctx", "import", "--all"]}},
                "progress": {"phase": "indexing", "completed_sources": 2, "total_sources": 7, "applied_changes": 89}
            }}),
        );
        let text = doc.render_plain();
        assert!(text.contains("behind current history"));
        assert!(text.contains("disabled by blame.enabled = false"));
        assert!(text.contains("indexing history"));
        assert!(text.contains("2/7 sources; 89 changes applied"));
        assert!(!text.contains("ctx import --all"));
    }
}
