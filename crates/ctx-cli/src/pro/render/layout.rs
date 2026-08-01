use chrono::{DateTime, SecondsFormat, Utc};
use ctx_pro_host_protocol::{FactConfidence, FactState, LineRange, ResourceRef};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::{Document, Line, RenderContext, Span, Token};

pub(super) const FIELD_GAP: usize = 2;
pub(super) const METADATA_LABEL_WIDTH: usize = 12;

pub(super) fn timestamp_text(unix_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(unix_ms).map_or_else(
        || format!("{unix_ms} ms since Unix epoch"),
        |timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

pub(super) fn confidence_token(confidence: FactConfidence) -> Token {
    match confidence {
        FactConfidence::Ambiguous | FactConfidence::Unknown => Token::Warning,
        FactConfidence::Explicit
        | FactConfidence::High
        | FactConfidence::Medium
        | FactConfidence::Low => Token::Text,
    }
}

pub(super) fn state_token(state: FactState) -> Token {
    match state {
        FactState::Asserted => Token::Success,
        FactState::Ambiguous | FactState::Superseded => Token::Warning,
        FactState::Contradicted => Token::Error,
    }
}

pub(super) fn push_enum_field<T: serde::Serialize>(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    label: &str,
    value: T,
    token: Token,
) {
    push_field(
        document,
        context,
        indent,
        label,
        METADATA_LABEL_WIDTH,
        &enum_text(value),
        token,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_field(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    label: &str,
    label_width: usize,
    value: &str,
    token: Token,
    atomic: bool,
) {
    let effective_label_width = label_width.max(display_width(label));
    let prefix_width = indent
        .saturating_add(effective_label_width)
        .saturating_add(FIELD_GAP);
    let value_width = context
        .content_width()
        .map(|width| width.saturating_sub(prefix_width).max(1));
    let aligned = context.content_width().is_none_or(|width| {
        if atomic {
            prefix_width.saturating_add(display_width(value)) <= width
        } else {
            width >= prefix_width.saturating_add(8)
        }
    });

    if aligned {
        let values = if atomic {
            vec![value.to_owned()]
        } else {
            wrap_authored(value, value_width)
        };
        for (index, value) in values.into_iter().enumerate() {
            let mut line = Line::new().with(Span::text(" ".repeat(indent)));
            if index == 0 {
                line.push(Span::new(label, Token::Label));
                line.push(Span::text(" ".repeat(
                    effective_label_width.saturating_sub(display_width(label)),
                )));
            } else {
                line.push(Span::text(" ".repeat(effective_label_width)));
            }
            line.push(Span::text(" ".repeat(FIELD_GAP)));
            line.push(Span::new(value, token));
            document.push_line(line);
        }
        return;
    }

    push_atomic(document, indent, label, Token::Label);
    if atomic {
        push_atomic(document, indent + 2, value, token);
    } else {
        push_authored(document, context, indent + 2, value, token);
    }
}

pub(super) fn push_references(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    label: &str,
    label_width: usize,
    numbers: &[u32],
) {
    if numbers.is_empty() {
        push_field(
            document,
            context,
            indent,
            label,
            label_width,
            "none",
            Token::Warning,
            false,
        );
        return;
    }

    let references = numbers
        .iter()
        .map(|number| format!("[{number}]"))
        .collect::<Vec<_>>();
    let references_width = references
        .iter()
        .map(|reference| display_width(reference))
        .sum::<usize>()
        .saturating_add(references.len().saturating_sub(1));
    let effective_label_width = label_width.max(display_width(label));
    let prefix_width = indent
        .saturating_add(effective_label_width)
        .saturating_add(FIELD_GAP);
    let same_line = context
        .content_width()
        .is_none_or(|width| prefix_width.saturating_add(references_width) <= width);

    if same_line {
        let mut line = Line::new()
            .with(Span::text(" ".repeat(indent)))
            .with(Span::new(label, Token::Label))
            .with(Span::text(" ".repeat(
                effective_label_width.saturating_sub(display_width(label)),
            )))
            .with(Span::text(" ".repeat(FIELD_GAP)));
        for (index, reference) in references.into_iter().enumerate() {
            if index > 0 {
                line.push(Span::text(" "));
            }
            line.push(Span::new(reference, Token::Reference));
        }
        document.push_line(line);
        return;
    }

    push_atomic(document, indent, label, Token::Label);
    let child_indent = indent + 2;
    let available = context
        .content_width()
        .map(|width| width.saturating_sub(child_indent).max(1));
    let mut line = Line::new().with(Span::text(" ".repeat(child_indent)));
    let mut used = 0usize;
    for reference in references {
        let separator = usize::from(used > 0);
        let width = display_width(&reference);
        if available.is_some_and(|available| {
            used.saturating_add(separator).saturating_add(width) > available && used > 0
        }) {
            document.push_line(line);
            line = Line::new().with(Span::text(" ".repeat(child_indent)));
            used = 0;
        }
        if used > 0 {
            line.push(Span::text(" "));
            used = used.saturating_add(1);
        }
        line.push(Span::new(reference, Token::Reference));
        used = used.saturating_add(width);
    }
    document.push_line(line);
}

pub(super) fn push_target_resource(
    document: &mut Document,
    context: &RenderContext,
    label: &str,
    label_width: usize,
    resource: &ResourceRef,
) {
    push_field(
        document,
        context,
        0,
        label,
        label_width,
        &resource.display,
        Token::Text,
        true,
    );
    if !resource_id_is_redundant(resource) {
        let id_label = format!("{label} ID");
        push_field(
            document,
            context,
            0,
            &id_label,
            label_width.max(display_width(&id_label)),
            &resource.id,
            Token::Text,
            true,
        );
    }
}

pub(super) fn push_resource_primary(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    resource: &ResourceRef,
) {
    let kind = resource.kind.wire_name();
    let same_line_width = indent
        .saturating_add(display_width(kind))
        .saturating_add(1)
        .saturating_add(display_width(&resource.display));
    if context
        .content_width()
        .is_none_or(|width| same_line_width <= width)
    {
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(indent)))
                .with(Span::new(kind, Token::Label))
                .with(Span::text(" "))
                .with(Span::text(&resource.display)),
        );
    } else {
        push_atomic(document, indent, kind, Token::Label);
        push_atomic(document, indent + 2, &resource.display, Token::Text);
    }
    if !resource_id_is_redundant(resource) {
        push_field(
            document,
            context,
            indent + 2,
            "id",
            METADATA_LABEL_WIDTH,
            &resource.id,
            Token::Text,
            true,
        );
    }
}

pub(super) fn push_role_resource(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    role: &str,
    resource: &ResourceRef,
) {
    let value = format!("{} {}", resource.kind.wire_name(), resource.display);
    push_field(
        document,
        context,
        indent,
        role,
        METADATA_LABEL_WIDTH,
        &value,
        Token::Text,
        true,
    );
    if !resource_id_is_redundant(resource) {
        push_field(
            document,
            context,
            indent + 2,
            "id",
            METADATA_LABEL_WIDTH.saturating_sub(2),
            &resource.id,
            Token::Text,
            true,
        );
    }
}

pub(super) fn same_resource(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.kind == right.kind && left.id == right.id
}

pub(super) fn push_heading(document: &mut Document, indent: usize, text: &str) {
    document.push_line(
        Line::new()
            .with(Span::text(" ".repeat(indent)))
            .with(Span::new(text, Token::Heading)),
    );
}

pub(super) fn push_notice(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    text: &str,
) {
    let text_width = context
        .content_width()
        .map(|width| width.saturating_sub(indent).saturating_sub(2).max(1));
    for (index, value) in wrap_authored(text, text_width).into_iter().enumerate() {
        let mut line = Line::new().with(Span::text(" ".repeat(indent)));
        if index == 0 {
            line.push(Span::new("!", Token::Warning));
            line.push(Span::text(" "));
        } else {
            line.push(Span::text("  "));
        }
        line.push(Span::text(value));
        document.push_line(line);
    }
}

pub(super) fn push_atomic(document: &mut Document, indent: usize, text: &str, token: Token) {
    document.push_line(
        Line::new()
            .with(Span::text(" ".repeat(indent)))
            .with(Span::new(text, token)),
    );
}

pub(super) fn push_authored(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    text: &str,
    token: Token,
) {
    let width = context
        .content_width()
        .map(|width| width.saturating_sub(indent).max(1));
    for value in wrap_authored(text, width) {
        push_atomic(document, indent, &value, token);
    }
}

pub(super) fn line_range_text(lines: &LineRange) -> String {
    if lines.start == lines.end {
        lines.start.to_string()
    } else {
        format!("{}-{}", lines.start, lines.end)
    }
}

pub(super) fn enum_text<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
        .replace('_', " ")
}

