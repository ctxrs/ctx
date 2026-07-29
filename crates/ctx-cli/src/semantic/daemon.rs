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
        self, count_bucket, DaemonBackoffV1, DaemonCycleFactsV1, DaemonCycleResultV1,
        DaemonCycleStateV1, DaemonOperationV1, DaemonRunFactsV1, DaemonRuntimeObservationV1,
        DaemonRuntimeSnapshotV1, DaemonStartModeV1, DaemonSupervisorV1, DaemonTriggerV1,
        OperationCompletedV1, Outcome, PublicEventV1, RuntimeObservationV1,
    },
    compact_json,
    config::{self, AppConfig, DaemonMode, CONFIG_FILE},
    output::print_json,
    DaemonArgs, DaemonCommand, DaemonDisableArgs, DaemonRunArgs, DaemonStartModeArg,
    DaemonTriggerCommandArg, FormatArgs,
};

#[cfg(test)]
use super::source_backed_refresh_coordinator::SourceBackedRefreshCoordinator;

use super::{
    daemon_autostart::{
        current_process_owns_daemon_upgrade_handoff, daemon_upgrade_handoff_blocks_current_process,
        resume_completed_installation_daemons, InstallationDaemonLease,
    },
    daemon_retry::DaemonRetryBackoff,
    daemon_scheduler::{
        daemon_run_start_mode, restore_daemon_source_refresh_retry, run_daemon_once_with_activity,
        source_refresh_retry_due,
    },
    daemon_status::{daemon_report_failure_message, print_daemon_status_human},
    daemon_wakeup::{write_degraded_wakeup_receipt, DaemonFileWatcher, DaemonWakeup},
    daemon_worker::write_daemon_lifecycle_status_with_runtime,
    health_search::semantic_env_flag,
    model_runtime::SharedSemanticRuntime,
    paths_status::{
        daemon_lock_is_active, daemon_report, daemon_report_with_disabled_status,
        daemon_source_backed_refresh_job_path, lower_semantic_worker_priority,
        read_daemon_job_status, read_daemon_status, write_daemon_status, DaemonLock,
    },
    query_service::{
        daemon_can_begin_idle_shutdown, daemon_query_service_transport_supported,
        daemon_source_refresh_request, observe_daemon_query_activity,
        semantic_query_service_supported, start_daemon_query_service,
        start_daemon_source_refresh_service, DaemonQueryService,
    },
    runtime_limits::DAEMON_BACKGROUND_CHILD_ENV,
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

    pub(super) fn with_provider_refresh_events(mut self, events: Vec<PublicEventV1>) -> Self {
        self.provider_refresh_events = events;
        self
    }
}

const DAEMON_LIVENESS_MIN_INTERVAL: StdDuration = StdDuration::from_secs(23 * 60 * 60);
const DAEMON_LIVENESS_JITTER_WINDOW: StdDuration = StdDuration::from_secs(60 * 60);
const DAEMON_SAFETY_RECONCILE_MIN_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_SAFETY_RECONCILE_JITTER_WINDOW: StdDuration = StdDuration::from_secs(5 * 60);

#[derive(Debug)]
struct DaemonTelemetry {
    run: DaemonRunFactsV1,
    started: Instant,
    next_liveness: Instant,
    jitter_seed: u64,
    liveness_sequence: u64,
    current_state: DaemonCycleStateV1,
    idle_state: Option<DaemonCycleStateV1>,
    pending_idle_cycles: u64,
    pending_idle_duration: StdDuration,
    failure_active: bool,
}

impl DaemonTelemetry {
    fn new(run: DaemonRunFactsV1, started: Instant, jitter_seed: u64) -> Self {
        Self {
            run,
            started,
            next_liveness: started + daemon_liveness_interval(jitter_seed),
            jitter_seed,
            liveness_sequence: 1,
            current_state: DaemonCycleStateV1::unknown(),
            idle_state: None,
            pending_idle_cycles: 0,
            pending_idle_duration: StdDuration::ZERO,
            failure_active: false,
        }
    }

