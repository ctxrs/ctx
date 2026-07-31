use std::{fs, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Command, CommandFactory, Subcommand};
use serde_json::{json, Value};

use crate::{
    analytics::{count_bucket, text_length_bucket, DocTopicId, DocsOperation, DocsTelemetry},
    local_usage::CliUsage,
    output::JsonOutputFormat,
    ui::{
        canonical_human_output_bytes, empty_state, fields, hint, outcome, section, table, Action,
        Document, EmptyState, Field, Hint, Line, Outcome, OutcomeState, RenderContext, Span, Table,
        Token, Ui,
    },
    Cli,
};

#[derive(Debug, Args)]
pub struct DocsArgs {
    #[command(subcommand)]
    pub command: Option<DocsCommand>,
}

#[derive(Debug, Subcommand)]
pub enum DocsCommand {
    #[command(about = "List embedded ctx documentation topics")]
    List(DocsListArgs),
    #[command(about = "Search embedded ctx documentation")]
    Search(DocsSearchArgs),
    #[command(about = "Show one embedded documentation topic")]
    Show(DocsShowArgs),
    #[command(about = "Generate or print ctx man pages")]
    Man(DocsManArgs),
}

#[derive(Debug, Args)]
pub struct DocsListArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct DocsSearchArgs {
    pub query: String,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct DocsShowArgs {
    pub id: String,
    #[arg(long, value_enum, default_value_t = DocsFormat::Markdown)]
    pub format: DocsFormat,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DocsFormat {
    Markdown,
    Text,
    Json,
}

#[derive(Debug, Args)]
pub struct DocsManArgs {
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long)]
    pub print: Option<String>,
}

