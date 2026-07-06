use ctx_history_core::{Event, HistoryRecord, RedactionState};

use crate::filters::{
    context_has_excluded_provider_session, hit_matches_excluded_provider_session,
    is_agent_history_bookkeeping_record, record_text_matches_agent_scope,
};
use crate::model::RecordContext;
use crate::query::{query_terms, SearchFilters};
use crate::ranking::search_sections;

pub fn display_snippet(input: &str, max_chars: usize) -> String {
    local_snippet(input, max_chars)
}

pub(crate) fn joined<const N: usize>(parts: [&str; N]) -> String {
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn event_weight(event: &Event) -> f32 {
    match event.event_type {
        ctx_history_core::EventType::Message => 4.0,
        ctx_history_core::EventType::ToolCall | ctx_history_core::EventType::ToolOutput => 3.5,
        ctx_history_core::EventType::CommandStarted
        | ctx_history_core::EventType::CommandOutput
        | ctx_history_core::EventType::CommandFinished => 3.0,
        _ => 2.0,
    }
}

pub(crate) fn event_text(event: &Event) -> String {
    let payload_text = event_preview_text(event);
    let dedupe_key = event.dedupe_key.as_deref().unwrap_or_default();
    joined([
        event.event_type.as_str(),
        event.role.map(|role| role.as_str()).unwrap_or_default(),
        payload_text.as_str(),
        dedupe_key,
    ])
}

pub fn event_preview_text(event: &Event) -> String {
    if matches!(
        event.redaction_state,
        RedactionState::Raw | RedactionState::Withheld
    ) {
        return "raw event payload withheld".to_owned();
    }
    if let Some(preview) = event_payload_preview(&event.payload) {
        return local_snippet(&preview, 900);
    }
    if event.payload.is_object() || event.payload.is_array() {
        return local_snippet(&event.payload.to_string(), 900);
    }
    String::new()
}

pub(crate) fn event_payload_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_value_preview(body) {
            return Some(preview);
        }
    }
    event_value_preview(payload)
}

pub(crate) fn event_value_preview(value: &serde_json::Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return non_blank(value);
    }
    let object = value.as_object()?;
    for key in [
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ] {
        if let Some(value) = object.get(key).and_then(preview_fragment) {
            return Some(value);
        }
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(preview_fragment)
                .map(|value| format!("{key}: {value}"))
        })
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" | "))
    }
}

pub(crate) fn preview_fragment(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => non_blank(value),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn non_blank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub(crate) fn matches_terms(value: &str, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let haystack = value.to_lowercase();
    terms.iter().all(|term| haystack.contains(term))
}

pub(crate) fn search_snippet(
    record: &HistoryRecord,
    context: &RecordContext,
    query: &str,
    max_chars: usize,
    filters: &SearchFilters,
) -> String {
    let terms = query_terms(query);
    for section in search_sections(record, context, filters) {
        if hit_matches_excluded_provider_session(&section.hit, filters) {
            continue;
        }
        if matches_terms(&section.text, &terms) {
            return matched_snippet(&section.text, &terms, max_chars);
        }
    }
    if !record.body.trim().is_empty()
        && !is_agent_history_bookkeeping_record(record)
        && record_text_matches_agent_scope(context, filters)
        && !context_has_excluded_provider_session(context, filters)
    {
        return local_snippet(&record.body, max_chars);
    }
    String::new()
}

pub(crate) fn matched_snippet(input: &str, terms: &[String], max_chars: usize) -> String {
    let body = input.trim();
    if body.is_empty() {
        return String::new();
    }
    let lower = body.to_lowercase();
    let start = terms
        .iter()
        .filter_map(|term| lower.find(term))
        .min()
        .unwrap_or(0);
    let start = start.saturating_sub(max_chars / 4);
    let snippet = take_chars_from(body, start, max_chars);
    local_snippet(&snippet, max_chars)
}

pub(crate) fn local_snippet(input: &str, max_chars: usize) -> String {
    truncate_chars(input.trim(), max_chars)
}

pub(crate) fn truncate_chars(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

pub(crate) fn take_chars_from(input: &str, start: usize, max_chars: usize) -> String {
    input.chars().skip(start).take(max_chars).collect()
}
