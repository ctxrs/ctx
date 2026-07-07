use std::{fs, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Command, CommandFactory, Subcommand};
use serde_json::{json, Value};

use crate::Cli;

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
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DocsSearchArgs {
    pub query: String,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DocsShowArgs {
    pub id: String,
    #[arg(long, value_enum, default_value_t = DocsFormat::Markdown)]
    pub format: DocsFormat,
    #[arg(long)]
    pub json: bool,
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
            Some(DocsCommand::List(args)) => args.json,
            Some(DocsCommand::Search(args)) => args.json,
            Some(DocsCommand::Show(args)) => args.json || args.format == DocsFormat::Json,
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
        id: "upgrade",
        title: "Upgrade",
        audience: "human-agent",
        summary: "Managed upgrades, background auto-upgrade behavior, and local state.",
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
        tags: &["storage", "privacy", "upgrade"],
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

pub fn run(args: DocsArgs) -> Result<()> {
    match args.command {
        Some(DocsCommand::List(args)) => list_docs(args.json),
        Some(DocsCommand::Search(args)) => search_docs(&args.query, args.limit, args.json),
        Some(DocsCommand::Show(args)) => show_doc(args),
        Some(DocsCommand::Man(args)) => man_docs(args),
        None => list_docs(false),
    }
}

fn list_docs(json_output: bool) -> Result<()> {
    if json_output {
        let topics: Vec<Value> = TOPICS.iter().map(topic_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "topics": topics
            }))?
        );
    } else {
        println!("Embedded ctx docs:");
        for topic in TOPICS {
            println!("  {:<20} {}", topic.id, topic.summary);
        }
        println!();
        println!("Try: ctx docs search \"file path\"");
        println!("Try: ctx docs show search");
    }
    Ok(())
}

fn search_docs(query: &str, limit: usize, json_output: bool) -> Result<()> {
    let terms = docs_query_terms(query);
    let mut results: Vec<(usize, &DocTopic)> = TOPICS
        .iter()
        .filter_map(|topic| {
            let score = score_doc_topic(topic, &terms);
            (score >= docs_min_score(&terms)).then_some((score, topic))
        })
        .collect();
    results.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.id.cmp(right.1.id)));
    results.truncate(limit.max(1));
    if json_output {
        let rows: Vec<Value> = results
            .iter()
            .map(|(score, topic)| {
                let mut value = topic_json(topic);
                value["score"] = json!(score);
                value
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "query": query,
                "results": rows,
                "suggested_next_commands": docs_search_suggestions(query, rows.is_empty())
            }))?
        );
    } else if results.is_empty() {
        println!("no docs matched");
        for command in docs_search_suggestions(query, true) {
            println!("next: {command}");
        }
    } else {
        for (index, (score, topic)) in results.iter().enumerate() {
            println!("{}. {} - {}", index + 1, topic.id, topic.title);
            println!("   score {score} | {}", topic.summary);
            println!("   inspect: ctx docs show {}", topic.id);
        }
    }
    Ok(())
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

fn show_doc(args: DocsShowArgs) -> Result<()> {
    let topic = TOPICS
        .iter()
        .find(|topic| topic.id == args.id)
        .ok_or_else(|| unknown_doc_topic_error(&args.id))?;
    let body = if args.json || args.format == DocsFormat::Json {
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
    } else {
        println!("{body}");
    }
    Ok(())
}

fn man_docs(args: DocsManArgs) -> Result<()> {
    if let Some(page) = args.print {
        let (_, command) = man_page(&page)?;
        let mut out = Vec::new();
        clap_mangen::Man::new(command).render(&mut out)?;
        print!("{}", String::from_utf8(out)?);
        return Ok(());
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
    println!("wrote ctx man pages to {}", out_dir.display());
    Ok(())
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