impl DocsArgs {
    pub fn json_output(&self) -> bool {
        match &self.command {
            Some(DocsCommand::List(args)) => args.format.is_json(),
            Some(DocsCommand::Search(args)) => args.format.is_json(),
            Some(DocsCommand::Show(args)) => args.format == DocsFormat::Json,
            Some(DocsCommand::Man(_)) | None => false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DocTopic {
    id: &'static str,
    title: &'static str,
    audience: &'static str,
    summary: &'static str,
    tags: &'static [&'static str],
    source_path: &'static str,
    body: &'static str,
}

const TOPICS: &[DocTopic] = &[
    DocTopic {
        id: "getting-started",
        title: "Getting Started",
        audience: "human-agent",
        summary: "Install ctx, set up local storage, import history, and run first searches.",
        tags: &["install", "setup", "search"],
        source_path: "docs/getting-started.md",
        body: include_str!("../../../docs/getting-started.md"),
    },
    DocTopic {
        id: "first-10-minutes",
        title: "First 10 Minutes",
        audience: "human-agent",
        summary: "A concise first-run checklist and common failure paths.",
        tags: &["setup", "sources", "troubleshooting"],
        source_path: "docs/first-10-minutes.md",
        body: include_str!("../../../docs/first-10-minutes.md"),
    },
    DocTopic {
        id: "cli-reference",
        title: "CLI Reference",
        audience: "human-agent",
        summary: "Command and option reference for the installed ctx CLI.",
        tags: &["commands", "flags", "reference"],
        source_path: "docs/cli-reference.md",
        body: include_str!("../../../docs/cli-reference.md"),
    },
    DocTopic {
        id: "docs",
        title: "Docs",
        audience: "human-agent",
        summary: "Use embedded ctx docs, local documentation search, and generated man pages.",
        tags: &["docs", "help", "man"],
        source_path: "docs/docs.md",
        body: include_str!("../../../docs/docs.md"),
    },
    DocTopic {
        id: "search",
        title: "Search",
        audience: "agent",
        summary: "Search behavior, filters, result metadata, and agent-readable output.",
        tags: &["search", "filters", "json"],
        source_path: "docs/search.md",
        body: include_str!("../../../docs/search.md"),
    },
    DocTopic {
        id: "sql",
        title: "SQL",
        audience: "agent",
        summary: "Read-only SQL usage, stable view schemas, limits, and examples.",
        tags: &["sql", "sqlite", "views", "advanced"],
        source_path: "docs/sql.md",
        body: include_str!("../../../docs/sql.md"),
    },
    DocTopic {
        id: "mcp",
        title: "MCP",
        audience: "agent",
        summary: "Read-only MCP server tools, behavior, and privacy expectations.",
        tags: &["mcp", "tools", "agents"],
        source_path: "docs/mcp.md",
        body: include_str!("../../../docs/mcp.md"),
    },
    DocTopic {
        id: "mcp-integrations",
        title: "MCP Integrations",
        audience: "human-agent",
        summary: "Install ctx MCP server config for supported coding-agent clients.",
        tags: &["mcp", "integrations", "agents", "install"],
        source_path: "docs/mcp-integrations.md",
        body: include_str!("../../../docs/mcp-integrations.md"),
    },
    DocTopic {
        id: "upgrade",
        title: "Upgrade",
        audience: "human-agent",
        summary: "Managed upgrades, daemon-owned automatic upgrades, and installation state.",
        tags: &["upgrade", "auto-upgrade", "install"],
        source_path: "docs/upgrade.md",
        body: include_str!("../../../docs/upgrade.md"),
    },
    DocTopic {
        id: "unmanaged-installs",
        title: "Package Managers And Unmanaged Installs",
        audience: "human",
        summary: "GitHub release binaries, mise, Homebrew, source builds, and unmanaged install behavior.",
        tags: &["install", "github", "mise", "homebrew", "package-manager"],
        source_path: "docs/unmanaged-installs.md",
        body: include_str!("../../../docs/unmanaged-installs.md"),
    },
    DocTopic {
        id: "agent-usage",
        title: "Agent Usage",
        audience: "agent",
        summary: "How agents should search, inspect, cite, and report local history.",
        tags: &["agents", "citations", "workflow"],
        source_path: "docs/agent-usage.md",
        body: include_str!("../../../docs/agent-usage.md"),
    },
    DocTopic {
        id: "agent-skill-install",
        title: "Agent Skill Install",
        audience: "human",
        summary: "Install the ctx agent-history search skill for supported agents.",
        tags: &["skills", "agents", "install"],
        source_path: "docs/agent-skill-install.md",
        body: include_str!("../../../docs/agent-skill-install.md"),
    },
    DocTopic {
        id: "slash-command-integrations",
        title: "Slash Command Integrations",
        audience: "human-agent",
        summary: "Provider matrix and installer behavior for ctx slash-command entry points.",
        tags: &["integrations", "slash-commands", "skills", "agents"],
        source_path: "docs/slash-command-integrations.md",
        body: include_str!("../../../docs/slash-command-integrations.md"),
    },
    DocTopic {
        id: "sdks",
        title: "SDKs",
        audience: "human-agent",
        summary: "Use experimental in-repo SDKs for ctx agent history search.",
        tags: &["sdk", "agent-history", "contracts"],
        source_path: "docs/sdks.md",
        body: include_str!("../../../docs/sdks.md"),
    },
    DocTopic {
        id: "json-contracts",
        title: "JSON Contracts",
        audience: "agent",
        summary: "Machine-readable JSON output contracts for scripts and integrations.",
        tags: &["json", "contracts", "scripts"],
        source_path: "docs/contracts/json.md",
        body: include_str!("../../../docs/contracts/json.md"),
    },
    DocTopic {
        id: "storage",
        title: "Storage And Privacy",
        audience: "human-agent",
        summary: "Local storage layout, command read/write behavior, privacy, and upgrades.",
        tags: &["storage", "privacy"],
        source_path: "docs/storage.md",
        body: include_str!("../../../docs/storage.md"),
    },
    DocTopic {
        id: "providers",
        title: "Providers",
        audience: "human-agent",
        summary: "Supported local provider imports and fidelity rules.",
        tags: &["providers", "imports"],
        source_path: "docs/providers.md",
        body: include_str!("../../../docs/providers.md"),
    },
    DocTopic {
        id: "custom-history-import-format",
        title: "Custom History Import Format",
        audience: "integrator-agent",
        summary: "ctx-history-jsonl-v1 records, transport, identity, cursors, and import rules.",
        tags: &["providers", "imports", "jsonl", "custom"],
        source_path: "docs/custom-history-import-format.md",
        body: include_str!("../../../docs/custom-history-import-format.md"),
    },
    DocTopic {
        id: "history-source-plugins",
        title: "History Source Plugins",
        audience: "integrator-agent",
        summary: "Local plugin manifests, stdout import, cursor handoff, and adapter shapes.",
        tags: &["providers", "plugins", "imports", "custom"],
        source_path: "docs/history-source-plugins.md",
        body: include_str!("../../../docs/history-source-plugins.md"),
    },
    DocTopic {
        id: "provider-support",
        title: "Provider Support",
        audience: "human-agent",
        summary: "Current Supported provider imports and source formats.",
        tags: &["providers", "matrix"],
        source_path: "docs/provider-support.md",
        body: include_str!("../../../docs/provider-support.md"),
    },
    DocTopic {
        id: "provider-import-policy",
        title: "Provider Import Policy",
        audience: "integrator-agent",
        summary: "Native provider content policy, storage families, and fixture expectations.",
        tags: &["providers", "imports", "policy", "testing"],
        source_path: "docs/provider-import-policy.md",
        body: include_str!("../../../docs/provider-import-policy.md"),
    },
    DocTopic {
        id: "troubleshooting",
        title: "Troubleshooting",
        audience: "human-agent",
        summary: "Common source, freshness, JSON, and store problems.",
        tags: &["troubleshooting", "doctor"],
        source_path: "docs/troubleshooting.md",
        body: include_str!("../../../docs/troubleshooting.md"),
    },
    DocTopic {
        id: "limitations",
        title: "Limitations",
        audience: "human-agent",
        summary: "Provider, import, search, retrieval, and operations limits.",
        tags: &["limits", "scope"],
        source_path: "docs/limitations.md",
        body: include_str!("../../../docs/limitations.md"),
    },
];

pub fn run(
    args: DocsArgs,
    telemetry: &mut DocsTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let output_bytes = match args.command {
        Some(DocsCommand::List(args)) => {
            telemetry.operation = Some(DocsOperation::List);
            list_docs(args.format.is_json(), telemetry, ui)
        }
        Some(DocsCommand::Search(args)) => {
            telemetry.operation = Some(DocsOperation::Search);
            search_docs(
                &args.query,
                args.limit,
                args.format.is_json(),
                telemetry,
                ui,
            )
        }
        Some(DocsCommand::Show(args)) => {
            telemetry.operation = Some(DocsOperation::Show);
            telemetry.writes_output = args.out.is_some();
            show_doc(args, telemetry, ui)
        }
        Some(DocsCommand::Man(args)) => {
            telemetry.operation = Some(if args.print.is_some() {
                DocsOperation::ManPrint
            } else {
                DocsOperation::ManGenerate
            });
            telemetry.writes_output = args.out.is_some();
            man_docs(args, ui)
        }
        None => {
            telemetry.operation = Some(DocsOperation::List);
            telemetry.implicit_list = true;
            list_docs(false, telemetry, ui)
        }
    }?;
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

fn list_docs(json_output: bool, telemetry: &mut DocsTelemetry, ui: &mut Ui) -> Result<usize> {
    telemetry.result_count = Some(count_bucket(TOPICS.len() as u64));
    telemetry.zero_result = Some(TOPICS.is_empty());
    if json_output {
        let topics: Vec<Value> = TOPICS.iter().map(topic_json).collect();
        let output = format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "topics": topics
            }))?
        );
        print!("{output}");
        Ok(output.len())
    } else {
        let document = render_docs_list(ui.stdout_context());
        let output_bytes = canonical_human_output_bytes(render_docs_list);
        ui.write_stdout(&document)?;
        Ok(output_bytes)
    }
}

