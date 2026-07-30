use std::{
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::{
    analytics::{
        DaemonCycleStateV1, DaemonOperationV1, DaemonRunFactsV1, DaemonStartModeV1,
        DaemonSupervisorV1, DaemonTriggerV1, OperationCompletedV1, Outcome, PublicEventV1,
    },
    compact_json,
    config::{self, AppConfig, DaemonMode, CONFIG_FILE},
    output::print_json,
    DaemonArgs, DaemonCommand, DaemonDisableArgs, DaemonRunArgs, DaemonStartModeArg,
    DaemonTriggerCommandArg, FormatArgs,
};

#[cfg(test)]
use crate::analytics::DaemonRuntimeObservationV1;

#[cfg(test)]
use super::source_backed_refresh_coordinator::SourceBackedRefreshCoordinator;

use super::{
    daemon_autostart::{
        current_process_owns_daemon_upgrade_handoff, daemon_upgrade_handoff_blocks_current_process,
        resume_completed_installation_daemons, InstallationDaemonLease,
    },
    daemon_retry::DaemonRetryBackoff,
    daemon_scheduler::{
        daemon_retry_due, daemon_run_start_mode, restore_daemon_consumer_retries,
        restore_daemon_source_refresh_retry, run_daemon_once_with_activity,
    },
    daemon_status::{
        daemon_report_failure_message, render_daemon_disable_receipt, render_daemon_enable_receipt,
        render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
    },
    daemon_wakeup::{write_degraded_wakeup_receipt, DaemonFileWatcher, DaemonWakeup},
    daemon_worker::write_daemon_lifecycle_status_with_runtime,
    health_search::semantic_env_flag,
    model_runtime::SharedSemanticRuntime,
    paths_status::{
        daemon_lock_is_active, daemon_report, daemon_report_with_disabled_status,
        daemon_source_backed_refresh_job_path, read_daemon_job_status, read_daemon_status,
        write_daemon_status, DaemonLock,
    },
    query_service::{
        daemon_can_begin_idle_shutdown, daemon_source_refresh_request,
        observe_daemon_query_activity, DaemonQueryService,
    },
    runtime_limits::DAEMON_BACKGROUND_CHILD_ENV,
    source_backed_refresh_coordinator::reconcile_verified_source_epoch,
};
use crate::ui::Ui;

mod config_reload;
mod telemetry;

use config_reload::{
    daemon_semantic_runtime_active, reload_daemon_runtime_config, DaemonConfigReloadOutcome,
    DaemonConfigReloadState,
};
use telemetry::{daemon_safety_reconcile_interval, send_daemon_events, DaemonTelemetry};

#[cfg(test)]
use config_reload::daemon_semantic_runtime_requested;
#[cfg(test)]
use telemetry::{
    daemon_liveness_interval, reload_daemon_analytics_config, runtime_event,
    DAEMON_LIVENESS_JITTER_WINDOW, DAEMON_LIVENESS_MIN_INTERVAL,
};

#[derive(Debug)]
pub(super) struct DaemonIteration {
    pub(super) did_work: bool,
    pub(super) failed: bool,
    pub(super) telemetry_state: DaemonCycleStateV1,
    // Provider refresh drafts are not present on this branch yet. This owned
    // handoff lets the scheduler append their completed events without changing
    // the daemon loop or provider-owned foreground refresh code later.
    pub(super) provider_refresh_events: Vec<PublicEventV1>,
}

impl DaemonIteration {
    pub(super) fn new(did_work: bool, failed: bool, telemetry_state: DaemonCycleStateV1) -> Self {
        Self {
            did_work,
            failed,
            telemetry_state,
            provider_refresh_events: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_provider_refresh_events(mut self, events: Vec<PublicEventV1>) -> Self {
        self.provider_refresh_events = events;
        self
    }
}

#[derive(Default)]
pub(super) struct DaemonRuntime {
    pub(super) semantic_runtime: SharedSemanticRuntime,
    pub(super) history_retry: DaemonRetryBackoff,
    pub(super) pro_retry: DaemonRetryBackoff,
    pub(super) relational_retry: DaemonRetryBackoff,
    pub(super) semantic_retry: DaemonRetryBackoff,
    pub(super) semantic_blocked_job: Option<Value>,
    pub(super) config: AppConfig,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct DaemonTestJobHooks {
    pub(super) calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    pub(super) history_refresh: Option<Value>,
    pub(super) relational_projection: Option<Value>,
    pub(super) semantic_index: Option<Value>,
}

#[cfg(test)]
thread_local! {
    static DAEMON_TEST_JOB_HOOKS: std::cell::RefCell<Option<DaemonTestJobHooks>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) struct DaemonTestJobHookGuard;

#[cfg(test)]
impl Drop for DaemonTestJobHookGuard {
    fn drop(&mut self) {
        DAEMON_TEST_JOB_HOOKS.with(|hooks| {
            *hooks.borrow_mut() = None;
        });
    }
}

#[cfg(test)]
pub(super) fn install_daemon_test_job_hooks(hooks: DaemonTestJobHooks) -> DaemonTestJobHookGuard {
    DAEMON_TEST_JOB_HOOKS.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "daemon test job hook already installed"
        );
        *slot.borrow_mut() = Some(hooks);
    });
    DaemonTestJobHookGuard
}

#[cfg(test)]
pub(super) fn daemon_test_job(job: &'static str) -> Option<Value> {
    DAEMON_TEST_JOB_HOOKS.with(|slot| {
        let hooks = slot.borrow();
        let hooks = hooks.as_ref()?;
        hooks.calls.borrow_mut().push(job);
        match job {
            "history_refresh" => hooks.history_refresh.clone(),
            "relational_projection" => hooks.relational_projection.clone(),
            "semantic_index" => hooks.semantic_index.clone(),
            _ => None,
        }
    })
}

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
        DaemonCommand::Run(args) if args.once => {
            Some(DaemonOperationV1::run_once(daemon_run_facts(args)))
        }
        DaemonCommand::Run(_) => None,
        DaemonCommand::Status(_) => Some(DaemonOperationV1::Status),
        DaemonCommand::Enable(_) => Some(DaemonOperationV1::Enable),
        DaemonCommand::Disable(_) => Some(DaemonOperationV1::Disable),
    }
}

