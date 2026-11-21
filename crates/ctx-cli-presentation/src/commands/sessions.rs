use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::{
    analytics::{ClientOperationDraft, RenderFormat, ShowTelemetry, TargetKind},
    local_usage::CliUsage,
    output::JsonOutputFormat,
    provider_args::ProviderArg,
    ui::Ui,
};

pub use ctx_history_cli::{DEFAULT_SESSIONS_LIMIT, MAX_SESSIONS_LIMIT};

#[derive(Debug, Args)]
pub struct SessionsArgs {
    #[arg(
        long,
        help = "Only include sessions with activity since this relative window (e.g. 2h, 30d), date (YYYY-MM-DD), or RFC3339 timestamp"
    )]
    pub since: Option<String>,
    #[arg(
        long,
        value_parser = crate::parse_provider_arg,
        hide_possible_values = true,
        help = "Filter by provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub provider: Option<ProviderArg>,
    #[arg(
        long,
        help = "Filter by case-insensitive workspace, cwd, or project substring"
    )]
    pub workspace: Option<String>,
    #[arg(long = "no-subagents", help = "Exclude subagent sessions")]
    pub no_subagents: bool,
    #[arg(
        long,
        default_value_t = DEFAULT_SESSIONS_LIMIT,
        value_parser = parse_sessions_limit,
        help = "Maximum sessions to return, from 1 to 1000"
    )]
    pub limit: usize,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

pub fn parse_sessions_limit(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|err| format!("invalid limit: {err}"))?;
    if !(1..=MAX_SESSIONS_LIMIT).contains(&parsed) {
        return Err(format!(
            "limit must be between 1 and {MAX_SESSIONS_LIMIT}, got {parsed}"
        ));
    }
    Ok(parsed)
}

pub fn operation(
    args: &SessionsArgs,
) -> ctx_client_observability::operation_descriptor::CliOperation {
    ctx_client_observability::operation_descriptor::CliOperation::ShowSession(ShowTelemetry {
        target_kind: TargetKind::Session,
        transcript_mode: None,
        output_format: match args.format {
            JsonOutputFormat::Text => RenderFormat::Text,
            JsonOutputFormat::Json => RenderFormat::Json,
        },
        writes_out_file: false,
        provider_lookup: args.provider.is_some(),
        window: None,
        events_returned: None,
    })
}

pub fn run_sessions(
    args: SessionsArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    ctx_history_cli::run_sessions(
        ctx_history_cli::SessionsArgs {
            since: args.since,
            provider: args.provider.map(history_provider),
            workspace: args.workspace,
            no_subagents: args.no_subagents,
            limit: args.limit,
            format: history_format(args.format),
        },
        data_root,
        telemetry,
        local_usage,
        ui,
    )
}

pub fn run_sessions_with_draft(
    args: SessionsArgs,
    data_root: PathBuf,
    draft: Option<&mut ClientOperationDraft>,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    run_sessions(
        args,
        data_root,
        draft.expect("sessions has a telemetry draft").show_mut(),
        local_usage,
        ui,
    )
}

fn history_provider(provider: ProviderArg) -> ctx_history_cli::ProviderArg {
    ctx_history_cli::ProviderArg(ctx_history_cli::HistoryProvider::from(
        provider.capture_provider(),
    ))
}

fn history_format(format: JsonOutputFormat) -> ctx_history_cli::JsonOutputFormat {
    match format {
        JsonOutputFormat::Text => ctx_history_cli::JsonOutputFormat::Text,
        JsonOutputFormat::Json => ctx_history_cli::JsonOutputFormat::Json,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: SessionsArgs,
    }

    #[test]
    fn defaults_and_all_flags_parse() {
        let defaulted = TestCli::try_parse_from(["sessions"]).unwrap().args;
        assert!(defaulted.since.is_none());
        assert!(defaulted.provider.is_none());
        assert!(defaulted.workspace.is_none());
        assert!(!defaulted.no_subagents);
        assert_eq!(defaulted.limit, DEFAULT_SESSIONS_LIMIT);
        assert_eq!(defaulted.format, JsonOutputFormat::Text);

        let parsed = TestCli::try_parse_from([
            "sessions",
            "--since",
            "2h",
            "--provider",
            "codex",
            "--workspace",
            "ctx",
            "--no-subagents",
            "--limit",
            "100",
            "--format",
            "json",
        ])
        .unwrap()
        .args;
        assert_eq!(parsed.since.as_deref(), Some("2h"));
        assert_eq!(
            parsed.provider.map(|p| p.capture_provider()),
            Some(ctx_history_core::CaptureProvider::Codex)
        );
        assert_eq!(parsed.workspace.as_deref(), Some("ctx"));
        assert!(parsed.no_subagents);
        assert_eq!(parsed.limit, 100);
        assert_eq!(parsed.format, JsonOutputFormat::Json);
    }

    #[test]
    fn limit_bounds_and_invalid_json_alias_are_rejected() {
        assert!(TestCli::try_parse_from(["sessions", "--limit", "0"]).is_err());
        assert!(TestCli::try_parse_from(["sessions", "--limit", "1001"]).is_err());
        assert!(TestCli::try_parse_from(["sessions", "--json"]).is_err());
        assert_eq!(
            TestCli::try_parse_from(["sessions", "--limit", "1"])
                .unwrap()
                .args
                .limit,
            1
        );
        assert_eq!(
            TestCli::try_parse_from(["sessions", "--limit", "1000"])
                .unwrap()
                .args
                .limit,
            1000
        );
    }

    #[test]
    fn help_mentions_documented_flags() {
        let help = TestCli::try_parse_from(["sessions", "--help"])
            .unwrap_err()
            .to_string();
        for expected in [
            "--since",
            "--provider",
            "--workspace",
            "--no-subagents",
            "--limit",
            "--format",
        ] {
            assert!(help.contains(expected), "missing {expected} in:\n{help}");
        }
    }
}