fn search_docs(
    query: &str,
    limit: usize,
    json_output: bool,
    telemetry: &mut DocsTelemetry,
    ui: &mut Ui,
) -> Result<usize> {
    let terms = docs_query_terms(query);
    telemetry.query_length = Some(text_length_bucket(query.chars().count()));
    telemetry.query_term_count = Some(count_bucket(terms.len() as u64));
    let mut results: Vec<(usize, &DocTopic)> = TOPICS
        .iter()
        .filter_map(|topic| {
            let score = score_doc_topic(topic, &terms);
            (score >= docs_min_score(&terms)).then_some((score, topic))
        })
        .collect();
    results.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.id.cmp(right.1.id)));
    results.truncate(limit.max(1));
    telemetry.result_count = Some(count_bucket(results.len() as u64));
    telemetry.zero_result = Some(results.is_empty());
    if json_output {
        let rows: Vec<Value> = results
            .iter()
            .map(|(score, topic)| {
                let mut value = topic_json(topic);
                value["score"] = json!(score);
                value
            })
            .collect();
        let output = format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "query": query,
                "results": rows,
                "suggested_next_commands": docs_search_suggestions(query, rows.is_empty())
            }))?
        );
        print!("{output}");
        Ok(output.len())
    } else {
        let document = render_docs_search(ui.stdout_context(), query, &results);
        let output_bytes =
            canonical_human_output_bytes(|context| render_docs_search(context, query, &results));
        ui.write_stdout(&document)?;
        Ok(output_bytes)
    }
}

