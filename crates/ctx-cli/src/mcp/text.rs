use serde_json::Value;

mod pro;

use pro::{is_blame_result, render_blame_text};

const MCP_TEXT_MAX_SEARCH_RESULTS: usize = 5;
const MCP_TEXT_MAX_SOURCES: usize = 12;
const MCP_TEXT_MAX_EVENTS: usize = 8;
const MCP_TEXT_MAX_SNIPPET_CHARS: usize = 320;
const MCP_TEXT_MAX_EVENT_CHARS: usize = 500;
const MCP_TEXT_MAX_CELL_CHARS: usize = 80;

pub(super) fn render_tool_text(value: &Value) -> String {
    if is_blame_result(value) {
        return render_blame_text(value);
    }

    let payload_type = value.get("payload_type").and_then(Value::as_str);
    match payload_type {
        Some("session_transcript") => render_session_text(value),
        Some("event_window") => render_event_window_text(value),
        Some("search_results") => render_search_text(value),
        _ if value.get("sources").and_then(Value::as_array).is_some() => render_sources_text(value),
        _ if value.get("initialized").and_then(Value::as_bool).is_some() => {
            render_status_text(value)
        }
        _ => render_generic_text(value),
    }
}

fn render_status_text(value: &Value) -> String {
    let mut out = String::from("ctx status\n");
    push_key_value(&mut out, "initialized", value.get("initialized"));
    push_key_value(&mut out, "data_root", value.get("data_root"));
    push_key_value(&mut out, "indexed_items", value.get("indexed_items"));
    push_key_value(&mut out, "indexed_sessions", value.get("indexed_sessions"));
    push_key_value(&mut out, "indexed_events", value.get("indexed_events"));
    push_key_value(&mut out, "indexed_sources", value.get("indexed_sources"));
    push_component_summary(&mut out, "history_epoch", value.get("history_epoch"));
    push_component_summary(&mut out, "lexical", value.get("lexical"));
    if let Some(lexical) = value.get("lexical") {
        push_key_value(&mut out, "lexical_path", lexical.get("path"));
        push_key_value(&mut out, "lexical_generation", lexical.get("generation_id"));
        push_key_value(
            &mut out,
            "lexical_policy_hash",
            lexical
                .get("policy")
                .and_then(|policy| policy.get("published_hash")),
        );
    }
    push_component_summary(&mut out, "source_refresh", value.get("refresh"));
    if let Some(refresh) = value.get("refresh") {
        push_object_summary(
            &mut out,
            "source_refresh_metrics",
            refresh,
            &[
                ("sources", "source_count"),
                ("certified_sources", "certified_source_count"),
                ("certified_bytes", "certified_source_bytes"),
                ("timings_us", "timings_us"),
            ],
        );
    }
    if let Some(pro_projection) = value.get("pro_projection") {
        push_component_summary(&mut out, "pro_projection", Some(pro_projection));
        push_key_value(
            &mut out,
            "pro_projection_authority",
            pro_projection.get("authority"),
        );
        push_component_summary(
            &mut out,
            "pro_core_materialization_receipt",
            pro_projection.get("receipt"),
        );
        push_key_value(
            &mut out,
            "pro_core_materialization_receipt_generation",
            pro_projection
                .get("receipt")
                .and_then(|receipt| receipt.get("core_generation_id")),
        );
    }
    push_key_value(&mut out, "read_only", value.get("read_only"));
    push_key_value(&mut out, "local_only", value.get("local_only"));
    push_status_semantic_summary(&mut out, value.get("semantic"));
    push_status_daemon_summary(&mut out, value.get("daemon"));
    out
}

