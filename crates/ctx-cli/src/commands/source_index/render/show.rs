use serde_json::Value;

use crate::{
    commands::mcp_tool_call::{mcp_tool_call_display, MCP_TOOL_CALL_JSON_GUIDANCE},
    ui::{Document, Line, RenderContext, Span, Token},
};

use super::human::{display_width, push_field, push_heading, push_prefixed, push_wrapped};

const HEADER_LABEL_WIDTH: usize = 16;
const EVENT_INDENT: usize = 3;
const EVENT_LABEL_WIDTH: usize = 5;
const MCP_EVENT_LABEL_WIDTH: usize = 10;
const LINEAGE_EVENT_LABEL_WIDTH: usize = 16;

pub(in crate::commands::source_index) fn render_show_document(
    value: &Value,
    context: &RenderContext,
) -> Document {
    match value["_stream_part"].as_str() {
        Some("session_header") => return render_session_header_document(value, context),
        Some("session_event") => {
            let position = value["position"]
                .as_u64()
                .and_then(|position| usize::try_from(position).ok())
                .unwrap_or(1);
            return render_session_event_document(&value["event"], context, position);
        }
        Some("session_empty") => return render_session_empty_document(),
        Some("session_truncated") => {
            return render_session_truncated_document(value["max_events"].as_u64(), context);
        }
        _ => {}
    }
    let mut document = Document::new();
    if value["target"].as_str() == Some("session") {
        document.append(render_session_header_document(value, context));
    } else {
        render_event_header(&mut document, context, value);
        render_copied_lineage(&mut document, context, value);
    }

    let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if events.is_empty() {
        document.append(render_session_empty_document());
        return document;
    }

    for (position, event) in events.iter().enumerate() {
        document.append(render_session_event_document(event, context, position + 1));
    }

    if value.get("truncated").is_some_and(Value::is_object) {
        document.append(render_session_truncated_document(
            value["truncated"]["max_events"].as_u64(),
            context,
        ));
    }
    document
}

