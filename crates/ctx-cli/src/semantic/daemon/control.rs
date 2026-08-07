use super::*;

pub(crate) fn run_daemon_command(
    args: DaemonArgs,
    data_root: PathBuf,
    config: &AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    let started = Instant::now();
    let operation = daemon_operation_for_command(&args.command);
    let telemetry_root = data_root.clone();
    let result = match args.command {
        DaemonCommand::Run(args) => run_daemon(args, data_root, config, ui),
        DaemonCommand::Status(args) => run_daemon_status(args, data_root, ui),
        DaemonCommand::Enable(args) => run_daemon_enabled_update(args, data_root, true, ui),
        DaemonCommand::Disable(args) => run_daemon_disable(args, data_root, ui),
    };
    if let Some(operation) = operation {
        let event = PublicEventV1::OperationCompleted(OperationCompletedV1::for_daemon(
            operation,
            if result.is_ok() {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            started.elapsed(),
        ));
        send_daemon_events(&telemetry_root, &[event]);
    }
    result
}

fn daemon_operation_for_command(command: &DaemonCommand) -> Option<DaemonOperationV1> {
    match command {
        DaemonCommand::Run(_) => None,
        DaemonCommand::Status(_) => Some(DaemonOperationV1::Status),
        DaemonCommand::Enable(_) => Some(DaemonOperationV1::Enable),
        DaemonCommand::Disable(_) => Some(DaemonOperationV1::Disable),
    }
}

pub(super) fn daemon_run_facts(args: &DaemonRunArgs) -> DaemonRunFactsV1 {
    let start_mode = match daemon_run_start_mode(args) {
        DaemonStartModeArg::Auto => DaemonStartModeV1::Auto,
        DaemonStartModeArg::Manual => DaemonStartModeV1::Manual,
    };
    let supervisor = if start_mode == DaemonStartModeV1::Auto
        && semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
    {
        DaemonSupervisorV1::CliAutostart
    } else {
        DaemonSupervisorV1::User
    };
    let trigger = args.trigger_command.map(|trigger| match trigger {
        DaemonTriggerCommandArg::Setup => DaemonTriggerV1::Setup,
        DaemonTriggerCommandArg::Import => DaemonTriggerV1::Import,
        DaemonTriggerCommandArg::Search => DaemonTriggerV1::Search,
    });
    DaemonRunFactsV1::new(start_mode, supervisor, trigger)
}

pub(super) fn run_daemon_status(args: FormatArgs, data_root: PathBuf, ui: &mut Ui) -> Result<()> {
    let daemon = daemon_report(&data_root);
    let pro = crate::pro::lifecycle_status_json(&data_root);
    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemon": daemon,
            "pro": pro,
            "local_only": true,
        }))?;
    } else {
        let document = render_daemon_status_human(
            ui.stdout_context(),
            DaemonStatusView::from_reports(&daemon, &pro),
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
}

pub(super) fn run_daemon_enabled_update(
    args: FormatArgs,
    data_root: PathBuf,
    enabled: bool,
    ui: &mut Ui,
) -> Result<()> {
    config::set_daemon_enabled(&data_root, enabled)?;
    let handoff = if enabled {
        let config = AppConfig::load(&data_root)?;
        Some(super::super::daemon_autostart::autostart_daemon_and_wait(
            &data_root,
            &config,
            DaemonTriggerCommandArg::Setup,
        )?)
    } else {
        request_daemon_shutdown_and_wait(&data_root)?;
        super::super::daemon_supervisor::disable_daemon_supervisor(&data_root)?;
        super::super::cancel_core_finalization_generation_lease(&data_root, "daemon was disabled")?;
        None
    };
    let supervisor = super::super::daemon_supervisor::daemon_supervisor_report(&data_root);
    let persistent = supervisor.get("status").and_then(Value::as_str) == Some("installed")
        && supervisor
            .get("registration_verified")
            .and_then(Value::as_bool)
            == Some(true)
        && supervisor
            .get("live_owner_verified")
            .and_then(Value::as_bool)
            == Some(true);
    let running = handoff.is_some();
    let config_path = data_root.join(CONFIG_FILE);
    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemon_enabled": enabled,
            "running": running,
            "pid": handoff.map(|handoff| handoff.pid),
            "persistent": enabled && persistent,
            "supervisor": supervisor,
            "config_path": config_path,
            "local_only": true,
        }))?;
    } else if enabled {
        let document = render_daemon_enable_receipt(
            ui.stdout_context(),
            running,
            persistent,
            &supervisor,
            &config_path,
        );
        ui.write_stdout(&document)?;
    } else {
        let document =
            render_daemon_disable_receipt(ui.stdout_context(), &supervisor, &config_path);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn run_daemon_disable(args: DaemonDisableArgs, data_root: PathBuf, ui: &mut Ui) -> Result<()> {
    if !args.prepare_uninstall {
        return run_daemon_enabled_update(
            FormatArgs {
                format: args.format,
            },
            data_root,
            false,
            ui,
        );
    }
    let report = super::super::daemon_autostart::prepare_daemon_uninstall(&data_root)?;
    if args.format.is_json() {
        print_json(report)?;
    } else {
        let document = render_daemon_prepare_uninstall_receipt(ui.stdout_context(), &report);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn request_daemon_shutdown_and_wait(data_root: &Path) -> Result<()> {
    const SHUTDOWN_TIMEOUT: StdDuration = StdDuration::from_secs(5);
    const FORCED_SHUTDOWN_TIMEOUT: StdDuration = StdDuration::from_secs(5);
    const SHUTDOWN_RETRY: StdDuration = StdDuration::from_millis(50);
    let mut deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    let mut forced = false;
    while daemon_lock_is_active(data_root) {
        let _ = daemon_source_refresh_request(
            data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": "shutdown",
            })),
            StdDuration::from_millis(500),
            16 * 1024,
        );
        if Instant::now() >= deadline {
            if forced {
                return Err(anyhow!(
                    "daemon was disabled but retained lifecycle ownership after identity-verified termination"
                ));
            }
            terminate_current_executable_daemon(data_root).context(
                "terminate identity-verified daemon after cooperative shutdown timed out",
            )?;
            forced = true;
            deadline = Instant::now() + FORCED_SHUTDOWN_TIMEOUT;
        }
        std::thread::sleep(SHUTDOWN_RETRY);
    }
    remove_released_daemon_service_artifacts(data_root)
}

pub(super) fn remove_released_daemon_service_artifacts(data_root: &Path) -> Result<()> {
    if daemon_lock_is_active(data_root) {
        return Err(anyhow!(
            "refusing to remove daemon service artifacts while lifecycle ownership remains active"
        ));
    }
    for service in [
        DaemonIpcService::SemanticQuery,
        DaemonIpcService::SourceRefresh,
    ] {
        let identity = read_daemon_service_endpoint_identity(data_root, service)
            .context("inspect released daemon service endpoint")?;
        if daemon_lock_is_active(data_root) {
            return Err(anyhow!(
                "daemon lifecycle ownership resumed while released service artifacts were being removed"
            ));
        }
        #[cfg(unix)]
        if let Some(identity) = identity {
            let DaemonQueryEndpoint::Unix { path, .. } = identity.endpoint;
            remove_file_if_present(&path)
                .with_context(|| format!("remove released daemon socket {}", path.display()))?;
        }
        #[cfg(not(unix))]
        let _ = identity;
        let endpoint_path = daemon_service_endpoint_path(data_root, service);
        remove_file_if_present(&endpoint_path).with_context(|| {
            format!(
                "remove released daemon endpoint identity {}",
                endpoint_path.display()
            )
        })?;
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
