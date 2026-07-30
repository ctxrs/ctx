use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, ContinuationReason, EvidenceCitation, LineRange, ResolvedBlameTarget,
};

use crate::ui::{Document, Line, RenderContext, Span, Token};

use super::layout::{
    display_width, enum_text, line_range_text, push_atomic, push_authored, push_heading, FIELD_GAP,
};

pub(super) fn render_list(document: &mut Document, context: &RenderContext, result: &BlameResult) {
    if result.evidence.is_empty() {
        return;
    }
    document.push_blank();
    push_heading(document, 0, "Evidence");
    for evidence in &result.evidence {
        let reference = format!("[{}]", evidence.number);
        let citation = citation_text(&evidence.citation);
        let citation_token =
            if evidence.citation.event_id.is_some() || evidence.citation.session_id.is_some() {
                Token::Command
            } else {
                Token::Text
            };
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
    if let Some(event_id) = citation.event_id {
        return format!("ctx show event {event_id}");
    }
    if let Some(session_id) = citation.session_id {
        return format!("ctx show session {session_id}");
    }
    if let Some(observation_id) = citation.observation_id {
        let mut value = format!("observation {observation_id}");
        if let Some(sequence) = citation.observation_seq {
            value.push_str(&format!(" sequence {sequence}"));
        }
        if let Some(kind) = citation.observation_kind {
            value.push_str(&format!(" ({})", enum_text(kind)));
        }
        return value;
    }
    if let Some(locator) = &citation.source_locator {
        return serde_json::to_string(locator).unwrap_or_else(|_| "source record".to_owned());
    }
    if let Some(path) = &citation.source_path {
        let mut location = path.clone();
        if let Some(line) = citation.fixture_line {
            location.push_str(&format!(":{line}"));
        }
        if let Some(ordinal) = citation.source_record_ordinal {
            location.push_str(&format!(" record {ordinal}"));
            if let Some(subrecord) = citation.source_record_subrecord_index {
                location.push_str(&format!(".{subrecord}"));
            }
        }
        if let Some(range) = &citation.byte_range {
            location.push_str(&format!(" bytes {}-{}", range.start, range.end_exclusive));
        }
        if let Some(sha256) = &citation.source_sha256 {
            location.push_str(&format!(" sha256 {sha256}"));
        }
        return location;
    }
    "canonical evidence".to_owned()
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