pub(super) fn enum_heading<T: serde::Serialize>(value: T) -> String {
    let mut text = enum_text(value);
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    text
}

pub(super) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn resource_id_is_redundant(resource: &ResourceRef) -> bool {
    resource.id == format!("{}:{}", resource.kind.wire_name(), resource.display)
}

fn wrap_authored(text: &str, width: Option<usize>) -> Vec<String> {
    let Some(width) = width else {
        return text.split('\n').map(ToOwned::to_owned).collect();
    };
    let width = width.max(1);
    let mut output = Vec::new();
    for logical_line in text.split('\n') {
        let mut current = String::new();
        for word in logical_line.split_whitespace() {
            let joined_width = display_width(&current)
                .saturating_add(usize::from(!current.is_empty()))
                .saturating_add(display_width(word));
            if !current.is_empty() && joined_width > width {
                output.push(std::mem::take(&mut current));
            }
            if display_width(word) <= width {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
                continue;
            }
            for character in word.chars() {
                let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
                if !current.is_empty()
                    && display_width(&current).saturating_add(character_width) > width
                {
                    output.push(std::mem::take(&mut current));
                }
                current.push(character);
            }
        }
        if !current.is_empty() || logical_line.trim().is_empty() {
            output.push(current);
        }
    }
    if output.is_empty() {
        output.push(String::new());
    }
    output
}
