use super::{hint, layout::wrap_text, Action, Hint};
use crate::ui::{Document, Line, RenderContext, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EmptyState<'a> {
    pub(crate) title: &'a str,
    pub(crate) detail: &'a str,
    pub(crate) action: Option<Action<'a>>,
}

pub(crate) fn empty_state(context: &RenderContext, state: EmptyState<'_>) -> Document {
    let mut document = Document::from_line(Line::styled(state.title, Token::Heading));
    for line in wrap_text(state.detail, context.content_width()) {
        document.push_line(Line::text(line));
    }
    if let Some(action) = state.action {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Try this command",
            },
            Some(action),
        ));
    }
    document
}
