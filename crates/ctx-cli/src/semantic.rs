//! Final-binary composition for daemon and semantic adapters.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Mutex,
};
use std::{borrow::Cow, io::Write, path::Path, time::Duration};

use anyhow::Result;
use ctx_client_observability::analytics::PublicEventV1;
use ctx_companion_bridge::CancellationToken;
use ctx_daemon_cli::{AppConfig as DaemonCliConfig, DaemonConfig, DaemonMode};

pub(crate) use ctx_daemon_cli::{
    begin_daemon_upgrade_handoff, begin_legacy_daemon_upgrade_handoff,
    complete_replacement_daemon_handoff, coordinate_import_source_backed_refresh_with_progress,
    coordinate_setup_source_backed_refresh_with_progress,
    coordinate_source_backed_refresh_with_progress, current_rejected_record_count,
    daemon_autostart_suppression_reason, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, published_explicit_source_relocation_authority,
    replacement_helper_owns_daemon_handoff, semantic_managed_model_snapshot_dir,
    semantic_native_accelerator_target, semantic_provisioning_coreml_asset_matches,
    semantic_provisioning_model_contract_matches, semantic_provisioning_model_path_count,
    semantic_provisioning_model_path_matches, semantic_query_service_supported,
    semantic_required_model_file_count, semantic_required_model_file_matches,
    semantic_runtime_cache_dir, semantic_worker_cache_dir, DaemonHandoff, DaemonSetupHandoff,
    DaemonUpgradeHandoff, RefreshStatus, SemanticNativeAcceleratorTarget, SemanticNotReady,
    SemanticOrtModelVariant, SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshMode,
    SourceBackedRefreshObservation, SourceBackedRefreshPendingPublication,
    SourceBackedRefreshTerminalError, SEMANTIC_WORKER_BATCH_MAX,
};

struct CtxDaemonCliHost;

static HOST: CtxDaemonCliHost = CtxDaemonCliHost;
const COMPANION_MAINTENANCE_WAKE_RUNNING: u8 = 1;
const COMPANION_MAINTENANCE_WAKE_PENDING: u8 = 2;
static COMPANION_MAINTENANCE_WAKE_STATE: AtomicU8 = AtomicU8::new(0);
static COMPANION_MAINTENANCE_WORKER: Mutex<Option<CompanionMaintenanceWorker>> = Mutex::new(None);

struct CompanionMaintenanceWorker {
    cancellation: CancellationToken,
    handle: std::thread::JoinHandle<()>,
}

fn request_companion_maintenance_worker(state: &AtomicU8) -> bool {
    let mut observed = state.fetch_or(COMPANION_MAINTENANCE_WAKE_PENDING, Ordering::AcqRel)
        | COMPANION_MAINTENANCE_WAKE_PENDING;
    loop {
        if observed & COMPANION_MAINTENANCE_WAKE_RUNNING != 0 {
            return false;
        }
        match state.compare_exchange_weak(
            observed,
            observed | COMPANION_MAINTENANCE_WAKE_RUNNING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => observed = actual,
        }
    }
}

fn take_companion_maintenance_request(state: &AtomicU8) {
    state.fetch_and(!COMPANION_MAINTENANCE_WAKE_PENDING, Ordering::AcqRel);
}

fn companion_maintenance_should_continue(state: &AtomicU8) -> bool {
    loop {
        let observed = state.load(Ordering::Acquire);
        if observed & COMPANION_MAINTENANCE_WAKE_PENDING != 0 {
            return true;
        }
        if state
            .compare_exchange_weak(
                COMPANION_MAINTENANCE_WAKE_RUNNING,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            return false;
        }
    }
}

fn stop_companion_maintenance_worker_in(
    state: &AtomicU8,
    worker: &Mutex<Option<CompanionMaintenanceWorker>>,
) {
    let worker = worker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(worker) = worker {
        worker.cancellation.cancel();
        let _ = worker.handle.join();
    }
    state.store(0, Ordering::Release);
}

