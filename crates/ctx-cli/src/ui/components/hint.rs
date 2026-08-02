use super::layout::{display_width, wrap_text};
use crate::ui::{Document, Line, RenderContext, Span, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hint<'a> {
    pub(crate) text: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Action<'a> {
    pub(crate) command: &'a str,
}

pub(crate) fn hint(
    context: &RenderContext,
    hint: Hint<'_>,
    action: Option<Action<'_>>,
) -> Document {
    let mut document = labelled_text(context, "Hint", hint.text);
    if let Some(action) = action {
        if !document.is_empty() {
            document.push_blank();
        }
        document.push_line(Line::styled("Next", Token::Label));
        // A recovery command is a single copyable atom. Keep it byte-for-byte on
        // one child line even when it exceeds the display width; wrapping a shell
        // command can change its meaning when copied from a terminal.
        document.push_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(action.command, Token::Command)),
        );
    }
    document
}

fn labelled_text(context: &RenderContext, label: &str, text: &str) -> Document {
    let prefix = format!("{label}: ");
    let prefix_width = display_width(&prefix);
    let text_width = context
        .content_width()
        .map(|width| width.saturating_sub(prefix_width).max(1));
    let wrapped = wrap_text(text, text_width);
    let mut document = Document::new();

    for (index, text) in wrapped.into_iter().enumerate() {
        let mut line = Line::new();
        if index == 0 {
            line.push(Span::new(label, Token::Label));
            line.push(Span::text(": "));
        } else {
            line.push(Span::text(" ".repeat(prefix_width)));
        }
        line.push(Span::text(text));
        document.push_line(line);
    }
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    #[test]
    fn action_commands_are_never_wrapped_or_split() {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 24).color(ColorMode::Never),
        );
        let command = "ctx search 'provider session lookup with a deliberately long query'";
        let rendered = hint(
            &context,
            Hint {
                text: "Rerun the query.",
            },
            Some(Action { command }),
        )
        .render_plain();

        assert!(
            rendered.contains(&format!("Next\n  {command}\n")),
            "{rendered}"
        );
        assert_eq!(rendered.matches(command).count(), 1, "{rendered}");
    }
}
