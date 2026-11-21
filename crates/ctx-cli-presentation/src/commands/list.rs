use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    analytics::{RenderFormat, ShowTelemetry, TargetKind},
    local_usage::CliUsage,
    ui::Ui,
};

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(subcommand)]
    pub target: ListTarget,
}

#[derive(Debug, Subcommand)]
pub enum ListTarget {
    #[command(about = "List deterministic Core event pages")]
    Events(Box<ListEventsArgs>),
}

pub mod events {
    use clap::{Args, ValueEnum};

    #[derive(Debug, Args)]
    pub struct ListEventsArgs {
        #[arg(
            long,
            requires = "until",
            help = "Inclusive millisecond-aligned absolute RFC3339 lower bound"
        )]
        pub since: Option<String>,
        #[arg(
            long,
            requires = "since",
            help = "Exclusive millisecond-aligned absolute RFC3339 upper bound"
        )]
        pub until: Option<String>,
        #[arg(
            long,
            help = "Filter by exact provider; repeat to select more than one"
        )]
        pub provider: Vec<String>,
        #[arg(long, help = "Filter by exact public ctx source UUID")]
        pub source: Option<String>,
        #[arg(
            long = "history-source",
            help = "Filter custom history source as provider-key/source-id"
        )]
        pub history_source: Option<String>,
        #[arg(long = "provider-key", help = "Filter by custom history provider key")]
        pub provider_key: Option<String>,
        #[arg(long = "source-id", help = "Filter by custom history source ID")]
        pub source_id: Option<String>,
        #[arg(long = "source-format", help = "Filter by exact indexed source format")]
        pub source_format: Option<String>,
        #[arg(
            long = "provider-session",
            help = "Filter by exact provider-native session ID"
        )]
        pub provider_session: Option<String>,
        #[arg(long, help = "Filter by exact public ctx session UUID")]
        pub session: Option<String>,
        #[arg(
            long = "parent-session",
            help = "Filter by exact public parent ctx session UUID"
        )]
        pub parent_session: Option<String>,
        #[arg(
            long = "root-session",
            help = "Filter by exact public root ctx session UUID"
        )]
        pub root_session: Option<String>,
        #[arg(long, help = "Filter by exact branch")]
        pub branch: Option<String>,
        #[arg(long, help = "Filter by case-insensitive workspace or cwd substring")]
        pub workspace: Option<String>,
        #[arg(
            long = "event-type",
            help = "Filter by exact event type, including provider-defined values"
        )]
        pub event_type: Option<String>,
        #[arg(long, help = "Filter by exact role")]
        pub role: Option<String>,
        #[arg(long, value_enum, default_value_t = EventQueryScope::All)]
        pub scope: EventQueryScope,
        #[arg(long, help = "Filter by case-insensitive touched-file substring")]
        pub file: Option<String>,
        #[arg(long, value_enum, default_value_t = EventQueryDirection::Ascending)]
        pub direction: EventQueryDirection,
        #[arg(long, help = "Resume from an opaque cursor returned by a prior page")]
        pub cursor: Option<String>,
        #[arg(long, default_value_t = ctx_history_cli::DEFAULT_EVENT_QUERY_LIMIT, help = "Maximum events returned across the complete invocation")]
        pub limit: u64,
        #[arg(long, value_enum, default_value_t = EventContentProjectionArg::Full)]
        pub content: EventContentProjectionArg,
        #[arg(long, value_enum, default_value_t = EventQueryFormat::Json)]
        pub format: EventQueryFormat,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    pub enum EventQueryFormat {
        Json,
        Jsonl,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    pub enum EventContentProjectionArg {
        Full,
        Text,
        None,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    pub enum EventQueryScope {
        All,
        Primary,
        Subagent,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    pub enum EventQueryDirection {
        Ascending,
        Descending,
    }

    pub use ctx_history_cli::{
        decode_cursor, event_query_error_value, event_range_page_value,
        list_events_selection as selection, mcp_event_query_core_record_bytes, render_event,
        validated_event_limit as validated_limit, EventContentProjection, EventQueryError,
        EventQueryWireRequest,
    };
}

pub use events::{EventQueryFormat, ListEventsArgs};

pub fn operation(args: &ListArgs) -> ctx_client_observability::operation_descriptor::CliOperation {
    match &args.target {
        ListTarget::Events(args) => {
            ctx_client_observability::operation_descriptor::CliOperation::ShowEvent(ShowTelemetry {
                target_kind: TargetKind::Events,
                transcript_mode: None,
                output_format: match args.format {
                    EventQueryFormat::Json => RenderFormat::Json,
                    EventQueryFormat::Jsonl => RenderFormat::Jsonl,
                },
                writes_out_file: false,
                provider_lookup: !args.provider.is_empty(),
                window: None,
                events_returned: None,
            })
        }
    }
}

pub fn run_list(
    args: ListArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    match args.target {
        ListTarget::Events(args) => {
            ctx_history_cli::run_list_events(adapt(*args), data_root, telemetry, local_usage, ui)
        }
    }
}

fn adapt(args: ListEventsArgs) -> ctx_history_cli::ListEventsArgs {
    ctx_history_cli::ListEventsArgs {
        since: args.since,
        until: args.until,
        provider: args.provider,
        source: args.source,
        history_source: args.history_source,
        provider_key: args.provider_key,
        source_id: args.source_id,
        source_format: args.source_format,
        provider_session: args.provider_session,
        session: args.session,
        parent_session: args.parent_session,
        root_session: args.root_session,
        branch: args.branch,
        workspace: args.workspace,
        event_type: args.event_type,
        role: args.role,
        scope: match args.scope {
            events::EventQueryScope::All => ctx_history_cli::EventQueryScope::All,
            events::EventQueryScope::Primary => ctx_history_cli::EventQueryScope::Primary,
            events::EventQueryScope::Subagent => ctx_history_cli::EventQueryScope::Subagent,
        },
        file: args.file,
        direction: match args.direction {
            events::EventQueryDirection::Ascending => {
                ctx_history_cli::EventQueryDirection::Ascending
            }
            events::EventQueryDirection::Descending => {
                ctx_history_cli::EventQueryDirection::Descending
            }
        },
        cursor: args.cursor,
        limit: args.limit,
        content: match args.content {
            events::EventContentProjectionArg::Full => {
                ctx_history_cli::EventContentProjectionArg::Full
            }
            events::EventContentProjectionArg::Text => {
                ctx_history_cli::EventContentProjectionArg::Text
            }
            events::EventContentProjectionArg::None => {
                ctx_history_cli::EventContentProjectionArg::None
            }
        },
        format: match args.format {
            events::EventQueryFormat::Json => ctx_history_cli::EventQueryFormat::Json,
            events::EventQueryFormat::Jsonl => ctx_history_cli::EventQueryFormat::Jsonl,
        },
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: ListTarget,
    }

    fn parse_events(arguments: &[&str]) -> ListEventsArgs {
        let arguments = std::iter::once(arguments[0]).chain(arguments.iter().copied().skip(2));
        let cli = TestCli::try_parse_from(arguments).expect("event query arguments should parse");
        let ListTarget::Events(events) = cli.command;
        *events
    }

    #[test]
    fn all_domain_is_explicit_and_defaults_when_omitted() {
        let defaulted = parse_events(&["ctx", "list", "events"]);
        assert!(defaulted.since.is_none() && defaulted.until.is_none());
        assert_eq!(defaulted.limit, ctx_history_cli::DEFAULT_EVENT_QUERY_LIMIT);
    }

    #[test]
    fn wire_receipt_uses_the_normalized_selection() {
        let args = adapt(parse_events(&[
            "ctx",
            "list",
            "events",
            "--since",
            "2026-08-01T00:00:00+00:00",
            "--until",
            "2026-08-02T00:00:00Z",
            "--provider",
            " codex ",
            "--provider",
            "codex",
            "--workspace",
            " CTX ",
            "--scope",
            "subagent",
            "--direction",
            "descending",
        ]));
        let content = args.content.into();
        let selection = ctx_history_cli::list_events_selection_from_request(
            ctx_history_cli::ListEventsRequest::from(args),
        )
        .unwrap();
        let request =
            ctx_history_cli::EventQueryWireRequest::from_selection(&selection, content, 10);

        assert_eq!(request.domain["range"]["since"], "2026-08-01T00:00:00.000Z");
        assert_eq!(
            request.filters,
            json!({"providers": ["codex"], "workspace": "ctx", "scope": "subagent"})
        );
        assert_eq!(request.direction, "descending");
    }

    #[test]
    fn unreleased_aliases_and_page_budget_flags_are_rejected() {
        for flag in [
            "--all",
            "--parent",
            "--root",
            "--max-items",
            "--page-items",
            "--max-bytes",
            "--byte-budget",
        ] {
            assert!(
                TestCli::try_parse_from(["ctx", "events", flag]).is_err(),
                "unexpectedly accepted {flag}"
            );
        }
    }

    #[test]
    fn removed_agent_type_filter_is_rejected() {
        assert!(TestCli::try_parse_from(["ctx", "events", "--agent-type", "subagent"]).is_err());
    }

    #[test]
    fn every_core_filter_and_canonical_relationship_flag_maps_to_selection() {
        let id = "01234567-89ab-4def-8123-456789abcdef";
        let args = adapt(parse_events(&[
            "ctx",
            "list",
            "events",
            "--provider",
            "codex",
            "--provider",
            "claude",
            "--source",
            id,
            "--history-source",
            "plugin/source",
            "--provider-key",
            "plugin",
            "--source-id",
            "source",
            "--source-format",
            "future-format",
            "--provider-session",
            "native-session",
            "--session",
            id,
            "--parent-session",
            id,
            "--root-session",
            id,
            "--branch",
            "main",
            "--workspace",
            "workspace",
            "--event-type",
            "future-event",
            "--role",
            "assistant",
            "--scope",
            "subagent",
            "--file",
            "src/lib.rs",
            "--direction",
            "descending",
        ]));
        let selection = ctx_history_cli::list_events_selection_from_request(
            ctx_history_cli::ListEventsRequest::from(args),
        )
        .unwrap();
        let filters = selection.filters();
        assert_eq!(filters.providers, ["claude", "codex"]);
        assert_eq!(filters.source_identity.unwrap().to_string(), id);
        assert_eq!(filters.history_source.as_deref(), Some("plugin/source"));
        assert_eq!(format!("{:?}", filters.scope), "Subagent");
        assert_eq!(format!("{:?}", filters.direction), "Descending");
    }

    #[test]
    fn help_exposes_only_the_compact_list_events_route() {
        let help = TestCli::try_parse_from(["ctx", "events", "--help"])
            .unwrap_err()
            .to_string();
        for expected in ["--since", "--until", "--parent-session", "--root-session"] {
            assert!(help.contains(expected), "missing {expected} from help");
        }
        for removed in [
            "--all",
            "--parent ",
            "--root ",
            "--max-items",
            "--page-items",
            "--max-bytes",
            "--byte-budget",
            "--agent-type",
        ] {
            assert!(!help.contains(removed), "unexpected {removed} in help");
        }
    }
}
