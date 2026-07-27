use std::{env, process::ExitCode, time::Instant};

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
        status::run_status,
    },
    complete_content,
    config::AppConfig,
    deprecated_controls::DeprecatedControls,
    docs, integrations, mcp,
    output::{LocateFormat, OutputFormat, SqlFormat},
    pro, semantic, upgrade,
};

#[derive(Debug, thiserror::Error)]
#[error("JSON error was already rendered")]
struct RenderedJsonError;

pub(crate) fn run() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is::<RenderedJsonError>() => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_cli() -> Result<()> {
    let started = Instant::now();
    let cli = Cli::parse();
    let deprecated_controls = DeprecatedControls::detect();
    if command_deprecation_warning_eligible(&cli.command) {
        if let Some(warning) = deprecated_controls.warning() {
            eprintln!("{warning}");
        }
    }
    let json_output = command_json_output(&cli.command);
    let daemon_autostart_trigger = command_daemon_autostart_trigger(&cli.command);
    let mut analytics_draft = ClientOperationDraft::from_command(&cli.command, json_output);
    let mut provider_refreshes = ProviderRefreshCollector::default();
    let quiet = quiet_output(cli.quiet);
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;
    let mut config =
        match AppConfig::load_with_deprecated_controls(&data_root, &deprecated_controls) {
            Ok(config) => config,
            Err(_) if command_can_report_malformed_config(&cli.command) => {
                // Daemon status reads retained lifecycle/config-reload state. Keep
                // that diagnostic available when the malformed file is itself the
                // reload failure being diagnosed; ordinary commands remain strict.
                AppConfig::default()
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
        CommandRoot::Blame(args) => crate::commands::blame::run(args, data_root.clone()),
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
    if json_output {
        if let Err(error) = &result {
            if let Some(error) =
                error.downcast_ref::<ctx_history_capture::complete_content::CompleteContentError>()
            {
                eprintln!(
                    "{}",
                    serde_json::to_string(&complete_content::complete_content_error_json(error))?
                );
                return Err(RenderedJsonError.into());
            }
        }
    }
    result
}

fn command_json_output(command: &CommandRoot) -> bool {
    match command {
        CommandRoot::Setup(args) => args.json,
        CommandRoot::Status(args) => args.json,
        CommandRoot::Index(args) => args.json_output(),
        CommandRoot::Sources(args) => args.json,
        CommandRoot::Import(args) => args.json,
        CommandRoot::Show(args) => show_json_output(args),
        CommandRoot::Locate(args) => locate_json_output(args),
        CommandRoot::Search(args) => args.json,
        CommandRoot::Pro(args) => args.json_output(),
        CommandRoot::Blame(args) => args.json_output(),
        CommandRoot::Sql(args) => args.output_format() == SqlFormat::Json,
        CommandRoot::Docs(args) => args.json_output(),
        CommandRoot::Integrations(args) => args.json_output(),
        CommandRoot::Mcp(_) => false,
        CommandRoot::Daemon(args) => match &args.command {
            DaemonCommand::Run(args) => args.json,
            DaemonCommand::Status(args)
            | DaemonCommand::Enable(args)
            | DaemonCommand::Disable(args) => args.json,
        },
        CommandRoot::Upgrade(args) => args.json_output(),
        CommandRoot::Doctor(args) => args.json,
    }
}

fn show_json_output(args: &ShowArgs) -> bool {
    match &args.target {
        ShowTarget::Session(args) => args.json || args.format == OutputFormat::Json,
        ShowTarget::Event(args) => args.json || args.format == OutputFormat::Json,
    }
}

fn locate_json_output(args: &LocateArgs) -> bool {
    match &args.target {
        LocateTarget::Session(args) => args.json || args.format == LocateFormat::Json,
        LocateTarget::Event(args) => args.json || args.format == LocateFormat::Json,
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
        CommandRoot::Doctor(args) => args.progress == crate::progress::ProgressArg::Json,
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
    )
}

fn import_should_autostart_daemon(args: &ImportArgs) -> bool {
    !args.no_daemon
        && args.format.is_none()
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
            &["setup", "--json"][..],
            &["setup", "--progress", "json"],
            &["import", "--json"],
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