    fn ready_events(&self, recovered: bool, now: Instant) -> Vec<PublicEventV1> {
        let elapsed = now.saturating_duration_since(self.started);
        let mut events = vec![runtime_event(
            DaemonRuntimeObservationV1::ready(self.run),
            Outcome::Success,
            elapsed,
        )];
        if recovered {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::recovered(self.snapshot()),
                Outcome::Success,
                elapsed,
            ));
        }
        events
    }

    fn observe_cycle(
        &mut self,
        iteration: &mut DaemonIteration,
        duration: StdDuration,
    ) -> Vec<PublicEventV1> {
        let mut events = std::mem::take(&mut iteration.provider_refresh_events);
        let state = iteration.telemetry_state;
        self.current_state = state;
        let result = if iteration.failed {
            DaemonCycleResultV1::Failure
        } else if iteration.did_work {
            DaemonCycleResultV1::Work
        } else {
            DaemonCycleResultV1::NoWork
        };

        if result == DaemonCycleResultV1::NoWork {
            match self.idle_state {
                None => {
                    events.push(self.cycle_event(result, 1, state, duration));
                    self.idle_state = Some(state);
                }
                Some(previous) if previous == state => {
                    self.pending_idle_cycles = self.pending_idle_cycles.saturating_add(1);
                    self.pending_idle_duration =
                        self.pending_idle_duration.saturating_add(duration);
                }
                Some(_) => {
                    self.flush_pending_idle(&mut events);
                    events.push(self.cycle_event(result, 1, state, duration));
                    self.idle_state = Some(state);
                }
            }
        } else {
            self.flush_pending_idle(&mut events);
            self.idle_state = None;
            events.push(self.cycle_event(result, 1, state, duration));
        }
        if iteration.failed && !self.failure_active {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::failed(self.snapshot()),
                Outcome::Failure,
                duration,
            ));
            self.failure_active = true;
        } else if !iteration.failed
            && self.failure_active
            && state.retry_backoff() == DaemonBackoffV1::None
        {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::recovered(self.snapshot()),
                Outcome::Success,
                duration,
            ));
            self.failure_active = false;
        }
        events
    }

    fn liveness_events(&mut self, now: Instant) -> Vec<PublicEventV1> {
        if now < self.next_liveness {
            return Vec::new();
        }
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        events.push(runtime_event(
            DaemonRuntimeObservationV1::liveness(self.snapshot()),
            Outcome::Success,
            now.saturating_duration_since(self.started),
        ));
        let seed = self
            .jitter_seed
            .wrapping_add(self.liveness_sequence.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        self.liveness_sequence = self.liveness_sequence.saturating_add(1);
        self.next_liveness = now + daemon_liveness_interval(seed);
        events
    }

    fn stopped_events(&mut self, failed: bool, now: Instant) -> Vec<PublicEventV1> {
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        events.push(runtime_event(
            DaemonRuntimeObservationV1::stopped(self.snapshot()),
            if failed {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            now.saturating_duration_since(self.started),
        ));
        events
    }

    fn fatal_events(&mut self, now: Instant) -> Vec<PublicEventV1> {
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        if !self.failure_active {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::failed(self.snapshot()),
                Outcome::Failure,
                now.saturating_duration_since(self.started),
            ));
            self.failure_active = true;
        }
        events
    }

    fn flush_pending_idle(&mut self, events: &mut Vec<PublicEventV1>) {
        if self.pending_idle_cycles == 0 {
            return;
        }
        let state = self.idle_state.unwrap_or(self.current_state);
        events.push(self.cycle_event(
            DaemonCycleResultV1::NoWork,
            self.pending_idle_cycles,
            state,
            self.pending_idle_duration,
        ));
        self.pending_idle_cycles = 0;
        self.pending_idle_duration = StdDuration::ZERO;
    }

    fn cycle_event(
        &self,
        result: DaemonCycleResultV1,
        cycles: u64,
        state: DaemonCycleStateV1,
        duration: StdDuration,
    ) -> PublicEventV1 {
        runtime_event(
            DaemonRuntimeObservationV1::cycle(DaemonCycleFactsV1::new(
                self.run,
                result,
                count_bucket(cycles),
                state,
            )),
            if result == DaemonCycleResultV1::Failure {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            duration,
        )
    }

    fn snapshot(&self) -> DaemonRuntimeSnapshotV1 {
        DaemonRuntimeSnapshotV1::new(self.run, self.current_state)
    }
}

fn daemon_liveness_interval(seed: u64) -> StdDuration {
    let jitter_window_secs = DAEMON_LIVENESS_JITTER_WINDOW.as_secs();
    let jitter_secs = if jitter_window_secs == 0 {
        0
    } else {
        seed % jitter_window_secs
    };
    DAEMON_LIVENESS_MIN_INTERVAL + StdDuration::from_secs(jitter_secs)
}

fn daemon_safety_reconcile_interval(seed: u64) -> StdDuration {
    let jitter_window_secs = DAEMON_SAFETY_RECONCILE_JITTER_WINDOW.as_secs();
    let jitter_secs = if jitter_window_secs == 0 {
        0
    } else {
        seed % jitter_window_secs
    };
    DAEMON_SAFETY_RECONCILE_MIN_INTERVAL + StdDuration::from_secs(jitter_secs)
}

fn runtime_event(
    observation: DaemonRuntimeObservationV1,
    outcome: Outcome,
    duration: StdDuration,
) -> PublicEventV1 {
    PublicEventV1::RuntimeObservation(RuntimeObservationV1::daemon(observation, outcome, duration))
}

