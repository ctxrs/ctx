use std::{
    env,
    io::{self, Write},
    process::ExitCode,
    time::{Duration, Instant},
};

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
        stats::{malformed_config_failure as malformed_stats_config_failure, run as run_stats},
        status::{
            malformed_config_failure, removed_cloud_config_failure, run_status, run_usage_action,
        },
    },
    complete_content,
    config::AppConfig,
    deprecated_controls::DeprecatedControls,
    docs,
    hydration_error::source_hydration_error_contract,
    integrations, local_usage, mcp,
    output::{JsonOutputFormat, OutputFormat, OutputMeasurement, SqlFormat},
    pro, semantic,
    ui::{
        outcome, scan_color_mode, scan_machine_output_hint, ColorMode, Outcome, OutcomeState, Ui,
    },
    upgrade,
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

#[derive(Debug, thiserror::Error)]
enum FinalOutputFlushError {
    #[error("flush CLI stdout: {0}")]
    Stdout(io::Error),
    #[error("flush CLI stderr: {0}")]
    Stderr(io::Error),
    #[error("flush CLI stdout: {stdout}; flush CLI stderr: {stderr}")]
    Both {
        stdout: io::Error,
        stderr: io::Error,
    },
}

fn flush_cli_output_then<T>(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    after_delivery: impl FnOnce() -> T,
) -> std::result::Result<T, FinalOutputFlushError> {
    let stdout_result = stdout.flush();
    let stderr_result = stderr.flush();
    match (stdout_result, stderr_result) {
        (Ok(()), Ok(())) => Ok(after_delivery()),
        (Err(stdout), Ok(())) => Err(FinalOutputFlushError::Stdout(stdout)),
        (Ok(()), Err(stderr)) => Err(FinalOutputFlushError::Stderr(stderr)),
        (Err(stdout), Err(stderr)) => Err(FinalOutputFlushError::Both { stdout, stderr }),
    }
}