fn stop_companion_maintenance_worker() {
    stop_companion_maintenance_worker_in(
        &COMPANION_MAINTENANCE_WAKE_STATE,
        &COMPANION_MAINTENANCE_WORKER,
    );
}

pub(crate) fn initialize() -> Result<()> {
    ctx_daemon_cli::install_host(&HOST)
}

fn daemon_cli_config<'a>(config: &'a crate::config::AppConfig) -> DaemonCliConfig<'a> {
    DaemonCliConfig::new(
        config.analytics.enabled,
        config.auto_upgrade_enabled(),
        Cow::Borrowed(config.upgrade.channel.as_str()),
        config.upgrade.interval,
        DaemonConfig {
            enabled: config.automatic_indexing_enabled(),
            mode: match config.daemon.mode {
                crate::config::DaemonMode::Full => DaemonMode::Full,
                crate::config::DaemonMode::SourceRefreshOnly => DaemonMode::SourceRefreshOnly,
            },
        },
        config.semantic_search_enabled(),
        config.semantic_search_source(),
    )
    .with_automatic_provider_discovery(config.automatic_source_discovery_enabled())
    .with_provider_roots(config.provider_root_definitions())
}

fn owned_daemon_cli_config(config: crate::config::AppConfig) -> DaemonCliConfig<'static> {
    let analytics_enabled = config.analytics.enabled;
    let automatic_upgrade_enabled = config.auto_upgrade_enabled();
    let upgrade_interval = config.upgrade.interval;
    let daemon_enabled = config.automatic_indexing_enabled();
    let daemon_mode = match config.daemon.mode {
        crate::config::DaemonMode::Full => DaemonMode::Full,
        crate::config::DaemonMode::SourceRefreshOnly => DaemonMode::SourceRefreshOnly,
    };
    let semantic_enabled = config.semantic_search_enabled();
    let semantic_source = config.semantic_search_source();
    let automatic_provider_discovery = config.automatic_source_discovery_enabled();
    let provider_roots = config.provider_root_definitions();
    DaemonCliConfig::new(
        analytics_enabled,
        automatic_upgrade_enabled,
        Cow::Owned(config.upgrade.channel),
        upgrade_interval,
        DaemonConfig {
            enabled: daemon_enabled,
            mode: daemon_mode,
        },
        semantic_enabled,
        semantic_source,
    )
    .with_automatic_provider_discovery(automatic_provider_discovery)
    .with_provider_roots(provider_roots)
}

fn daemon_trigger(
    trigger: crate::DaemonTriggerCommandArg,
) -> ctx_daemon_cli::DaemonTriggerCommandArg {
    match trigger {
        crate::DaemonTriggerCommandArg::Setup => ctx_daemon_cli::DaemonTriggerCommandArg::Setup,
        crate::DaemonTriggerCommandArg::Import => ctx_daemon_cli::DaemonTriggerCommandArg::Import,
        crate::DaemonTriggerCommandArg::Search => ctx_daemon_cli::DaemonTriggerCommandArg::Search,
    }
}

fn output_format(format: crate::output::JsonOutputFormat) -> ctx_terminal::JsonOutputFormat {
    match format {
        crate::output::JsonOutputFormat::Text => ctx_terminal::JsonOutputFormat::Text,
        crate::output::JsonOutputFormat::Json => ctx_terminal::JsonOutputFormat::Json,
    }
}

pub(crate) fn source_epoch_status_report(
    data_root: &Path,
    config: &crate::config::AppConfig,
) -> Result<ctx_daemon_cli::SourceEpochStatus> {
    ctx_daemon_cli::source_epoch_status_report(data_root, &daemon_cli_config(config))
}

pub(crate) fn autostart_daemon_and_wait(
    data_root: &Path,
    config: &crate::config::AppConfig,
    trigger: crate::DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    ctx_daemon_cli::autostart_daemon_and_wait(
        data_root,
        &daemon_cli_config(config),
        daemon_trigger(trigger),
    )
}