fn send_daemon_events(data_root: &Path, events: &[PublicEventV1]) {
    if events.is_empty() {
        return;
    }
    let Some(config) = reload_daemon_analytics_config(data_root) else {
        return;
    };
    analytics::send_batch(data_root, &config, events);
}

fn reload_daemon_analytics_config(data_root: &Path) -> Option<AppConfig> {
    let config = AppConfig::load(data_root).ok()?;
    config.analytics.enabled.then_some(config)
}

#[derive(Default)]
pub(super) struct DaemonRuntime {
    pub(super) semantic_runtime: SharedSemanticRuntime,
    pub(super) history_retry: DaemonRetryBackoff,
    pub(super) semantic_retry: DaemonRetryBackoff,
    pub(super) semantic_blocked_job: Option<Value>,
    pub(super) config: AppConfig,
}

#[derive(Debug, Clone)]
struct DaemonConfigReloadState {
    status: &'static str,
    last_attempt_at_ms: i64,
    last_applied_at_ms: Option<i64>,
    requested_daemon_enabled: bool,
    requested_daemon_mode: DaemonMode,
    requested_semantic_enabled: bool,
    applied_daemon_enabled: Option<bool>,
    applied_daemon_mode: Option<DaemonMode>,
    applied_semantic_enabled: Option<bool>,
    last_error: Option<String>,
}

impl DaemonConfigReloadState {
    fn pending(config: &AppConfig) -> Self {
        Self {
            status: "pending",
            last_attempt_at_ms: utc_now().timestamp_millis(),
            last_applied_at_ms: None,
            requested_daemon_enabled: config.daemon.enabled,
            requested_daemon_mode: config.daemon.mode,
            requested_semantic_enabled: config.semantic_search_enabled(),
            applied_daemon_enabled: None,
            applied_daemon_mode: None,
            applied_semantic_enabled: None,
            last_error: None,
        }
    }

    fn begin_attempt(&mut self, config: &AppConfig) {
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        self.requested_daemon_enabled = config.daemon.enabled;
        self.requested_daemon_mode = config.daemon.mode;
        self.requested_semantic_enabled = config.semantic_search_enabled();
        self.last_error = None;
    }

    fn applied(&mut self) {
        self.status = "applied";
        self.last_applied_at_ms = Some(self.last_attempt_at_ms);
        self.applied_daemon_enabled = Some(self.requested_daemon_enabled);
        self.applied_daemon_mode = Some(self.requested_daemon_mode);
        self.applied_semantic_enabled = Some(self.requested_semantic_enabled);
        self.last_error = None;
    }

    fn load_failed(&mut self, error: anyhow::Error) {
        self.status = "failed";
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        self.last_error = Some(format!("{error:#}"));
    }

    fn activation_failed(&mut self, error: anyhow::Error) {
        self.status = "activation_failed";
        self.applied_daemon_enabled = Some(self.requested_daemon_enabled);
        self.last_error = Some(format!("{error:#}"));
    }

    fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "last_attempt_at_ms": self.last_attempt_at_ms,
            "last_applied_at_ms": self.last_applied_at_ms,
            "requested": {
                "daemon_enabled": self.requested_daemon_enabled,
                "daemon_mode": self.requested_daemon_mode.as_str(),
                "semantic_enabled": self.requested_semantic_enabled,
            },
            "applied": {
                "daemon_enabled": self.applied_daemon_enabled,
                "daemon_mode": self.applied_daemon_mode.map(DaemonMode::as_str),
                "semantic_enabled": self.applied_semantic_enabled,
            },
            "last_error": self.last_error,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonConfigReloadOutcome {
    Continue,
    StopDisabled,
}

fn reload_daemon_runtime_config(
    data_root: &Path,
    args: &DaemonRunArgs,
    runtime: &mut DaemonRuntime,
    query_service: &mut Option<DaemonQueryService>,
    refresh_service: &mut Option<DaemonQueryService>,
    reload: &mut DaemonConfigReloadState,
    wakeup: &Arc<DaemonWakeup>,
) -> DaemonConfigReloadOutcome {
    let config = match AppConfig::load(data_root) {
        Ok(config) => config,
        Err(error) => {
            reload.load_failed(error);
            return DaemonConfigReloadOutcome::Continue;
        }
    };
    reload.begin_attempt(&config);
    runtime.config = config;

    if !runtime.config.daemon.enabled && !args.force {
        drop(query_service.take());
        drop(refresh_service.take());
        let _ = runtime.semantic_runtime.release_if_idle();
        reload.applied();
        return DaemonConfigReloadOutcome::StopDisabled;
    }

    let semantic_runtime_requested = daemon_semantic_runtime_requested(
        &runtime.config,
        semantic_query_service_supported() && daemon_query_service_transport_supported(),
    );
    if daemon_query_service_transport_supported() && refresh_service.is_none() {
        match start_daemon_source_refresh_service(
            data_root,
            runtime.semantic_runtime.clone(),
            Arc::clone(wakeup),
        ) {
            Ok(service) => *refresh_service = Some(service),
            Err(error) => {
                reload.activation_failed(error);
                return DaemonConfigReloadOutcome::Continue;
            }
        }
    }
    if semantic_runtime_requested && query_service.is_none() {
        match start_daemon_query_service(data_root, runtime.semantic_runtime.clone()) {
            Ok(service) => {
                *query_service = Some(service);
                // The query service thread keeps normal interactive priority.
                lower_semantic_worker_priority();
            }
            Err(error) => {
                reload.activation_failed(error);
                return DaemonConfigReloadOutcome::Continue;
            }
        }
    } else if !semantic_runtime_requested && query_service.is_some() {
        drop(query_service.take());
        let _ = runtime.semantic_runtime.release_if_idle();
    }

    reload.applied();
    DaemonConfigReloadOutcome::Continue
}

fn daemon_semantic_runtime_requested(config: &AppConfig, service_supported: bool) -> bool {
    service_supported
        && config.semantic_search_enabled()
        && !config.daemon.mode.runs_only_source_refresh()
}

fn daemon_semantic_runtime_active(
    runtime: &DaemonRuntime,
    query_service: Option<&DaemonQueryService>,
) -> bool {
    query_service.is_some()
        && runtime.config.semantic_search_enabled()
        && semantic_query_service_supported()
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct DaemonTestJobHooks {
    pub(super) calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    pub(super) history_refresh: Option<Value>,
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
            "semantic_index" => hooks.semantic_index.clone(),
            _ => None,
        }
    })
}