fn render_copied_lineage(document: &mut Document, context: &RenderContext, value: &Value) {
    let lineage = &value["copied_lineage"];
    let observed = lineage["observed_count"].as_u64().unwrap_or(0);
    if observed == 0 {
        return;
    }
    let truncated = lineage["truncated"].as_bool().unwrap_or(true);
    document.push_blank();
    let noun = if observed == 1 { "session" } else { "sessions" };
    let heading = if truncated {
        format!("Inherited by at least {observed} {noun}")
    } else {
        format!("Inherited by {observed} {noun}")
    };
    push_heading(
        document,
        &heading,
        if truncated {
            Token::Warning
        } else {
            Token::Heading
        },
    );

    if let Some(counts) = lineage["relationship_counts"].as_object() {
        let summary = counts
            .iter()
            .filter_map(|(relationship, count)| {
                count
                    .as_u64()
                    .filter(|count| *count != 0)
                    .map(|count| format!("{relationship} {count}"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        if !summary.is_empty() {
            push_wrapped(document, context, 2, &summary, Token::Label);
        }
    }

    let command_prefix = value["_command_prefix"].as_str().unwrap_or("ctx");
    let occurrences = lineage["occurrences"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (position, occurrence) in occurrences.iter().take(20).enumerate() {
        let relationship = occurrence["session_relationship"]
            .as_str()
            .unwrap_or("inherited");
        document.push_blank();
        push_prefixed(
            document,
            context,
            0,
            &format!("{}. ", position + 1),
            Token::Accent,
            relationship,
            Token::Heading,
        );
        for (label, key) in [
            ("Session", "ctx_session_id"),
            ("Event", "ctx_event_id"),
            ("Copied from session", "copied_from_ctx_session_id"),
            ("Copied from event", "copied_from_ctx_event_id"),
            ("Parent", "parent_ctx_session_id"),
            ("Root", "root_ctx_session_id"),
        ] {
            if let Some(reference) = occurrence[key].as_str() {
                push_field(
                    document,
                    context,
                    EVENT_INDENT,
                    label,
                    LINEAGE_EVENT_LABEL_WIDTH,
                    reference,
                    Token::Reference,
                );
            }
        }
        if let Some(depth) = occurrence["depth"].as_u64() {
            push_field(
                document,
                context,
                EVENT_INDENT,
                "Depth",
                LINEAGE_EVENT_LABEL_WIDTH,
                &depth.to_string(),
                Token::Text,
            );
        }
        if let Some(session_id) = occurrence["ctx_session_id"].as_str() {
            super::human::push_action(
                document,
                context,
                EVENT_INDENT,
                "Open session",
                &format!("{command_prefix} show session {session_id}"),
            );
        }
    }
    let returned = lineage["returned"]
        .as_u64()
        .unwrap_or(occurrences.len() as u64);
    if !truncated && observed > returned {
        push_wrapped(
            document,
            context,
            2,
            &format!("+{} more", observed - returned),
            Token::Label,
        );
    }
}

pub(in crate::commands::source_index) fn render_session_header_document(
    value: &Value,
    context: &RenderContext,
) -> Document {
    let mut document = Document::new();
    render_session_header(&mut document, context, value);
    document
}

pub(in crate::commands::source_index) fn render_session_event_document(
    event: &Value,
    context: &RenderContext,
    position: usize,
) -> Document {
    let mut document = Document::new();
    document.push_blank();
    render_event(&mut document, context, position, event);
    document
}

pub(in crate::commands::source_index) fn render_session_empty_document() -> Document {
    let mut document = Document::new();
    document.push_blank();
    push_heading(&mut document, "No transcript events.", Token::Warning);
    document
}

pub(in crate::commands::source_index) fn render_session_truncated_document(
    max_events: Option<u64>,
    context: &RenderContext,
) -> Document {
    let mut document = Document::new();
    document.push_blank();
    push_heading(&mut document, "Transcript is truncated.", Token::Warning);
    if let Some(max_events) = max_events {
        push_wrapped(
            &mut document,
            context,
            2,
            &format!("Showing the first {max_events} events."),
            Token::Text,
        );
    }
    document
}

fn render_session_header(document: &mut Document, context: &RenderContext, value: &Value) {
    push_heading(document, "Session", Token::Heading);
    let session = &value["session"];
    push_session_lineage(
        document,
        context,
        session["ctx_session_id"]
            .as_str()
            .or_else(|| value["ctx_session_id"].as_str()),
        session["parent_ctx_session_id"].as_str(),
        session["root_ctx_session_id"].as_str(),
    );
    push_optional_field(
        document,
        context,
        "Relationship",
        session["session_relationship"].as_str(),
        Token::Accent,
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
    let selected = &value["event"];
    push_optional_field(
        document,
        context,
        "Event",
        value["ctx_event_id"].as_str(),
        Token::Reference,
    );
    push_session_lineage(
        document,
        context,
        selected["ctx_session_id"]
            .as_str()
            .or_else(|| value["ctx_session_id"].as_str()),
        selected["parent_ctx_session_id"].as_str(),
        selected["root_ctx_session_id"].as_str(),
    );
    push_optional_field(
        document,
        context,
        "Relationship",
        selected["session_relationship"].as_str(),
        Token::Accent,
    );
    push_event_origin(document, context, selected, 0);
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

fn push_session_lineage(
    document: &mut Document,
    context: &RenderContext,
    direct: Option<&str>,
    parent: Option<&str>,
    root: Option<&str>,
) {
    push_optional_field(document, context, "Session", direct, Token::Reference);

    let parent = parent.filter(|parent| Some(*parent) != direct);
    let root = root.filter(|root| Some(*root) != direct);
    match (parent, root) {
        (Some(parent), Some(root)) if parent == root => {
            push_optional_field(
                document,
                context,
                "Parent / root",
                Some(parent),
                Token::Reference,
            );
        }
        (parent, root) => {
            push_optional_field(document, context, "Parent", parent, Token::Reference);
            push_optional_field(document, context, "Root", root, Token::Reference);
        }
    }
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
    let (time, time_token) = event["occurred_at"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map_or(("time unavailable", Token::Label), |value| {
            (value, Token::Text)
        });
    push_field(
        document,
        context,
        EVENT_INDENT,
        "Time",
        EVENT_LABEL_WIDTH,
        time,
        time_token,
    );
    render_event_identity(document, context, event);
    push_event_origin(document, context, event, EVENT_INDENT);
    if let Some(attribution) = mcp_tool_call_display(event) {
        push_field(
            document,
            context,
            EVENT_INDENT,
            "MCP server",
            MCP_EVENT_LABEL_WIDTH,
            &attribution.server,
            Token::Reference,
        );
        push_field(
            document,
            context,
            EVENT_INDENT,
            "MCP tool",
            MCP_EVENT_LABEL_WIDTH,
            &attribution.tool,
            Token::Accent,
        );
        if attribution.truncated {
            push_wrapped(
                document,
                context,
                EVENT_INDENT,
                MCP_TOOL_CALL_JSON_GUIDANCE,
                Token::Warning,
            );
        }
    }

    document.push_blank();
    for logical_line in event["text"].as_str().unwrap_or_default().split('\n') {
        push_wrapped(document, context, EVENT_INDENT, logical_line, Token::Text);
    }
}

fn render_event_identity(document: &mut Document, context: &RenderContext, event: &Value) {
    let Some(event_id) = event["ctx_event_id"].as_str() else {
        return;
    };
    let Some(sequence) = event["sequence"].as_u64() else {
        push_field(
            document,
            context,
            EVENT_INDENT,
            "Event",
            EVENT_LABEL_WIDTH,
            event_id,
            Token::Reference,
        );
        return;
    };
    let sequence = sequence.to_string();
    let separator = if context.unicode() { " · " } else { " | " };
    let prefix_width = EVENT_INDENT
        .saturating_add(EVENT_LABEL_WIDTH)
        .saturating_add(2);
    let combined_width = prefix_width
        .saturating_add(display_width(event_id))
        .saturating_add(display_width(separator))
        .saturating_add(display_width("seq "))
        .saturating_add(display_width(&sequence));

    if context
        .content_width()
        .is_none_or(|available| combined_width <= available)
    {
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(EVENT_INDENT)))
                .with(Span::new("Event", Token::Label))
                .with(Span::text("  "))
                .with(Span::new(event_id, Token::Reference))
                .with(Span::new(separator, Token::Label))
                .with(Span::new("seq ", Token::Label))
                .with(Span::new(sequence, Token::Text)),
        );
    } else {
        push_field(
            document,
            context,
            EVENT_INDENT,
            "Event",
            EVENT_LABEL_WIDTH,
            event_id,
            Token::Reference,
        );
        push_field(
            document,
            context,
            EVENT_INDENT,
            "Sequence",
            EVENT_LABEL_WIDTH,
            &sequence,
            Token::Text,
        );
    }
}

fn push_event_origin(
    document: &mut Document,
    context: &RenderContext,
    event: &Value,
    indent: usize,
) {
    let origin = &event["event_origin"];
    let Some(kind) = origin["kind"].as_str() else {
        return;
    };
    push_field(
        document,
        context,
        indent,
        "Origin",
        LINEAGE_EVENT_LABEL_WIDTH,
        kind,
        if kind == "copied_from_ancestor" {
            Token::Warning
        } else {
            Token::Text
        },
    );
    if kind != "copied_from_ancestor" {
        return;
    }
    for (label, key) in [
        ("Original event", "ancestor_event_id"),
        ("Original session", "ancestor_session_id"),
        ("Copy proof", "proof"),
    ] {
        if let Some(value) = origin[key].as_str() {
            push_field(
                document,
                context,
                indent,
                label,
                LINEAGE_EVENT_LABEL_WIDTH,
                value,
                if key == "proof" {
                    Token::Text
                } else {
                    Token::Reference
                },
            );
        }
    }
}
