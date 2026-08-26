use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    analytics::{count_bucket, SourcesTelemetry},
    local_usage::{CliUsage, ResultObservationAction},
    output::JsonOutputFormat,
};

#[derive(Debug, Args, Clone)]
pub struct SourcesArgs {
    #[command(subcommand)]
    pub command: Option<SourcesCommand>,
    #[arg(long, global = true, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(
        long,
        value_parser = crate::parse_provider_arg,
        hide_possible_values = true,
        help = "Show sources for one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub provider: Option<crate::ProviderArg>,
    #[arg(long, help = "Show every supported provider location")]
    pub all: bool,
    #[arg(long, help = "Show missing locations for every known provider")]
    pub show_missing: bool,
}

#[derive(Debug, Subcommand, Clone)]
pub enum SourcesCommand {
    #[command(about = "Register a named provider history root")]
    Add {
        #[arg(help = "Stable local name, for example personal or work")]
        name: String,
        #[arg(
            long,
            value_parser = crate::parse_provider_arg,
            hide_possible_values = true,
            help = "History provider; configured-root support is provider-specific"
        )]
        provider: crate::ProviderArg,
        #[arg(
            long,
            value_name = "PATH",
            help = "Existing provider history root (file or directory, depending on provider)"
        )]
        root: PathBuf,
        #[arg(
            long = "source-group",
            value_name = "GROUP",
            help = "Optional search group, for example personal or work"
        )]
        source_group: Option<String>,
        #[arg(
            long,
            value_name = "KIND",
            help = "OpenHands layout: current-conversations or legacy-persistence"
        )]
        kind: Option<ctx_history_cli::ProviderRootKind>,
        #[arg(
            long,
            help = "Atomically replace an existing same-provider root; omitting --source-group clears its group"
        )]
        replace: bool,
    },
    #[command(about = "Remove a named provider history root")]
    Remove {
        #[arg(help = "Configured history-root name")]
        name: String,
    },
}

#[derive(Debug, Clone)]
pub struct SourcesEnvironment {
    pub data_root: PathBuf,
    pub home_dir: Option<PathBuf>,
    pub automatic_provider_discovery: bool,
    pub provider_roots: Vec<ctx_history_cli::ProviderRootDefinition>,
}

