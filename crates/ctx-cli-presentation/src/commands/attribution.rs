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
    if let Some(detail) = detail {
        rows.push(Field::new("Detail", detail));
    }
    if let Some(action) = &action {
        rows.push(Field::new("Complete", action));
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
}
