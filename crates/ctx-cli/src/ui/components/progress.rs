use super::layout::{display_width, pad, wrap_text};
use crate::ui::{glyph::Glyph, Document, Line, RenderContext, Span, Token};

const MAX_BAR_WIDTH: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Progress<'a> {
    pub(crate) label: &'a str,
    pub(crate) current: u64,
    pub(crate) total: Option<u64>,
    pub(crate) detail: Option<&'a str>,
}

pub(crate) fn progress(context: &RenderContext, progress: Progress<'_>) -> Document {
    let percentage = progress.total.filter(|total| *total > 0).map(|total| {
        let complete = u128::from(progress.current.min(total));
        let total = u128::from(total);
        format!("{}%", complete.saturating_mul(100) / total)
    });
    let mut document = Document::new();
    push_heading(
        &mut document,
        context,
        progress.label,
        percentage.as_deref(),
    );
    push_bar(&mut document, context, progress.current, progress.total);

    if let Some(detail) = progress.detail {
        for line in wrap_text(detail, context.content_width()) {
            document.push_line(Line::styled(line, Token::Label));
        }
    }
    document
}

fn push_heading(
    document: &mut Document,
    context: &RenderContext,
    label: &str,
    percentage: Option<&str>,
) {
    let available = context.content_width();
    let same_line = percentage.is_some_and(|percentage| {
        available.is_none_or(|width| {
            display_width(label)
                .saturating_add(2)
                .saturating_add(display_width(percentage))
                <= width
        })
    });

    if same_line {
        let percentage = percentage.unwrap_or_default();
        let gap = available.map_or(2, |width| {
            width
                .saturating_sub(display_width(label))
                .saturating_sub(display_width(percentage))
                .max(2)
        });
        document.push_line(
            Line::new()
                .with(Span::new(label, Token::Heading))
                .with(Span::text(pad(gap)))
                .with(Span::new(percentage, Token::Accent)),
        );
        return;
    }

    for line in wrap_text(label, available) {
        document.push_line(Line::styled(line, Token::Heading));
    }
    if let Some(percentage) = percentage {
        document.push_line(Line::styled(percentage, Token::Accent));
    }
}

fn push_bar(document: &mut Document, context: &RenderContext, current: u64, total: Option<u64>) {
    let bar_width = context
        .content_width()
        .map_or(MAX_BAR_WIDTH, |width| width.min(MAX_BAR_WIDTH))
        .max(1);
    let Some(total) = total.filter(|total| *total > 0) else {
        document.push_line(Line::styled(Glyph::Ellipsis.render(context), Token::Accent));
        return;
    };

    let filled = (u128::from(current.min(total)).saturating_mul(bar_width as u128)
        / u128::from(total)) as usize;
    let remaining = bar_width.saturating_sub(filled);
    document.push_line(
        Line::new()
            .with(Span::new(
                Glyph::Progress.render(context).repeat(filled),
                Token::Accent,
            ))
            .with(Span::new(
                Glyph::Rule.render(context).repeat(remaining),
                Token::Label,
            )),
    );
}