pub(crate) fn run() -> ExitCode {
    #[cfg(any(test, ctx_pro_test_helper))]
    if let Some(exit_code) = run_index_dashboard_fixture_if_requested() {
        return exit_code;
    }

    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is::<RenderedJsonError>() || error.is::<RenderedCliError>() => {
            ExitCode::FAILURE
        }
        Err(error) => {
            if render_unhandled_command_error(&error).is_err() {
                eprintln!("Error: {error:?}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(any(test, ctx_pro_test_helper))]
fn run_index_dashboard_fixture_if_requested() -> Option<ExitCode> {
    use std::ffi::{OsStr, OsString};

    let mut process_args = env::args_os();
    let _program = process_args.next();
    if process_args.next().as_deref()
        != Some(OsStr::new(
            crate::commands::index::dashboard_fixture::COMMAND_NAME,
        ))
    {
        return None;
    }

    let fixture_args = std::iter::once(OsString::from(
        crate::commands::index::dashboard_fixture::COMMAND_NAME,
    ))
    .chain(process_args);
    let args = match crate::cli::IndexDashboardFixtureArgs::try_parse_from(fixture_args) {
        Ok(args) => args,
        Err(error) => {
            let exit_code = u8::try_from(error.exit_code()).unwrap_or(2);
            let _ = error.print();
            return Some(ExitCode::from(exit_code));
        }
    };
    let mut ui = Ui::stdio(args.color);
    let result =
        crate::commands::index::dashboard_fixture::run(args, &mut ui).and_then(|exit_code| {
            ui.flush()
                .context("flush index dashboard fixture output")
                .map(|()| exit_code)
        });
    Some(match result {
        Ok(exit_code) => exit_code,
        Err(error) => {
            let summary = format!("{error:#}");
            let document = crate::ui::diagnostic(
                ui.stderr_context(),
                crate::ui::Diagnostic {
                    level: crate::ui::DiagnosticLevel::Error,
                    summary: &summary,
                    detail: None,
                    fields: &[],
                    action: None,
                },
            );
            let _ = ui.write_stderr(&document);
            let _ = ui.flush();
            ExitCode::FAILURE
        }
    })
}

fn render_unhandled_command_error(error: &anyhow::Error) -> Result<()> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let machine_output = scan_machine_output_hint(&arguments);
    let mode = if machine_output {
        ColorMode::Never
    } else {
        scan_color_mode(arguments).unwrap_or(ColorMode::Auto)
    };
    let mut ui = Ui::stdio(mode);
    render_generic_command_error(error, machine_output, &mut ui)?;
    ui.flush().context("flush pre-dispatch error")
}

pub(crate) fn run_cli() -> Result<()> {
    if upgrade::run_legacy_automatic_upgrade_bridge()? {
        return Ok(());
    }
    let started = Instant::now();
    let output_measurement = OutputMeasurement::start();
    let cli = Cli::parse();
    let mut ui = Ui::stdio(cli.color);
    if let CommandRoot::Referral(args) = &cli.command {
        pro::human_result(
            args.validate_invocation(),
            !args.json_output(),
            "ctx referral create <codename>",
            &mut ui,
        )?;
    }
    let deprecated_controls = DeprecatedControls::detect();
    if command_deprecation_warning_eligible(&cli.command) {
        if let Some(warning) = deprecated_controls.warning() {
            eprintln!("{warning}");
        }
    }
    let json_output = command_json_output(&cli.command);
    let machine_output = command_machine_readable_output(&cli.command, json_output);
    let usage_control_action = matches!(
        &cli.command,
        CommandRoot::Status(args) if args.usage.is_some()
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
        let Some(mode) = args.usage else {
            unreachable!("usage control mode was checked above");
        };
        return run_usage_action(mode, &data_root, args.format.is_json(), quiet, &mut ui);
    }
    let daemon_autostart_trigger = command_daemon_autostart_trigger(&cli.command);
    let mut analytics_draft = ClientOperationDraft::from_command(&cli.command, json_output);
    let mut local_usage_draft = command_local_usage_draft(&cli.command);
    let mut provider_refreshes = ProviderRefreshCollector::default();
    let mut config =
        match AppConfig::load_with_deprecated_controls(&data_root, &deprecated_controls) {
            Ok(config) => config,
            Err(error)
                if command_is_status_report(&cli.command)
                    && crate::config::is_removed_cloud_mode_error(&error) =>
            {
                return removed_cloud_config_failure(json_output, &mut ui);
            }
            Err(_) if command_is_status_report(&cli.command) => {
                return malformed_config_failure(json_output, &mut ui);
            }
            Err(_) if matches!(&cli.command, CommandRoot::Stats(_)) => {
                return malformed_stats_config_failure(json_output, &mut ui);
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
            &mut ui,
        ),
        CommandRoot::Status(args) => run_status(
            args,
            data_root.clone(),
            quiet,
            analytics_draft
                .as_mut()
                .expect("status has a telemetry draft")
                .status_mut(),
            &mut ui,
        ),
        CommandRoot::Stats(args) => {
            run_stats(args, data_root.clone(), config.local_usage.enabled, &mut ui)
        }
        CommandRoot::Index(args) => run_index(
            args,
            data_root.clone(),
            quiet,
            analytics_draft
                .as_mut()
                .expect("index has a telemetry draft")
                .index_mut(),
            &mut ui,
        ),
        CommandRoot::Sources(args) => run_sources(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("sources has a telemetry draft")
                .sources_mut(),
            &mut local_usage_draft,
            &mut ui,
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
            &mut ui,
        ),
        CommandRoot::Show(args) => run_show(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("show has a telemetry draft")
                .show_mut(),
            &mut local_usage_draft,
            &mut ui,
        ),
        CommandRoot::Locate(args) => run_locate(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("locate has a telemetry draft")
                .locate_mut(),
            &mut local_usage_draft,
            &mut ui,
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
            &mut local_usage_draft,
            &mut ui,
        ),
        CommandRoot::Pro(args) => {
            let validation = args.validate_invocation();
            if validation.is_err() && !data_root.exists() {
                // Invalid Pro syntax is non-mutating for a missing data root.
                // A pre-existing root remains eligible for the common local
                // completion hook below.
                local_usage_draft = local_usage::CliUsage::excluded();
            }
            validation.and_then(|()| pro::run_lifecycle(args, data_root.clone(), &mut ui))
        }
        CommandRoot::Referral(args) => pro::run_referral(args, data_root.clone(), &mut ui),
        CommandRoot::Blame(args) => {
            crate::commands::blame::run(args, data_root.clone(), &mut local_usage_draft, &mut ui)
        }
        CommandRoot::Sql(args) => run_sql(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("sql has a telemetry draft")
                .sql_mut(),
            &mut local_usage_draft,
            &mut ui,
        ),
        CommandRoot::Docs(args) => docs::run(
            args,
            analytics_draft
                .as_mut()
                .expect("docs has a telemetry draft")
                .docs_mut(),
            &mut local_usage_draft,
            &mut ui,
        ),
        CommandRoot::Integrations(args) => integrations::run(
            args,
            analytics_draft
                .as_mut()
                .expect("integrations has a telemetry draft")
                .integration_mut(),
            &mut ui,
        ),
        CommandRoot::Mcp(args) => mcp::run(args, data_root.clone()),
        CommandRoot::Daemon(args) => {
            semantic::run_daemon_command(args, data_root.clone(), &config, &mut ui)
        }
        CommandRoot::Upgrade(args) => upgrade::run(
            args,
            data_root.clone(),
            config.clone(),
            analytics_draft
                .as_mut()
                .expect("upgrade has a telemetry draft")
                .upgrade_mut(),
            &mut ui,
        ),
        CommandRoot::Doctor(args) => run_doctor(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("doctor has a telemetry draft")
                .doctor_mut(),
            &mut ui,
        ),
    };
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
            } else if let Some(error) = source_hydration_error_contract(error) {
                eprintln!("{}", serde_json::to_string(&error.structured())?);
                Some(RenderedJsonError.into())
            } else if let Some(error) =
                error.downcast_ref::<semantic::SourceBackedSemanticNotReady>()
            {
                eprintln!("{}", serde_json::to_string(&error.structured())?);
                Some(RenderedJsonError.into())
            } else {
                eprintln!("Error: {error:?}");
                Some(RenderedCliError.into())
            }
        } else {
            render_generic_command_error(error, machine_output, &mut ui)?;
            Some(RenderedCliError.into())
        }
    } else {
        None
    };
    ui.flush().context("flush structured terminal output")?;
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let delivery_result = flush_cli_output_then(&mut stdout, &mut stderr, || {
        let duration = started.elapsed();
        if let Some(operation) = complete_local_usage(
            local_usage_draft,
            result.is_ok(),
            duration,
            output_measurement.total_bytes(),
        ) {
            local_usage::record_best_effort(&data_root, config.local_usage.enabled, operation);
        }
        duration
    });
    drop(output_measurement);
    let duration = delivery_result
        .as_ref()
        .copied()
        .unwrap_or_else(|_| started.elapsed());
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
    delivery_result?;
    if let Some(error) = rendered_error {
        return Err(error);
    }
    result
}

