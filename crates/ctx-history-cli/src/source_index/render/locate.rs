use serde_json::Value;

use crate::ui::{Document, RenderContext, Token};

use super::human::{push_field, push_heading};

const LABEL_WIDTH: usize = 16;

pub(in crate::source_index) fn render_locate_document(
    value: &Value,
    context: &RenderContext,
) -> Document {
    let mut document = Document::new();
    let target = value["target"].as_str().unwrap_or("item");
    push_heading(&mut document, &capitalize(target), Token::Heading);

    let id_field = if target == "session" {
        "ctx_session_id"
    } else {
        "ctx_event_id"
    };
    push_optional_field(
        &mut document,
        context,
        "ID",
        value[id_field].as_str(),
        Token::Reference,
    );
    if target == "event" {
        push_optional_field(
            &mut document,
            context,
            "Session",
            value["ctx_session_id"].as_str(),
            Token::Reference,
        );
    }
    push_optional_field(
        &mut document,
        context,
        "Provider",
        value["provider"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        &mut document,
        context,
        "Provider key",
        value["provider_key"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        &mut document,
        context,
        "Source ID",
        value["source_id"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        &mut document,
        context,
        "Provider session",
        value["provider_session_id"].as_str(),
        Token::Reference,
    );
    if target == "session" {
        push_time_field(
            &mut document,
            context,
            "First event",
            value["started_at"].as_str(),
        );
    } else {
        push_time_field(
            &mut document,
            context,
            "Time",
            value["occurred_at"].as_str(),
        );
        if let Some(sequence) = value["sequence"].as_u64() {
            push_field(
                &mut document,
                context,
                0,
                "Sequence",
                LABEL_WIDTH,
                &sequence.to_string(),
                Token::Text,
            );
        }
    }

    document.push_blank();
    push_heading(&mut document, "Core source", Token::Heading);
    for (label, field) in [
        ("ID", "ctx_source_id"),
        ("Format", "source_format"),
        ("Schema", "schema_variant"),
    ] {
        push_optional_field(
            &mut document,
            context,
            label,
            value["source"][field].as_str(),
            Token::Reference,
        );
    }
    document
}

fn push_optional_field(
    document: &mut Document,
    context: &RenderContext,
    label: &str,
    value: Option<&str>,
    token: Token,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        push_field(document, context, 0, label, LABEL_WIDTH, value, token);
    }
}

fn push_time_field(
    document: &mut Document,
    context: &RenderContext,
    label: &str,
    value: Option<&str>,
) {
    let (value, token) = value
        .filter(|value| !value.is_empty())
        .map_or(("time unavailable", Token::Label), |value| {
            (value, Token::Text)
        });
    push_field(document, context, 0, label, LABEL_WIDTH, value, token);
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