/// Final-host shell for the sources command. Clap conversion and result delivery
/// remain here; application execution and presentation live in `ctx-history-cli`.
pub fn run_sources(
    args: SourcesArgs,
    environment: SourcesEnvironment,
    telemetry: &mut SourcesTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut ctx_terminal::Ui,
) -> Result<()> {
    debug_assert!(args.command.is_none());
    let SourcesEnvironment {
        data_root,
        home_dir,
        automatic_provider_discovery,
        provider_roots,
    } = environment;
    let request = ctx_history_cli::SourcesRequest {
        provider: args
            .provider
            .map(|provider| provider.capture_provider().into()),
        all: args.all,
        show_missing: args.show_missing,
        format: match args.format {
            JsonOutputFormat::Text => ctx_history_cli::OutputFormat::Text,
            JsonOutputFormat::Json => ctx_history_cli::OutputFormat::Json,
        },
    };
    let observation = ctx_history_cli::run_sources(
        request,
        &data_root,
        home_dir,
        automatic_provider_discovery,
        provider_roots,
        |observation| {
            telemetry.providers_detected = Some(count_bucket(observation.providers_detected));
            telemetry.providers_existing = Some(count_bucket(observation.providers_existing));
            telemetry.providers_importable = Some(count_bucket(observation.providers_importable));
        },
        ui,
    )?;
    local_usage.set_result_observation(
        ResultObservationAction::Sources,
        observation.result_count,
        observation.content_bytes,
    );
    local_usage.set_measured_output_bytes(observation.output_bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{error::ErrorKind, CommandFactory, Parser};

    use super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        sources: SourcesArgs,
    }

    #[test]
    fn sources_add_accepts_provider_specific_history_roots_and_source_groups() {
        let parsed = TestCli::try_parse_from([
            "ctx",
            "add",
            "personal",
            "--provider",
            "claude",
            "--root",
            "/tmp/claude",
            "--source-group",
            "work",
        ])
        .unwrap();
        assert!(matches!(
            parsed.sources.command,
            Some(SourcesCommand::Add {
                provider,
                root,
                source_group: Some(ref group),
                kind: None,
                replace: false,
                ..
            }) if provider.capture_provider() == ctx_history_core::CaptureProvider::Claude
                && root == std::path::Path::new("/tmp/claude")
                && group == "work"
        ));

        let error = TestCli::try_parse_from([
            "ctx",
            "add",
            "personal",
            "--provider",
            "claude",
            "--root",
            "/tmp/claude",
            "--scope",
            "work",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("--scope"));

        let openhands = TestCli::try_parse_from([
            "ctx",
            "add",
            "openhands-current",
            "--provider",
            "openhands",
            "--root",
            "/tmp/openhands",
            "--kind",
            "current-conversations",
        ])
        .unwrap();
        assert!(matches!(
            openhands.sources.command,
            Some(SourcesCommand::Add {
                kind: Some(ctx_history_cli::ProviderRootKind::OpenHandsCurrentConversations),
                ..
            })
        ));

        assert!(TestCli::try_parse_from([
            "ctx",
            "add",
            "openhands-current",
            "--provider",
            "openhands",
            "--root",
            "/tmp/openhands",
            "--kind",
            "Current-Conversations",
        ])
        .is_err());
    }

    #[test]
    fn sources_add_replace_parses_complete_group_replacement_semantics() {
        let cleared = TestCli::try_parse_from([
            "ctx",
            "add",
            "work",
            "--provider",
            "claude",
            "--root",
            "/tmp/history",
            "--replace",
        ])
        .unwrap();
        assert!(matches!(
            cleared.sources.command,
            Some(SourcesCommand::Add {
                source_group: None,
                kind: None,
                replace: true,
                ..
            })
        ));

        let set = TestCli::try_parse_from([
            "ctx",
            "add",
            "work",
            "--provider",
            "claude",
            "--root",
            "/tmp/history",
            "--source-group",
            "team",
            "--replace",
        ])
        .unwrap();
        assert!(matches!(
            set.sources.command,
            Some(SourcesCommand::Add {
                source_group: Some(ref group),
                kind: None,
                replace: true,
                ..
            }) if group == "team"
        ));
    }

    #[test]
    fn sources_add_replace_still_requires_provider_and_root() {
        let provider_error =
            TestCli::try_parse_from(["ctx", "add", "work", "--root", "/tmp/history", "--replace"])
                .unwrap_err();
        assert_eq!(provider_error.kind(), ErrorKind::MissingRequiredArgument);

        let root_error =
            TestCli::try_parse_from(["ctx", "add", "work", "--provider", "claude", "--replace"])
                .unwrap_err();
        assert_eq!(root_error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn sources_mutation_help_is_provider_neutral_and_path_kind_aware() {
        let mut command = TestCli::command();
        let add_help = command
            .find_subcommand_mut("add")
            .unwrap()
            .render_long_help()
            .to_string();
        let normalized_help = add_help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            add_help.contains("named provider history root"),
            "{add_help}"
        );
        assert!(add_help.contains("--root <PATH>"), "{add_help}");
        assert!(
            add_help.contains("file or directory, depending on provider"),
            "{add_help}"
        );
        assert!(!add_help.contains("Claude or Codex home"), "{add_help}");
        assert!(!add_help.contains("home directory"), "{add_help}");
        assert!(add_help.contains("--replace"), "{add_help}");
        assert!(
            normalized_help.contains("omitting --source-group clears its group"),
            "{add_help}"
        );
        assert!(command.find_subcommand("update").is_none());
    }
}
