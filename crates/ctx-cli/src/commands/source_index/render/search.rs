use serde_json::Value;

use crate::{
    transcript::shell_quote_arg,
    ui::{Document, Line, RenderContext, Span, Token},
};

use super::human::{push_action, push_field, push_heading, push_prefixed, push_wrapped, short_id};

const CARD_INDENT: usize = 3;
const CARD_LABEL_WIDTH: usize = 7;
const VERBOSE_LABEL_WIDTH: usize = 16;

pub(in crate::commands::source_index) fn render_search_document(
    value: &Value,
    verbose: bool,
    context: &RenderContext,
) -> Document {
    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if results.is_empty() {
        return render_empty(value, context);
    }

    let mut document = Document::new();
    push_heading(
        &mut document,
        &format!(
            "{} {}",
            results.len(),
            if results.len() == 1 {
                "result"
            } else {
                "results"
            }
        ),
        Token::Heading,
    );

    for (position, result) in results.iter().enumerate() {
        document.push_blank();
        render_result(&mut document, context, position + 1, result, verbose);
    }

    if value["truncation"]["candidate_pool_truncated"] == true {
        document.push_blank();
        push_heading(&mut document, "Warning", Token::Warning);
        push_wrapped(
            &mut document,
            context,
            2,
            "Session diversity reached the current candidate bound.",
            Token::Text,
        );
        push_wrapped(
            &mut document,
            context,
            2,
            "Refine the query or add a provider, workspace, file, or session filter.",
            Token::Text,
        );
    }
    if value["result_window"]["more_available"] == true {
        document.push_blank();
        push_heading(&mut document, "More results available.", Token::Warning);
    }
    document
}

fn render_empty(value: &Value, context: &RenderContext) -> Document {
    let query = value["query"].as_str().unwrap_or_default();
    let mut document = Document::new();
    push_wrapped(
        &mut document,
        context,
        0,
        &format!("No results for {}", shell_quote_arg(query)),
        Token::Warning,
    );
    document.push_blank();
    document.push_line(Line::styled("Try broader terms", Token::Heading));
    super::human::push_command(&mut document, context, 2, "ctx search \"<term>\"");
    document
}

fn render_result(
    document: &mut Document,
    context: &RenderContext,
    position: usize,
    result: &Value,
    verbose: bool,
) {
    let title = result["title"].as_str().unwrap_or("indexed event");
    let snippet = result["snippet"].as_str().unwrap_or_default();
    let mut snippet_lines = snippet.split('\n');
    let first_snippet = snippet_lines.next().unwrap_or_default();
    let headline = if first_snippet.is_empty() {
        title
    } else {
        first_snippet
    };
    push_prefixed(
        document,
        context,
        0,
        &format!("{position}. "),
        Token::Success,
        headline,
        Token::Heading,
    );
    if first_snippet.is_empty() && !snippet.is_empty() {
        push_wrapped(document, context, CARD_INDENT, "", Token::Text);
    }
    for line in snippet_lines {
        push_wrapped(document, context, CARD_INDENT, line, Token::Text);
    }

    let provider = result["provider"].as_str().unwrap_or("unknown");
    let provider_session = result["provider_session_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    let ctx_session = result["ctx_session_id"].as_str().unwrap_or("unknown");
    let separator = if context.unicode() { " · " } else { " | " };
    let session = provider_session.map_or_else(
        || format!("{provider}{separator}session {}", short_id(ctx_session)),
        |provider_session| format!("{provider}{separator}{provider_session}"),
    );
    push_field(
        document,
        context,
        CARD_INDENT,
        "Session",
        CARD_LABEL_WIDTH,
        &session,
        Token::Text,
    );

    let rank = result["rank"].as_u64().unwrap_or_default();
    let event_id = result["ctx_event_id"].as_str().unwrap_or("unknown");
    let matched = format!("#{rank}{separator}event {}", short_id(event_id));
    push_field(
        document,
        context,
        CARD_INDENT,
        "Match",
        CARD_LABEL_WIDTH,
        &matched,
        Token::Text,
    );

    if let Some(more) = result["more_matches_in_session"]
        .as_u64()
        .filter(|more| *more > 0)
    {
        let detail = format!(
            "{more} {} from this session",
            if more == 1 { "result" } else { "results" }
        );
        push_field(
            document,
            context,
            CARD_INDENT,
            "More",
            CARD_LABEL_WIDTH,
            &detail,
            Token::Label,
        );
    }

    if verbose {
        render_verbose_fields(document, context, result);
    }

    let commands = result["suggested_next_commands"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if let Some(inspect) = commands.first().and_then(Value::as_str) {
        document.push_blank();
        push_action(document, context, CARD_INDENT, "Inspect", inspect);
    }
    if verbose {
        let remaining = commands
            .iter()
            .skip(1)
            .filter_map(Value::as_str)
            .take(2)
            .collect::<Vec<_>>();
        if !remaining.is_empty() {
            document.push_blank();
            document.push_line(
                Line::new()
                    .with(Span::text(" ".repeat(CARD_INDENT)))
                    .with(Span::new("Next", Token::Heading)),
            );
            for command in remaining {
                super::human::push_command(
                    document,
                    context,
                    CARD_INDENT.saturating_add(2),
                    command,
                );
            }
        }
        if event_id != "unknown" {
            document.push_blank();
            push_field(
                document,
                context,
                CARD_INDENT,
                "Citation",
                VERBOSE_LABEL_WIDTH,
                &format!("event {event_id}"),
                Token::Reference,
            );
        }
    }
}

fn render_verbose_fields(document: &mut Document, context: &RenderContext, result: &Value) {
    for (label, key, token) in [
        ("Type", "title", Token::Text),
        ("Event", "ctx_event_id", Token::Reference),
        ("Ctx session", "ctx_session_id", Token::Reference),
        ("Provider session", "provider_session_id", Token::Reference),
        ("Source", "source_format", Token::Text),
    ] {
        if let Some(value) = result[key].as_str().filter(|value| !value.is_empty()) {
            push_field(
                document,
                context,
                CARD_INDENT,
                label,
                VERBOSE_LABEL_WIDTH,
                value,
                token,
            );
        }
    }
    if let Some(rank) = result["rank"].as_u64() {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Rank",
            VERBOSE_LABEL_WIDTH,
            &format!("#{rank}"),
            Token::Text,
        );
    }
    if let Some(score) = result["retrieval_score"].as_f64() {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Retrieval score",
            VERBOSE_LABEL_WIDTH,
            &format!("{score:.2}"),
            Token::Text,
        );
    }
}