fn render_sources_text(value: &Value) -> String {
    let sources = value
        .get("sources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let available = sources
        .iter()
        .filter(|source| source.get("status").and_then(Value::as_str) == Some("available"))
        .count();
    let importable = sources
        .iter()
        .filter(|source| source.get("importable").and_then(Value::as_bool) == Some(true))
        .count();

    let mut out = String::from("ctx sources\n");
    out.push_str(&format!("sources: {}\n", sources.len()));
    out.push_str(&format!("available: {available}\n"));
    out.push_str(&format!("importable: {importable}\n"));
    if sources.is_empty() {
        return out;
    }

    let mut visible_sources = sources.iter().collect::<Vec<_>>();
    visible_sources.sort_by_key(|source| {
        (
            source.get("status").and_then(Value::as_str) != Some("available"),
            source.get("importable").and_then(Value::as_bool) != Some(true),
            value_field(source, "provider").unwrap_or_default(),
            value_field(source, "history_source")
                .or_else(|| value_field(source, "path"))
                .unwrap_or_default(),
        )
    });

    out.push_str("\n| provider | status | import | source |\n");
    out.push_str("| --- | --- | --- | --- |\n");
    for source in visible_sources.iter().take(MCP_TEXT_MAX_SOURCES) {
        let provider = value_field(source, "provider").unwrap_or_else(|| "-".to_owned());
        let status = value_field(source, "status").unwrap_or_else(|| "-".to_owned());
        let import = value_field(source, "import_support")
            .or_else(|| value_field(source, "native_import"))
            .unwrap_or_else(|| "-".to_owned());
        let source_label = value_field(source, "history_source")
            .or_else(|| value_field(source, "path"))
            .or_else(|| value_field(source, "manifest_path"))
            .or_else(|| value_field(source, "source_format"))
            .unwrap_or_else(|| "-".to_owned());
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            table_cell(&provider, MCP_TEXT_MAX_CELL_CHARS),
            table_cell(&status, MCP_TEXT_MAX_CELL_CHARS),
            table_cell(&import, MCP_TEXT_MAX_CELL_CHARS),
            table_cell(&source_label, MCP_TEXT_MAX_CELL_CHARS)
        ));
    }
    push_omitted_line(&mut out, sources.len(), MCP_TEXT_MAX_SOURCES, "sources");
    out
}

fn render_search_text(value: &Value) -> String {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut out = String::from("ctx search\n");
    if let Some(query) = value.get("query").and_then(Value::as_str) {
        out.push_str(&format!(
            "query: {}\n",
            clip_inline(query, MCP_TEXT_MAX_SNIPPET_CHARS)
        ));
    }
    if let Some(freshness) = value.get("freshness") {
        let mode = value_field(freshness, "mode");
        let status = value_field(freshness, "status");
        match (mode, status) {
            (Some(mode), Some(status)) => out.push_str(&format!("freshness: {mode}/{status}\n")),
            (Some(mode), None) => out.push_str(&format!("freshness: {mode}\n")),
            (None, Some(status)) => out.push_str(&format!("freshness: {status}\n")),
            (None, None) => {}
        }
    }
    push_retrieval_summary(&mut out, value.get("retrieval"));
    push_filter_summary(&mut out, value.get("filters"));
    out.push_str(&format!("results: {}\n", results.len()));
    if results.is_empty() {
        push_more_results_footer(&mut out, value);
        return out;
    }

    for (index, result) in results.iter().take(MCP_TEXT_MAX_SEARCH_RESULTS).enumerate() {
        let heading = value_field(result, "title")
            .filter(|title| !title.trim().is_empty())
            .or_else(|| value_field(result, "result_type"))
            .unwrap_or_else(|| "result".to_owned());
        out.push_str(&format!(
            "\n{}. {}\n",
            index + 1,
            clip_inline(&heading, MCP_TEXT_MAX_SNIPPET_CHARS)
        ));
        push_indented_key_value(&mut out, "ctx_session_id", result.get("ctx_session_id"));
        push_indented_key_value(&mut out, "ctx_event_id", result.get("ctx_event_id"));
        push_indented_key_value(&mut out, "provider", result.get("provider"));
        push_indented_key_value(&mut out, "timestamp", result.get("timestamp"));
        if let Some(snippet) = value_field(result, "snippet").filter(|snippet| !snippet.is_empty())
        {
            out.push_str(&format!(
                "   snippet: {}\n",
                clip_inline(&snippet, MCP_TEXT_MAX_SNIPPET_CHARS)
            ));
        }
        if let Some(commands) = result
            .get("suggested_next_commands")
            .and_then(Value::as_array)
        {
            for command in commands.iter().filter_map(Value::as_str).take(2) {
                out.push_str(&format!("   next: {command}\n"));
            }
        }
    }
    push_omitted_line(
        &mut out,
        results.len(),
        MCP_TEXT_MAX_SEARCH_RESULTS,
        "results",
    );
    push_more_results_footer(&mut out, value);
    out
}

