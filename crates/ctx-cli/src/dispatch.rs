use std::{env, io::Write as _, process::ExitCode, time::Instant};

use anyhow::{Context, Result};
use clap::Parser;
use ctx_history_core::default_data_root;

use crate::{
    analytics::{self, ClientOperationDraft},
    cli::{
        Cli, CommandRoot, DaemonCommand, DaemonTriggerCommandArg, ImportArgs, LocateArgs,
        LocateTarget, ShowArgs, ShowTarget,
    },
    commands::{
        doctor::run_doctor,
        import::{run_import, ProviderRefreshCollector},
        index::run_index,
        locate::run_locate,
        search::run_search,
        setup::run_setup,
        show::run_show,
        sources::run_sources,
        sql::run_sql,
        status::{
            malformed_config_failure, removed_cloud_config_failure, run_status, run_usage_action,
        },
    },
    complete_content,
    config::AppConfig,
    deprecated_controls::DeprecatedControls,
    docs, integrations, local_usage, mcp,
    output::{JsonOutputFormat, OutputFormat, SqlFormat},
    pro, semantic, upgrade,
};

#[derive(Debug, thiserror::Error)]
#[error("JSON error was already rendered")]
struct RenderedJsonError;

#[derive(Debug, thiserror::Error)]
#[error("CLI error was already rendered")]
pub(crate) struct RenderedCliError;

pub(crate) fn rendered_cli_error() -> anyhow::Error {
    RenderedCliError.into()
}

