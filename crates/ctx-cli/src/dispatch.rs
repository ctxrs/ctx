use std::{env, io, process::ExitCode, time::Instant};

use anyhow::{Context, Result};
use clap::CommandFactory as _;
use ctx_history_platform::default_data_root;

use crate::{
    analytics::{
        self, count_bucket, ClientOperationDraft, DocsTelemetry, DoctorTelemetry, IndexTelemetry,
        IntegrationTelemetry, LocateTelemetry, RenderFormat, SearchTelemetry, SetupTelemetry,
        ShowTelemetry, SourcesTelemetry, StatusTelemetry, TargetKind, UpgradeMode,
        UpgradeOperation, UpgradeTelemetry,
    },
    cli::{CommandRoot, DaemonCommand, DaemonTriggerCommandArg, ImportArgs},
    commands::{
        doctor::run_doctor,
        import::{run_import, ProviderRefreshCollector},
        index::run_index,
        list::run_list,
        locate::run_locate,
        search::run_search,
        semantic::run_semantic,
        setup::run_setup,
        show::{run_show, ShowArgs, ShowTarget},
        sources::run_sources,
        stats::{malformed_config_failure as malformed_stats_config_failure, run as run_stats},
        status::{
            malformed_config_failure, removed_cloud_config_failure,
            run_status_authorized as run_status, run_usage_action,
        },
    },
    docs, integrations, local_usage, mcp,
    operation_descriptor::{CliOperation, OperationDescriptor},
    output::{OutputFormat, OutputMeasurement},
    presentation_limit, semantic,
    ui::{
        diagnostic, outcome, scan_color_mode, scan_machine_output_hint, ColorMode, Diagnostic,
        DiagnosticLevel, Outcome, OutcomeState, Ui,
    },
    upgrade,
};
use ctx_app_config::{AppConfig, DeprecatedControls};

mod finalization;
mod parse;

use finalization::{
    complete_local_usage, flush_cli_output, record_search_final_delivery, send_online_after_output,
};
use parse::parse_cli_from;

#[derive(Debug, thiserror::Error)]
#[error("JSON error was already rendered")]
struct RenderedJsonError;

#[derive(Debug, thiserror::Error)]
#[error("CLI parser output was already rendered")]
struct RenderedClapError(u8);

pub(crate) use ctx_history_cli::RenderedCliError;

pub(crate) fn rendered_cli_error() -> anyhow::Error {
    RenderedCliError.into()
}

