use crate::ui::{Document, Line, RenderContext, Span, Token};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const FIELD_GAP: usize = 2;

pub(super) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub(super) fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

pub(super) fn push_heading(document: &mut Document, text: &str, token: Token) {
    document.push_line(Line::styled(text, token));
}

pub(super) fn push_wrapped(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    text: &str,
    token: Token,
) {
    let safe = safe_text(text);
    let width = context
        .content_width()
        .map(|width| width.saturating_sub(indent).max(1));
    for segment in wrap_safe(&safe, width) {
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(indent)))
                .with(Span::new(segment, token)),
        );
    }
}

pub(super) fn push_prefixed(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    prefix: &str,
    prefix_token: Token,
    text: &str,
    text_token: Token,
) {
    let safe_prefix = safe_text(prefix);
    let safe_text = safe_text(text);
    let prefix_width = display_width(&safe_prefix);
    let text_width = context.content_width().map(|width| {
        width
            .saturating_sub(indent)
            .saturating_sub(prefix_width)
            .max(1)
    });
    for (index, segment) in wrap_safe(&safe_text, text_width).into_iter().enumerate() {
        let mut line = Line::new().with(Span::text(" ".repeat(indent)));
        if index == 0 {
            line.push(Span::new(&safe_prefix, prefix_token));
        } else {
            line.push(Span::text(" ".repeat(prefix_width)));
        }
        line.push(Span::new(segment, text_token));
        document.push_line(line);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_field(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    label: &str,
    label_width: usize,
    value: &str,
    value_token: Token,
) {
    let safe_label = safe_text(label);
    let safe_value = safe_text(value);
    let label_width = label_width.max(display_width(&safe_label));
    let prefix_width = indent.saturating_add(label_width).saturating_add(FIELD_GAP);
    let aligned = context
        .content_width()
        .is_none_or(|width| width >= prefix_width.saturating_add(8));

    if !aligned {
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(indent)))
                .with(Span::new(safe_label, Token::Label)),
        );
        push_wrapped(
            document,
            context,
            indent.saturating_add(FIELD_GAP),
            &safe_value,
            value_token,
        );
        return;
    }

    let value_width = context
        .content_width()
        .map(|width| width.saturating_sub(prefix_width).max(1));
    for (index, segment) in wrap_safe(&safe_value, value_width).into_iter().enumerate() {
        let mut line = Line::new().with(Span::text(" ".repeat(indent)));
        if index == 0 {
            line.push(Span::new(&safe_label, Token::Label));
            line.push(Span::text(
                " ".repeat(label_width.saturating_sub(display_width(&safe_label))),
            ));
        } else {
            line.push(Span::text(" ".repeat(label_width)));
        }
        line.push(Span::text(" ".repeat(FIELD_GAP)));
        line.push(Span::new(segment, value_token));
        document.push_line(line);
    }
}

pub(super) fn push_action(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    title: &str,
    command: &str,
) {
    document.push_line(
        Line::new()
            .with(Span::text(" ".repeat(indent)))
            .with(Span::new(title, Token::Heading)),
    );
    push_command(document, context, indent.saturating_add(2), command);
}

pub(super) fn push_command(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    command: &str,
) {
    let safe = safe_text(command);
    let available = context
        .content_width()
        .map(|width| width.saturating_sub(indent).max(1));
    if available.is_none_or(|width| display_width(&safe) <= width) {
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(indent)))
                .with(Span::new(safe, Token::Command)),
        );
        return;
    }

    let available = available.unwrap_or(1);
    let line_width = available.saturating_sub(2).max(1);
    let mut physical_lines = Vec::new();
    let mut current = String::new();
    for word in safe.split_whitespace() {
        let joined_width = if current.is_empty() {
            display_width(word)
        } else {
            display_width(&current)
                .saturating_add(1)
                .saturating_add(display_width(word))
        };
        if !current.is_empty() && joined_width > line_width {
            physical_lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        if display_width(&current) > line_width {
            physical_lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        physical_lines.push(current);
    }
    if physical_lines.is_empty() {
        physical_lines.push(String::new());
    }

    let last = physical_lines.len().saturating_sub(1);
    for (index, mut line_text) in physical_lines.into_iter().enumerate() {
        if index < last {
            line_text.push_str(" \\");
        }
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(indent)))
                .with(Span::new(line_text, Token::Command)),
        );
    }
}

fn safe_text(text: &str) -> String {
    Span::text(text).content().to_owned()
}

fn wrap_safe(text: &str, width: Option<usize>) -> Vec<String> {
    let Some(width) = width else {
        return vec![text.to_owned()];
    };
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut remaining = text;
    let mut wrapped = Vec::new();
    while !remaining.is_empty() {
        let mut used = 0usize;
        let mut last_space_end = None;
        let mut overflow_at = None;

        for (index, character) in remaining.char_indices() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if used.saturating_add(character_width) > width && index > 0 {
                overflow_at = Some(index);
                break;
            }
            used = used.saturating_add(character_width);
            if character == ' ' {
                last_space_end = Some(index + character.len_utf8());
            }
        }

        let Some(overflow_at) = overflow_at else {
            wrapped.push(remaining.to_owned());
            break;
        };
        let split_at = last_space_end
            .filter(|split_at| *split_at > 0 && *split_at <= overflow_at)
            .unwrap_or(overflow_at);
        let split_at = protected_escape_range(remaining, split_at).map_or(split_at, |range| {
            if range.start > 0 {
                range.start
            } else {
                range.end
            }
        });
        wrapped.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }
    wrapped
}

fn protected_escape_range(text: &str, split_at: usize) -> Option<std::ops::Range<usize>> {
    for (start, _) in text.match_indices('\\') {
        let tail = &text[start..];
        let length = if tail.starts_with("\\x1b") {
            Some(5)
        } else if tail.starts_with("\\r") || tail.starts_with("\\t") || tail.starts_with("\\n") {
            Some(2)
        } else if tail.starts_with("\\u{") {
            tail.find('}').map(|end| end.saturating_add(1))
        } else {
            None
        };
        let Some(length) = length else {
            continue;
        };
        let end = start.saturating_add(length);
        if start < split_at && split_at < end {
            return Some(start..end);
        }
    }
    None
}
