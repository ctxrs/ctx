use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, ContinuationReason, EvidenceCitation, LineRange, ResolvedBlameTarget,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::ui::{sanitize_untrusted_history_body_for_terminal, ColorMode, StreamKind, TestContext};
use crate::ui::{Document, Line, RenderContext, Span, Token};

use super::layout::{
    display_width, enum_heading, line_range_text, push_atomic, push_authored, push_heading,
    FIELD_GAP,
};
use crate::pro::evidence_preview::{
    EvidencePreview, EvidencePreviewModel, MAX_EVIDENCE_PREVIEW_CITATIONS,
    MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
};

const EVIDENCE_PREVIEW_BUDGET_WIDTH: usize = 32;
pub(super) const MAX_EVIDENCE_PREVIEW_RENDERED_BYTES: usize = 4_096;
const EVIDENCE_CONTEXT_HEADING: &str = "Evidence context (local history content)";

pub(super) fn admitted_previews(model: &EvidencePreviewModel) -> EvidencePreviewModel {
    let budget_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, EVIDENCE_PREVIEW_BUDGET_WIDTH)
            .color(ColorMode::Always),
    );
    let mut rendered = Document::new();
    rendered.push_blank();
    rendered.append(preview_header(&budget_context));
    let mut previews = Vec::new();

    for preview in model.previews.iter().take(MAX_EVIDENCE_PREVIEW_CITATIONS) {
        if preview.excerpt.len() > MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES {
            continue;
        }
        let excerpt_lines = preview
            .excerpt
            .split('\n')
            .map(sanitize_untrusted_history_body_for_terminal)
            .collect::<Vec<_>>();
        let Some(item) = preview_item(&budget_context, preview, &excerpt_lines) else {
            continue;
        };
        let mut candidate = rendered.clone();
        candidate.append(item);
        if !within_rendered_preview_budget(&candidate, &budget_context) {
            continue;
        }
        rendered = candidate;
        previews.push(preview.clone());
    }

    EvidencePreviewModel { previews }
}

pub(super) fn render_previews(
    document: &mut Document,
    context: &RenderContext,
    model: &EvidencePreviewModel,
) {
    let admitted = admitted_previews(model);
    if admitted.previews.is_empty() {
        return;
    }
    let mut actual = Document::new();
    actual.push_blank();
    actual.append(preview_header(context));

    for preview in &admitted.previews {
        let excerpt_lines = preview
            .excerpt
            .split('\n')
            .map(sanitize_untrusted_history_body_for_terminal)
            .collect::<Vec<_>>();
        let Some(actual_item) = preview_item(context, preview, &excerpt_lines) else {
            continue;
        };
        actual.append(actual_item);
    }
    document.append(actual);
}

fn preview_header(context: &RenderContext) -> Document {
    let mut document = Document::new();
    push_authored(
        &mut document,
        context,
        0,
        EVIDENCE_CONTEXT_HEADING,
        Token::Heading,
    );
    document
}

fn preview_item(
    context: &RenderContext,
    preview: &EvidencePreview,
    excerpt_lines: &[String],
) -> Option<Document> {
    if preview.citation_numbers.is_empty()
        || preview.citation_numbers.len() > MAX_EVIDENCE_PREVIEW_CITATIONS
        || preview.citation_numbers.contains(&0)
    {
        return None;
    }
    let mut document = Document::new();
    let tool_name = sanitize_untrusted_history_body_for_terminal(&preview.tool_name);
    let kind = format!(
        "{} file request via {tool_name}",
        enum_heading(preview.operation)
    );
    let references_width = preview
        .citation_numbers
        .iter()
        .map(|number| display_width(&format!("[{number}]")))
        .sum::<usize>()
        .saturating_add(preview.citation_numbers.len().saturating_sub(1));
    let combined_width = 2usize
        .saturating_add(references_width)
        .saturating_add(1)
        .saturating_add(display_width(&kind));
    let mut references = preview_reference_line(&preview.citation_numbers);
    if context
        .content_width()
        .is_none_or(|width| combined_width <= width)
    {
        references.push(Span::text(" "));
        references.push(Span::new(kind, Token::Label));
        document.push_line(references);
    } else {
        document.push_line(references);
        push_atomic(&mut document, 4, &kind, Token::Label);
    }
    for line in excerpt_lines {
        push_literal_excerpt_line(&mut document, context, 4, line, Token::Text);
    }
    Some(document)
}

fn push_literal_excerpt_line(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    text: &str,
    token: Token,
) {
    let Some(width) = context
        .content_width()
        .map(|width| width.saturating_sub(indent).max(1))
    else {
        push_atomic(document, indent, text, token);
        return;
    };
    let mut fragment = String::new();
    let mut fragment_width = 0usize;
    let mut whitespace_break = None;
    for grapheme in text.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if !fragment.is_empty() && fragment_width.saturating_add(grapheme_width) > width {
            if let Some(index) = whitespace_break.take() {
                let remainder = fragment.split_off(index);
                push_atomic(document, indent, &fragment, token);
                fragment = remainder;
                fragment_width = display_width(&fragment);
            }
            if !fragment.is_empty() && fragment_width.saturating_add(grapheme_width) > width {
                push_atomic(document, indent, &fragment, token);
                fragment.clear();
                fragment_width = 0;
            }
        }
        fragment.push_str(grapheme);
        fragment_width = fragment_width.saturating_add(grapheme_width);
        if grapheme.chars().next().is_some_and(char::is_whitespace) {
            whitespace_break = Some(fragment.len());
        }
    }
    push_atomic(document, indent, &fragment, token);
}