fn push_more_results_footer(out: &mut String, value: &Value) {
    if value
        .pointer("/result_window/more_available")
        .and_then(Value::as_bool)
        == Some(true)
    {
        out.push_str("More results available.\n");
    }
}

fn push_status_semantic_summary(out: &mut String, semantic: Option<&Value>) {
    let Some(semantic) = semantic else {
        return;
    };
    push_object_summary(
        out,
        "semantic",
        semantic,
        &[
            ("status", "status"),
            ("enabled", "enabled"),
            ("reason", "reason"),
        ],
    );
    if let Some(flat_f32) = semantic.get("flat_f32") {
        push_object_summary(
            out,
            "flat_f32",
            flat_f32,
            &[
                ("status", "status"),
                ("reason", "reason"),
                ("core_generation_id", "core_generation_id"),
                ("flat_generation", "flat_generation"),
                ("flat_generation_hash", "flat_generation_hash"),
                ("active_events", "active_events"),
                ("active_chunks", "active_chunks"),
            ],
        );
        push_key_value(out, "semantic_path", flat_f32.get("path"));
    }
}

fn push_status_daemon_summary(out: &mut String, daemon: Option<&Value>) {
    let Some(daemon) = daemon else {
        return;
    };
    push_object_summary(
        out,
        "daemon",
        daemon,
        &[
            ("enabled", "enabled"),
            ("status", "status"),
            ("running", "running"),
            ("mode", "mode"),
            ("pid", "pid"),
            ("start_mode", "start_mode"),
            ("trigger_command", "trigger_command"),
            ("trigger_provenance", "trigger_provenance"),
        ],
    );
    if let Some(lock) = daemon.get("lock_identity") {
        push_object_summary(
            out,
            "daemon_lock",
            lock,
            &[("path", "path"), ("active", "active"), ("pid", "pid")],
        );
    }
    if let Some(endpoint) = daemon.get("core_refresh_endpoint") {
        push_object_summary(
            out,
            "daemon_endpoint",
            endpoint,
            &[
                ("available", "available"),
                ("transport", "transport"),
                ("address", "address"),
                ("owner_pid", "owner_pid"),
            ],
        );
    }
    let Some(jobs) = daemon.get("jobs") else {
        return;
    };
    let job_parts = ["core_refresh"]
        .into_iter()
        .filter_map(|key| {
            jobs.get(key)
                .and_then(|job| value_field(job, "status"))
                .filter(|status| !status.trim().is_empty())
                .map(|status| format!("{key}={status}"))
        })
        .collect::<Vec<_>>();
    if !job_parts.is_empty() {
        out.push_str(&format!("daemon_jobs: {}\n", job_parts.join(", ")));
    }
}

fn push_component_summary(out: &mut String, label: &str, component: Option<&Value>) {
    let Some(component) = component else {
        return;
    };
    push_object_summary(
        out,
        label,
        component,
        &[("status", "status"), ("reason", "reason")],
    );
}

fn push_retrieval_summary(out: &mut String, retrieval: Option<&Value>) {
    let Some(retrieval) = retrieval else {
        return;
    };
    push_object_summary(
        out,
        "retrieval",
        retrieval,
        &[
            ("requested", "requested_mode"),
            ("effective", "effective_mode"),
            ("semantic_weight", "semantic_weight"),
            ("semantic_status", "semantic_status"),
        ],
    );
    if let Some(fallback_code) =
        value_field(retrieval, "semantic_fallback_code").filter(|code| !code.trim().is_empty())
    {
        out.push_str(&format!("semantic_fallback: {fallback_code}\n"));
    }
    if let Some(fallback) =
        value_field(retrieval, "semantic_fallback").filter(|message| !message.trim().is_empty())
    {
        out.push_str(&format!(
            "semantic_fallback_detail: {}\n",
            clip_inline(&fallback, MCP_TEXT_MAX_SNIPPET_CHARS)
        ));
    }
    if let Some(coverage) = retrieval.get("coverage") {
        push_object_summary(
            out,
            "semantic_coverage",
            coverage,
            &[
                ("searchable_items", "searchable_items"),
                ("embedded_items", "embedded_items"),
                ("embedded_chunks", "embedded_chunks"),
                ("indexed_now", "indexed_now"),
                ("dirty_items", "dirty_items"),
            ],
        );
    }
    if let Some(diagnostics) = retrieval.get("diagnostics") {
        push_object_summary(
            out,
            "retrieval_diagnostics",
            diagnostics,
            &[
                ("vector_backend", "vector_backend"),
                ("semantic_candidates", "semantic_candidates"),
                ("stale_events_dropped", "stale_events_dropped"),
            ],
        );
    }
}

