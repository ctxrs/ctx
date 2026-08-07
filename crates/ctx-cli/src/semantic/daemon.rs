use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use ctx_semantic_model::SharedSemanticRuntime;
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

use super::source_backed_refresh_coordinator::CoreRefreshEngine;

use super::{
    daemon_autostart::{
        current_process_owns_daemon_upgrade_handoff, daemon_upgrade_handoff_blocks_current_process,
        resume_completed_installation_daemons, terminate_current_executable_daemon,
        InstallationDaemonLease,
    },
    daemon_retry::DaemonRetryBackoff,
    daemon_scheduler::{
        daemon_retry_due, daemon_run_start_mode, daemon_scheduled_refresh_due,
        preserve_daemon_background_refresh_recovery_provenance,
        restore_daemon_background_refresh_cadence, restore_daemon_consumer_retries,
        restore_daemon_source_refresh_retry, run_daemon_scheduler_cycle_with_activity,
        DaemonBackgroundRefreshCadence, DaemonConsumerRetryDeferral, DaemonSidecarDrain,
    },
    daemon_status::{
        daemon_report_failure_message, render_daemon_disable_receipt, render_daemon_enable_receipt,
        render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
    },
    daemon_wakeup::{DaemonWakeup, SourceWatchBatch},
    daemon_worker::write_daemon_lifecycle_status_with_runtime,
    health_search::semantic_env_flag,
    paths_status::{
        daemon_core_refresh_job_path, daemon_lock_is_active, daemon_report,
        daemon_report_with_disabled_status, read_daemon_job_status, read_daemon_status,
        write_daemon_status, DaemonLock,
    },
    query_service::{
        daemon_can_begin_idle_shutdown, daemon_service_endpoint_path,
        daemon_source_refresh_request, observe_daemon_query_activity,
        read_daemon_service_endpoint_identity, DaemonIpcService, DaemonQueryEndpoint,
        DaemonQueryService,
    },
    runtime_limits::DAEMON_BACKGROUND_CHILD_ENV,
};
use crate::ui::Ui;

mod config_reload;
mod control;
mod lifecycle;
mod telemetry;
mod watch_runtime;

use config_reload::{
    daemon_semantic_runtime_active, reload_daemon_runtime_config, DaemonConfigReloadOutcome,
    DaemonConfigReloadState,
};
use control::daemon_run_facts;
pub(crate) use control::run_daemon_command;
use lifecycle::*;
use telemetry::{daemon_safety_reconcile_interval, send_daemon_events, DaemonTelemetry};
use watch_runtime::{DaemonWatchRuntime, WatchCatalogReconcileTrigger};

#[cfg(test)]
use super::daemon_wakeup::DaemonFileWatcher;
#[cfg(test)]
use ctx_history_capture::SourceBackedWatchCatalog;

#[cfg(test)]
use config_reload::daemon_semantic_runtime_requested;
#[cfg(test)]
use control::remove_released_daemon_service_artifacts;
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
    pub(super) continue_immediately: bool,
    pub(super) provider_refresh_events: Vec<PublicEventV1>,
}

