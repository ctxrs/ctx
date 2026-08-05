use serde_json::Value;

use crate::{
    transcript::shell_quote_arg,
    ui::{
        diagnostic, Action, Diagnostic, DiagnosticLevel, Document, Line, RenderContext, Span, Token,
    },
};

use super::human::{
    display_width, push_action, push_field, push_heading, push_prefixed, push_wrapped, short_id,
};

const CARD_INDENT: usize = 3;
const CARD_LABEL_WIDTH: usize = 7;
const VERBOSE_LABEL_WIDTH: usize = 16;

pub(in crate::commands::source_index) fn render_search_not_ready_document(
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
    render_results_heading(&mut document, value, results.len(), context);

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

fn render_results_heading(
    document: &mut Document,
    value: &Value,
    result_count: usize,
    context: &RenderContext,
) {
    let outcome = format!(
        "{result_count} {}",
        if result_count == 1 {
            "result"
        } else {
            "results"
        }
    );
    let order = "relevance order";
    let includes_subagents =
        value["filters"]["include_subagents"] == true && value["filters"]["primary_only"] != true;
    let scope = if includes_subagents {
        "primary + subagent sessions"
    } else {
        "primary sessions"
    };
    let separator = if context.unicode() { " · " } else { " | " };
    let width = display_width(&outcome)
        .saturating_add(display_width(separator).saturating_mul(2))
        .saturating_add(display_width(order))
        .saturating_add(display_width(scope));

    if context
        .content_width()
        .is_none_or(|available| width <= available)
    {
        document.push_line(
            Line::new()
                .with(Span::new(outcome, Token::Heading))
                .with(Span::new(separator, Token::Label))
                .with(Span::new(order, Token::Label))
                .with(Span::new(separator, Token::Label))
                .with(Span::new(scope, Token::Label)),
        );
    } else {
        push_heading(document, &outcome, Token::Heading);
        push_wrapped(document, context, 2, order, Token::Label);
        push_wrapped(document, context, 2, scope, Token::Label);
    }
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
    let event_id = short_id(event_id);
    let (time, time_token) = timestamp
        .filter(|timestamp| !timestamp.is_empty())
        .map_or(("time unavailable", Token::Label), |timestamp| {
            (timestamp, Token::Text)
        });
    let separator = if context.unicode() { " · " } else { " | " };
    let prefix_width = CARD_INDENT
        .saturating_add(CARD_LABEL_WIDTH)
        .saturating_add(2);
    let combined_width = prefix_width
        .saturating_add(display_width(&event_id))
        .saturating_add(display_width(separator))
        .saturating_add(display_width(time));

    if context
        .content_width()
        .is_none_or(|available| combined_width <= available)
    {
        document.push_line(
            Line::new()
                .with(Span::text(" ".repeat(CARD_INDENT)))
                .with(Span::new("Event", Token::Label))
                .with(Span::text(" ".repeat(
                    CARD_LABEL_WIDTH.saturating_sub(display_width("Event")),
                )))
                .with(Span::text("  "))
                .with(Span::new(event_id, Token::Reference))
                .with(Span::new(separator, Token::Label))
                .with(Span::new(time, time_token)),
        );
    } else {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Event",
            CARD_LABEL_WIDTH,
            &event_id,
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
    let workspace = result["workspace"]
        .as_str()
        .filter(|value| !value.is_empty());
    let cwd = result["cwd"].as_str().filter(|value| !value.is_empty());
    if let Some(workspace) = workspace {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Workspace",
            VERBOSE_LABEL_WIDTH,
            workspace,
            Token::Text,
        );
    }
    if let Some(cwd) = cwd.filter(|cwd| Some(*cwd) != workspace) {
        push_field(
            document,
            context,
            CARD_INDENT,
            "CWD",
            VERBOSE_LABEL_WIDTH,
            cwd,
            Token::Text,
        );
    }
    if let Some(branch) = result["branch"].as_str().filter(|value| !value.is_empty()) {
        push_field(
            document,
            context,
            CARD_INDENT,
            "Branch",
            VERBOSE_LABEL_WIDTH,
            branch,
            Token::Text,
        );
    }
    render_agent_field(document, context, result);
    render_lineage_fields(document, context, result);
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

fn render_agent_field(document: &mut Document, context: &RenderContext, result: &Value) {
    let scope = result["is_primary"]
        .as_bool()
        .map(|is_primary| if is_primary { "primary" } else { "subagent" });
    let agent_type = result["agent_type"]
        .as_str()
        .filter(|value| !value.is_empty())
        .filter(|agent_type| Some(*agent_type) != scope);
    let agent = match (scope, agent_type) {
        (Some(scope), Some(agent_type)) => {
            let separator = if context.unicode() { " · " } else { " | " };
            format!("{scope}{separator}{agent_type}")
        }
        (Some(scope), None) => scope.to_owned(),
        (None, Some(agent_type)) => agent_type.to_owned(),
        (None, None) => return,
    };
    push_field(
        document,
        context,
        CARD_INDENT,
        "Agent",
        VERBOSE_LABEL_WIDTH,
        &agent,
        Token::Text,
    );
}

fn render_lineage_fields(document: &mut Document, context: &RenderContext, result: &Value) {
    let direct = result["ctx_session_id"].as_str();
    let parent = result["parent_ctx_session_id"]
        .as_str()
        .filter(|parent| Some(*parent) != direct);
    let root = result["root_ctx_session_id"]
        .as_str()
        .filter(|root| Some(*root) != direct);
    match (parent, root) {
        (Some(parent), Some(root)) if parent == root => push_field(
            document,
            context,
            CARD_INDENT,
            "Parent / root",
            VERBOSE_LABEL_WIDTH,
            parent,
            Token::Reference,
        ),
        (parent, root) => {
            if let Some(parent) = parent {
                push_field(
                    document,
                    context,
                    CARD_INDENT,
                    "Parent",
                    VERBOSE_LABEL_WIDTH,
                    parent,
                    Token::Reference,
                );
            }
            if let Some(root) = root {
                push_field(
                    document,
                    context,
                    CARD_INDENT,
                    "Root",
                    VERBOSE_LABEL_WIDTH,
                    root,
                    Token::Reference,
                );
            }
        }
    }
}