fn push_object_summary(out: &mut String, label: &str, value: &Value, fields: &[(&str, &str)]) {
    let parts = fields
        .iter()
        .filter_map(|(label, key)| {
            value_field(value, key)
                .filter(|field| !field.trim().is_empty())
                .map(|field| format!("{label}={field}"))
        })
        .collect::<Vec<_>>();
    if !parts.is_empty() {
        out.push_str(&format!("{label}: {}\n", parts.join(", ")));
    }
}

fn push_filter_summary(out: &mut String, filters: Option<&Value>) {
    let Some(filters) = filters.and_then(Value::as_object) else {
        return;
    };
    let filter_parts = [
        "provider",
        "history_source",
        "provider_key",
        "source_id",
        "source_format",
        "workspace",
        "since",
        "event_type",
        "file",
        "session",
    ]
    .into_iter()
    .filter_map(|key| value_field(filters.get(key)?, "").map(|value| format!("{key}={value}")))
    .collect::<Vec<_>>();
    if !filter_parts.is_empty() {
        out.push_str(&format!("filters: {}\n", filter_parts.join(", ")));
    }
}

fn render_session_text(value: &Value) -> String {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut out = String::from("ctx show session\n");
    push_key_value(&mut out, "ctx_session_id", value.get("ctx_session_id"));
    push_key_value(&mut out, "provider", value.get("provider"));
    push_key_value(
        &mut out,
        "provider_session_id",
        value.get("provider_session_id"),
    );
    push_key_value(&mut out, "mode", value.get("mode"));
    out.push_str(&format!("events: {}\n", events.len()));
    if let Some(pagination) = value.get("pagination") {
        push_session_pagination(&mut out, value, pagination);
    } else if let Some(max_events) = value
        .get("truncated")
        .and_then(|truncated| truncated.get("max_events"))
        .and_then(Value::as_u64)
    {
        out.push_str(&format!("event list capped at {max_events} events\n"));
    }

    for (index, event) in events.iter().take(MCP_TEXT_MAX_EVENTS).enumerate() {
        push_event_summary(&mut out, index + 1, event);
    }
    push_omitted_line(&mut out, events.len(), MCP_TEXT_MAX_EVENTS, "events");
    out
}

fn push_session_pagination(out: &mut String, value: &Value, pagination: &Value) {
    let limit = pagination.get("limit").and_then(Value::as_u64);
    let returned = pagination.get("returned").and_then(Value::as_u64);
    let has_more = pagination.get("has_more").and_then(Value::as_bool);
    if let (Some(limit), Some(returned), Some(has_more)) = (limit, returned, has_more) {
        out.push_str(&format!(
            "page: limit={limit}, returned={returned}, has_more={has_more}\n"
        ));
    }

    match has_more {
        Some(true) => {
            let session_id = value.get("ctx_session_id").and_then(Value::as_str);
            let mode = value.get("mode").and_then(Value::as_str);
            let cursor = pagination.get("next_cursor").and_then(Value::as_str);
            if let (Some(session_id), Some(mode), Some(limit), Some(cursor)) =
                (session_id, mode, limit, cursor)
            {
                out.push_str(&format!(
                    "continue: call show_session with ctx_session_id={}, mode={}, limit={limit}, cursor={}\n",
                    Value::String(session_id.to_owned()),
                    Value::String(mode.to_owned()),
                    Value::String(cursor.to_owned()),
                ));
            } else {
                out.push_str("more events remain, but continuation metadata is incomplete\n");
            }
        }
        Some(false) => out.push_str("terminal page: no more events\n"),
        None => {}
    }
}

