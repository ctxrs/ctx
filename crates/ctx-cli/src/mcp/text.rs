use serde_json::Value;

const MCP_TEXT_MAX_SEARCH_RESULTS: usize = 5;
const MCP_TEXT_MAX_SOURCES: usize = 12;
const MCP_TEXT_MAX_SQL_ROWS: usize = 8;
const MCP_TEXT_MAX_SQL_COLUMNS: usize = 6;
const MCP_TEXT_MAX_EVENTS: usize = 8;
const MCP_TEXT_MAX_SNIPPET_CHARS: usize = 320;
const MCP_TEXT_MAX_EVENT_CHARS: usize = 500;
const MCP_TEXT_MAX_CELL_CHARS: usize = 80;

pub(super) fn render_tool_text(value: &Value) -> String {
    match value.get("item_type").and_then(Value::as_str) {
        Some("sql_result") => render_sql_text(value),
        Some("session_transcript") => render_session_text(value),
        Some("event_window") => render_event_window_text(value),
        _ if value.get("results").and_then(Value::as_array).is_some() => render_search_text(value),
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
    push_key_value(&mut out, "database_path", value.get("database_path"));
    push_key_value(&mut out, "indexed_items", value.get("indexed_items"));
    push_key_value(&mut out, "indexed_sources", value.get("indexed_sources"));
    push_key_value(
        &mut out,
        "cataloged_sessions",
        value.get("cataloged_sessions"),
    );
    push_key_value(
        &mut out,
        "indexed_catalog_sessions",
        value.get("indexed_catalog_sessions"),
    );
    push_key_value(
        &mut out,
        "pending_catalog_sessions",
        value.get("pending_catalog_sessions"),
    );
    push_key_value(
        &mut out,
        "failed_catalog_sessions",
        value.get("failed_catalog_sessions"),
    );
    push_key_value(&mut out, "read_only", value.get("read_only"));
    push_key_value(&mut out, "local_only", value.get("local_only"));
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
    push_filter_summary(&mut out, value.get("filters"));
    out.push_str(&format!("results: {}\n", results.len()));
    if results.is_empty() {
        return out;
    }

    for (index, result) in results.iter().take(MCP_TEXT_MAX_SEARCH_RESULTS).enumerate() {
        let heading = value_field(result, "title")
            .filter(|title| !title.trim().is_empty())
            .or_else(|| value_field(result, "item_type"))
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
    out
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

fn render_sql_text(value: &Value) -> String {
    let columns = value
        .get("columns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let rows = value
        .get("rows")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut out = String::from("ctx sql\n");
    push_key_value(&mut out, "returned_rows", value.get("returned_rows"));
    if let Some(truncated) = value.get("truncated") {
        let rows_truncated = value_field(truncated, "rows").unwrap_or_else(|| "false".to_owned());
        let values_truncated =
            value_field(truncated, "values").unwrap_or_else(|| "false".to_owned());
        out.push_str(&format!(
            "truncated: rows={rows_truncated}, values={values_truncated}\n"
        ));
    }
    push_key_value(&mut out, "elapsed_ms", value.get("elapsed_ms"));
    if columns.is_empty() {
        out.push_str("columns: 0\n");
        return out;
    }

    let visible_column_count = columns.len().min(MCP_TEXT_MAX_SQL_COLUMNS);
    let headers = columns
        .iter()
        .take(visible_column_count)
        .map(|column| table_cell(&scalar_text(column), MCP_TEXT_MAX_CELL_CHARS))
        .collect::<Vec<_>>();
    out.push_str("\n| ");
    out.push_str(&headers.join(" | "));
    out.push_str(" |\n| ");
    out.push_str(
        &(0..visible_column_count)
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push_str(" |\n");
    for row in rows.iter().take(MCP_TEXT_MAX_SQL_ROWS) {
        let cells = row
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .take(visible_column_count)
            .map(sql_cell_text)
            .collect::<Vec<_>>();
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
    }
    push_omitted_line(&mut out, rows.len(), MCP_TEXT_MAX_SQL_ROWS, "rows");
    push_omitted_line(&mut out, columns.len(), MCP_TEXT_MAX_SQL_COLUMNS, "columns");
    out
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
    if let Some(max_events) = value
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
        "\n{}. {sequence}{role} {event_type}{suffix}\n",
        index
    ));
    push_indented_key_value(out, "ctx_event_id", event.get("ctx_event_id"));
    if let Some(text) = value_field(event, "text")
        .or_else(|| value_field(event, "preview"))
        .filter(|text| !text.is_empty())
    {
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

fn sql_cell_text(value: &Value) -> String {
    let text = match value {
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("text") => {
                let mut text = object
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if object.get("truncated").and_then(Value::as_bool) == Some(true) {
                    text.push_str("... (truncated)");
                }
                text
            }
            Some("blob") => {
                let bytes = object
                    .get("bytes")
                    .and_then(Value::as_u64)
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "?".to_owned());
                let preview = object
                    .get("preview_hex")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let suffix = if object.get("truncated").and_then(Value::as_bool) == Some(true) {
                    " truncated"
                } else {
                    ""
                };
                format!("blob {bytes} bytes {preview}{suffix}")
            }
            _ => scalar_text(value),
        },
        _ => scalar_text(value),
    };
    table_cell(&text, MCP_TEXT_MAX_CELL_CHARS)
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
