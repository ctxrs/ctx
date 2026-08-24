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
    #[command(about = "Register a named Claude or Codex home")]
    Add {
        #[arg(help = "Stable local name, for example personal or work")]
        name: String,
        #[arg(long, value_parser = crate::parse_provider_arg, hide_possible_values = true)]
        provider: crate::ProviderArg,
        #[arg(long, value_name = "DIRECTORY", help = "Provider home directory")]
        root: PathBuf,
        #[arg(
            long = "source-group",
            value_name = "GROUP",
            help = "Optional search group, for example personal or work"
        )]
        source_group: Option<String>,
    },
    #[command(about = "Remove a named provider home")]
    Remove {
        #[arg(help = "Configured provider-root name")]
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
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        sources: SourcesArgs,
    }

    #[test]
    fn sources_add_accepts_source_group_and_rejects_the_unreleased_scope_spelling() {
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
                source_group: Some(ref group),
                ..
            }) if group == "work"
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
    }
}
