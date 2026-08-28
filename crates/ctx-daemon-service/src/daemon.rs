use std::{
    path::Path,
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use ctx_semantic_model::SharedSemanticRuntime;
use ctx_upgrade_engine::{DaemonUpgradeLease, DaemonUpgradePort, PreparedAutomaticUpgrade};
use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use crate::{
    analytics::{
        DaemonCycleStateV1, DaemonRunFactsV1, DaemonStartModeV1, DaemonSupervisorV1,
        DaemonTriggerV1, PublicEventV1,
    },
    config::{AppConfig, DaemonMode},
    CoreGenerationPublishedPort, DaemonAvailabilityPort, DaemonConfigPort, DaemonInstallationLease,
    DaemonInstallationPort, DaemonObservationPort, DaemonRunArgs, DaemonRunProfile,
    DaemonServicePorts, DaemonStartModeArg, DaemonTriggerCommandArg, DaemonUpgradePorts,
};

use super::source_backed_refresh_coordinator::CoreRefreshEngine;

use super::{
    daemon_process_signal::install_daemon_process_signal_handler,
    daemon_retry::DaemonRetryBackoff,
    daemon_scheduler::{
        daemon_retry_due, daemon_run_start_mode, daemon_scheduled_refresh_due,
        restore_daemon_consumer_retries, restore_daemon_source_refresh_retry,
        run_daemon_scheduler_cycle_with_activity, DaemonConsumerRetryDeferral,
        DaemonSchedulerCycleContext, DaemonSchedulerPorts, DaemonSemanticJobPorts,
        DaemonSidecarDrain,
    },
    daemon_wakeup::DaemonWakeup,
    daemon_worker::write_daemon_lifecycle_status_with_runtime,
    paths_status::{
        daemon_core_refresh_job_path, read_daemon_job_status, read_daemon_status, DaemonLock,
    },
    query_service::{DaemonLifecycleState, DaemonQueryService},
};

mod automatic_upgrade;
mod config_reload;
mod lifecycle;
mod source_watch;
mod telemetry;
mod watch_runtime;

use automatic_upgrade::abort_prepared_automatic_upgrade;
use config_reload::{
    daemon_semantic_runtime_active, reload_daemon_runtime_config, DaemonConfigReloadOutcome,
    DaemonConfigReloadState, DaemonConfigReloadTargets,
};
use lifecycle::*;
pub(super) use source_watch::daemon_wait_duration;
use source_watch::{
    daemon_scheduler_source_refresh, install_source_watch_ingress, source_route_ledger_now_ms,
};
use telemetry::{daemon_safety_reconcile_interval, send_daemon_events, DaemonTelemetry};
use watch_runtime::{DaemonWatchRuntime, WatchCatalogReconcileTrigger};

#[cfg(test)]
use super::daemon_wakeup::DaemonFileWatcher;
#[cfg(test)]
use ctx_history_capture::SourceBackedWatchCatalog;

#[cfg(test)]
use config_reload::daemon_semantic_runtime_requested;
#[cfg(test)]
use telemetry::{
    daemon_liveness_interval, DAEMON_LIVENESS_JITTER_WINDOW, DAEMON_LIVENESS_MIN_INTERVAL,
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
    pub(super) semantic_retry: DaemonRetryBackoff,
    pub(super) semantic_blocked_job: Option<Value>,
    pub(super) sidecar_drain: DaemonSidecarDrain,
    pub(super) consumer_retry_deferral: DaemonConsumerRetryDeferral,
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

#[cfg(test)]
pub(super) fn daemon_iteration_events_without_telemetry(
    iteration: &mut DaemonIteration,
    duration: StdDuration,
) -> Vec<PublicEventV1> {
    daemon_iteration_events(None, iteration, duration)
}

fn daemon_run_facts(args: &DaemonRunArgs) -> DaemonRunFactsV1 {
    let start_mode = match daemon_run_start_mode(args) {
        DaemonStartModeArg::Auto => DaemonStartModeV1::Auto,
        DaemonStartModeArg::Manual => DaemonStartModeV1::Manual,
    };
    let supervisor = match args.supervisor {
        crate::DaemonSupervisor::User => DaemonSupervisorV1::User,
        crate::DaemonSupervisor::CliAutostart => DaemonSupervisorV1::CliAutostart,
    };
    let trigger = args.trigger_command.map(|trigger| match trigger {
        DaemonTriggerCommandArg::Setup => DaemonTriggerV1::Setup,
        DaemonTriggerCommandArg::Import => DaemonTriggerV1::Import,
        DaemonTriggerCommandArg::Search => DaemonTriggerV1::Search,
        DaemonTriggerCommandArg::Semantic => DaemonTriggerV1::Semantic,
    });
    DaemonRunFactsV1::new(start_mode, supervisor, trigger)
}

pub fn run_daemon<I, N, D, AP, UO>(
    args: DaemonRunArgs,
    data_root: &Path,
    config: AppConfig,
    ports: &DaemonServicePorts<
        'static,
        dyn DaemonConfigPort,
        dyn DaemonAvailabilityPort,
        I,
        N,
        dyn DaemonObservationPort,
    >,
    upgrade: &DaemonUpgradePorts<'_, D, AP, UO>,
) -> Result<()>
where
    I: DaemonInstallationPort,
    N: CoreGenerationPublishedPort + ?Sized,
    D: DaemonUpgradePort + ?Sized,
    AP: ctx_upgrade_engine::AutomaticUpgradePolicyProvider<Snapshot = AppConfig>,
    UO: ctx_upgrade_engine::UpgradeObserver<AppConfig>,
{
    run_daemon_inner(args, data_root, config, ports, upgrade)
}

fn publish_daemon_fatal_status_while_owned(
    _lock: &DaemonLock,
    data_root: &Path,
    args: &DaemonRunArgs,
    started_at_ms: i64,
    error: &anyhow::Error,
) {
    let config_reload = read_daemon_status(data_root)
        .as_ref()
        .and_then(|status| status.get("config_reload"))
        .cloned()
        .unwrap_or(Value::Null);
    let _ = write_daemon_lifecycle_status_with_runtime(
        data_root,
        args,
        "failed",
        started_at_ms,
        Some(utc_now().timestamp_millis()),
        Some(format!("{error:#}")),
        false,
        &config_reload,
    );
}

fn run_daemon_inner<I, N, D, AP, UO>(
    args: DaemonRunArgs,
    data_root: &Path,
    mut config: AppConfig,
    ports: &DaemonServicePorts<
        'static,
        dyn DaemonConfigPort,
        dyn DaemonAvailabilityPort,
        I,
        N,
        dyn DaemonObservationPort,
    >,
    upgrade: &DaemonUpgradePorts<'_, D, AP, UO>,
) -> Result<()>
where
    I: DaemonInstallationPort,
    N: CoreGenerationPublishedPort + ?Sized,
    D: DaemonUpgradePort + ?Sized,
    AP: ctx_upgrade_engine::AutomaticUpgradePolicyProvider<Snapshot = AppConfig>,
    UO: ctx_upgrade_engine::UpgradeObserver<AppConfig>,
{
    let finite_core_worker = args.profile == DaemonRunProfile::FiniteCoreWorker;
    if finite_core_worker {
        config.daemon.mode = DaemonMode::SourceRefreshOnly;
        config.semantic_enabled = false;
    }
    if !config.daemon.enabled && !args.force && !finite_core_worker {
        return Ok(());
    }
    let automatic_recovery_allowed = daemon_automatic_recovery_allowed(&config, finite_core_worker);
    if ports
        .installation
        .lifecycle_blocks_current_process(data_root, automatic_recovery_allowed)
    {
        return Ok(());
    }
    if ports
        .installation
        .upgrade_handoff_blocks_current_process(data_root)
    {
        return Ok(());
    }
    let Some(lock) = DaemonLock::acquire(data_root)? else {
        return Ok(());
    };
    // Close the check/acquire race with an installation lifecycle owner that
    // fenced daemon starts after the first observation but before this process
    // acquired ownership.
    if ports
        .installation
        .upgrade_handoff_blocks_current_process(data_root)
        || ports
            .installation
            .lifecycle_blocks_current_process(data_root, automatic_recovery_allowed)
    {
        drop(lock);
        return Ok(());
    }
    let run_started = Instant::now();
    // Status and the advisory lock describe one lifecycle owner. Reuse the
    // lock's timestamp so observers can compare the complete owner identity
    // without accepting two independently sampled clocks as equivalent.
    let started_at_ms = lock
        .started_at_ms()
        .ok_or_else(|| anyhow!("active daemon lock is missing its start timestamp"))?;
    let recovered_previous_run =
        daemon_previous_status_needs_recovery(read_daemon_status(data_root).as_ref());
    let mut telemetry =
        DaemonTelemetry::new(daemon_run_facts(&args), run_started, started_at_ms as u64);
    let safety_interval = args.loop_interval_seconds.map_or_else(
        || daemon_safety_reconcile_interval(started_at_ms as u64),
        StdDuration::from_secs,
    );
    let upgrade_restart_trigger = args
        .trigger_command
        .unwrap_or(DaemonTriggerCommandArg::Search);
    let installation_daemon_lease = match ports.installation.acquire(
        data_root,
        upgrade_restart_trigger,
        args.loop_interval_seconds,
        ports
            .installation
            .current_process_owns_upgrade_handoff(data_root),
        automatic_recovery_allowed,
        !finite_core_worker,
    ) {
        Ok(Some(lease)) => Some(lease),
        Ok(None) => {
            drop(lock);
            return Ok(());
        }
        Err(error) => {
            publish_daemon_fatal_status_while_owned(&lock, data_root, &args, started_at_ms, &error);
            drop(lock);
            return Err(error);
        }
    };
    let mut prepared_auto_upgrade = None;
    let mut auto_upgrade_handoff = None;
    let wakeup = Arc::new(DaemonWakeup::default());
    let lifecycle_state = Arc::new(DaemonLifecycleState::starting());
    if args.handle_process_signals {
        install_daemon_process_signal_handler(Arc::clone(&wakeup), Arc::clone(&lifecycle_state))?;
    }
    let active_result = (|| -> Result<bool> {
        let mut failed = false;
        let mut runtime = DaemonRuntime {
            config: config.clone(),
            ..DaemonRuntime::default()
        };
        let mut config_reload = DaemonConfigReloadState::pending(&config);
        let mut query_service = None;
        let mut refresh_service = None;
        let _lifecycle_stopping = lifecycle_state.stopping_guard();
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
        if finite_core_worker {
            restore_daemon_source_refresh_retry(&mut runtime, data_root);
        } else if !runtime.config.daemon.mode.runs_only_source_refresh() {
            restore_daemon_source_refresh_retry(&mut runtime, data_root);
            restore_daemon_consumer_retries(&mut runtime, data_root);
        }
        // Recover the durable queue into the exact coordinator that will own
        // IPC before publishing the source-refresh endpoint. Otherwise a
        // restart-time admission could overwrite the unrecovered queue root.
        recover_source_refresh_coordinator_before_ipc(&mut runtime, data_root, ports.config)?;
        let stop_disabled = reload_daemon_runtime_config(
            data_root,
            &args,
            &mut runtime,
            DaemonConfigReloadTargets {
                query_service: &mut query_service,
                refresh_service: &mut refresh_service,
                state: &mut config_reload,
            },
            &wakeup,
            &lifecycle_state,
            ports.config,
        ) == DaemonConfigReloadOutcome::StopDisabled;
        if !finite_core_worker {
            install_source_watch_ingress(
                &wakeup,
                refresh_service
                    .as_ref()
                    .and(runtime.source_refresh_coordinator.as_ref()),
            );
        }
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
        #[cfg(any(test, feature = "test-support"))]
        fail_daemon_before_ready_for_test(data_root)?;
        if !finite_core_worker && !runtime.config.daemon.mode.runs_only_source_refresh() {
            ports.installation.resume_completed(data_root)?;
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
        ctx_daemon_runtime::block_daemon_main_before_ready_for_test(data_root)?;
        let mut watch_runtime = (!finite_core_worker)
            .then(|| DaemonWatchRuntime::new(Arc::clone(&wakeup), ports.config));
        if let Some(watch_runtime) = watch_runtime.as_mut() {
            watch_runtime.reconcile_catalog_and_route_authority(
                data_root,
                refresh_service
                    .as_ref()
                    .and(runtime.source_refresh_coordinator.as_deref()),
                WatchCatalogReconcileTrigger::Startup,
                false,
            );
            if let (Some(source_refresh), Some(catalog)) = (
                refresh_service
                    .as_ref()
                    .and(runtime.source_refresh_coordinator.as_deref()),
                watch_runtime.catalog.snapshot(),
            ) {
                source_refresh.enqueue_overdue_hermes_exact_reconciliation(
                    data_root,
                    &catalog,
                    source_route_ledger_now_ms(),
                )?;
            }
        }
        // Linearize final handoff, Ready publication, and restart acknowledgement with durable intent.
        let lifecycle_ready = !stop_disabled
            && publish_lifecycle_ready(
                data_root,
                &lifecycle_state,
                ports.installation,
                !finite_core_worker,
            )?;
        if lifecycle_ready {
            ctx_daemon_runtime::block_daemon_main_after_ready_for_test(data_root)?;
        }
        // The ready persistent daemon is the automatic-check driver; foreground commands never are.
        if lifecycle_ready
            && !finite_core_worker
            && daemon_should_schedule_auto_upgrade(
                runtime.config.daemon.enabled,
                runtime.config.daemon.mode,
                runtime.config.automatic_upgrade_enabled,
            )
        {
            prepared_auto_upgrade = upgrade
                .engine
                .prepare_automatic(
                    upgrade.automatic_policy,
                    upgrade.observer,
                    data_root,
                    &runtime.config,
                )
                .unwrap_or(None);
        }
        if lifecycle_ready {
            let events = telemetry.ready_events(recovered_previous_run, Instant::now());
            send_daemon_events(ports.observation, data_root, &events);
        }
        let mut next_safety_reconcile = Instant::now() + safety_interval;
        // Recovery installs the coordinator once before IPC activation, and
        // configuration reload only starts or stops services around that same
        // coordinator. Retain one stable owner outside the hot scheduler loop
        // so choosing the borrowed scheduler input does not refcount-clone on
        // every iteration.
        let source_refresh_coordinator = runtime.source_refresh_coordinator.clone();
        let mut finite_core_worker_exit =
            finite_core_worker.then(|| FiniteCoreWorkerExit::new(refresh_service.as_ref()));
        loop {
            // Hermetic callers may remove their complete temporary data root
            // during shutdown. Do not recreate the deleted root merely to
            // publish a terminal receipt.
            if !data_root.exists() || !lifecycle_ready || lifecycle_state.is_stopping() {
                break;
            }
            if stop_disabled {
                break;
            }
            let reload_outcome = reload_daemon_runtime_config(
                data_root,
                &args,
                &mut runtime,
                DaemonConfigReloadTargets {
                    query_service: &mut query_service,
                    refresh_service: &mut refresh_service,
                    state: &mut config_reload,
                },
                &wakeup,
                &lifecycle_state,
                ports.config,
            );
            if !daemon_should_schedule_auto_upgrade(
                runtime.config.daemon.enabled,
                runtime.config.daemon.mode,
                runtime.config.automatic_upgrade_enabled,
            ) {
                // Live policy revocation cancels staged automatic work before handoff publication.
                if let Some(prepared) = prepared_auto_upgrade.take() {
                    let canceled =
                        anyhow!("automatic upgrade canceled after daemon maintenance was disabled");
                    prepared
                        .abort(&canceled)
                        .context("terminalize automatic upgrade after daemon policy change")?;
                }
            }
            if reload_outcome == DaemonConfigReloadOutcome::StopDisabled {
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
            if !finite_core_worker {
                install_source_watch_ingress(
                    &wakeup,
                    refresh_service
                        .as_ref()
                        .and(source_refresh_coordinator.as_ref()),
                );
            }
            if let (Some(watch_runtime), Some(source_refresh)) = (
                watch_runtime.as_mut(),
                refresh_service
                    .as_ref()
                    .and(daemon_scheduler_source_refresh(&source_refresh_coordinator))
                    .filter(|refresh| !refresh.watch_routes_initialized()),
            ) {
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
            let automatic_recovery_allowed =
                daemon_automatic_recovery_allowed(&runtime.config, finite_core_worker);
            if prepared_auto_upgrade.is_none() && automatic_recovery_allowed {
                prepared_auto_upgrade = upgrade
                    .engine
                    .prepare_automatic(
                        upgrade.automatic_policy,
                        upgrade.observer,
                        data_root,
                        &runtime.config,
                    )
                    .unwrap_or(None);
            }
            if !finite_core_worker
                && prepared_auto_upgrade.is_none()
                && !runtime.config.daemon.mode.runs_only_source_refresh()
            {
                ports.installation.resume_completed(data_root)?;
            }
            if prepared_auto_upgrade.is_some()
                || ports
                    .installation
                    .upgrade_handoff_blocks_current_process(data_root)
                || ports
                    .installation
                    .lifecycle_blocks_current_process(data_root, automatic_recovery_allowed)
            {
                break;
            }
            let events = telemetry.liveness_events(Instant::now());
            send_daemon_events(ports.observation, data_root, &events);
            let cycle_started = Instant::now();
            let semantic_runtime_active =
                daemon_semantic_runtime_active(&runtime, query_service.as_ref());
            let source_refresh = refresh_service
                .as_ref()
                .and(daemon_scheduler_source_refresh(&source_refresh_coordinator));
            if source_refresh.is_some_and(CoreRefreshEngine::has_pending_request) {
                ctx_daemon_runtime::block_daemon_main_after_ready_for_test(data_root)?;
            }
            if finite_core_worker_exit.as_mut().is_some_and(|exit| {
                exit.begin_stopping(
                    source_refresh,
                    refresh_service.as_ref(),
                    &lifecycle_state,
                    Instant::now(),
                )
            }) {
                break;
            }
            let mut iteration = run_daemon_scheduler_cycle_with_activity(
                &args,
                data_root,
                &mut runtime,
                DaemonSchedulerCycleContext {
                    deadline: None,
                    semantic_enabled: semantic_runtime_active,
                    query_activity: query_service
                        .as_ref()
                        .map(|service| service.activity.as_ref()),
                    source_refresh,
                },
                DaemonSchedulerPorts {
                    generation_published: ports.generation_published,
                    semantic: DaemonSemanticJobPorts {
                        artifact_fetcher: ports.artifact_fetcher,
                        config: ports.config,
                    },
                    observation: ports.observation,
                },
            )?;
            let continue_immediately = iteration.continue_immediately;
            let cycle_duration = cycle_started.elapsed();
            let iteration_events =
                daemon_iteration_events(Some(&mut telemetry), &mut iteration, cycle_duration);
            send_daemon_events(ports.observation, data_root, &iteration_events);
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
            if finite_core_worker {
                let pending_core_refresh = source_refresh
                    .is_some_and(|source_refresh| source_refresh.has_pending_request());
                if finite_core_worker_exit.as_mut().is_some_and(|exit| {
                    exit.begin_stopping(
                        source_refresh,
                        refresh_service.as_ref(),
                        &lifecycle_state,
                        Instant::now(),
                    )
                }) {
                    break;
                }
                if continue_immediately || (pending_core_refresh && runtime.history_retry.ready()) {
                    continue;
                }
                let now = Instant::now();
                let exit_wait = finite_core_worker_exit
                    .as_ref()
                    .map_or(StdDuration::ZERO, |exit| exit.wait_duration(now));
                let wait_for = if pending_core_refresh {
                    runtime
                        .history_retry
                        .retry_after_ms()
                        .map_or(exit_wait, |retry_after_ms| {
                            exit_wait.min(StdDuration::from_millis(retry_after_ms))
                        })
                } else {
                    exit_wait
                };
                let wake = wakeup.wait(wait_for);
                if wake.shutdown {
                    break;
                }
                if wake.timed_out {
                    wakeup.record_scheduled_retry_wakeup();
                }
                continue;
            }
            if continue_immediately {
                continue;
            }
            let now = Instant::now();
            let wait_for = daemon_wait_duration(
                &runtime,
                refresh_service
                    .as_ref()
                    .and(daemon_scheduler_source_refresh(&source_refresh_coordinator)),
                next_safety_reconcile,
                now,
            );
            let wake = wakeup.wait(wait_for);
            if wake.shutdown {
                break;
            }
            // Native activity must not starve the safety deadline. A busy or
            // degraded watcher can keep producing wakes indefinitely.
            let safety_due = Instant::now() >= next_safety_reconcile;
            let retry_wakeup_due = wake.timed_out && daemon_retry_due(&runtime);
            if retry_wakeup_due {
                wakeup.record_scheduled_retry_wakeup();
            }
            let source_refresh = refresh_service
                .as_ref()
                .and(daemon_scheduler_source_refresh(&source_refresh_coordinator));
            let scheduled_refresh_wakeup_due = wake.timed_out
                && !retry_wakeup_due
                && daemon_scheduled_refresh_due(source_refresh, source_route_ledger_now_ms());
            if scheduled_refresh_wakeup_due {
                wakeup.record_scheduled_refresh_wakeup();
            }
            let source_retry_due = retry_wakeup_due
                && runtime.history_retry.consecutive_failures > 0
                && runtime.history_retry.ready();
            let watch_runtime = watch_runtime
                .as_mut()
                .expect("persistent daemon owns watch runtime");
            if safety_due {
                next_safety_reconcile = Instant::now() + safety_interval;
                watch_runtime.reconcile_catalog_and_route_authority(
                    data_root,
                    source_refresh,
                    WatchCatalogReconcileTrigger::SafetyTimeout,
                    false,
                );
                if let (Some(source_refresh), Some(catalog)) =
                    (source_refresh, watch_runtime.catalog.snapshot())
                {
                    source_refresh.enqueue_overdue_hermes_exact_reconciliation(
                        data_root,
                        &catalog,
                        source_route_ledger_now_ms(),
                    )?;
                }
            }
            let watch_reconcile_trigger = wake
                .source_watch
                .reconcile
                .map(WatchCatalogReconcileTrigger::CatalogControl)
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
                source_refresh.record_watch_routes_with_members(
                    wake.source_watch.routes,
                    wake.source_watch.members,
                    source_route_ledger_now_ms(),
                );
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
            let automatic_recovery_allowed =
                daemon_automatic_recovery_allowed(&runtime.config, finite_core_worker);
            if ports
                .installation
                .upgrade_handoff_blocks_current_process(data_root)
                || ports
                    .installation
                    .lifecycle_blocks_current_process(data_root, automatic_recovery_allowed)
            {
                break;
            }
        }

        lifecycle_state.mark_stopping();
        if let Some(attempt_id) = prepared_auto_upgrade
            .as_ref()
            .and_then(PreparedAutomaticUpgrade::attempt_id)
        {
            auto_upgrade_handoff = Some(upgrade.daemon.begin_current(
                data_root,
                attempt_id,
                upgrade_restart_trigger.as_str(),
                args.loop_interval_seconds,
            )?);
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
                if failed { "failed" } else { "stopped" },
                started_at_ms,
                Some(utc_now().timestamp_millis()),
                failure_message,
                daemon_semantic_runtime_active(&runtime, query_service.as_ref()),
                &config_reload.to_json(),
            )?;
        }
        // Keep daemon ownership until every IPC service has removed its endpoint
        // and joined its listener thread. Otherwise a replacement can publish a
        // new endpoint that a retiring service's destructor then removes.
        drop(watch_runtime);
        drop(query_service);
        drop(refresh_service);
        Ok(failed)
    })();

    let failed = match active_result {
        Ok(failed) => failed,
        Err(error) => {
            publish_daemon_fatal_status_while_owned(&lock, data_root, &args, started_at_ms, &error);
            drop(installation_daemon_lease);
            drop(lock);
            let error = abort_prepared_automatic_upgrade(
                prepared_auto_upgrade.take(),
                auto_upgrade_handoff.take(),
                error,
            );
            let events = telemetry.fatal_events(Instant::now());
            send_daemon_events(ports.observation, data_root, &events);
            return Err(error);
        }
    };
    let owned_shutdown_result = (|| -> Result<()> {
        if let Some(installation_daemon_lease) = installation_daemon_lease {
            if finite_core_worker {
                drop(installation_daemon_lease);
                return Ok(());
            }
            let active_installation_attempt =
                ctx_upgrade_engine::active_installation_upgrade_attempt_id()?;
            let upgrade_attempt_id = prepared_auto_upgrade
                .as_ref()
                .and_then(PreparedAutomaticUpgrade::attempt_id)
                .map(str::to_owned)
                .or(active_installation_attempt);
            if let Some(attempt_id) = upgrade_attempt_id.as_deref() {
                installation_daemon_lease.acknowledge(attempt_id)?;
            } else {
                drop(installation_daemon_lease);
            }
        }
        Ok(())
    })();
    if let Err(error) = owned_shutdown_result {
        publish_daemon_fatal_status_while_owned(&lock, data_root, &args, started_at_ms, &error);
        drop(lock);
        return Err(abort_prepared_automatic_upgrade(
            prepared_auto_upgrade.take(),
            auto_upgrade_handoff.take(),
            error,
        ));
    }
    drop(lock);
    if let Some(handoff) = auto_upgrade_handoff.as_ref() {
        if let Err(error) = handoff.wait_for_installation_quiescence() {
            return Err(abort_prepared_automatic_upgrade(
                prepared_auto_upgrade.take(),
                auto_upgrade_handoff.take(),
                error,
            ));
        }
    }
    let events = telemetry.stopped_events(failed, Instant::now());
    send_daemon_events(ports.observation, data_root, &events);
    if let Some(prepared) = prepared_auto_upgrade {
        upgrade.engine.finish_automatic(
            upgrade.automatic_policy,
            upgrade.observer,
            prepared,
            auto_upgrade_handoff,
        )?;
    }
    Ok(())
}

fn publish_lifecycle_ready<I: DaemonInstallationPort>(
    data_root: &Path,
    lifecycle: &DaemonLifecycleState,
    installation: &I,
    acknowledge_restart_requests: bool,
) -> Result<bool> {
    let _transition = ctx_daemon_runtime::DaemonLifecycleTransitionLock::acquire(data_root)?;
    if installation.upgrade_handoff_blocks_current_process(data_root) || !lifecycle.mark_ready() {
        return Ok(false);
    }
    if acknowledge_restart_requests {
        installation.acknowledge_restart_requests(data_root);
    }
    Ok(true)
}

fn recover_source_refresh_before_ipc(
    data_root: &Path,
    source_refresh: &CoreRefreshEngine,
) -> Result<()> {
    source_refresh
        .recover_interrupted_publication(data_root)
        .map(|_| ())
        .context("recover interrupted Core refresh before daemon readiness")
}

fn recover_source_refresh_coordinator_before_ipc(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
    config: &'static dyn DaemonConfigPort,
) -> Result<Arc<CoreRefreshEngine>> {
    let source_refresh = Arc::new(super::source_backed_refresh_adapter::refresh_engine(config));
    recover_source_refresh_before_ipc(data_root, source_refresh.as_ref())?;
    runtime.source_refresh_coordinator = Some(Arc::clone(&source_refresh));
    Ok(source_refresh)
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod telemetry_tests;