fn daemon_run_facts(args: &DaemonRunArgs) -> DaemonRunFactsV1 {
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

fn daemon_previous_status_needs_recovery(status: Option<&Value>) -> bool {
    status
        .and_then(|status| status.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "failed" | "running"))
}

fn daemon_iteration_events(
    telemetry: Option<&mut DaemonTelemetry>,
    iteration: &mut DaemonIteration,
    duration: StdDuration,
) -> Vec<PublicEventV1> {
    if let Some(telemetry) = telemetry {
        telemetry.observe_cycle(iteration, duration)
    } else {
        std::mem::take(&mut iteration.provider_refresh_events)
    }
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
        Some(super::daemon_autostart::autostart_daemon_and_wait(
            &data_root,
            &config,
            DaemonTriggerCommandArg::Setup,
        )?)
    } else {
        request_daemon_shutdown_and_wait(&data_root)?;
        super::daemon_supervisor::disable_daemon_supervisor(&data_root)?;
        None
    };
    let supervisor = super::daemon_supervisor::daemon_supervisor_report(&data_root);
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
    let report = super::daemon_autostart::prepare_daemon_uninstall(&data_root)?;
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
    const SHUTDOWN_RETRY: StdDuration = StdDuration::from_millis(50);
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
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
            return Err(anyhow!(
                "daemon was disabled but did not release lifecycle ownership within {} seconds",
                SHUTDOWN_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(SHUTDOWN_RETRY);
    }
    Ok(())
}