fn render_docs_list(context: &RenderContext) -> Document {
    if TOPICS.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No embedded documentation is available",
                detail: "Use command help for the installed CLI surface.",
                action: Some(Action {
                    command: "ctx --help",
                }),
            },
        );
    }
    let title = format!("{} embedded documentation topics", TOPICS.len());
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Neutral,
            title: &title,
            detail: Some("Search locally or open a topic by its ID."),
        },
    );
    let mut topics = Table::new(["Topic", "Summary"]);
    for topic in TOPICS {
        topics.push_row([topic.id.to_owned(), topic.summary.to_owned()]);
    }
    document.push_blank();
    document.append(section("Topics", table(context, &topics)));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Search the embedded documentation.",
        },
        Some(Action {
            command: "ctx docs search \"file path\"",
        }),
    ));
    document
}

fn render_docs_search(
    context: &RenderContext,
    query: &str,
    results: &[(usize, &DocTopic)],
) -> Document {
    if results.is_empty() {
        let title = format!("No docs matched \"{query}\"");
        return empty_state(
            context,
            EmptyState {
                title: &title,
                detail: "Try a broader term or list every embedded topic.",
                action: Some(Action {
                    command: "ctx docs list",
                }),
            },
        );
    }
    let title = match results.len() {
        1 => format!("1 doc matched \"{query}\""),
        count => format!("{count} docs matched \"{query}\""),
    };
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: &title,
            detail: None,
        },
    );
    let mut matches = Table::new(["Topic", "Title", "Score", "Summary"]);
    for (score, topic) in results {
        matches.push_row([
            topic.id.to_string(),
            topic.title.to_string(),
            score.to_string(),
            topic.summary.to_string(),
        ]);
    }
    document.push_blank();
    document.append(section("Matches", table(context, &matches)));
    let command = format!("ctx docs show {}", results[0].1.id);
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Open the top match.",
        },
        Some(Action { command: &command }),
    ));
    document
}

fn docs_query_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| term.trim().to_ascii_lowercase())
        .filter(|term| !term.is_empty())
        .collect()
}

fn docs_min_score(terms: &[String]) -> usize {
    if terms.is_empty() {
        usize::MAX
    } else {
        terms.len().max(2)
    }
}

fn score_doc_topic(topic: &DocTopic, terms: &[String]) -> usize {
    let haystack = format!(
        "{} {} {} {}",
        topic.id, topic.title, topic.summary, topic.body
    )
    .to_ascii_lowercase();
    let title = topic.title.to_ascii_lowercase();
    terms
        .iter()
        .map(|term| {
            let exact_topic_match = topic.id == term
                || title == *term
                || topic.tags.iter().any(|tag| tag.eq_ignore_ascii_case(term));
            let text_matches = if term.len() >= 3 {
                haystack.matches(term).count()
            } else {
                0
            };
            text_matches + usize::from(exact_topic_match) * 1_000
        })
        .sum()
}

fn docs_search_suggestions(query: &str, no_results: bool) -> Vec<String> {
    if no_results {
        let mut suggestions = vec!["ctx docs list".to_owned()];
        let trimmed = query.trim();
        if !trimmed.is_empty() {
            suggestions.push(format!(
                "ctx docs search {}",
                docs_shell_quote_arg(first_docs_search_term(trimmed))
            ));
        }
        suggestions
    } else {
        Vec::new()
    }
}

fn first_docs_search_term(query: &str) -> &str {
    query.split_whitespace().next().unwrap_or(query)
}