fn preview_reference_line(numbers: &[u32]) -> Line {
    let mut line = Line::new().with(Span::text("  "));
    for (index, number) in numbers.iter().enumerate() {
        if index > 0 {
            line.push(Span::text(" "));
        }
        line.push(Span::new(format!("[{number}]"), Token::Reference));
    }
    line
}

pub(super) fn within_rendered_preview_budget(document: &Document, context: &RenderContext) -> bool {
    document.render(context).len() <= MAX_EVIDENCE_PREVIEW_RENDERED_BYTES
}

pub(super) fn render_list(document: &mut Document, context: &RenderContext, result: &BlameResult) {
    if result.evidence.is_empty() {
        return;
    }
    document.push_blank();
    push_heading(document, 0, "Evidence");
    for evidence in &result.evidence {
        let reference = format!("[{}]", evidence.number);
        let citation = citation_text(&evidence.citation);
        let citation_token = Token::Command;
        let same_line_width = 2usize
            .saturating_add(display_width(&reference))
            .saturating_add(FIELD_GAP)
            .saturating_add(display_width(&citation));
        if context
            .content_width()
            .is_none_or(|width| same_line_width <= width)
        {
            document.push_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new(reference, Token::Reference))
                    .with(Span::text(" ".repeat(FIELD_GAP)))
                    .with(Span::new(citation, citation_token)),
            );
        } else {
            document.push_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new(reference, Token::Reference)),
            );
            push_atomic(document, 4, &citation, citation_token);
        }
    }
}

pub(super) fn render_continuation(
    document: &mut Document,
    context: &RenderContext,
    result: &BlameResult,
) {
    let Some(next) = &result.next else {
        return;
    };
    document.push_blank();
    push_heading(document, 0, "More results");
    let summary = match next.reason {
        ContinuationReason::MoreMatches => {
            format!(
                "{} matches shown; more matches are available.",
                result.matches.len()
            )
        }
        ContinuationReason::MoreCommittedLines => {
            let window = emitted_file_window(&result.matches)
                .map(|lines| format!(" for committed lines {}", line_range_text(&lines)))
                .unwrap_or_default();
            format!(
                "{} matches shown{window}; more committed lines are available.",
                result.matches.len(),
            )
        }
    };
    push_authored(document, context, 2, &summary, Token::Text);
    push_atomic(document, 2, "Continue", Token::Label);
    push_atomic(
        document,
        4,
        &continuation_command(result, &next.cursor),
        Token::Command,
    );
}

fn citation_text(citation: &EvidenceCitation) -> String {
    format!(
        "ctx show event {} · Core {} · source {} · sequence {}",
        citation.event_id,
        &citation.core_generation_id[..12],
        citation.source.identity(),
        citation.event_sequence,
    )
}

fn continuation_command(result: &BlameResult, cursor: &str) -> String {
    let (mut command, repository) = match &result.target {
        ResolvedBlameTarget::File {
            path,
            repository,
            requested_lines,
        } => {
            let mut command = format!("ctx blame file {}", shell_display(path));
            if let Some(lines) = requested_lines {
                command.push_str(" --lines ");
                command.push_str(&line_range_argument(lines));
            }
            (command, repository)
        }
        ResolvedBlameTarget::Commit { commit, repository } => (
            format!("ctx blame commit {}", shell_display(&commit.display)),
            repository,
        ),
        ResolvedBlameTarget::PullRequest {
            selector,
            pull_request: _,
            repository,
        } => (
            format!("ctx blame pr {}", shell_display(selector)),
            repository,
        ),
    };
    command.push_str(" --repository ");
    command.push_str(&shell_display(&repository.display));
    command.push_str(" --cursor ");
    command.push_str(&shell_display(cursor));
    command
}

fn emitted_file_window(matches: &[BlameMatch]) -> Option<LineRange> {
    let mut ranges = matches.iter().filter_map(|value| match value {
        BlameMatch::File(value) => Some(&value.lines),
        BlameMatch::Commit(_) | BlameMatch::PullRequest(_) => None,
    });
    let first = ranges.next()?;
    let mut start = first.start;
    let mut end = first.end;
    for range in ranges {
        start = start.min(range.start);
        end = end.max(range.end);
    }
    Some(LineRange { start, end })
}

fn line_range_argument(lines: &LineRange) -> String {
    if lines.start == lines.end {
        lines.start.to_string()
    } else {
        format!("{}:{}", lines.start, lines.end)
    }
}

fn shell_display(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:@".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
