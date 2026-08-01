use super::layout::{display_width, is_copyable_atom, pad_after, wrap_text};
use crate::ui::{Document, Line, RenderContext, Span, Token};

const FIELD_GAP: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Field<'a> {
    pub(crate) label: &'a str,
    pub(crate) value: &'a str,
    value_token: Token,
}

impl<'a> Field<'a> {
    pub(crate) const fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            value_token: Token::Text,
        }
    }

    pub(crate) const fn with_value_token(mut self, token: Token) -> Self {
        self.value_token = token;
        self
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
    let stacked = context.content_width().is_some_and(|width| {
        let aligned_value_width = width
            .saturating_sub(label_width)
            .saturating_sub(FIELD_GAP)
            .max(1);
        width <= label_width.saturating_add(FIELD_GAP).saturating_add(12)
            || values.iter().any(|field| {
                field
                    .value
                    .split_whitespace()
                    .any(|word| is_copyable_atom(word) && display_width(word) > aligned_value_width)
            })
    });

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
            line.push(Span::new(value, field.value_token));
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
                    .with(Span::new(value, field.value_token)),
            );
        }
    }
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    #[test]
    fn oversized_copyable_values_receive_their_own_child_line() {
        let url = "https://connect.stripe.com/setup/s/test";
        for width in [32, 48] {
            let rendered = fields(
                &context(width),
                &[
                    Field::new("State", "pending"),
                    Field::new("Setup link", url),
                ],
            )
            .render_plain();
            assert!(rendered.contains("Setup link\n  https://"), "{rendered}");
            assert!(!rendered.contains("Setup link  https://"), "{rendered}");
            assert_eq!(rendered.matches(url).count(), 1, "{rendered}");
        }

        let rendered = fields(
            &context(80),
            &[
                Field::new("State", "pending"),
                Field::new("Setup link", url),
            ],
        )
        .render_plain();
        assert!(rendered.contains("Setup link  https://"), "{rendered}");
        assert_eq!(rendered.matches(url).count(), 1, "{rendered}");
    }

    #[test]
    fn value_tokens_survive_aligned_and_stacked_layouts() {
        let values = [
            Field::new("Status", "running").with_value_token(Token::Success),
            Field::new("Persistence", "not verified").with_value_token(Token::Warning),
            Field::new("Configuration", "failed").with_value_token(Token::Error),
            Field::new("Caveat", "restart required"),
        ];

        let aligned_context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always),
        );
        let aligned = fields(&aligned_context, &values);
        assert_eq!(
            aligned.render_plain(),
            "Status         running\n\
             Persistence    not verified\n\
             Configuration  failed\n\
             Caveat         restart required\n"
        );
        assert_eq!(
            aligned.render(&aligned_context),
            "\u{1b}[2mStatus\u{1b}[0m         \u{1b}[32mrunning\u{1b}[0m\n\
             \u{1b}[2mPersistence\u{1b}[0m    \u{1b}[33mnot verified\u{1b}[0m\n\
             \u{1b}[2mConfiguration\u{1b}[0m  \u{1b}[31mfailed\u{1b}[0m\n\
             \u{1b}[2mCaveat\u{1b}[0m         restart required\n"
        );

        let stacked_context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 24).color(ColorMode::Always),
        );
        let stacked = fields(&stacked_context, &values);
        assert_eq!(
            stacked.render_plain(),
            concat!(
                "Status\n",
                "  running\n",
                "Persistence\n",
                "  not verified\n",
                "Configuration\n",
                "  failed\n",
                "Caveat\n",
                "  restart required\n",
            )
        );
        assert_eq!(
            stacked.render(&stacked_context),
            concat!(
                "\u{1b}[2mStatus\u{1b}[0m\n",
                "  \u{1b}[32mrunning\u{1b}[0m\n",
                "\u{1b}[2mPersistence\u{1b}[0m\n",
                "  \u{1b}[33mnot verified\u{1b}[0m\n",
                "\u{1b}[2mConfiguration\u{1b}[0m\n",
                "  \u{1b}[31mfailed\u{1b}[0m\n",
                "\u{1b}[2mCaveat\u{1b}[0m\n",
                "  restart required\n",
            )
        );
    }
}