fn render_generic_command_error(
    error: &anyhow::Error,
    machine_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    if machine_output {
        writeln!(ui.stderr_writer(), "Error: {error:?}")?;
        return Ok(());
    }
    let message = error.to_string();
    let detail = error
        .chain()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    let document = outcome(
        ui.stderr_context(),
        Outcome {
            state: OutcomeState::Error,
            title: &message,
            detail: (!detail.is_empty()).then_some(detail.as_str()),
        },
    );
    ui.write_stderr(&document)?;
    Ok(())
}

fn command_json_output(command: &CommandRoot) -> bool {
    match command {
        CommandRoot::Setup(args) => args.format.is_json(),
        CommandRoot::Status(args) => args.format.is_json(),
        CommandRoot::Stats(args) => args.format.is_json(),
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
            DaemonCommand::Status(args) | DaemonCommand::Enable(args) => args.format.is_json(),
            DaemonCommand::Disable(args) => args.format.is_json(),
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
                ShowTarget::Session(args)
                    if matches!(args.format, OutputFormat::Jsonl | OutputFormat::Markdown)
            ) || matches!(
                &args.target,
                ShowTarget::Event(args)
                    if matches!(args.format, OutputFormat::Jsonl | OutputFormat::Markdown)
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

fn command_local_usage_draft(command: &CommandRoot) -> local_usage::CliUsage {
    match command {
        CommandRoot::Stats(_) => local_usage::CliUsage::excluded(),
        _ => local_usage::CliUsage::from_command(command),
    }
}

fn complete_local_usage(
    mut draft: local_usage::CliUsage,
    success: bool,
    duration: Duration,
    delivered_output_bytes: u64,
) -> Option<local_usage::CompletedOperation> {
    // Runtime accounting is authoritative over command-local canonical
    // estimates: this is the final adapted stdout + stderr byte count after
    // error rendering and successful delivery flushes.
    let output_bytes = usize::try_from(delivered_output_bytes).unwrap_or(usize::MAX);
    draft.set_measured_output_bytes(output_bytes);
    draft.completed(success, duration)
}

fn command_is_status_report(command: &CommandRoot) -> bool {
    matches!(command, CommandRoot::Status(_))
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
    use std::{
        cell::Cell,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext};

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

    #[test]
    fn stats_is_excluded_from_local_and_remote_analytics() {
        for args in [
            &["stats"][..],
            &["stats", "--detail"][..],
            &["stats", "--format=json"][..],
        ] {
            let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
                .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
            assert!(
                ClientOperationDraft::from_command(&cli.command, command_json_output(&cli.command))
                    .is_none(),
                "{args:?}"
            );
            assert!(
                command_local_usage_draft(&cli.command)
                    .completed(true, std::time::Duration::ZERO)
                    .is_none(),
                "{args:?}"
            );
        }
    }

    #[derive(Clone, Default)]
    struct SharedBytes(Arc<Mutex<Vec<u8>>>);

    impl SharedBytes {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().map(|bytes| bytes.clone()).unwrap_or_default()
        }
    }

    impl Write for SharedBytes {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("shared test writer was poisoned"))?
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn forced_color_test_ui(stderr: SharedBytes) -> Ui {
        Ui::with_writers(
            SharedBytes::default(),
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Always)),
        )
    }

    #[test]
    fn forced_color_never_decorates_generic_machine_mode_errors() {
        for args in [
            &["show", "session", "bad", "--format", "jsonl"][..],
            &["show", "event", "bad", "--format", "markdown"][..],
            &["sql", "SELECT 1", "--format", "csv"][..],
            &["sql", "SELECT 1", "--format", "raw"][..],
            &["setup", "--progress", "json"][..],
            &["import", "--progress", "json"][..],
            &["mcp", "serve"][..],
            &["mcp", "--quiet", "serve"][..],
        ] {
            let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
                .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
            let json_output = command_json_output(&cli.command);
            assert!(
                command_machine_readable_output(&cli.command, json_output),
                "{args:?}"
            );

            let styled_stderr = SharedBytes::default();
            let styled_stderr_copy = styled_stderr.clone();
            let mut ui = forced_color_test_ui(styled_stderr);
            render_generic_command_error(
                &anyhow::anyhow!("representative command failure"),
                true,
                &mut ui,
            )
            .unwrap();
            ui.flush().unwrap();

            let machine_stderr = styled_stderr_copy.bytes();
            assert!(!machine_stderr.contains(&0x1b), "{args:?}");
            assert!(String::from_utf8_lossy(&machine_stderr)
                .starts_with("Error: representative command failure"));
        }
    }

    #[test]
    fn forced_color_still_styles_generic_human_mode_errors() {
        let styled_stderr = SharedBytes::default();
        let styled_stderr_copy = styled_stderr.clone();
        let mut ui = forced_color_test_ui(styled_stderr);

        render_generic_command_error(&anyhow::anyhow!("human command failure"), false, &mut ui)
            .unwrap();
        ui.flush().unwrap();

        assert!(styled_stderr_copy.bytes().contains(&0x1b));
    }

    #[test]
    fn generic_human_errors_include_the_actionable_cause_chain() {
        let stderr = SharedBytes::default();
        let stderr_copy = stderr.clone();
        let mut ui = Ui::with_writers(
            SharedBytes::default(),
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Never)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Never)),
        );
        let error = anyhow::anyhow!("No such file or directory")
            .context("approve explicit source path /tmp/missing.jsonl");

        render_generic_command_error(&error, false, &mut ui).unwrap();
        ui.flush().unwrap();

        let rendered = String::from_utf8(stderr_copy.bytes()).unwrap();
        assert!(rendered.contains("approve explicit source path /tmp/missing.jsonl"));
        assert!(rendered.contains("No such file or directory"));
        assert!(!rendered.contains("Stack backtrace"));
    }

    #[test]
    fn final_accounting_replaces_estimates_with_both_delivered_streams() {
        for success in [true, false] {
            let measurement = OutputMeasurement::start();
            let stdout = SharedBytes::default();
            let stdout_copy = stdout.clone();
            let stderr = SharedBytes::default();
            let stderr_copy = stderr.clone();
            let mut ui = Ui::with_writers(
                stdout,
                RenderContext::for_test(
                    TestContext::tty(StreamKind::Stdout, 32).color(ColorMode::Always),
                ),
                stderr,
                RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, 48).color(ColorMode::Always),
                ),
            );
            let document = crate::ui::Document::from_line(crate::ui::Line::text(
                "stdout result with enough words to wrap",
            ));
            ui.write_stdout(&document).unwrap();
            if success {
                let document =
                    crate::ui::Document::from_line(crate::ui::Line::text("stderr delivery note"));
                ui.write_stderr(&document).unwrap();
            } else {
                render_generic_command_error(
                    &anyhow::anyhow!("final command failure"),
                    false,
                    &mut ui,
                )
                .unwrap();
            }
            ui.flush().unwrap();

            let cli = Cli::try_parse_from(["ctx", "docs", "list"]).unwrap();
            let mut draft = command_local_usage_draft(&cli.command);
            draft.set_measured_output_bytes(1);
            let delivered = measurement.total_bytes();
            let completed =
                complete_local_usage(draft, success, Duration::from_millis(25), delivered).unwrap();

            let expected = stdout_copy.bytes().len() + stderr_copy.bytes().len();
            assert_eq!(usize::try_from(delivered).unwrap(), expected);
            assert_eq!(
                completed.delivered_output_bytes_for_test(),
                u64::try_from(expected).unwrap()
            );
            assert_eq!(completed.duration_bucket_for_test(), "10_to_49_ms");
        }
    }

    struct FlushWriter {
        failure: Option<&'static str>,
        flushes: Rc<Cell<usize>>,
    }

    impl Write for FlushWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.set(self.flushes.get() + 1);
            match self.failure {
                Some(message) => Err(io::Error::new(io::ErrorKind::BrokenPipe, message)),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn local_usage_hook_runs_only_after_both_final_output_flushes_succeed() {
        for (stdout_failure, stderr_failure, expected_delivery, expected_error) in [
            (None, None, 1, None),
            (Some("stdout"), None, 0, Some("flush CLI stdout: stdout")),
            (None, Some("stderr"), 0, Some("flush CLI stderr: stderr")),
            (
                Some("stdout"),
                Some("stderr"),
                0,
                Some("flush CLI stdout: stdout; flush CLI stderr: stderr"),
            ),
        ] {
            let stdout_flushes = Rc::new(Cell::new(0));
            let stderr_flushes = Rc::new(Cell::new(0));
            let mut stdout = FlushWriter {
                failure: stdout_failure,
                flushes: stdout_flushes.clone(),
            };
            let mut stderr = FlushWriter {
                failure: stderr_failure,
                flushes: stderr_flushes.clone(),
            };
            let mut deliveries = 0;

            let result = flush_cli_output_then(&mut stdout, &mut stderr, || {
                deliveries += 1;
                (stdout_flushes.get(), stderr_flushes.get())
            });

            assert_eq!(deliveries, expected_delivery);
            assert_eq!(stdout_flushes.get(), 1);
            assert_eq!(stderr_flushes.get(), 1);
            match expected_error {
                Some(expected) => {
                    assert!(result.is_err());
                    assert!(result.unwrap_err().to_string().contains(expected));
                }
                None => assert_eq!(result.unwrap(), (1, 1)),
            }
        }
    }

    #[test]
    fn duration_is_closed_after_both_final_stream_flushes() {
        struct TimedFlushWriter {
            clock_ms: Rc<Cell<u64>>,
            finish_at_ms: u64,
        }

        impl Write for TimedFlushWriter {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                Ok(buffer.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.clock_ms.set(self.finish_at_ms);
                Ok(())
            }
        }

        let clock_ms = Rc::new(Cell::new(0));
        let mut stdout = TimedFlushWriter {
            clock_ms: clock_ms.clone(),
            finish_at_ms: 11,
        };
        let mut stderr = TimedFlushWriter {
            clock_ms: clock_ms.clone(),
            finish_at_ms: 57,
        };
        let duration = flush_cli_output_then(&mut stdout, &mut stderr, || {
            Duration::from_millis(clock_ms.get())
        })
        .unwrap();

        assert_eq!(duration, Duration::from_millis(57));
        let cli = Cli::try_parse_from(["ctx", "doctor"]).unwrap();
        let completed =
            complete_local_usage(command_local_usage_draft(&cli.command), true, duration, 0)
                .unwrap();
        assert_eq!(completed.duration_bucket_for_test(), "50_to_249_ms");
    }
}
