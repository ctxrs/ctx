use super::layout::{display_width, is_copyable_atom, pad_after, wrap_text};
use crate::ui::{Document, Line, RenderContext, Span, Token};

const FIELD_GAP: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    pub label: &'a str,
    pub value: &'a str,
    value_token: Token,
    continuation: bool,
}

impl<'a> Field<'a> {
    pub const fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            value_token: Token::Text,
            continuation: false,
        }
    }

    /// Continues the preceding field's value on another aligned row.
    pub const fn continuation(value: &'a str) -> Self {
        Self {
            label: "",
            value,
            value_token: Token::Text,
            continuation: true,
        }
    }

    pub const fn with_value_token(mut self, token: Token) -> Self {
        self.value_token = token;
        self
    }

    const fn value_is_copyable(&self) -> bool {
        matches!(self.value_token, Token::Command | Token::Reference)
    }
}

pub fn fields(context: &RenderContext, values: &[Field<'_>]) -> Document {
    if values.is_empty() {
        return Document::new();
    }

    let label_width = values
        .iter()
        .map(|field| display_width(field.label))
        .max()
        .unwrap_or(0);
    fields_with_label_width(context, values, label_width)
}

/// Renders a field group with a caller-owned label column. Separate groups in
/// one live frame can therefore keep their values at the same horizontal
/// position without merging their semantic spacing.
pub(super) fn fields_with_label_width(
    context: &RenderContext,
    values: &[Field<'_>],
    label_width: usize,
) -> Document {
    if values.is_empty() {
        return Document::new();
    }

    let label_width = label_width.max(
        values
            .iter()
            .map(|field| display_width(field.label))
            .max()
            .unwrap_or(0),
    );
    let stacked = context.content_width().is_some_and(|width| {
        let aligned_value_width = width
            .saturating_sub(label_width)
            .saturating_sub(FIELD_GAP)
            .max(1);
        width <= label_width.saturating_add(FIELD_GAP).saturating_add(12)
            || values.iter().any(|field| {
                if field.value_is_copyable() {
                    display_width(field.value) > aligned_value_width
                } else {
                    field.value.split_whitespace().any(|word| {
                        is_copyable_atom(word) && display_width(word) > aligned_value_width
                    })
                }
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
        let wrapped = field_value_lines(field, value_width);
        for (index, value) in wrapped.into_iter().enumerate() {
            let mut line = Line::new();
            if index == 0 {
                if field.continuation {
                    line.push(Span::text(" ".repeat(label_width)));
                } else {
                    line.push(Span::new(field.label, Token::Label));
                    line.push(Span::text(pad_after(field.label, label_width)));
                }
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
        if !field.continuation {
            document.push_line(Line::styled(field.label, Token::Label));
        }
        for value in field_value_lines(field, value_width) {
            document.push_line(
                Line::new()
                    .with(Span::text(" ".repeat(FIELD_GAP)))
                    .with(Span::new(value, field.value_token)),
            );
        }
    }
    document
}

fn field_value_lines(field: &Field<'_>, width: Option<usize>) -> Vec<String> {
    if field.value_is_copyable() {
        vec![crate::ui::document::neutralize_controls(field.value)]
    } else {
        wrap_text(field.value, width)
    }
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

    #[test]
    fn explicit_label_width_aligns_separate_field_groups() {
        let context = context(80);
        assert_eq!(
            fields_with_label_width(&context, &[Field::new("Agent histories", "Codex")], 19,)
                .render_plain(),
            "Agent histories      Codex\n"
        );
        assert_eq!(
            fields_with_label_width(&context, &[Field::new("Sessions", "1")], 19).render_plain(),
            "Sessions             1\n"
        );
    }

    #[test]
    fn semantic_values_are_indivisible_in_aligned_and_stacked_layouts() {
        let value = "  路径  two\tthree\rfour\u{0001}\u{001b}\u{007f}\u{0085}\u{009f}  ";
        let visible = "  路径  two\\tthree\\rfour\\u{0001}\\x1b\\u{007f}\\u{0085}\\u{009f}  ";

        for (width, expected) in [
            (16, format!("Path\n  {visible}\n")),
            (80, format!("Path  {visible}\n")),
        ] {
            for token in [Token::Reference, Token::Command] {
                let rendered = fields(
                    &context(width),
                    &[Field::new("Path", value).with_value_token(token)],
                )
                .render_plain();

                assert_eq!(rendered, expected, "width {width}");
            }
        }
    }

    #[test]
    fn ordinary_prose_fields_keep_existing_whitespace_collapsing_and_wrapping() {
        let rendered = fields(
            &context(32),
            &[Field::new(
                "Detail",
                "  leading   repeated spaces wrap as ordinary prose  ",
            )],
        )
        .render_plain();

        assert_eq!(
            rendered,
            "Detail  leading repeated spaces\n        wrap as ordinary prose\n"
        );
    }
}