pub(crate) fn autostart_daemon_for_setup_and_wait(
    data_root: &Path,
    config: &crate::config::AppConfig,
    trigger: crate::DaemonTriggerCommandArg,
) -> Result<DaemonSetupHandoff> {
    ctx_daemon_cli::autostart_daemon_for_setup_and_wait(
        data_root,
        &daemon_cli_config(config),
        daemon_trigger(trigger),
    )
}

pub(crate) fn observe_daemon_for_setup_and_wait(
    data_root: &Path,
    config: &crate::config::AppConfig,
) -> Result<DaemonSetupHandoff> {
    ctx_daemon_cli::observe_daemon_for_setup_and_wait(data_root, &daemon_cli_config(config))
}

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &crate::config::AppConfig,
    trigger: crate::DaemonTriggerCommandArg,
) {
    ctx_daemon_cli::maybe_autostart_daemon(
        data_root,
        &daemon_cli_config(config),
        daemon_trigger(trigger),
    );
}

pub(crate) fn update_indexing_mode(
    data_root: &Path,
    config: &crate::config::AppConfig,
    automatic: bool,
) -> Result<ctx_daemon_cli::IndexingModeUpdate> {
    ctx_daemon_cli::update_indexing_mode(data_root, &daemon_cli_config(config), automatic)
}

pub(crate) fn begin_current_daemon_upgrade_handoff(
    data_root: &Path,
    attempt_id: &str,
    trigger: crate::DaemonTriggerCommandArg,
    loop_interval_seconds: Option<u64>,
) -> Result<DaemonUpgradeHandoff> {
    ctx_daemon_cli::begin_current_daemon_upgrade_handoff(
        data_root,
        attempt_id,
        daemon_trigger(trigger),
        loop_interval_seconds,
    )
}

pub(crate) fn daemon_config_snapshot(
    config: &crate::config::AppConfig,
) -> ctx_daemon_cli::DaemonConfigSnapshot {
    ctx_daemon_cli::daemon_service_ports::config_snapshot(&daemon_cli_config(config))
}

pub(crate) fn deliver_daemon_events(data_root: &Path, events: &[PublicEventV1]) {
    ctx_daemon_cli::daemon_service_ports::deliver_daemon_events(data_root, events);
}

pub(crate) fn run_daemon_command(
    args: crate::DaemonArgs,
    data_root: std::path::PathBuf,
    config: &crate::config::AppConfig,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    use crate::DaemonCommand as C;

    let command = match args.command {
        C::Run(args) => ctx_daemon_cli::DaemonCommand::Run(ctx_daemon_cli::DaemonRunArgs {
            loop_interval_seconds: args.loop_interval_seconds,
            max_chunks: args.max_chunks,
            finite_core_worker: args.finite_core_worker,
            force: args.force,
            start_mode: args.start_mode.map(|mode| match mode {
                crate::DaemonStartModeArg::Auto => ctx_daemon_cli::DaemonStartModeArg::Auto,
                crate::DaemonStartModeArg::Manual => ctx_daemon_cli::DaemonStartModeArg::Manual,
            }),
            trigger_command: args.trigger_command.map(daemon_trigger),
            format: output_format(args.format),
        }),
        C::Status(args) => ctx_daemon_cli::DaemonCommand::Status(ctx_daemon_cli::FormatArgs {
            format: output_format(args.format),
        }),
        C::Enable(args) => ctx_daemon_cli::DaemonCommand::Enable(ctx_daemon_cli::FormatArgs {
            format: output_format(args.format),
        }),
        C::Disable(args) => {
            ctx_daemon_cli::DaemonCommand::Disable(ctx_daemon_cli::DaemonDisableArgs {
                format: output_format(args.format),
                prepare_uninstall: args.prepare_uninstall,
            })
        }
    };
    ctx_daemon_cli::run_daemon_command(
        ctx_daemon_cli::DaemonArgs { command },
        data_root,
        &daemon_cli_config(config),
        ui,
    )
    .map_err(|error| {
        if error.is::<ctx_daemon_cli::RenderedCliError>() {
            crate::dispatch::rendered_cli_error()
        } else {
            error
        }
    })
}