fn render_event_window_text(value: &Value) -> String {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut out = String::from("ctx show event\n");
    push_key_value(&mut out, "ctx_event_id", value.get("ctx_event_id"));
    push_key_value(&mut out, "ctx_session_id", value.get("ctx_session_id"));
    out.push_str(&format!("events: {}\n", events.len()));
    if let Some(event) = value.get("event") {
        out.push_str("\nselected event\n");
        push_event_summary(&mut out, 1, event);
    }

    let selected_event_id = value.get("ctx_event_id").and_then(Value::as_str);
    let window_events = events
        .iter()
        .filter(|event| value_field(event, "ctx_event_id").as_deref() != selected_event_id)
        .collect::<Vec<_>>();
    if !window_events.is_empty() {
        out.push_str("\nwindow\n");
        for (index, event) in window_events.iter().take(MCP_TEXT_MAX_EVENTS).enumerate() {
            push_event_summary(&mut out, index + 1, event);
        }
        push_omitted_line(&mut out, window_events.len(), MCP_TEXT_MAX_EVENTS, "events");
    }
    out
}

fn push_event_summary(out: &mut String, index: usize, event: &Value) {
    let sequence = value_field(event, "sequence")
        .map(|sequence| format!("#{sequence} "))
        .unwrap_or_default();
    let role = value_field(event, "role")
        .filter(|role| !role.is_empty())
        .unwrap_or_else(|| "-".to_owned());
    let event_type = value_field(event, "event_type").unwrap_or_else(|| "event".to_owned());
    let occurred_at = value_field(event, "occurred_at").unwrap_or_default();
    let suffix = if occurred_at.is_empty() {
        String::new()
    } else {
        format!(" {occurred_at}")
    };
    out.push_str(&format!(
        "\n{index}. {sequence}{role} {event_type}{suffix}\n"
    ));
    push_indented_key_value(out, "ctx_event_id", event.get("ctx_event_id"));
    if let Some(text) = value_field(event, "text").filter(|text| !text.is_empty()) {
        out.push_str(&format!(
            "   text: {}\n",
            clip_inline(&text, MCP_TEXT_MAX_EVENT_CHARS)
        ));
    }
}

fn push_key_value(out: &mut String, key: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(value_to_text) {
        out.push_str(&format!("{key}: {value}\n"));
    }
}

fn push_indented_key_value(out: &mut String, key: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(value_to_text) {
        out.push_str(&format!("   {key}: {value}\n"));
    }
}

fn value_field(value: &Value, key: &str) -> Option<String> {
    if key.is_empty() {
        return value_to_text(value);
    }
    value.get(key).and_then(value_to_text)
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_text(value: &Value) -> String {
    value_to_text(value).unwrap_or_else(|| match value {
        Value::Array(values) => format!("[{} values]", values.len()),
        Value::Object(object) => format!("[{} fields]", object.len()),
        Value::Null => "null".to_owned(),
        Value::String(_) | Value::Bool(_) | Value::Number(_) => unreachable!(),
    })
}

fn render_generic_text(value: &Value) -> String {
    let mut out = String::from("ctx tool result\n");
    match value {
        Value::Object(object) => {
            for (key, value) in object.iter().take(12) {
                match value {
                    Value::Array(values) => {
                        out.push_str(&format!("{key}: [{} items]\n", values.len()));
                    }
                    Value::Object(fields) => {
                        out.push_str(&format!("{key}: [{} fields]\n", fields.len()));
                    }
                    _ => push_key_value(&mut out, key, Some(value)),
                }
            }
            push_omitted_line(&mut out, object.len(), 12, "fields");
        }
        Value::Array(values) => {
            out.push_str(&format!("items: {}\n", values.len()));
            for (index, value) in values.iter().take(12).enumerate() {
                out.push_str(&format!("{}. {}\n", index + 1, scalar_text(value)));
            }
            push_omitted_line(&mut out, values.len(), 12, "items");
        }
        _ => push_key_value(&mut out, "value", Some(value)),
    }
    out
}

fn table_cell(text: &str, max_chars: usize) -> String {
    clip_inline(text, max_chars).replace('|', "\\|")
}

fn clip_inline(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    clip_chars(&compact, max_chars)
}

fn clip_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(15);
    let mut clipped = text.chars().take(keep).collect::<String>();
    clipped.push_str("... [truncated]");
    clipped
}

