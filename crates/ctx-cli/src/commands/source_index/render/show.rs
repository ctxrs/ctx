use serde_json::Value;

use crate::ui::{Document, RenderContext, Token};

use super::human::{push_field, push_heading, push_prefixed, push_wrapped};

const HEADER_LABEL_WIDTH: usize = 16;
const EVENT_INDENT: usize = 3;
const EVENT_LABEL_WIDTH: usize = 5;

pub(in crate::commands::source_index) fn render_show_document(
    value: &Value,
    context: &RenderContext,
) -> Document {
    let mut document = Document::new();
    if value["target"].as_str() == Some("session") {
        render_session_header(&mut document, context, value);
    } else {
        render_event_header(&mut document, context, value);
    }

    let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if events.is_empty() {
        document.push_blank();
        push_heading(&mut document, "No transcript events.", Token::Warning);
        return document;
    }

    for (position, event) in events.iter().enumerate() {
        document.push_blank();
        render_event(&mut document, context, position + 1, event);
    }

    if value.get("truncated").is_some_and(Value::is_object) {
        document.push_blank();
        push_heading(&mut document, "Transcript is truncated.", Token::Warning);
        if let Some(max_events) = value["truncated"]["max_events"].as_u64() {
            push_wrapped(
                &mut document,
                context,
                2,
                &format!("Showing the first {max_events} events."),
                Token::Text,
            );
        }
    }
    document
}

fn render_session_header(document: &mut Document, context: &RenderContext, value: &Value) {
    push_heading(document, "Session", Token::Heading);
    push_optional_field(
        document,
        context,
        "ID",
        value["ctx_session_id"].as_str(),
        Token::Reference,
    );
    push_optional_field(
        document,
        context,
        "Provider",
        value["provider"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        document,
        context,
        "Provider session",
        value["provider_session_id"].as_str(),
        Token::Reference,
    );
    push_optional_field(
        document,
        context,
        "Mode",
        value["mode"].as_str(),
        Token::Text,
    );
}

fn render_event_header(document: &mut Document, context: &RenderContext, value: &Value) {
    push_heading(document, "Event window", Token::Heading);
    push_optional_field(
        document,
        context,
        "Event",
        value["ctx_event_id"].as_str(),
        Token::Reference,
    );
    push_optional_field(
        document,
        context,
        "Session",
        value["ctx_session_id"].as_str(),
        Token::Reference,
    );
    let selected = &value["event"];
    push_optional_field(
        document,
        context,
        "Provider",
        selected["provider"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        document,
        context,
        "Provider session",
        selected["provider_session_id"].as_str(),
        Token::Reference,
    );
}

fn push_optional_field(
    document: &mut Document,
    context: &RenderContext,
    label: &str,
    value: Option<&str>,
    token: Token,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        push_field(
            document,
            context,
            0,
            label,
            HEADER_LABEL_WIDTH,
            value,
            token,
        );
    }
}

fn render_event(document: &mut Document, context: &RenderContext, position: usize, event: &Value) {
    let event_type = event["event_type"].as_str().unwrap_or("event");
    let role = event["role"].as_str();
    let title = match role {
        Some(role) if role != event_type => format!("{role} {event_type}"),
        Some(role) => role.to_owned(),
        None => event_type.to_owned(),
    };
    push_prefixed(
        document,
        context,
        0,
        &format!("{position}. "),
        Token::Accent,
        &title,
        Token::Heading,
    );
    if let Some(occurred_at) = event["occurred_at"].as_str() {
        push_field(
            document,
            context,
            EVENT_INDENT,
            "Time",
            EVENT_LABEL_WIDTH,
            occurred_at,
            Token::Text,
        );
    }
    if let Some(event_id) = event["ctx_event_id"].as_str() {
        push_field(
            document,
            context,
            EVENT_INDENT,
            "Event",
            EVENT_LABEL_WIDTH,
            event_id,
            Token::Reference,
        );
    }

    document.push_blank();
    for logical_line in event["text"].as_str().unwrap_or_default().split('\n') {
        push_wrapped(document, context, EVENT_INDENT, logical_line, Token::Text);
    }
}
