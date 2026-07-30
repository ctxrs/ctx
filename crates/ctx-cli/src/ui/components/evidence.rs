use super::layout::{display_width, wrap_text};
use crate::ui::{Document, Line, RenderContext, Span, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Evidence<'a> {
    pub(crate) reference: &'a str,
    pub(crate) summary: &'a str,
    pub(crate) detail: Option<&'a str>,
}

pub(crate) fn evidence_list(context: &RenderContext, evidence: &[Evidence<'_>]) -> Document {
    let mut document = Document::new();
    for (index, item) in evidence.iter().enumerate() {
        if index > 0 {
            document.push_blank();
        }
        let reference = format!("[{}]", item.reference);
        let prefix_width = display_width(&reference).saturating_add(1);
        let summary_width = context
            .content_width()
            .map(|width| width.saturating_sub(prefix_width).max(1));
        for (line_index, summary) in wrap_text(item.summary, summary_width)
            .into_iter()
            .enumerate()
        {
            let mut line = Line::new();
            if line_index == 0 {
                line.push(Span::new(&reference, Token::Reference));
                line.push(Span::text(" "));
            } else {
                line.push(Span::text(" ".repeat(prefix_width)));
            }
            line.push(Span::text(summary));
            document.push_line(line);
        }
        if let Some(detail) = item.detail {
            let detail_width = context
                .content_width()
                .map(|width| width.saturating_sub(2).max(1));
            for detail in wrap_text(detail, detail_width) {
                document.push_line(
                    Line::new()
                        .with(Span::text("  "))
                        .with(Span::new(detail, Token::Label)),
                );
            }
        }
    }
    document
}
