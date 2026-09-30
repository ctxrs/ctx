use anyhow::{ensure, Result};
use clap::{parser::ValueSource, ArgMatches};
use ctx_agent_integrations::tool_backend::{
    ToolSearchBackend, ToolSearchContentScope, ToolSearchRequest,
};
use ctx_history_server::{Citation, CitationKind};

use crate::{
    commands::{
        search::{CliRefreshArg, ContentScopeArg, SearchBackendArg},
        show::{ShowArgs, ShowTarget},
    },
    output::OutputFormat,
    transcript::TranscriptMode,
    unified_search::{ScopedSearchArgs, SearchScope},
};

const FILTER_HELP: &str = "shared search supports a lexical query and --limit; local provider, workspace, session, content, and semantic filters are not supported";

pub(super) fn select_cli_mode(args: &mut ShowArgs, matches: &ArgMatches) {
    // The shared CLI parser's lite default belongs to local transcripts. Only
    // replace that default; an explicit lite/full request must fail visibly.
    if let ShowTarget::Session(session) = &mut args.target {
        let mode_source = matches
            .subcommand_matches("show")
            .and_then(|show| show.subcommand_matches("session"))
            .and_then(|session| session.value_source("mode"));
        if mode_source == Some(ValueSource::DefaultValue) {
            session.mode = TranscriptMode::Log;
        }
    }
}

pub(super) fn citation(value: &str, kind: CitationKind, collection: &str) -> Result<()> {
    // Keep the wire format with its owner. Do not include supplied selector
    // bytes or a decoder's error payload in a terminal/MCP failure.
    let parsed = Citation::parse(value)
        .map_err(|_| anyhow::anyhow!("an exact shared history citation is required"))?;
    ensure!(
        parsed.kind == kind,
        "shared citation has the wrong event/session kind"
    );
    ensure!(
        parsed.collection == collection,
        "shared citation belongs to another collection"
    );
    Ok(())
}

pub(super) fn cli_search(args: &ScopedSearchArgs) -> Result<()> {
    ensure!(
        args.scope == SearchScope::History && args.graph_db.is_none(),
        "--server searches shared history; graph search remains local"
    );
    ensure!(
        args.query.as_deref().is_some_and(|q| !q.trim().is_empty()),
        "shared search requires a query"
    );
    ensure!(
        (1..=100).contains(&args.limit),
        "shared search --limit must be 1..100"
    );
    let unsupported = [
        ("--term", !args.term.is_empty()),
        ("--provider", args.provider.is_some()),
        ("--history-source", args.history_source.is_some()),
        ("--provider-key", args.provider_key.is_some()),
        ("--source-id", args.source_id.is_some()),
        ("--source-format", args.source_format.is_some()),
        ("--source-root", !args.source_roots.is_empty()),
        ("--source-group", !args.source_groups.is_empty()),
        ("--workspace", args.workspace.is_some()),
        ("--since", args.since.is_some()),
        ("--primary-only", args.primary_only),
        ("--event-type", args.event_type.is_some()),
        ("--file", args.file.is_some()),
        ("--session", args.session.is_some()),
        ("--exclude-session", !args.exclude_sessions.is_empty()),
        (
            "--content-scope",
            !matches!(args.content_scope, None | Some(ContentScopeArg::All)),
        ),
        (
            "--backend",
            !matches!(args.backend, None | Some(SearchBackendArg::Lexical)),
        ),
        ("--semantic-weight", args.semantic_weight != 0.35),
    ]
    .into_iter()
    .filter_map(|(flag, supplied)| supplied.then_some(flag))
    .collect::<Vec<_>>();
    ensure!(
        unsupported.is_empty(),
        "{} not supported with --server; use one lexical query and --limit",
        unsupported.join(", ")
    );
    ensure!(
        args.refresh != CliRefreshArg::Wait,
        "shared search reads server coverage; --refresh wait applies only to local history"
    );
    Ok(())
}

pub(super) fn tool_search(args: &ToolSearchRequest) -> Result<()> {
    ensure!(
        !args.query.trim().is_empty() && (1..=100).contains(&args.limit),
        "shared search needs a query and limit 1..100"
    );
    ensure!(
        args.provider.is_none()
            && args.history_source.is_none()
            && args.provider_key.is_none()
            && args.source_id.is_none()
            && args.source_format.is_none()
            && args.source_roots.is_empty()
            && args.source_groups.is_empty()
            && args.workspace.is_none()
            && args.since.is_none()
            && !args.primary_only
            && args.event_type.is_none()
            && args.file.is_none()
            && args.session.is_none()
            && args.content_scope == ToolSearchContentScope::All
            && matches!(args.backend, None | Some(ToolSearchBackend::Lexical))
            && args.semantic_weight == 0.35,
        FILTER_HELP
    );
    Ok(())
}

pub(super) fn cli_show(args: &ShowArgs) -> Result<()> {
    let format = match &args.target {
        ShowTarget::Session(args) => {
            ensure!(
                args.id.is_some(),
                "shared show needs the exact session citation returned by search"
            );
            ensure!(
                args.provider.is_none()
                    && args.provider_session.is_none()
                    && args.provider_key.is_none()
                    && args.source_id.is_none(),
                "shared show uses an exact citation, not a provider selector"
            );
            ensure!(
                args.out.is_none(),
                "shared show writes to stdout; redirect it to save a transcript"
            );
            ensure!(
                args.mode == TranscriptMode::Log,
                "shared sessions support --mode log only; lite/full transcript selection is local-only"
            );
            ensure!(args.max_events != Some(0), "--max-events must be positive");
            args.format
        }
        ShowTarget::Event(args) => {
            ensure!(args.before == 0 && args.after == 0 && args.window.is_none_or(|n| n == 0), "shared event show returns the exact cited event; open its session citation for surrounding events");
            args.format
        }
    };
    ensure!(
        format != OutputFormat::Markdown,
        "shared show supports text, json, or jsonl"
    );
    Ok(())
}