pub(crate) fn run() -> ExitCode {
    #[cfg(test)]
    if let Some(exit_code) = run_index_dashboard_fixture_if_requested() {
        return exit_code;
    }

    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is::<RenderedClapError>() => {
            let exit_code = error
                .downcast_ref::<RenderedClapError>()
                .map_or(2, |rendered| rendered.0);
            ExitCode::from(exit_code)
        }
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

#[cfg(test)]
fn run_index_dashboard_fixture_if_requested() -> Option<ExitCode> {
    use std::ffi::{OsStr, OsString};

    use clap::Parser as _;

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
    let mut ui = Ui::stdio(args.color.into());
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
    semantic::initialize()?;
    if upgrade::ports::engine().run_legacy_automatic_bridge()? {
        return Ok(());
    }
    let started = Instant::now();
    let output_measurement = OutputMeasurement::start();
    let cli = parse_cli_from(env::args_os())?;
    let mut ui = Ui::stdio(cli.color.into());
    let json_output = command_json_output(&cli.command);
    let machine_output = command_machine_readable_output(&cli.command, json_output);
    let query_authority_error_json =
        command_uses_query_authority_error_json(&cli.command, json_output);
    let _analytics_delivery_failure_output =
        analytics::quiet_delivery_failure_output(machine_output);
    let deprecated_controls = DeprecatedControls::detect();
    if command_deprecation_warning_eligible(&cli.command) {
        if let Some(warning) = deprecated_controls.warning() {
            let detail = warning
                .strip_prefix("warning: ")
                .unwrap_or(warning.as_str());
            let document = diagnostic(
                ui.stderr_context(),
                Diagnostic {
                    level: DiagnosticLevel::Warning,
                    summary: "Deprecated environment variables detected",
                    detail: Some(detail),
                    fields: &[],
                    action: None,
                },
            );
            ui.write_stderr(&document)?;
        }
    }
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
        .context("resolve ctx data root");
    let data_root = data_root?;
    let local_usage_authority =
        crate::observability_composition::local_usage_storage_authority(&data_root);
    if usage_control_action {
        let CommandRoot::Status(args) = cli.command else {
            unreachable!("usage controls are status commands");
        };
        let Some(mode) = args.usage else {
            unreachable!("usage control mode was checked above");
        };
        return run_usage_action(
            mode,
            &data_root,
            &local_usage_authority,
            args.format.is_json(),
            quiet,
            &mut ui,
        );
    }
    let daemon_autostart_trigger = command_daemon_autostart_trigger(&cli.command);
    let operation_descriptor = command_operation_descriptor(&cli.command);
    let mut analytics_draft =
        ClientOperationDraft::from_descriptor(operation_descriptor, json_output);
    let mut provider_refreshes = ProviderRefreshCollector::default();
    let mut config =
        match AppConfig::load_with_deprecated_controls(&data_root, &deprecated_controls) {
            Ok(config) => config,
            Err(error)
                if command_is_status_report(&cli.command)
                    && ctx_app_config::is_removed_cloud_mode_error(&error) =>
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
                    ctx_app_config::resolve_local_usage_control(&data_root).effective_on_startup();
                fallback
            }
            Err(error) => return Err(error),
        };
    crate::semantic::bind_embedding_auth_endpoint(&config);
    if let Some(draft) = analytics_draft.as_mut() {
        draft.set_deprecated_controls(deprecated_controls.nonprivacy_analytics_ids().as_deref());
    }
    let usage_control =
        crate::observability_composition::usage_control_snapshot(config.local_usage.enabled);
    let mut local_usage_draft = if usage_control.enabled() {
        let descriptor = command_operation_descriptor(&cli.command);
        local_usage::CliUsage::from_descriptor(&descriptor)
    } else {
        local_usage::CliUsage::excluded()
    };

    let search_operation = matches!(&cli.command, CommandRoot::Search(_));
    let result = match cli.command {
        CommandRoot::Pro | CommandRoot::Blame | CommandRoot::Referral => Err(anyhow::anyhow!(
            "companion-owned command bypassed native argv routing"
        )),
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
        CommandRoot::Semantic(args) => {
            run_semantic(args, data_root.clone(), quiet, &mut config, &mut ui)
        }
        CommandRoot::Status(args) => run_status(
            args,
            &data_root,
            &config,
            quiet,
            analytics_draft
                .as_mut()
                .expect("status has a telemetry draft")
                .status_mut(),
            &local_usage_authority,
            &usage_control,
            &mut ui,
        ),
        CommandRoot::Stats(args) => {
            run_stats(args, &local_usage_authority, &usage_control, &mut ui)
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
            crate::commands::sources::SourcesEnvironment {
                data_root: data_root.clone(),
                home_dir: crate::identity::home_dir(),
                automatic_provider_discovery: config.automatic_source_discovery_enabled(),
                provider_roots: config.provider_root_definitions(),
            },
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
        CommandRoot::List(args) => run_list(
            args,
            data_root.clone(),
            analytics_draft
                .as_mut()
                .expect("list has a telemetry draft")
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
            ctx_history_cli::HistoryCliConfig {
                daemon_enabled: config.automatic_indexing_enabled(),
                semantic_search_enabled: config.semantic_search_enabled(),
                semantic_executor: config.semantic_embedding_executor().clone(),
                local_usage_enabled: config.local_usage.enabled,
                automatic_provider_discovery: config.automatic_source_discovery_enabled(),
                provider_roots: config.provider_root_definitions(),
            },
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
            &crate::Cli::command(),
        ),
        CommandRoot::Integrations(args) => integrations::run(
            args,
            ctx_agent_application::ProductIdentity {
                name: "ctx",
                version: env!("CARGO_PKG_VERSION"),
            },
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
    let output_started = Instant::now();
    let (rendered_error, search_error_render_failure) = match render_command_result_error(
        &result,
        json_output,
        query_authority_error_json,
        machine_output,
        search_operation,
        &mut ui,
    ) {
        Ok(rendered_error) => (rendered_error, None),
        Err(error) if search_operation => (None, Some(error)),
        Err(error) => return Err(error),
    };
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let delivery_result = if search_operation {
        ui.flush()
            .context("flush structured terminal output")
            .and_then(|()| flush_cli_output(&mut stdout, &mut stderr).map_err(Into::into))
    } else {
        // Preserve the released non-Search ordering: buffered UI failure
        // returns here; final process-stream failure is returned after the
        // analytics and the daemon post-command hook below.
        ui.flush().context("flush structured terminal output")?;
        flush_cli_output(&mut stdout, &mut stderr).map_err(Into::into)
    };
    let output_duration = output_started.elapsed();
    let duration = started.elapsed();
    let output_result = delivery_result.and_then(|()| match search_error_render_failure {
        Some(error) => Err(error),
        None => Ok(()),
    });
    if output_result.is_ok() {
        local_usage::record_best_effort(&local_usage_authority, &usage_control, || {
            complete_local_usage(
                local_usage_draft,
                result.is_ok(),
                duration,
                output_measurement.total_bytes(),
            )
        });
    }
    drop(output_measurement);
    let search_output_served = search_operation.then(|| {
        record_search_final_delivery(
            analytics_draft
                .as_mut()
                .expect("search has a telemetry draft")
                .search_mut(),
            output_result.is_ok(),
            output_duration,
        )
    });
    let mut events = provider_refreshes.finish();
    if let Some(draft) = analytics_draft {
        if draft.should_emit() {
            events.push(draft.finish(
                result.is_ok() && search_output_served.unwrap_or(true),
                duration,
            ));
        }
    }
    let output_result = send_online_after_output(output_result, || {
        analytics::send_batch(&data_root, &config, &events);
    });
    if result.is_ok() {
        if let Some(trigger) = daemon_autostart_trigger {
            semantic::maybe_autostart_daemon(&data_root, &config, trigger);
        }
    }
    output_result?;
    if let Some(error) = rendered_error {
        return Err(error);
    }
    result
}

fn render_command_result_error(
    result: &Result<()>,
    json_output: bool,
    query_authority_error_json: bool,
    machine_output: bool,
    search_operation: bool,
    ui: &mut Ui,
) -> Result<Option<anyhow::Error>> {
    let rendered_error = if let Err(error) = result {
        if error.is::<RenderedJsonError>() || error.is::<RenderedCliError>() {
            Some(RenderedCliError.into())
        } else if json_output {
            if let Some(authority_error) = error
                .downcast_ref::<ctx_history_refresh::GenerationQueryAuthorityError>()
                .filter(|_| query_authority_error_json)
            {
                write_machine_error(
                    search_operation,
                    ui,
                    &serde_json::to_string(
                        &crate::commands::source_index::generation_query_authority_error_json(
                            authority_error,
                        ),
                    )?,
                )?;
                Some(RenderedJsonError.into())
            } else if let Some(error) =
                error.downcast_ref::<presentation_limit::PresentationOutputLimitError>()
            {
                write_machine_error(
                    search_operation,
                    ui,
                    &serde_json::to_string(
                        &presentation_limit::presentation_output_limit_error_json(error),
                    )?,
                )?;
                Some(RenderedJsonError.into())
            } else if let Some(error) = error.downcast_ref::<semantic::SemanticNotReady>() {
                write_machine_error(
                    search_operation,
                    ui,
                    &serde_json::to_string(&error.structured())?,
                )?;
                Some(RenderedJsonError.into())
            } else {
                write_machine_error(search_operation, ui, &format!("Error: {error:?}"))?;
                Some(RenderedCliError.into())
            }
        } else {
            render_generic_command_error(error, machine_output, ui)?;
            Some(RenderedCliError.into())
        }
    } else {
        None
    };
    Ok(rendered_error)
}

fn write_machine_error(search_operation: bool, ui: &mut Ui, message: &str) -> Result<()> {
    if search_operation {
        writeln!(ui.stderr_writer(), "{message}")?;
    } else {
        eprintln!("{message}");
    }
    Ok(())
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

fn write_clap_output(error: &clap::Error, ui: &mut Ui) -> Result<()> {
    write_clap_output_with_line_ends(error, ui, false)
}

fn write_human_clap_output(error: &clap::Error, ui: &mut Ui) -> Result<()> {
    write_clap_output_with_line_ends(error, ui, true)
}

fn write_clap_output_with_line_ends(
    error: &clap::Error,
    ui: &mut Ui,
    trim_line_ends: bool,
) -> Result<()> {
    let rendered = error.render();
    if error.use_stderr() {
        let rendered = if ui.stderr_context().color_enabled() {
            rendered.ansi().to_string()
        } else {
            rendered.to_string()
        };
        let rendered = if trim_line_ends {
            crate::ui::trim_terminal_line_ends(&rendered)
        } else {
            rendered
        };
        write!(ui.stderr_writer(), "{rendered}")?;
    } else {
        let rendered = if ui.stdout_context().color_enabled() {
            rendered.ansi().to_string()
        } else {
            rendered.to_string()
        };
        let rendered = if trim_line_ends {
            crate::ui::trim_terminal_line_ends(&rendered)
        } else {
            rendered
        };
        write!(ui.stdout_writer(), "{rendered}")?;
    }
    Ok(())
}

fn command_json_output(command: &CommandRoot) -> bool {
    match command {
        CommandRoot::Pro | CommandRoot::Blame | CommandRoot::Referral => false,
        CommandRoot::Setup(args) => args.format.is_json(),
        CommandRoot::Semantic(args) => args.json_output(),
        CommandRoot::Status(args) => args.format.is_json(),
        CommandRoot::Stats(args) => args.format.is_json(),
        CommandRoot::Index(args) => args.json_output(),
        CommandRoot::Sources(args) => args.format.is_json(),
        CommandRoot::Import(args) => args.format.is_json(),
        CommandRoot::Show(args) => show_json_output(args),
        CommandRoot::List(_) => true,
        CommandRoot::Locate(args) => match &args.target {
            crate::LocateTarget::Session(args) => args.format.is_json(),
            crate::LocateTarget::Event(args) => args.format.is_json(),
        },
        CommandRoot::Search(args) => args.format.is_json(),
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

fn command_uses_query_authority_error_json(command: &CommandRoot, json_output: bool) -> bool {
    json_output
        && matches!(
            command,
            CommandRoot::Search(_) | CommandRoot::Show(_) | CommandRoot::Locate(_)
        )
}

fn show_json_output(args: &ShowArgs) -> bool {
    match &args.target {
        ShowTarget::Session(args) => args.format == OutputFormat::Json,
        ShowTarget::Event(args) => args.format == OutputFormat::Json,
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
        CommandRoot::List(_) => true,
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

pub(crate) fn command_operation_descriptor(command: &CommandRoot) -> OperationDescriptor {
    let operation = match command {
        CommandRoot::Pro | CommandRoot::Blame | CommandRoot::Referral => {
            unreachable!("companion-owned commands are routed before Clap")
        }
        CommandRoot::Setup(args) => CliOperation::Setup(SetupTelemetry {
            catalog_only: args.catalog_only,
            no_daemon: args.no_daemon,
            wait: args.wait,
            progress_mode: crate::observability_product::progress_mode(args.progress),
            mode: None,
            providers_detected: None,
            cataloged_sessions: None,
            inventory_sources: None,
            inventory_source_files: None,
            pending_sessions: None,
            catalog_source_bytes: None,
            inventory_source_bytes: None,
            has_indexed_content: None,
            import: crate::observability_product::setup_import_telemetry(
                args.progress,
                args.no_daemon,
            ),
        }),
        CommandRoot::Semantic(args) => match &args.command {
            ctx_cli_presentation::commands::SemanticCommand::Enable(_) => {
                CliOperation::SemanticEnable
            }
            ctx_cli_presentation::commands::SemanticCommand::Status(_) => {
                CliOperation::SemanticStatus
            }
            ctx_cli_presentation::commands::SemanticCommand::Disable(_) => {
                CliOperation::SemanticDisable
            }
        },
        CommandRoot::Status(_) => CliOperation::Status(StatusTelemetry::default()),
        CommandRoot::Stats(_) => CliOperation::Stats,
        CommandRoot::Index(_) => CliOperation::Index(IndexTelemetry::default()),
        CommandRoot::Sources(args) => CliOperation::Sources(SourcesTelemetry {
            all: args.all,
            show_missing: args.show_missing,
            provider_filter: args.provider.map(|provider| provider.capture_provider()),
            providers_detected: None,
            providers_existing: None,
            providers_importable: None,
        }),
        CommandRoot::Import(args) => {
            CliOperation::Import(crate::observability_product::import_telemetry(args))
        }
        CommandRoot::Show(args) => match &args.target {
            ShowTarget::Session(args) => CliOperation::ShowSession(ShowTelemetry {
                target_kind: TargetKind::Session,
                transcript_mode: Some(crate::observability_product::transcript_mode(args.mode)),
                output_format: crate::observability_product::render_format(args.format),
                writes_out_file: args.out.is_some(),
                provider_lookup: args.provider.is_some() || args.provider_session.is_some(),
                window: None,
                events_returned: None,
            }),
            ShowTarget::Event(args) => CliOperation::ShowEvent(ShowTelemetry {
                target_kind: TargetKind::Event,
                transcript_mode: None,
                output_format: crate::observability_product::render_format(args.format),
                writes_out_file: false,
                provider_lookup: false,
                window: Some(count_bucket(
                    args.window.unwrap_or(args.before.max(args.after)) as u64,
                )),
                events_returned: None,
            }),
        },
        CommandRoot::List(args) => match &args.target {
            crate::commands::list::ListTarget::Events(args) => {
                CliOperation::ShowEvent(ShowTelemetry {
                    target_kind: TargetKind::Events,
                    transcript_mode: None,
                    output_format: match args.format {
                        crate::commands::list::EventQueryFormat::Json => RenderFormat::Json,
                        crate::commands::list::EventQueryFormat::Jsonl => RenderFormat::Jsonl,
                    },
                    writes_out_file: false,
                    provider_lookup: !args.provider.is_empty(),
                    window: None,
                    events_returned: None,
                })
            }
        },
        CommandRoot::Locate(args) => match &args.target {
            crate::LocateTarget::Session(args) => CliOperation::Locate(LocateTelemetry {
                target_kind: TargetKind::Session,
                output_format: crate::observability_product::json_render_format(args.format),
                provider_lookup: args.provider.is_some() || args.provider_session.is_some(),
            }),
            crate::LocateTarget::Event(args) => CliOperation::Locate(LocateTelemetry {
                target_kind: TargetKind::Event,
                output_format: crate::observability_product::json_render_format(args.format),
                provider_lookup: false,
            }),
        },
        CommandRoot::Search(args) => CliOperation::Search(SearchTelemetry {
            has_query: args.query.is_some(),
            has_provider_filter: args.provider.is_some(),
            has_workspace_filter: args.workspace.is_some(),
            has_since_filter: args.since.is_some(),
            has_event_type_filter: args.event_type.is_some(),
            has_file_filter: args.file.is_some(),
            has_session_filter: args.session.is_some(),
            event_results: args.events || args.session.is_some(),
            primary_only: args.primary_only,
            include_current_session: args.include_current_session,
            limit: count_bucket(args.limit as u64),
            provider_filter: args.provider.map(|provider| provider.capture_provider()),
            refresh_duration: None,
            refresh_mode: None,
            refresh_status: None,
            refresh_source_count: None,
            has_indexed_content_after: None,
            query_length: None,
            query_term_count: None,
            query_duration: None,
            backend_requested: None,
            backend_effective: None,
            result_count: None,
            citation_count: None,
            zero_result: None,
            render_duration: None,
            output_duration: None,
            output_served: None,
            health: None,
        }),
        CommandRoot::Docs(_) => CliOperation::Docs(DocsTelemetry::default()),
        CommandRoot::Integrations(_) => CliOperation::Integrations(IntegrationTelemetry::default()),
        CommandRoot::Mcp(_) => CliOperation::McpServe,
        CommandRoot::Daemon(args) => match &args.command {
            DaemonCommand::Run(_) => CliOperation::DaemonRun,
            DaemonCommand::Status(_) => CliOperation::DaemonStatus,
            DaemonCommand::Enable(_) => CliOperation::DaemonEnable,
            DaemonCommand::Disable(_) => CliOperation::DaemonDisable,
        },
        CommandRoot::Upgrade(args) => CliOperation::Upgrade {
            telemetry: UpgradeTelemetry {
                mode: UpgradeMode::Manual,
                operation: match args.operation() {
                    "check" => UpgradeOperation::Check,
                    "status" => UpgradeOperation::Status,
                    "enable" => UpgradeOperation::Enable,
                    "disable" => UpgradeOperation::Disable,
                    _ => UpgradeOperation::Apply,
                },
                dry_run: args.dry_run,
                suppress_event: false,
                status: None,
                applied: None,
                scheduled: None,
                update_available: None,
                update_was_available: None,
                upgrade_attempt_id: None,
                managed_install: None,
                self_upgrade_allowed: None,
                auto_upgrade_allowed: None,
                warning_count: None,
                channel: None,
                failure_kind: None,
            },
            record_local_usage: !args.replacement_helper && args.hosted_transaction.is_none(),
        },
        CommandRoot::Doctor(_) => CliOperation::Doctor(DoctorTelemetry::default()),
    };
    OperationDescriptor::Cli(operation)
}

#[cfg(test)]
fn command_local_usage_draft(command: &CommandRoot) -> local_usage::CliUsage {
    let descriptor = command_operation_descriptor(command);
    local_usage::CliUsage::from_descriptor(&descriptor)
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
mod tests;