fn docs_shell_quote_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn show_doc(args: DocsShowArgs, telemetry: &mut DocsTelemetry, ui: &mut Ui) -> Result<usize> {
    let Some(topic) = TOPICS.iter().find(|topic| topic.id == args.id) else {
        if args.format == DocsFormat::Json {
            return Err(unknown_doc_topic_error(&args.id));
        }
        let document = render_unknown_doc_topic(ui.stderr_context(), &args.id);
        ui.write_stderr(&document)?;
        return Err(crate::dispatch::rendered_cli_error());
    };
    telemetry.topic = DocTopicId::from_known_id(topic.id);
    telemetry.result_count = Some(count_bucket(1));
    telemetry.zero_result = Some(false);
    let body = if args.format == DocsFormat::Json {
        serde_json::to_string_pretty(&topic_json_with_body(topic))?
    } else {
        match args.format {
            DocsFormat::Markdown => topic.body.to_owned(),
            DocsFormat::Text => markdown_to_text(topic.body),
            DocsFormat::Json => unreachable!(),
        }
    };
    if let Some(path) = args.out {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        Ok(0)
    } else {
        println!("{body}");
        Ok(body.len().saturating_add(1))
    }
}

fn render_unknown_doc_topic(context: &RenderContext, id: &str) -> Document {
    let title = format!("Unknown ctx docs topic: {id}");
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: &title,
            detail: None,
        },
    );

    let suggestions = suggested_doc_topics(id);
    if !suggestions.is_empty() {
        let mut topics = Document::new();
        for topic in suggestions {
            topics.push_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new(topic, Token::Reference)),
            );
        }
        document.push_blank();
        document.append(section("Nearest topics", topics));
    }

    let search = format!(
        "ctx docs search {}",
        docs_shell_quote_arg(first_docs_search_term(id))
    );
    let mut actions = Document::new();
    for command in ["ctx docs list", search.as_str()] {
        actions.push_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(command, Token::Command)),
        );
    }
    document.append(section("Next", actions));
    document
}