pub(super) fn run_daemon(
    args: DaemonRunArgs,
    data_root: PathBuf,
    config: &AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    if (args.start_mode.is_some() || args.trigger_command.is_some())
        && !semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
    {
        return Err(anyhow!(
            "daemon autostart metadata flags are internal; run `ctx daemon run` without --start-mode or --trigger-command"
        ));
    }
    let report = match run_daemon_inner(args.clone(), &data_root, config) {
        Ok(report) => report,
        Err(error) => {
            let message = format!("{error:#}");
            let now = utc_now().timestamp_millis();
            let previous_status = read_daemon_status(&data_root);
            let _ = write_daemon_status(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "status": "failed",
                    "pid": process::id(),
                    "heartbeat_at_ms": now,
                    "finished_at_ms": now,
                    "start_mode": daemon_run_start_mode(&args).as_str(),
                    "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
                    "last_error": message,
                    "semantic_runtime_active": false,
                    "config_reload": previous_status
                        .as_ref()
                        .and_then(|value| value.get("config_reload"))
                        .cloned(),
                }),
            );
            return Err(error);
        }
    };
    let failure = daemon_report_failure_message(&report);
    if args.format.is_json() {
        print_json(report)?;
    } else {
        let document =
            render_daemon_status_human(ui.stdout_context(), DaemonStatusView::daemon_only(&report));
        ui.write_stdout(&document)?;
    }
    if let Some(message) = failure {
        return Err(anyhow!(message));
    }
    Ok(())
}