pub(crate) fn run() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is::<RenderedJsonError>() || error.is::<RenderedCliError>() => {
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_cli() -> Result<()> {
    let started = Instant::now();
    let cli = Cli::parse();
    match &cli.command {
        CommandRoot::Pro(args) => args.validate_invocation()?,
        CommandRoot::Referral(args) => args.validate_invocation()?,
        _ => {}
    }
    let deprecated_controls = DeprecatedControls::detect();
    if command_deprecation_warning_eligible(&cli.command) {
        if let Some(warning) = deprecated_controls.warning() {
            eprintln!("{warning}");
        }
    }
    let json_output = command_json_output(&cli.command);
    let usage_control_action = matches!(
        &cli.command,
        CommandRoot::Status(args) if args.usage.modifies_state()
    );
    let quiet = quiet_output(cli.quiet);
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;
    if usage_control_action {
        let CommandRoot::Status(args) = cli.command else {
            unreachable!("usage controls are status commands");
        };
        return run_usage_action(args.usage, &data_root, args.format.is_json(), quiet);
    }
    let daemon_autostart_trigger = command_daemon_autostart_trigger(&cli.command);
    let mut analytics_draft = ClientOperationDraft::from_command(&cli.command, json_output);
    let mut local_usage_draft = local_usage::CliUsage::from_command(&cli.command);
    let mut provider_refreshes = ProviderRefreshCollector::default();
    let mut config =
        match AppConfig::load_with_deprecated_controls(&data_root, &deprecated_controls) {
            Ok(config) => config,
            Err(error)
                if command_is_usage_status_report(&cli.command)
                    && crate::config::is_removed_cloud_mode_error(&error) =>
            {
                return removed_cloud_config_failure(json_output);
            }
            Err(_) if command_is_usage_status_report(&cli.command) => {
                return malformed_config_failure(json_output);
            }
            Err(_) if command_can_report_malformed_config(&cli.command) => {
                // Daemon status reads retained lifecycle/config-reload state. Keep
                // that diagnostic available when the malformed file is itself the
                // reload failure being diagnosed; ordinary commands remain strict.
                let mut fallback = AppConfig::default();
                fallback.analytics.enabled = false;
                fallback.local_usage.enabled =
                    crate::config::resolve_local_usage_control(&data_root).effective_on_startup();
                fallback
            }
            Err(error) => return Err(error),
        };
    if let Some(draft) = analytics_draft.as_mut() {
        draft.set_deprecated_controls(deprecated_controls.nonprivacy_analytics_ids().as_deref());
    }

    let result = match cli.command {
        CommandRoot::Setup(args) => run_setup(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("setup has a telemetry draft")
                .setup_mut(),
            &mut provider_refreshes,
            quiet,
            &mut config,
        ),
        CommandRoot::Status(args) => run_status(
            args,
            data_root.clone(),
            quiet,
            analytics_draft
                .as_mut()
                .expect("status has a telemetry draft")
                .status_mut(),
        ),
        CommandRoot::Index(args) => run_index(
            args,
            data_root.clone(),
            quiet,
            analytics_draft
                .as_mut()
                .expect("index has a telemetry draft")
                .index_mut(),
        ),
        CommandRoot::Sources(args) => run_sources(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("sources has a telemetry draft")
                .sources_mut(),
        ),
        CommandRoot::Import(args) => run_import(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("import has a telemetry draft")
                .import_mut(),
            &mut provider_refreshes,
            &config,
        ),
        CommandRoot::Show(args) => run_show(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("show has a telemetry draft")
                .show_mut(),
        ),
        CommandRoot::Locate(args) => run_locate(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("locate has a telemetry draft")
                .locate_mut(),
        ),
        CommandRoot::Search(args) => run_search(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("search has a telemetry draft")
                .search_mut(),
            &mut provider_refreshes,
            &config,
        ),
        CommandRoot::Pro(args) => pro::run_lifecycle(args, data_root.clone()),
        CommandRoot::Referral(args) => pro::run_referral(args, data_root.clone()),
        CommandRoot::Blame(args) => {
            crate::commands::blame::run(args, data_root.clone(), &mut local_usage_draft)
        }
        CommandRoot::Sql(args) => run_sql(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("sql has a telemetry draft")
                .sql_mut(),
        ),
        CommandRoot::Docs(args) => docs::run(
            args,
            analytics_draft
                .as_mut()
                .expect("docs has a telemetry draft")
                .docs_mut(),
        ),
        CommandRoot::Integrations(args) => integrations::run(
            args,
            analytics_draft
                .as_mut()
                .expect("integrations has a telemetry draft")
                .integration_mut(),
        ),
        CommandRoot::Mcp(args) => mcp::run(args, data_root.clone()),
        CommandRoot::Daemon(args) => semantic::run_daemon_command(args, data_root.clone(), &config),
        CommandRoot::Upgrade(args) => upgrade::run(
            args,
            data_root.clone(),
            config.clone(),
            analytics_draft
                .as_mut()
                .expect("upgrade has a telemetry draft")
                .upgrade_mut(),
        ),
        CommandRoot::Doctor(args) => run_doctor(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("doctor has a telemetry draft")
                .doctor_mut(),
        ),
    };
    let duration = started.elapsed();
    let rendered_error = if let Err(error) = &result {
        if error.is::<RenderedJsonError>() || error.is::<RenderedCliError>() {
            Some(RenderedCliError.into())
        } else if json_output {
            if let Some(error) =
                error.downcast_ref::<ctx_history_capture::complete_content::CompleteContentError>()
            {
                eprintln!(
                    "{}",
                    serde_json::to_string(&complete_content::complete_content_error_json(error))?
                );
                Some(RenderedJsonError.into())
            } else {
                eprintln!("Error: {error:?}");
                Some(RenderedCliError.into())
            }
        } else {
            eprintln!("Error: {error:?}");
            Some(RenderedCliError.into())
        }
    } else {
        None
    };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    if let Some(operation) = local_usage_draft.completed(result.is_ok(), duration) {
        local_usage::record_best_effort(&data_root, config.local_usage.enabled, operation);
    }
    let mut events = provider_refreshes.finish();
    if let Some(draft) = analytics_draft {
        if draft.should_emit() {
            events.push(draft.finish(result.is_ok(), duration));
        }
    }
    analytics::send_batch(&data_root, &config, &events);
    if result.is_ok() {
        if let Some(trigger) = daemon_autostart_trigger {
            semantic::maybe_autostart_daemon(&data_root, &config, trigger);
        }
    }
    if let Some(error) = rendered_error {
        return Err(error);
    }
    result
}

fn command_json_output(command: &CommandRoot) -> bool {
    match command {
        CommandRoot::Setup(args) => args.format.is_json(),
        CommandRoot::Status(args) => args.format.is_json(),
        CommandRoot::Index(args) => args.json_output(),
        CommandRoot::Sources(args) => args.format.is_json(),
        CommandRoot::Import(args) => args.format.is_json(),
        CommandRoot::Show(args) => show_json_output(args),
        CommandRoot::Locate(args) => locate_json_output(args),
        CommandRoot::Search(args) => args.format.is_json(),
        CommandRoot::Pro(args) => args.json_output(),
        CommandRoot::Referral(args) => args.json_output(),
        CommandRoot::Blame(args) => args.json_output(),
        CommandRoot::Sql(args) => args.output_format() == SqlFormat::Json,
        CommandRoot::Docs(args) => args.json_output(),
        CommandRoot::Integrations(args) => args.json_output(),
        CommandRoot::Mcp(_) => false,
        CommandRoot::Daemon(args) => match &args.command {
            DaemonCommand::Run(args) => args.format.is_json(),
            DaemonCommand::Status(args)
            | DaemonCommand::Enable(args)
            | DaemonCommand::Disable(args) => args.format.is_json(),
        },
        CommandRoot::Upgrade(args) => args.json_output(),
        CommandRoot::Doctor(args) => args.format.is_json(),
    }
}

fn show_json_output(args: &ShowArgs) -> bool {
    match &args.target {
        ShowTarget::Session(args) => args.format == OutputFormat::Json,
        ShowTarget::Event(args) => args.format == OutputFormat::Json,
    }
}

fn locate_json_output(args: &LocateArgs) -> bool {
    match &args.target {
        LocateTarget::Session(args) => args.format == JsonOutputFormat::Json,
        LocateTarget::Event(args) => args.format == JsonOutputFormat::Json,
    }
}

fn command_machine_readable_output(command: &CommandRoot, json_output: bool) -> bool {
    if json_output {
        return true;
    }
    match command {
        CommandRoot::Setup(args) => args.progress == crate::progress::ProgressArg::Json,
        CommandRoot::Import(args) => args.progress == crate::progress::ProgressArg::Json,
        CommandRoot::Show(args) => {
            matches!(
                &args.target,
                ShowTarget::Session(args) if args.format == OutputFormat::Jsonl
            ) || matches!(
                &args.target,
                ShowTarget::Event(args) if args.format == OutputFormat::Jsonl
            )
        }
        CommandRoot::Sql(args) => args.output_format() != SqlFormat::Table,
        CommandRoot::Mcp(_) => true,
        _ => false,
    }
}

pub(crate) fn command_deprecation_warning_eligible(command: &CommandRoot) -> bool {
    if command_machine_readable_output(command, command_json_output(command)) {
        return false;
    }
    !matches!(command, CommandRoot::Mcp(_) | CommandRoot::Daemon(_))
}

fn command_daemon_autostart_trigger(command: &CommandRoot) -> Option<DaemonTriggerCommandArg> {
    if command_machine_readable_output(command, command_json_output(command)) {
        return None;
    }
    match command {
        CommandRoot::Import(args) if import_should_autostart_daemon(args) => {
            Some(DaemonTriggerCommandArg::Import)
        }
        _ => None,
    }
}

fn command_can_report_malformed_config(command: &CommandRoot) -> bool {
    matches!(
        command,
        CommandRoot::Daemon(crate::DaemonArgs {
            command: DaemonCommand::Status(_),
        })
    ) || matches!(command, CommandRoot::Mcp(_))
}

fn command_is_usage_status_report(command: &CommandRoot) -> bool {
    matches!(
        command,
        CommandRoot::Status(args) if !args.usage.modifies_state()
    )
}

fn import_should_autostart_daemon(args: &ImportArgs) -> bool {
    !args.no_daemon
        && args.input_format.is_none()
        && args.history_source.is_none()
        && args.history_source_manifest.is_empty()
}

fn quiet_output(flag: bool) -> bool {
    flag || env_truthy("CTX_QUIET")
}

fn env_truthy(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon_autostart_trigger(args: &[&str]) -> Option<DaemonTriggerCommandArg> {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        command_daemon_autostart_trigger(&cli.command)
    }

    #[test]
    fn setup_handoff_is_owned_by_setup_and_machine_import_does_not_autostart() {
        for args in [
            &["setup"][..],
            &["setup", "--format", "json"][..],
            &["setup", "--progress", "json"],
            &["import", "--format", "json"],
            &["import", "--progress", "json"],
        ] {
            assert!(daemon_autostart_trigger(args).is_none(), "{args:?}");
        }
    }

    #[test]
    fn human_import_retains_post_command_daemon_autostart() {
        assert!(matches!(
            daemon_autostart_trigger(&["import"]),
            Some(DaemonTriggerCommandArg::Import)
        ));
    }
}
