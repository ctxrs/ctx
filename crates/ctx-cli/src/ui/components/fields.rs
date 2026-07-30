use super::layout::{display_width, pad_after, wrap_text};
use crate::ui::{Document, Line, RenderContext, Span, Token};

const FIELD_GAP: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Field<'a> {
    pub(crate) label: &'a str,
    pub(crate) value: &'a str,
}

impl<'a> Field<'a> {
    pub(crate) const fn new(label: &'a str, value: &'a str) -> Self {
        Self { label, value }
    }
}

pub(crate) fn fields(context: &RenderContext, values: &[Field<'_>]) -> Document {
    if values.is_empty() {
        return Document::new();
    }

    let label_width = values
        .iter()
        .map(|field| display_width(field.label))
        .max()
        .unwrap_or(0);
    let stacked = context
        .content_width()
        .is_some_and(|width| width <= label_width.saturating_add(FIELD_GAP).saturating_add(12));

    if stacked {
        stacked_fields(context, values)
    } else {
        aligned_fields(context, values, label_width)
    }
}

fn aligned_fields(context: &RenderContext, values: &[Field<'_>], label_width: usize) -> Document {
    let value_width = context.content_width().map(|width| {
        width
            .saturating_sub(label_width)
            .saturating_sub(FIELD_GAP)
            .max(1)
    });
    let mut document = Document::new();

    for field in values {
        let wrapped = wrap_text(field.value, value_width);
        for (index, value) in wrapped.into_iter().enumerate() {
            let mut line = Line::new();
            if index == 0 {
                line.push(Span::new(field.label, Token::Label));
                line.push(Span::text(pad_after(field.label, label_width)));
            } else {
                line.push(Span::text(" ".repeat(label_width)));
            }
            line.push(Span::text(" ".repeat(FIELD_GAP)));
            line.push(Span::text(value));
            document.push_line(line);
        }
    }
    document
}

fn stacked_fields(context: &RenderContext, values: &[Field<'_>]) -> Document {
    let value_width = context
        .content_width()
        .map(|width| width.saturating_sub(FIELD_GAP).max(1));
    let mut document = Document::new();

    for field in values {
        document.push_line(Line::styled(field.label, Token::Label));
        for value in wrap_text(field.value, value_width) {
            document.push_line(
                Line::new()
                    .with(Span::text(" ".repeat(FIELD_GAP)))
                    .with(Span::text(value)),
            );
        }
    }
    document
}