impl ctx_daemon_cli::DaemonCliHost for CtxDaemonCliHost {
    fn load_config(&self, data_root: &Path) -> Result<DaemonCliConfig<'static>> {
        crate::config::AppConfig::load(data_root).map(owned_daemon_cli_config)
    }

    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool> {
        crate::config::persisted_daemon_enabled(data_root)
    }

    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()> {
        crate::config::set_daemon_enabled(data_root, enabled)
    }

    fn home_dir(&self) -> Option<std::path::PathBuf> {
        crate::identity::home_dir()
    }

    fn run_daemon_service(
        &self,
        data_root: &Path,
        request: ctx_daemon_cli::DaemonHostRunRequest,
        config: &DaemonCliConfig<'_>,
    ) -> Result<()> {
        let engine = crate::upgrade::ports::engine();
        let upgrade = ctx_daemon_cli::DaemonUpgradePorts {
            engine: &engine,
            daemon: &crate::upgrade::ports::DAEMON_UPGRADE,
            automatic_policy: &crate::upgrade::ports::AUTOMATIC_POLICY,
            observer: &crate::upgrade::ports::UPGRADE_OBSERVER,
        };
        let result = ctx_daemon_cli::daemon_service_ports::run_daemon_service(
            request, data_root, config, &upgrade,
        );
        // The daemon owns the maintenance worker. Cancel and join it before
        // returning so Pro and any contained descendants cannot outlive Core.
        stop_companion_maintenance_worker();
        result
    }

    fn deliver_daemon_events(&self, data_root: &Path, events: &[PublicEventV1]) {
        if events.is_empty() {
            return;
        }
        let Ok(config) = crate::config::AppConfig::load(data_root) else {
            return;
        };
        if config.analytics.enabled {
            crate::analytics::send_batch(data_root, &config, events);
        }
    }

    fn fetch_to_writer(
        &self,
        endpoint: &str,
        max_bytes: u64,
        timeout: Duration,
        writer: &mut dyn Write,
    ) -> Result<u64> {
        struct DynWriter<'a>(&'a mut dyn Write);
        impl Write for DynWriter<'_> {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                self.0.write(buffer)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.0.flush()
            }
        }
        crate::net::get_to_writer_limited(endpoint, max_bytes, timeout, &mut DynWriter(writer))
    }

    fn core_generation_published(
        &self,
        data_root: &Path,
        _publication: &ctx_daemon_cli::CoreGenerationPublished,
    ) -> Result<()> {
        if !request_companion_maintenance_worker(&COMPANION_MAINTENANCE_WAKE_STATE) {
            return Ok(());
        }
        let data_root = data_root.to_path_buf();
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::Builder::new()
            .name("ctx-pro-maintenance-wake".to_owned())
            .spawn(move || loop {
                take_companion_maintenance_request(&COMPANION_MAINTENANCE_WAKE_STATE);
                let _ = crate::companion::wake_verified_private_maintenance(
                    &data_root,
                    &worker_cancellation,
                );
                if worker_cancellation.is_cancelled() {
                    COMPANION_MAINTENANCE_WAKE_STATE.store(0, Ordering::Release);
                    break;
                }
                if !companion_maintenance_should_continue(&COMPANION_MAINTENANCE_WAKE_STATE) {
                    break;
                }
            });
        match worker {
            Ok(handle) => {
                let previous = COMPANION_MAINTENANCE_WORKER
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .replace(CompanionMaintenanceWorker {
                        cancellation,
                        handle,
                    });
                if let Some(previous) = previous {
                    previous.cancellation.cancel();
                    let _ = previous.handle.join();
                }
            }
            Err(_) => COMPANION_MAINTENANCE_WAKE_STATE.store(0, Ordering::Release),
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "semantic/tests.rs"]
mod tests;
