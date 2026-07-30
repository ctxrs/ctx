use super::layout::wrap_text;
use crate::ui::{glyph::Glyph, Document, Line, RenderContext, Span, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeState {
    Success,
    Warning,
    Error,
    Neutral,
}

impl OutcomeState {
    const fn marker(self) -> Option<Glyph> {
        match self {
            Self::Success => Some(Glyph::Success),
            Self::Warning => Some(Glyph::Warning),
            Self::Error => Some(Glyph::Failure),
            Self::Neutral => None,
        }
    }

    pub(super) const fn token(self) -> Token {
        match self {
            Self::Success => Token::Success,
            Self::Warning => Token::Warning,
            Self::Error => Token::Error,
            Self::Neutral => Token::Text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Outcome<'a> {
    pub(crate) state: OutcomeState,
    pub(crate) title: &'a str,
    pub(crate) detail: Option<&'a str>,
}

pub(crate) fn outcome(context: &RenderContext, outcome: Outcome<'_>) -> Document {
    let marker = outcome
        .state
        .marker()
        .map(|marker| marker.render(context).to_owned());
    let marker_width = marker.as_deref().map_or(0, |marker| {
        super::layout::display_width(marker).saturating_add(1)
    });
    let text_width = context
        .content_width()
        .map(|width| width.saturating_sub(marker_width).max(1));
    let title_lines = wrap_text(outcome.title, text_width);
    let mut document = Document::new();

    for (index, title) in title_lines.into_iter().enumerate() {
        let mut line = Line::new();
        if index == 0 {
            if let Some(marker) = marker.as_deref() {
                line.push(Span::new(marker, outcome.state.token()));
                line.push(Span::text(" "));
            }
        } else if marker_width > 0 {
            line.push(Span::text(" ".repeat(marker_width)));
        }
        line.push(Span::new(title, Token::Heading));
        document.push_line(line);
    }

    if let Some(detail) = outcome.detail {
        for line in wrap_text(detail, context.content_width()) {
            document.push_line(Line::text(line));
        }
    }

    document
}
