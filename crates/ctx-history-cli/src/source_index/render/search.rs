use serde_json::Value;

use crate::{
    transcript::shell_quote_arg,
    ui::{
        diagnostic, Action, Diagnostic, DiagnosticLevel, Document, Line, RenderContext, Span, Token,
    },
};

use super::human::{push_action, push_field, push_heading, push_prefixed, push_wrapped};

const CARD_INDENT: usize = 3;
const CARD_LABEL_WIDTH: usize = 8;
const VERBOSE_LABEL_WIDTH: usize = 16;

pub(in crate::source_index) fn render_search_not_ready_document(
    context: &RenderContext,
) -> Document {
    let mut document = diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: "History search is not ready",
            detail: Some(
                "There is no current searchable generation. Set up ctx to discover agent history, or import history if setup is already complete.",
            ),
            fields: &[],
            action: Some(Action {
                command: "ctx setup",
            }),
        },
    );
    document.push_blank();
    push_action(
        &mut document,
        context,
        0,
        "Already set up?",
        "ctx import --all",
    );
    document
}

pub(in crate::source_index) fn render_search_document(
    value: &Value,
    verbose: bool,
    context: &RenderContext,
) -> Document {
    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if results.is_empty() {
        let mut document = render_empty(value, context);
        render_search_footers(&mut document, value, context);
        return document;
    }

    let mut document = Document::new();
    render_results_heading(&mut document, results.len());

    for (position, result) in results.iter().enumerate() {
        document.push_blank();
        render_result(&mut document, context, position + 1, result, verbose);
    }

    render_search_footers(&mut document, value, context);
    document
}

fn render_search_footers(document: &mut Document, value: &Value, context: &RenderContext) {
    if value["truncation"]["candidate_pool_truncated"] == true {
        document.push_blank();
        push_heading(document, "Warning", Token::Warning);
        push_wrapped(
            document,
            context,
            2,
            "Search reached a bounded candidate or work limit.",
            Token::Text,
        );
        push_wrapped(
            document,
            context,
            2,
            "Refine the query or add a provider, workspace, file, or session filter.",
            Token::Text,
        );
    }
    if value["result_window"]["more_available"] == true {
        document.push_blank();
        push_heading(document, "More results available.", Token::Warning);
    }
}

fn render_results_heading(document: &mut Document, result_count: usize) {
    let outcome = format!(
        "{result_count} {}",
        if result_count == 1 {
            "result"
        } else {
            "results"
        }
    );
    push_heading(document, &outcome, Token::Heading);
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
        Token::Accent,
        headline,
        Token::Heading,
    );
    if first_snippet.is_empty() && !snippet.is_empty() {
        push_wrapped(document, context, CARD_INDENT, "", Token::Text);
    }
    for line in snippet_lines {
        push_wrapped(document, context, CARD_INDENT, line, Token::Text);
    }

    let provider = result_provider_label(result);
    push_field(
        document,
        context,
        CARD_INDENT,
        "Provider",
        CARD_LABEL_WIDTH,
        &provider,
        Token::Text,
    );
    if let Some(source) = result_source_identity(result) {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Source",
            CARD_LABEL_WIDTH,
            &source,
            Token::Text,
        );
    }
    let ctx_session = result["ctx_session_id"].as_str().unwrap_or("unknown");
    push_field(
        document,
        context,
        CARD_INDENT,
        "Session",
        CARD_LABEL_WIDTH,
        ctx_session,
        Token::Reference,
    );
    render_direct_lineage_fields(document, context, result);

    let event_id = result["ctx_event_id"].as_str().unwrap_or("unknown");
    render_event_summary(document, context, event_id, result["timestamp"].as_str());

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
    }
}

fn render_event_summary(
    document: &mut Document,
    context: &RenderContext,
    event_id: &str,
    timestamp: Option<&str>,
) {
    let time = timestamp
        .filter(|timestamp| !timestamp.is_empty())
        .map(|timestamp| context.human_timestamp(timestamp));
    let (time, time_token) = time
        .as_deref()
        .map_or(("time unavailable", Token::Label), |time| {
            (time, Token::Text)
        });
    push_field(
        document,
        context,
        CARD_INDENT,
        "Event",
        CARD_LABEL_WIDTH,
        event_id,
        Token::Reference,
    );
    push_field(
        document,
        context,
        CARD_INDENT,
        "Time",
        CARD_LABEL_WIDTH,
        time,
        time_token,
    );
}

fn render_verbose_fields(document: &mut Document, context: &RenderContext, result: &Value) {
    for (label, key, token) in [
        ("Type", "title", Token::Text),
        ("Provider session", "provider_session_id", Token::Reference),
        ("Provider key", "provider_key", Token::Text),
        ("Source ID", "source_id", Token::Text),
        ("Source format", "source_format", Token::Text),
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
    if let Some(sequence) = result["event_seq"].as_u64() {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Sequence",
            VERBOSE_LABEL_WIDTH,
            &sequence.to_string(),
            Token::Text,
        );
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

fn result_provider_label(result: &Value) -> String {
    crate::provider_display_name(result["provider"].as_str().unwrap_or("unknown"))
}

fn result_source_identity(result: &Value) -> Option<String> {
    let provider = result["provider"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let provider_key = result["provider_key"]
        .as_str()
        .filter(|value| !value.is_empty());
    let source_id = result["source_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    let identity = match (provider_key, source_id) {
        (Some(provider_key), Some(source_id)) if provider_key == source_id => {
            provider_key.to_owned()
        }
        (Some(provider_key), Some(source_id)) => format!("{provider_key}/{source_id}"),
        (Some(provider_key), None) => provider_key.to_owned(),
        (None, Some(source_id)) => source_id.to_owned(),
        (None, None) => return None,
    };
    let display_provider = crate::provider_display_name(provider);
    if identity == provider || identity == display_provider {
        None
    } else {
        Some(identity)
    }
}

fn render_direct_lineage_fields(document: &mut Document, context: &RenderContext, result: &Value) {
    for (label, key) in [
        ("Parent", "parent_ctx_session_id"),
        ("Root", "root_ctx_session_id"),
    ] {
        if let Some(reference) = result[key].as_str().filter(|value| !value.is_empty()) {
            // The compact application projection has already chosen the
            // shortest unambiguous prefix. Optional unresolved claims stay
            // full, and verbose rendering receives the unprojected full IDs.
            push_field(
                document,
                context,
                CARD_INDENT,
                label,
                CARD_LABEL_WIDTH,
                reference,
                Token::Reference,
            );
        }
    }
}