fn man_docs(args: DocsManArgs, ui: &mut Ui) -> Result<usize> {
    if let Some(page) = args.print {
        let (_, command) = man_page(&page)?;
        let mut out = Vec::new();
        clap_mangen::Man::new(command).render(&mut out)?;
        let output = String::from_utf8(out)?;
        print!("{output}");
        return Ok(output.len());
    }
    let out_dir = args
        .out
        .ok_or_else(|| anyhow!("ctx docs man requires --out DIR or --print PAGE"))?;
    fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    for (name, command) in man_pages() {
        let path = out_dir.join(format!("{name}.1"));
        let mut out = Vec::new();
        clap_mangen::Man::new(command).render(&mut out)?;
        fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
    }
    let directory = out_dir.display().to_string();
    let mut document = outcome(
        ui.stdout_context(),
        Outcome {
            state: OutcomeState::Success,
            title: "ctx man pages written",
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(
        ui.stdout_context(),
        &[Field::new("Directory", &directory)],
    ));
    let output_bytes = document.render_plain().len();
    ui.write_stdout(&document)?;
    Ok(output_bytes)
}

fn man_page(name: &str) -> Result<(String, Command)> {
    man_pages()
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .ok_or_else(|| unknown_man_page_error(name))
}

fn unknown_doc_topic_error(id: &str) -> anyhow::Error {
    let mut message = format!("unknown ctx docs topic: {id}");
    let suggestions = suggested_doc_topics(id);
    if !suggestions.is_empty() {
        message.push_str("\nnearest topics:");
        for topic in suggestions {
            message.push_str(&format!(" {topic}"));
        }
    }
    message.push_str("\ntry: ctx docs list");
    message.push_str(&format!(
        "\ntry: ctx docs search {}",
        docs_shell_quote_arg(first_docs_search_term(id))
    ));
    anyhow!(message)
}

fn suggested_doc_topics(id: &str) -> Vec<&'static str> {
    let query = id.to_ascii_lowercase();
    let terms = docs_query_terms(id);
    let mut scored: Vec<(usize, &'static str)> = TOPICS
        .iter()
        .filter_map(|topic| {
            let score = score_doc_topic(topic, &terms)
                + common_prefix_len(&query, topic.id)
                + usize::from(topic.id.contains(&query)) * 20;
            (score > 0).then_some((score, topic.id))
        })
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored.truncate(3);
    scored.into_iter().map(|(_, id)| id).collect()
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn unknown_man_page_error(name: &str) -> anyhow::Error {
    anyhow!(
        "unknown ctx man page: {name}\ntry: ctx docs man --print ctx\ntry: ctx docs man --out ./man"
    )
}

fn man_pages() -> Vec<(String, Command)> {
    let root = Cli::command();
    let mut pages = vec![("ctx".to_owned(), root.clone())];
    collect_subcommand_pages("ctx", &root, &mut pages);
    pages
}

fn collect_subcommand_pages(prefix: &str, command: &Command, pages: &mut Vec<(String, Command)>) {
    for subcommand in command.get_subcommands() {
        let page_name = format!("{prefix}-{}", subcommand.get_name());
        let mut page = subcommand.clone();
        let page_name_static: &'static str = Box::leak(page_name.clone().into_boxed_str());
        page = page.name(page_name_static);
        pages.push((page_name.clone(), page.clone()));
        collect_subcommand_pages(&page_name, &page, pages);
    }
}

fn topic_json(topic: &DocTopic) -> Value {
    json!({
        "id": topic.id,
        "title": topic.title,
        "audience": topic.audience,
        "summary": topic.summary,
        "tags": topic.tags,
        "source_path": topic.source_path,
    })
}

fn topic_json_with_body(topic: &DocTopic) -> Value {
    let mut value = topic_json(topic);
    value["schema_version"] = json!(1);
    value["body"] = json!(topic.body);
    value
}

fn markdown_to_text(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            line.trim_start_matches('#')
                .trim_start_matches("- ")
                .trim()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod ui_tests {
    use std::io::Write as _;

    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn docs_list_is_structured_and_responsive() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_docs_list(&context);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.starts_with(&format!("{} embedded documentation topics", TOPICS.len()))
            );
            assert!(rendered.contains("Topics\n"));
            assert!(rendered.contains("ctx docs search \"file path\""));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn docs_search_success_is_outcome_first_and_actionable() {
        let topic = TOPICS.iter().find(|topic| topic.id == "sql").unwrap();
        let context = context(48, ColorMode::Never);
        let document = render_docs_search(&context, "sql", &[(1_000, topic)]);
        let rendered = document.render_plain();
        assert!(rendered.starts_with("✓ 1 doc matched \"sql\"\n"));
        assert!(rendered.contains("Matches\n"));
        assert!(rendered.contains("Next\n  ctx docs show sql\n"));
        assert_fits(&document, &context);
    }

    #[test]
    fn docs_search_empty_state_neutralizes_query_controls() {
        let context = context(48, ColorMode::Never);
        let document = render_docs_search(&context, "missing\u{1b}[31m", &[]);
        let rendered = document.render_plain();
        assert!(rendered.starts_with("No docs matched \"missing\\x1b[31m\"\n"));
        assert!(rendered.contains("Next\n  ctx docs list\n"));
        assert!(!rendered.as_bytes().contains(&0x1b));
        assert_fits(&document, &context);
    }

    #[test]
    fn docs_plain_output_matches_ansi_stripped_output() {
        let context = context(80, ColorMode::Always);
        let document = render_docs_list(&context);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }

    #[test]
    fn unknown_topic_is_a_structured_diagnostic_without_literal_newline_escapes() {
        let context = context(80, ColorMode::Always);
        let document = render_unknown_doc_topic(&context, "cli");
        let plain = document.render_plain();
        assert!(
            plain.starts_with("✗ Unknown ctx docs topic: cli\n"),
            "{plain}"
        );
        assert!(
            plain.contains("Nearest topics\n  cli-reference\n"),
            "{plain}"
        );
        assert!(
            plain.contains("Next\n  ctx docs list\n  ctx docs search cli\n"),
            "{plain}"
        );
        assert_eq!(plain.lines().count(), 9, "{plain}");
        assert!(!plain.contains("\\n"), "{plain}");

        let styled = document.render(&context);
        assert!(styled.as_bytes().contains(&0x1b), "{styled:?}");
        assert_eq!(strip_ansi(&styled), plain);
    }

    #[test]
    fn unknown_topic_neutralizes_user_control_characters() {
        let context = context(80, ColorMode::Never);
        let rendered = render_unknown_doc_topic(&context, "cli\u{1b}[31m").render_plain();
        assert!(rendered.contains("cli\\x1b[31m"), "{rendered}");
        assert!(!rendered.as_bytes().contains(&0x1b), "{rendered:?}");
    }
}