fn push_omitted_line(out: &mut String, total: usize, shown: usize, noun: &str) {
    if total > shown {
        out.push_str(&format!(
            "... {} more {noun} omitted from text\n",
            total - shown
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn search_dispatch_and_text_remain_exact() {
        let value = json!({
            "payload_type": "search_results",
            "query": "journal replay",
            "results": [{
                "title": "Replay decision",
                "ctx_session_id": "session-1",
                "ctx_event_id": "event-2",
                "provider": "codex",
                "timestamp": "2026-07-22T12:00:00Z",
                "snippet": "Use the canonical journal checkpoint.",
                "suggested_next_commands": ["ctx show event event-2"]
            }]
        });
        assert_eq!(
            render_tool_text(&value),
            "ctx search\nquery: journal replay\nresults: 1\n\n1. Replay decision\n   ctx_session_id: session-1\n   ctx_event_id: event-2\n   provider: codex\n   timestamp: 2026-07-22T12:00:00Z\n   snippet: Use the canonical journal checkpoint.\n   next: ctx show event event-2\n"
        );
    }

    #[test]
    fn search_text_reports_only_a_truthful_additional_result() {
        let available = json!({
            "payload_type": "search_results",
            "query": "journal replay",
            "results": [{
                "title": "Replay decision",
                "ctx_session_id": "session-1",
                "ctx_event_id": "event-2"
            }],
            "result_window": {
                "limit": 1,
                "returned": 1,
                "more_available": true
            }
        });
        assert!(
            render_tool_text(&available).ends_with("More results available.\n"),
            "{}",
            render_tool_text(&available)
        );

        let complete = json!({
            "payload_type": "search_results",
            "query": "journal replay",
            "results": [{
                "title": "Replay decision",
                "ctx_session_id": "session-1",
                "ctx_event_id": "event-2"
            }],
            "result_window": {
                "limit": 1,
                "returned": 1,
                "more_available": false
            }
        });
        assert!(!render_tool_text(&complete).contains("More results available."));
    }

    #[test]
    fn session_text_gives_an_exact_continuation_call_for_a_bounded_page() {
        let value = json!({
            "payload_type": "session_transcript",
            "ctx_session_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "provider": "codex",
            "provider_session_id": "provider-session",
            "mode": "log",
            "events": [{"ctx_event_id": "event-1"}, {"ctx_event_id": "event-2"}],
            "pagination": {
                "limit": 2,
                "returned": 2,
                "has_more": true,
                "next_cursor": "opaque-page-2"
            }
        });

        let rendered = render_tool_text(&value);
        assert!(rendered.contains("page: limit=2, returned=2, has_more=true\n"));
        assert!(rendered.contains(
            "continue: call show_session with ctx_session_id=\"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\", mode=\"log\", limit=2, cursor=\"opaque-page-2\"\n"
        ));
        assert!(!rendered.contains("terminal page"));
    }

    #[test]
    fn session_text_marks_a_terminal_page_without_a_continuation_call() {
        let value = json!({
            "payload_type": "session_transcript",
            "ctx_session_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "mode": "full",
            "events": [{"ctx_event_id": "event-3"}],
            "pagination": {
                "limit": 2,
                "returned": 1,
                "has_more": false,
                "next_cursor": null
            }
        });

        let rendered = render_tool_text(&value);
        assert!(rendered.contains("page: limit=2, returned=1, has_more=false\n"));
        assert!(rendered.contains("terminal page: no more events\n"));
        assert!(!rendered.contains("continue: call show_session"));
    }

    #[test]
    fn results_without_an_authoritative_kind_are_generic_not_search() {
        assert_eq!(
            render_tool_text(&json!({"results": [{"title": "not search"}]})),
            "ctx tool result\nresults: [1 items]\n"
        );
    }

    #[test]
    fn non_query_pro_payloads_keep_their_generic_text() {
        let rendered = render_tool_text(&json!({
            "payload_type": "pro_status",
            "installed": true,
            "ready": true
        }));
        assert_eq!(
            rendered,
            "ctx tool result\ninstalled: true\npayload_type: pro_status\nready: true\n"
        );
    }
}
