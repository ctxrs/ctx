use std::fmt::Write as _;

use super::{RenderContext, Token};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Span {
    text: String,
    token: Token,
}

impl Span {
    pub(crate) fn new(text: impl Into<String>, token: Token) -> Self {
        Self {
            text: neutralize_controls(&text.into()),
            token,
        }
    }

    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::new(text, Token::Text)
    }

    pub(crate) fn content(&self) -> &str {
        &self.text
    }

    pub(crate) const fn token(&self) -> Token {
        self.token
    }
}

pub(super) fn neutralize_controls(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' => safe.push_str("\\n"),
            '\r' => safe.push_str("\\r"),
            '\t' => safe.push_str("\\t"),
            '\u{1b}' => safe.push_str("\\x1b"),
            character
                if character <= '\u{1f}'
                    || character == '\u{7f}'
                    || ('\u{80}'..='\u{9f}').contains(&character) =>
            {
                let _ = write!(safe, "\\u{{{:04x}}}", u32::from(character));
            }
            character => safe.push(character),
        }
    }
    safe
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Line {
    spans: Vec<Span>,
}

impl Line {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::text(text)],
        }
    }

    pub(crate) fn styled(text: impl Into<String>, token: Token) -> Self {
        Self {
            spans: vec![Span::new(text, token)],
        }
    }

    pub(crate) fn push(&mut self, span: Span) {
        self.spans.push(span);
    }

    pub(crate) fn with(mut self, span: Span) -> Self {
        self.push(span);
        self
    }

    pub(crate) fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.spans.iter().all(|span| span.content().is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Document {
    lines: Vec<Line>,
}

impl Document {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn from_line(line: Line) -> Self {
        Self { lines: vec![line] }
    }

    pub(crate) fn push_line(&mut self, line: Line) {
        self.lines.push(line);
    }

    pub(crate) fn line(mut self, line: Line) -> Self {
        self.push_line(line);
        self
    }

    pub(crate) fn push_blank(&mut self) {
        self.lines.push(Line::new());
    }

    pub(crate) fn append(&mut self, other: Self) {
        self.lines.extend(other.lines);
    }

    pub(crate) fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub(crate) fn render_plain(&self) -> String {
        self.render_with_styles(false)
    }

    pub(crate) fn render(&self, context: &RenderContext) -> String {
        self.render_with_styles(context.color_enabled())
    }

    fn render_with_styles(&self, styled: bool) -> String {
        if self.lines.is_empty() {
            return String::new();
        }

        let mut rendered = String::new();
        for (line_index, line) in self.lines.iter().enumerate() {
            for span in line.spans() {
                let style = span.token().style();
                if styled && style != anstyle::Style::new() {
                    let _ = write!(
                        rendered,
                        "{}{}{}",
                        style.render(),
                        span.content(),
                        style.render_reset()
                    );
                } else {
                    rendered.push_str(span.content());
                }
            }
            if line_index + 1 < self.lines.len() {
                rendered.push('\n');
            }
        }
        rendered.push('\n');
        rendered
    }
}