pub(crate) fn run_daemon_command(
    args: DaemonArgs,
    data_root: PathBuf,
    config: &AppConfig,
) -> Result<()> {
    let started = Instant::now();
    let operation = daemon_operation_for_command(&args.command);
    let telemetry_root = data_root.clone();
    let result = match args.command {
        DaemonCommand::Run(args) => run_daemon(args, data_root, config),
        DaemonCommand::Status(args) => run_daemon_status(args, data_root),
        DaemonCommand::Enable(args) => run_daemon_enabled_update(args, data_root, true),
        DaemonCommand::Disable(args) => run_daemon_disable(args, data_root),
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

pub(super) fn run_daemon_status(args: FormatArgs, data_root: PathBuf) -> Result<()> {
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
        print_daemon_status_human(&daemon);
        if pro["installed"].as_bool() == Some(true) {
            println!(
                "pro_status: {}",
                pro["state"].as_str().unwrap_or("unavailable")
            );
        }
    }
    Ok(())
}

pub(super) fn run_daemon_enabled_update(
    args: FormatArgs,
    data_root: PathBuf,
    enabled: bool,
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
    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemon_enabled": enabled,
            "running": handoff.is_some(),
            "pid": handoff.map(|handoff| handoff.pid),
            "config_path": data_root.join(CONFIG_FILE),
            "local_only": true,
        }))?;
    } else {
        println!("daemon_enabled: {enabled}");
        println!("config_path: {}", data_root.join(CONFIG_FILE).display());
    }
    Ok(())
}

fn run_daemon_disable(args: DaemonDisableArgs, data_root: PathBuf) -> Result<()> {
    if !args.prepare_uninstall {
        return run_daemon_enabled_update(
            FormatArgs {
                format: args.format,
            },
            data_root,
            false,
        );
    }
    #[cfg(unix)]
    {
        let report = super::daemon_autostart::prepare_posix_daemon_uninstall(&data_root)?;
        if args.format.is_json() {
            print_json(report)?;
        } else {
            println!("daemon_enabled: false");
            println!("daemon_running: false");
            println!("supervisor_removed: true");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (args, data_root);
        Err(anyhow!(
            "--prepare-uninstall is available only to POSIX hosted uninstallers"
        ))
    }
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
        print_daemon_status_human(&report);
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
                && source_refresh_retry_due(&runtime);
            let source_refresh_pending = refresh_service
                .as_ref()
                .is_some_and(|service| service.source_refresh.has_pending_request());
            if source_refresh_pending {
                idle_since = None;
            }
            if idle_exit.is_some_and(|limit| idle_since.is_some_and(|idle| idle.elapsed() >= limit))
                && !retry_due
                && !source_refresh_pending
            {
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
            if iteration.did_work {
                idle_since = None;
            } else if idle_since.is_none() {
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

fn installation_upgrade_blocks_current_process(data_root: &Path) -> bool {
    !super::daemon_autostart::current_process_owns_daemon_upgrade_handoff(data_root)
        && crate::upgrade::installation_upgrade_is_active().unwrap_or(false)
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod telemetry_tests;