pub(super) fn run_daemon_inner(
    args: DaemonRunArgs,
    data_root: &Path,
    config: &AppConfig,
) -> Result<Value> {
    if !config.daemon.enabled && !args.force {
        return Ok(daemon_report(data_root));
    }
    if installation_upgrade_blocks_current_process(data_root) {
        return Ok(daemon_report(data_root));
    }
    if daemon_upgrade_handoff_blocks_current_process(data_root) {
        return Ok(daemon_report(data_root));
    }
    let Some(lock) = DaemonLock::acquire(data_root)? else {
        return Ok(daemon_report(data_root));
    };
    // Close the check/acquire race with an upgrader that fenced daemon starts
    // after the first observation but before this process acquired ownership.
    if daemon_upgrade_handoff_blocks_current_process(data_root)
        || installation_upgrade_blocks_current_process(data_root)
    {
        drop(lock);
        return Ok(daemon_report(data_root));
    }
    let run_once = args.once;
    let run_started = Instant::now();
    let started_at_ms = utc_now().timestamp_millis();
    let recovered_previous_run =
        daemon_previous_status_needs_recovery(read_daemon_status(data_root).as_ref());
    let mut telemetry = (!run_once)
        .then(|| DaemonTelemetry::new(daemon_run_facts(&args), run_started, started_at_ms as u64));
    let idle_exit = args.idle_exit_seconds.map(StdDuration::from_secs);
    let safety_interval = args.loop_interval_seconds.map_or_else(
        || daemon_safety_reconcile_interval(started_at_ms as u64),
        StdDuration::from_secs,
    );
    let upgrade_restart_trigger = args
        .trigger_command
        .unwrap_or(DaemonTriggerCommandArg::Search);
    let Some(installation_daemon_lease) = InstallationDaemonLease::acquire(
        data_root,
        upgrade_restart_trigger,
        idle_exit.map(|duration| duration.as_secs()),
        args.loop_interval_seconds,
        current_process_owns_daemon_upgrade_handoff(data_root),
    )?
    else {
        drop(lock);
        return Ok(daemon_report(data_root));
    };
    let mut prepared_auto_upgrade = None;
    let mut auto_upgrade_handoff = None;
    let wakeup = Arc::new(DaemonWakeup::default());
    let active_result = (|| -> Result<bool> {
        // A crash can occur after the immutable generation commit and before
        // old Store-family cleanup. A verified active generation is sufficient
        // evidence to finish that idempotent cleanup without scheduling a
        // marker-driven rebuild.
        reconcile_verified_source_epoch(data_root)?;
        let mut failed = false;
        let mut runtime = DaemonRuntime {
            config: config.clone(),
            ..DaemonRuntime::default()
        };
        let mut config_reload = DaemonConfigReloadState::pending(config);
        let mut query_service = None;
        let mut refresh_service = None;
        write_daemon_lifecycle_status_with_runtime(
            data_root,
            &args,
            "running",
            started_at_ms,
            None,
            None,
            false,
            &config_reload.to_json(),
        )?;
        if !runtime.config.daemon.mode.runs_only_source_refresh() {
            restore_daemon_source_refresh_retry(&mut runtime, data_root);
            restore_daemon_consumer_retries(&mut runtime, data_root);
        }
        let stop_disabled = reload_daemon_runtime_config(
            data_root,
            &args,
            &mut runtime,
            &mut query_service,
            &mut refresh_service,
            &mut config_reload,
            &wakeup,
        ) == DaemonConfigReloadOutcome::StopDisabled;
        if config_reload.status == "activation_failed" {
            let activation_error = config_reload
                .last_error
                .clone()
                .unwrap_or_else(|| "query service activation failed".to_owned());
            write_daemon_lifecycle_status_with_runtime(
                data_root,
                &args,
                "failed",
                started_at_ms,
                Some(utc_now().timestamp_millis()),
                Some(activation_error.clone()),
                false,
                &config_reload.to_json(),
            )?;
            return Err(anyhow!(
                "activate daemon control service: {activation_error}"
            ));
        }
        #[cfg(test)]
        fail_daemon_before_ready_for_test(data_root)?;
        if !runtime.config.daemon.mode.runs_only_source_refresh() {
            resume_completed_installation_daemons(data_root)?;
        }
        write_daemon_lifecycle_status_with_runtime(
            data_root,
            &args,
            "running",
            started_at_ms,
            None,
            None,
            daemon_semantic_runtime_active(&runtime, query_service.as_ref()),
            &config_reload.to_json(),
        )?;
        let mut file_watcher = match DaemonFileWatcher::start(data_root, Arc::clone(&wakeup)) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                write_degraded_wakeup_receipt(data_root, &error)?;
                None
            }
        };
        // The daemon is ready only after every fallible lifecycle and runtime
        // initialization step has succeeded. Publish that status before
        // acknowledging any durable restart request so every parent observes
        // the same authoritative readiness condition.
        super::daemon_autostart::acknowledge_daemon_restart_requests(data_root);
        // This is the sole automatic scheduler authority. The first tick is
        // after readiness; later ticks only revisit installation-scoped
        // cadence/backoff or reconcile a completed helper.
        if daemon_should_schedule_auto_upgrade(
            runtime.config.daemon.enabled,
            runtime.config.daemon.mode,
            args.once,
        ) {
            prepared_auto_upgrade =
                crate::upgrade::prepare_daemon_auto_upgrade(data_root, &runtime.config)
                    .unwrap_or(None);
        }
        if let Some(telemetry) = telemetry.as_ref() {
            let events = telemetry.ready_events(recovered_previous_run, Instant::now());
            send_daemon_events(data_root, &events);
        }
        let mut idle_since: Option<Instant> = None;
        let mut observed_query_generation = 0;
        let mut observed_refresh_generation = 0;
        let mut next_safety_reconcile = Instant::now() + safety_interval;
        loop {
            // Hermetic callers may remove their complete temporary data root
            // while an explicitly finite test daemon is winding down. Treat
            // that as shutdown and, crucially, do not recreate the deleted
            // root merely to publish a terminal receipt.
            if !data_root.exists() {
                break;
            }
            if stop_disabled {
                break;
            }
            if reload_daemon_runtime_config(
                data_root,
                &args,
                &mut runtime,
                &mut query_service,
                &mut refresh_service,
                &mut config_reload,
                &wakeup,
            ) == DaemonConfigReloadOutcome::StopDisabled
            {
                write_daemon_lifecycle_status_with_runtime(
                    data_root,
                    &args,
                    "running",
                    started_at_ms,
                    None,
                    None,
                    false,
                    &config_reload.to_json(),
                )?;
                break;
            }
            if runtime.config.daemon.mode.runs_only_source_refresh() {
                // A live mode change must not carry a previously prepared
                // automatic upgrade into the source-refresh-only profile.
                // Dropping it retains any resumable upgrade journal for a
                // future full-mode daemon without applying it here.
                prepared_auto_upgrade = None;
            }
            write_daemon_lifecycle_status_with_runtime(
                data_root,
                &args,
                "running",
                started_at_ms,
                None,
                None,
                daemon_semantic_runtime_active(&runtime, query_service.as_ref()),
                &config_reload.to_json(),
            )?;
            if prepared_auto_upgrade.is_none()
                && daemon_should_schedule_auto_upgrade(
                    runtime.config.daemon.enabled,
                    runtime.config.daemon.mode,
                    args.once,
                )
            {
                prepared_auto_upgrade =
                    crate::upgrade::prepare_daemon_auto_upgrade(data_root, &runtime.config)
                        .unwrap_or(None);
            }
            if prepared_auto_upgrade.is_none()
                && !runtime.config.daemon.mode.runs_only_source_refresh()
            {
                resume_completed_installation_daemons(data_root)?;
            }
            if prepared_auto_upgrade.is_some()
                || daemon_upgrade_handoff_blocks_current_process(data_root)
                || installation_upgrade_blocks_current_process(data_root)
            {
                break;
            }
            observe_daemon_query_activity(
                query_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                &mut idle_since,
                &mut observed_query_generation,
            );
            observe_daemon_query_activity(
                refresh_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                &mut idle_since,
                &mut observed_refresh_generation,
            );
            if let Some(telemetry) = telemetry.as_mut() {
                let events = telemetry.liveness_events(Instant::now());
                send_daemon_events(data_root, &events);
            }
            let retry_due = !runtime.config.daemon.mode.runs_only_source_refresh()
                && daemon_retry_due(&runtime);
            let source_refresh_pending = refresh_service
                .as_ref()
                .is_some_and(|service| service.source_refresh.has_pending_request());
            // Retry and queued-refresh state describe future scheduler work,
            // not work currently executing. An explicit finite daemon must
            // still attempt shutdown once its idle lifetime expires; the
            // generation-aware service gate below protects active requests,
            // and refresh/publication work runs synchronously between gates.
            if daemon_should_attempt_finite_idle_shutdown(
                idle_exit,
                idle_since,
                retry_due,
                source_refresh_pending,
            ) {
                if daemon_services_can_begin_idle_shutdown(
                    query_service.as_ref(),
                    observed_query_generation,
                    refresh_service.as_ref(),
                    observed_refresh_generation,
                ) {
                    break;
                }
                observe_daemon_query_activity(
                    query_service
                        .as_ref()
                        .map(|service| service.activity.as_ref()),
                    &mut idle_since,
                    &mut observed_query_generation,
                );
                observe_daemon_query_activity(
                    refresh_service
                        .as_ref()
                        .map(|service| service.activity.as_ref()),
                    &mut idle_since,
                    &mut observed_refresh_generation,
                );
                continue;
            }
            let cycle_started = Instant::now();
            let semantic_runtime_active =
                daemon_semantic_runtime_active(&runtime, query_service.as_ref());
            let mut iteration = run_daemon_once_with_activity(
                &args,
                data_root,
                &mut runtime,
                None,
                semantic_runtime_active,
                query_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                refresh_service
                    .as_ref()
                    .map(|service| service.source_refresh.as_ref()),
            )?;
            let cycle_duration = cycle_started.elapsed();
            let iteration_events =
                daemon_iteration_events(telemetry.as_mut(), &mut iteration, cycle_duration);
            send_daemon_events(data_root, &iteration_events);
            wakeup.record_cycle(iteration.did_work);
            write_daemon_lifecycle_status_with_runtime(
                data_root,
                &args,
                "running",
                started_at_ms,
                None,
                None,
                daemon_semantic_runtime_active(&runtime, query_service.as_ref()),
                &config_reload.to_json(),
            )?;
            failed |= iteration.failed;
            if run_once {
                break;
            }
            observe_daemon_query_activity(
                query_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                &mut idle_since,
                &mut observed_query_generation,
            );
            observe_daemon_query_activity(
                refresh_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                &mut idle_since,
                &mut observed_refresh_generation,
            );
            if idle_since.is_none() {
                // A blocking persistent loop has no synthetic follow-up tick
                // after productive work. Start the explicit finite-idle clock
                // here so --idle-exit-seconds/test daemons can still stop
                // after their last work cycle without scheduler polling.
                idle_since = Some(Instant::now());
            }
            let now = Instant::now();
            let mut wait_for = next_safety_reconcile.saturating_duration_since(now);
            if let Some(retry_after_ms) = runtime.history_retry.retry_after_ms() {
                wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
            }
            if let Some(retry_after_ms) = runtime.semantic_retry.retry_after_ms() {
                wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
            }
            if let Some(retry_after_ms) = runtime.relational_retry.retry_after_ms() {
                wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
            }
            if let Some(retry_after_ms) = runtime.pro_retry.retry_after_ms() {
                wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
            }
            if let (Some(idle), Some(limit)) = (idle_since, idle_exit) {
                wait_for = wait_for.min(limit.saturating_sub(idle.elapsed()));
            }
            let wake = wakeup.wait(wait_for);
            if wake.shutdown {
                break;
            }
            let safety_due = wake.timed_out && Instant::now() >= next_safety_reconcile;
            if safety_due {
                next_safety_reconcile = Instant::now() + safety_interval;
            }
            if wake.filesystem || safety_due {
                if let Some(source_refresh) = refresh_service
                    .as_ref()
                    .map(|service| service.source_refresh.as_ref())
                {
                    let _ = source_refresh.enqueue_periodic(data_root);
                }
            }
            if (wake.filesystem || safety_due)
                && file_watcher
                    .as_mut()
                    .is_some_and(|watcher| watcher.reconcile().is_err())
            {
                // A native watch can become temporarily inaccessible. Keep
                // the last good watch set and let the bounded safety
                // reconciliation retry it.
            }
            if let Some(watcher) = file_watcher.as_ref() {
                let _ = watcher.write_receipt(if safety_due || wake.filesystem {
                    "active"
                } else {
                    "idle"
                });
            }
            if daemon_upgrade_handoff_blocks_current_process(data_root)
                || installation_upgrade_blocks_current_process(data_root)
            {
                break;
            }
        }

        if let Some(attempt_id) = prepared_auto_upgrade
            .as_ref()
            .and_then(crate::upgrade::PreparedDaemonUpgrade::attempt_id)
        {
            auto_upgrade_handoff = Some(
                super::daemon_autostart::begin_current_daemon_upgrade_handoff(
                    data_root,
                    attempt_id,
                    upgrade_restart_trigger,
                )?,
            );
        }
        let failure_message = failed.then(|| {
            let source_backed =
                read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root));
            source_backed
                .as_ref()
                .and_then(|job| job.get("last_error"))
                .and_then(Value::as_str)
                .filter(|error| !error.is_empty())
                .map(|error| format!("source-backed refresh failed: {error}"))
                .unwrap_or_else(|| "one or more daemon jobs failed".to_owned())
        });
        if data_root.exists() {
            write_daemon_lifecycle_status_with_runtime(
                data_root,
                &args,
                if failed { "failed" } else { "completed" },
                started_at_ms,
                Some(utc_now().timestamp_millis()),
                failure_message,
                daemon_semantic_runtime_active(&runtime, query_service.as_ref()),
                &config_reload.to_json(),
            )?;
        }
        // Keep daemon ownership until the query service has removed its endpoint
        // and joined its listener thread. Otherwise a replacement can publish a
        // new endpoint that this service's destructor then removes.
        drop(file_watcher);
        drop(query_service);
        drop(refresh_service);
        Ok(failed)
    })();

    let failed = match active_result {
        Ok(failed) => failed,
        Err(error) => {
            drop(installation_daemon_lease);
            drop(lock);
            if let Some(telemetry) = telemetry.as_mut() {
                let events = telemetry.fatal_events(Instant::now());
                send_daemon_events(data_root, &events);
            }
            return Err(error);
        }
    };
    let upgrade_attempt_id = prepared_auto_upgrade
        .as_ref()
        .and_then(crate::upgrade::PreparedDaemonUpgrade::attempt_id)
        .map(str::to_owned)
        .or(crate::upgrade::active_installation_upgrade_attempt_id()?);
    if let Some(attempt_id) = upgrade_attempt_id.as_deref() {
        installation_daemon_lease.acknowledge(attempt_id)?;
    } else {
        drop(installation_daemon_lease);
    }
    drop(lock);
    if let Some(handoff) = auto_upgrade_handoff.as_ref() {
        handoff.wait_for_installation_quiescence()?;
    }
    if let Some(telemetry) = telemetry.as_mut() {
        let events = telemetry.stopped_events(failed, Instant::now());
        send_daemon_events(data_root, &events);
    }
    if let Some(prepared) = prepared_auto_upgrade {
        crate::upgrade::finish_daemon_auto_upgrade(
            prepared,
            (
                upgrade_restart_trigger.as_str(),
                idle_exit
                    .map(|duration| duration.as_secs())
                    .unwrap_or(super::runtime_limits::DAEMON_IDLE_EXIT_SECONDS_CAP),
                safety_interval.as_secs(),
            ),
            auto_upgrade_handoff,
        )?;
    }
    Ok(daemon_report_with_disabled_status(data_root, !args.force))
}