impl DaemonIteration {
    pub(super) fn new(did_work: bool, failed: bool, telemetry_state: DaemonCycleStateV1) -> Self {
        Self {
            did_work,
            failed,
            telemetry_state,
            continue_immediately: false,
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
    pub(super) source_refresh_coordinator: Option<Arc<CoreRefreshEngine>>,
    pub(super) history_retry: DaemonRetryBackoff,
    pub(super) pro_retry: DaemonRetryBackoff,
    pub(super) semantic_retry: DaemonRetryBackoff,
    pub(super) semantic_blocked_job: Option<Value>,
    pub(super) sidecar_drain: DaemonSidecarDrain,
    pub(super) consumer_retry_deferral: DaemonConsumerRetryDeferral,
    pub(super) background_refresh_cadence: DaemonBackgroundRefreshCadence,
    pub(super) config: AppConfig,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct DaemonTestJobHooks {
    pub(super) calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
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
            "semantic_index" => hooks.semantic_index.clone(),
            _ => None,
        }
    })
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
        if !args.format.is_json() {
            return Err(crate::dispatch::rendered_cli_error());
        }
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
    if installation_lifecycle_blocks_current_process(data_root) {
        return Ok(daemon_report(data_root));
    }
    if daemon_upgrade_handoff_blocks_current_process(data_root) {
        return Ok(daemon_report(data_root));
    }
    let Some(lock) = DaemonLock::acquire(data_root)? else {
        return Ok(daemon_report(data_root));
    };
    // Close the check/acquire race with an installation lifecycle owner that
    // fenced daemon starts after the first observation but before this process
    // acquired ownership.
    if daemon_upgrade_handoff_blocks_current_process(data_root)
        || installation_lifecycle_blocks_current_process(data_root)
    {
        drop(lock);
        return Ok(daemon_report(data_root));
    }
    let run_started = Instant::now();
    let started_at_ms = utc_now().timestamp_millis();
    let recovered_previous_run =
        daemon_previous_status_needs_recovery(read_daemon_status(data_root).as_ref());
    let mut telemetry =
        DaemonTelemetry::new(daemon_run_facts(&args), run_started, started_at_ms as u64);
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
        // Recover the durable queue into the exact coordinator that will own
        // IPC before publishing the source-refresh endpoint. Otherwise a
        // restart-time admission could overwrite the unrecovered queue root.
        recover_source_refresh_coordinator_before_ipc(&mut runtime, data_root)?;
        let stop_disabled = reload_daemon_runtime_config(
            data_root,
            &args,
            &mut runtime,
            &mut query_service,
            &mut refresh_service,
            &mut config_reload,
            &wakeup,
        ) == DaemonConfigReloadOutcome::StopDisabled;
        install_source_watch_ingress(&wakeup, refresh_service.as_ref());
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
        ensure_daemon_ipc_services_healthy(query_service.as_ref(), refresh_service.as_ref())?;
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
        let mut watch_runtime = DaemonWatchRuntime::new(Arc::clone(&wakeup));
        watch_runtime.reconcile_catalog_and_route_authority(
            data_root,
            refresh_service
                .as_ref()
                .map(|service| service.source_refresh.as_ref()),
            WatchCatalogReconcileTrigger::Startup,
            false,
        );
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
        ) {
            prepared_auto_upgrade =
                crate::upgrade::prepare_daemon_auto_upgrade(data_root, &runtime.config)
                    .unwrap_or(None);
        }
        let events = telemetry.ready_events(recovered_previous_run, Instant::now());
        send_daemon_events(data_root, &events);
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
            ensure_daemon_ipc_services_healthy(query_service.as_ref(), refresh_service.as_ref())?;
            install_source_watch_ingress(&wakeup, refresh_service.as_ref());
            if runtime.config.daemon.mode.runs_only_source_refresh() {
                // A live mode change must not carry a previously prepared
                // automatic upgrade into the source-refresh-only profile.
                // Dropping it retains any resumable upgrade journal for a
                // future full-mode daemon without applying it here.
                prepared_auto_upgrade = None;
            }
            if let Some(source_refresh) = refresh_service
                .as_ref()
                .map(|service| service.source_refresh.as_ref())
                .filter(|refresh| !refresh.watch_routes_initialized())
            {
                watch_runtime.reconcile_catalog_and_route_authority(
                    data_root,
                    Some(source_refresh),
                    WatchCatalogReconcileTrigger::RuntimeActivation,
                    false,
                );
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
                || installation_lifecycle_blocks_current_process(data_root)
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
            let events = telemetry.liveness_events(Instant::now());
            send_daemon_events(data_root, &events);
            let retry_due = !runtime.config.daemon.mode.runs_only_source_refresh()
                && daemon_retry_due(&runtime);
            let source_refresh_pending = refresh_service.as_ref().is_some_and(|service| {
                service.source_refresh.has_pending_request()
                    || service.source_refresh.has_scheduled_route_work()
            });
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
            let mut iteration = run_daemon_scheduler_cycle_with_activity(
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
            let continue_immediately = iteration.continue_immediately;
            let cycle_duration = cycle_started.elapsed();
            let iteration_events =
                daemon_iteration_events(Some(&mut telemetry), &mut iteration, cycle_duration);
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
            if continue_immediately {
                continue;
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
            let wait_for = daemon_wait_duration(
                &runtime,
                refresh_service
                    .as_ref()
                    .map(|service| service.source_refresh.as_ref()),
                next_safety_reconcile,
                idle_since,
                idle_exit,
                now,
            );
            let wake = wakeup.wait(wait_for);
            if wake.shutdown {
                break;
            }
            let safety_due = wake.timed_out && Instant::now() >= next_safety_reconcile;
            let retry_wakeup_due = wake.timed_out && daemon_retry_due(&runtime);
            if retry_wakeup_due {
                wakeup.record_scheduled_retry_wakeup();
            }
            let source_refresh = refresh_service
                .as_ref()
                .map(|service| service.source_refresh.as_ref());
            let scheduled_refresh_wakeup_due = wake.timed_out
                && !retry_wakeup_due
                && daemon_scheduled_refresh_due(
                    &runtime,
                    source_refresh,
                    Instant::now(),
                    source_route_ledger_now_ms(),
                );
            if scheduled_refresh_wakeup_due {
                wakeup.record_scheduled_refresh_wakeup();
            }
            let source_retry_due = retry_wakeup_due
                && runtime.history_retry.consecutive_failures > 0
                && runtime.history_retry.ready();
            if safety_due {
                next_safety_reconcile = Instant::now() + safety_interval;
            }
            let watch_reconcile_trigger = wake
                .source_watch
                .reconcile
                .map(WatchCatalogReconcileTrigger::CatalogControl)
                .or_else(|| safety_due.then_some(WatchCatalogReconcileTrigger::SafetyTimeout))
                .or_else(|| {
                    wake.source_watch
                        .rearm
                        .then_some(WatchCatalogReconcileTrigger::WatcherRecovery)
                })
                .or_else(|| {
                    wake.filesystem
                        .then_some(WatchCatalogReconcileTrigger::Filesystem)
                });
            if let Some(trigger) = watch_reconcile_trigger {
                watch_runtime.reconcile_catalog_and_route_authority(
                    data_root,
                    source_refresh,
                    trigger,
                    wake.source_watch.rearm,
                );
            }
            if let Some(source_refresh) = source_refresh {
                source_refresh
                    .record_watch_routes(wake.source_watch.routes, source_route_ledger_now_ms());
            }
            if let Some(watcher) = watch_runtime.file_watcher.as_ref() {
                let _ = watcher.write_receipt(
                    if safety_due
                        || wake.filesystem
                        || source_retry_due
                        || scheduled_refresh_wakeup_due
                    {
                        "active"
                    } else {
                        "idle"
                    },
                );
            }
            if daemon_upgrade_handoff_blocks_current_process(data_root)
                || installation_lifecycle_blocks_current_process(data_root)
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
            let core_refresh = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
            core_refresh
                .as_ref()
                .and_then(|job| job.get("last_error"))
                .and_then(Value::as_str)
                .filter(|error| !error.is_empty())
                .map(|error| format!("Core refresh failed: {error}"))
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
        drop(watch_runtime);
        drop(query_service);
        drop(refresh_service);
        Ok(failed)
    })();

    let failed = match active_result {
        Ok(failed) => failed,
        Err(error) => {
            drop(installation_daemon_lease);
            drop(lock);
            let events = telemetry.fatal_events(Instant::now());
            send_daemon_events(data_root, &events);
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
    let events = telemetry.stopped_events(failed, Instant::now());
    send_daemon_events(data_root, &events);
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

fn recover_source_refresh_before_background_cadence(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<()> {
    if let Some(source_refresh) = source_refresh {
        preserve_daemon_background_refresh_recovery_provenance(data_root)
            .context("preserve automatic Core refresh provenance before recovery")?;
        source_refresh
            .recover_interrupted_publication(data_root)
            .context("recover interrupted Core refresh before daemon readiness")?;
    }
    // Recovery replaces the original trigger with `recovery`. Restore only
    // after that transition, using the request-bound automatic provenance
    // preserved above, so restart cannot erase background rest.
    restore_daemon_background_refresh_cadence(runtime, data_root);
    Ok(())
}

fn recover_source_refresh_coordinator_before_ipc(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
) -> Result<Arc<CoreRefreshEngine>> {
    let source_refresh = Arc::new(super::source_backed_refresh_adapter::refresh_engine());
    recover_source_refresh_before_background_cadence(
        runtime,
        data_root,
        Some(source_refresh.as_ref()),
    )?;
    runtime.source_refresh_coordinator = Some(Arc::clone(&source_refresh));
    Ok(source_refresh)
}

pub(super) fn daemon_wait_duration(
    runtime: &DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    next_safety_reconcile: Instant,
    idle_since: Option<Instant>,
    idle_exit: Option<StdDuration>,
    now: Instant,
) -> StdDuration {
    if runtime.history_retry.ready()
        && source_refresh.is_some_and(CoreRefreshEngine::has_pending_request)
    {
        return StdDuration::ZERO;
    }
    let mut wait_for = next_safety_reconcile.saturating_duration_since(now);
    if let Some(remaining) = runtime.consumer_retry_deferral.remaining(now) {
        wait_for = wait_for.min(remaining);
    } else {
        if let Some(retry_after_ms) = runtime.history_retry.retry_after_ms() {
            wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
        }
        if let Some(retry_after_ms) = runtime.semantic_retry.retry_after_ms() {
            wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
        }
        if let Some(retry_after_ms) = runtime.pro_retry.retry_after_ms() {
            wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
        }
    }
    if let (Some(idle), Some(limit)) = (idle_since, idle_exit) {
        wait_for = wait_for.min(limit.saturating_sub(now.saturating_duration_since(idle)));
    }
    if let Some(route_due_ms) = source_refresh
        .and_then(|refresh| refresh.next_dirty_route_due_in_ms(source_route_ledger_now_ms()))
    {
        let route_wait = StdDuration::from_millis(route_due_ms);
        let cadence_wait = runtime
            .background_refresh_cadence
            .remaining(now)
            .unwrap_or_default();
        wait_for = wait_for.min(route_wait.max(cadence_wait));
    }
    wait_for
}

fn install_source_watch_ingress(
    wakeup: &DaemonWakeup,
    refresh_service: Option<&DaemonQueryService>,
) {
    if wakeup.has_source_watch_sink() {
        return;
    }
    let Some(source_refresh) = refresh_service.map(|service| Arc::clone(&service.source_refresh))
    else {
        return;
    };
    wakeup.install_source_watch_sink(Arc::new(move |batch: &SourceWatchBatch| {
        source_refresh.record_watch_routes(
            batch
                .routes
                .iter()
                .map(|(route, watermark)| (route.clone(), *watermark)),
            source_route_ledger_now_ms(),
        );
    }));
}

fn source_route_ledger_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod telemetry_tests;
