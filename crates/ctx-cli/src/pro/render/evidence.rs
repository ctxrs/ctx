use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, ContinuationReason, EvidenceCitation, LineRange, ResolvedBlameTarget,
};

use crate::ui::{sanitize_untrusted_history_body_for_terminal, ColorMode, StreamKind, TestContext};
use crate::ui::{Document, Line, RenderContext, Span, Token};

use super::layout::{
    display_width, enum_text, line_range_text, push_atomic, push_authored, push_heading, FIELD_GAP,
};
use crate::pro::evidence_preview::{
    EvidencePreview, EvidencePreviewKind, EvidencePreviewModel,
    MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES, MAX_EVIDENCE_PREVIEW_CITATIONS,
    MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
};

const EVIDENCE_PREVIEW_BUDGET_WIDTH: usize = 32;
const EVIDENCE_PREVIEW_DISCLOSURE: &str = "Exact cited local-history evidence, explicitly requested. References match the Evidence citations above.";
const EVIDENCE_PREVIEW_UNAVAILABLE: &str =
    "Exact cited local-history evidence was requested but is unavailable for this result.";

pub(super) fn render_previews(
    document: &mut Document,
    context: &RenderContext,
    model: &EvidencePreviewModel,
) {
    let budget_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, EVIDENCE_PREVIEW_BUDGET_WIDTH)
            .color(ColorMode::Always),
    );
    let mut rendered = Document::new();
    rendered.push_blank();
    rendered.append(preview_header(&budget_context));
    let mut actual = Document::new();
    actual.push_blank();
    actual.append(preview_header(context));
    let mut admitted = 0usize;

    for preview in model.previews.iter().take(MAX_EVIDENCE_PREVIEW_CITATIONS) {
        if preview.excerpt.len() > MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES {
            continue;
        }
        let excerpt = sanitize_untrusted_history_body_for_terminal(&preview.excerpt);
        let Some(budget_item) = preview_item(&budget_context, preview, &excerpt) else {
            continue;
        };
        let mut candidate = rendered.clone();
        candidate.append(budget_item);
        if candidate.render(&budget_context).len() > MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES {
            continue;
        }
        rendered = candidate;
        if let Some(item) = preview_item(context, preview, &excerpt) {
            actual.append(item);
            admitted += 1;
        }
    }

    if admitted == 0 {
        let mut unavailable = Document::new();
        unavailable.push_blank();
        push_heading(&mut unavailable, 0, "Evidence preview");
        push_authored(
            &mut unavailable,
            context,
            2,
            EVIDENCE_PREVIEW_UNAVAILABLE,
            Token::Text,
        );
        document.append(unavailable);
    } else {
        document.append(actual);
    }
}

fn preview_header(context: &RenderContext) -> Document {
    let mut document = Document::new();
    push_heading(&mut document, 0, "Evidence preview");
    push_authored(
        &mut document,
        context,
        2,
        EVIDENCE_PREVIEW_DISCLOSURE,
        Token::Text,
    );
    document
}

fn preview_item(
    context: &RenderContext,
    preview: &EvidencePreview,
    excerpt: &str,
) -> Option<Document> {
    if preview.evidence_numbers.is_empty()
        || preview.evidence_numbers.len() > MAX_EVIDENCE_PREVIEW_CITATIONS
    {
        return None;
    }
    let mut document = Document::new();
    let references = preview
        .evidence_numbers
        .iter()
        .map(|number| format!("[{number}]"))
        .collect::<Vec<_>>()
        .join(" ");
    push_atomic(&mut document, 2, &references, Token::Reference);
    let kind = match preview.kind {
        EvidencePreviewKind::File(kind) => {
            format!("Exact cited file evidence ({})", enum_text(kind))
        }
        EvidencePreviewKind::Commit => "Exact cited commit evidence".to_owned(),
    };
    push_authored(&mut document, context, 4, &kind, Token::Label);
    push_atomic(&mut document, 4, "Event", Token::Label);
    push_atomic(&mut document, 6, &preview.event_id.to_string(), Token::Text);
    push_atomic(&mut document, 4, "Sequence", Token::Label);
    push_atomic(
        &mut document,
        6,
        &preview.event_sequence.to_string(),
        Token::Text,
    );
    push_atomic(&mut document, 4, "Excerpt", Token::Label);
    push_authored(&mut document, context, 6, excerpt, Token::Text);
    Some(document)
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