fn daemon_should_schedule_auto_upgrade(
    daemon_enabled: bool,
    daemon_mode: DaemonMode,
    run_once: bool,
) -> bool {
    daemon_enabled && daemon_mode == DaemonMode::Full && !run_once
}

#[cfg(test)]
fn fail_daemon_before_ready_for_test(data_root: &Path) -> Result<()> {
    if data_root
        .join(".fail-daemon-before-ready-for-test")
        .exists()
    {
        return Err(anyhow!("injected daemon failure before readiness"));
    }
    Ok(())
}

fn daemon_services_can_begin_idle_shutdown(
    query_service: Option<&DaemonQueryService>,
    observed_query_generation: u64,
    refresh_service: Option<&DaemonQueryService>,
    observed_refresh_generation: u64,
) -> bool {
    let refresh_activity = refresh_service.map(|service| service.activity.as_ref());
    if !daemon_can_begin_idle_shutdown(refresh_activity, observed_refresh_generation) {
        return false;
    }
    if daemon_can_begin_idle_shutdown(
        query_service.map(|service| service.activity.as_ref()),
        observed_query_generation,
    ) {
        return true;
    }
    if let Some(activity) = refresh_activity {
        activity.resume_accepting();
    }
    false
}

fn daemon_should_attempt_finite_idle_shutdown(
    idle_exit: Option<StdDuration>,
    idle_since: Option<Instant>,
    _retry_due: bool,
    _source_refresh_pending: bool,
) -> bool {
    idle_exit.is_some_and(|limit| idle_since.is_some_and(|idle| idle.elapsed() >= limit))
}

fn installation_upgrade_blocks_current_process(data_root: &Path) -> bool {
    !super::daemon_autostart::current_process_owns_daemon_upgrade_handoff(data_root)
        && crate::upgrade::installation_upgrade_is_active().unwrap_or(false)
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod telemetry_tests;
