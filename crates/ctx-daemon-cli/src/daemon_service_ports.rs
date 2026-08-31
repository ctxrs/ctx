use std::path::Path;

use anyhow::{Context, Result};
use ctx_client_observability::analytics::PublicEventV1;
use ctx_daemon_service::{
    CoreGenerationPublished, CoreGenerationPublishedPort, DaemonAvailability,
    DaemonAvailabilityDemand, DaemonAvailabilityPort, DaemonConfigPort, DaemonConfigSnapshot,
    DaemonInstallationLease, DaemonInstallationPort, DaemonMode, DaemonObservationPort,
    DaemonProductConfig, DaemonServicePorts, DaemonTrigger,
};
use ctx_history_capture::DiscoveryContext;
use ctx_semantic_model::{ArtifactFetchRequest, ArtifactFetcher, SemanticModelConfig};

use crate::{
    composition::{load_runtime_config, DaemonRuntimeConfig},
    DaemonTriggerCommandArg,
};

mod provider_refresh;

pub(super) static CONFIG: CliDaemonConfigPort = CliDaemonConfigPort;
pub(super) static AVAILABILITY: CliDaemonAvailabilityPort = CliDaemonAvailabilityPort;
pub(super) static INSTALLATION: CliDaemonInstallationPort = CliDaemonInstallationPort;
pub(super) static GENERATION_PUBLISHED: CliCoreGenerationPublishedPort =
    CliCoreGenerationPublishedPort;
pub(crate) static OBSERVATION: CliDaemonObservationPort = CliDaemonObservationPort;
pub(super) static ARTIFACT_FETCHER: CliDaemonArtifactFetcher = CliDaemonArtifactFetcher;

pub(super) static PORTS: DaemonServicePorts<
    'static,
    dyn DaemonConfigPort,
    dyn DaemonAvailabilityPort,
    CliDaemonInstallationPort,
    dyn CoreGenerationPublishedPort,
    dyn DaemonObservationPort,
> = DaemonServicePorts {
    config: &CONFIG,
    availability: &AVAILABILITY,
    installation: &INSTALLATION,
    generation_published: &GENERATION_PUBLISHED,
    observation: &OBSERVATION,
    artifact_fetcher: &ARTIFACT_FETCHER,
};

pub fn config_snapshot(config: &DaemonRuntimeConfig) -> DaemonConfigSnapshot {
    config_snapshot_with_channel(config, config.upgrade_channel().to_owned())
}

fn into_config_snapshot(mut config: DaemonRuntimeConfig) -> DaemonConfigSnapshot {
    let upgrade_channel = std::mem::take(&mut config.upgrade.channel);
    config_snapshot_with_channel(&config, upgrade_channel)
}

fn config_snapshot_with_channel(
    config: &DaemonRuntimeConfig,
    upgrade_channel: String,
) -> DaemonConfigSnapshot {
    DaemonConfigSnapshot {
        daemon: DaemonProductConfig {
            enabled: config.daemon.enabled,
            mode: match config.daemon.mode {
                crate::composition::DaemonMode::Full => DaemonMode::Full,
                crate::composition::DaemonMode::SourceRefreshOnly => DaemonMode::SourceRefreshOnly,
            },
        },
        semantic_enabled: config.semantic_search_enabled(),
        semantic_executor: config.semantic_embedding_executor().clone(),
        automatic_upgrade_enabled: config.auto_upgrade_enabled(),
        automatic_upgrade_interval: config.upgrade.interval,
        upgrade_channel,
    }
}

pub fn run_daemon_service<D, AP, UO>(
    request: ctx_daemon_application::DaemonHostRunRequest,
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    upgrade: &ctx_daemon_service::DaemonUpgradePorts<'_, D, AP, UO>,
) -> Result<()>
where
    D: ctx_upgrade_engine::DaemonUpgradePort + ?Sized,
    AP: ctx_upgrade_engine::AutomaticUpgradePolicyProvider<Snapshot = DaemonConfigSnapshot>,
    UO: ctx_upgrade_engine::UpgradeObserver<DaemonConfigSnapshot>,
{
    use ctx_daemon_service::{
        DaemonRunArgs, DaemonRunProfile, DaemonStartMode, DaemonSupervisor, DaemonTrigger,
    };

    let service_args = DaemonRunArgs {
        loop_interval_seconds: request.loop_interval_seconds,
        max_chunks: request.max_chunks,
        handle_process_signals: request.handle_process_signals,
        force: request.force,
        profile: match request.profile {
            ctx_daemon_application::DaemonHostRunProfile::Persistent => {
                DaemonRunProfile::Persistent
            }
            ctx_daemon_application::DaemonHostRunProfile::FiniteCoreWorker => {
                DaemonRunProfile::FiniteCoreWorker
            }
        },
        start_mode: request.start_mode.map(|mode| match mode {
            ctx_daemon_application::DaemonHostStartMode::Manual => DaemonStartMode::Manual,
            ctx_daemon_application::DaemonHostStartMode::Auto => DaemonStartMode::Auto,
        }),
        trigger_command: request.trigger.map(|trigger| match trigger {
            ctx_daemon_application::DaemonTrigger::Setup => DaemonTrigger::Setup,
            ctx_daemon_application::DaemonTrigger::Import => DaemonTrigger::Import,
            ctx_daemon_application::DaemonTrigger::Search => DaemonTrigger::Search,
            ctx_daemon_application::DaemonTrigger::Semantic => DaemonTrigger::Semantic,
        }),
        supervisor: if matches!(
            request.start_mode,
            Some(ctx_daemon_application::DaemonHostStartMode::Auto)
        ) && super::health_search::semantic_env_flag(
            super::runtime_limits::DAEMON_BACKGROUND_CHILD_ENV,
        ) {
            DaemonSupervisor::CliAutostart
        } else {
            DaemonSupervisor::User
        },
    };
    ctx_daemon_service::run_daemon(
        service_args,
        data_root,
        config_snapshot(config),
        &PORTS,
        upgrade,
    )
}

pub(super) struct CliDaemonConfigPort;

impl DaemonConfigPort for CliDaemonConfigPort {
    fn load(&self, data_root: &Path) -> Result<DaemonConfigSnapshot> {
        load_runtime_config(data_root).map(into_config_snapshot)
    }

    fn semantic_model_config(&self, data_root: &Path) -> SemanticModelConfig {
        super::model_config::semantic_model_config(data_root)
    }

    fn semantic_executor_auth(&self) -> Result<ctx_semantic_model::SemanticEmbeddingExecutorAuth> {
        crate::semantic_embedding_executor_auth_from_environment()
    }

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext> {
        let config = load_runtime_config(data_root)
            .context("load configured provider roots for source-backed discovery")?;
        let home = crate::identity::home_dir();
        let home_available = home.is_some();
        Ok(
            DiscoveryContext::from_process(home.as_deref().unwrap_or(data_root))
                .with_home_directory_available(home_available)
                .with_data_root(data_root)
                .with_automatic_provider_discovery(config.automatic_provider_discovery_enabled())
                .with_configured_provider_roots(config.provider_roots().to_vec()),
        )
    }
}

pub(super) struct CliDaemonAvailabilityPort;

impl DaemonAvailabilityPort for CliDaemonAvailabilityPort {
    fn ensure_available(
        &self,
        data_root: &Path,
        trigger: DaemonTrigger,
        demand: DaemonAvailabilityDemand,
    ) -> Result<DaemonAvailability> {
        let config = load_runtime_config(data_root)
            .context("load daemon configuration before availability check")?;
        if config.daemon.enabled {
            super::daemon_autostart::autostart_core_daemon_and_wait(
                data_root,
                &config,
                cli_trigger(trigger),
            )?;
            return Ok(DaemonAvailability::Available);
        }
        if demand == DaemonAvailabilityDemand::Background || trigger == DaemonTrigger::Setup {
            return Ok(DaemonAvailability::Disabled);
        }
        super::daemon_autostart::start_finite_core_worker_and_wait(
            data_root,
            &config,
            cli_trigger(trigger),
        )?;
        Ok(DaemonAvailability::Available)
    }
}

pub(super) struct CliDaemonInstallationPort;

pub(super) struct CliDaemonInstallationLease(super::daemon_autostart::InstallationDaemonLease);

impl DaemonInstallationLease for CliDaemonInstallationLease {
    fn acknowledge(self, attempt_id: &str) -> Result<()> {
        self.0.acknowledge(attempt_id)
    }
}

impl DaemonInstallationPort for CliDaemonInstallationPort {
    type Lease = CliDaemonInstallationLease;

    fn lifecycle_blocks_current_process(
        &self,
        data_root: &Path,
        allow_automatic_recovery: bool,
    ) -> bool {
        ctx_upgrade_engine::installation_hosted_uninstall_is_active().unwrap_or(true)
            || (!super::daemon_autostart::current_process_owns_daemon_upgrade_handoff(data_root)
                && ctx_upgrade_engine::installation_upgrade_is_active().unwrap_or(false)
                && !(allow_automatic_recovery
                    && ctx_upgrade_engine::installation_interrupted_automatic_upgrade_is_recoverable()
                        .unwrap_or(false)))
    }

    fn upgrade_handoff_blocks_current_process(&self, data_root: &Path) -> bool {
        super::daemon_autostart::daemon_upgrade_handoff_blocks_current_process(data_root)
    }

    fn current_process_owns_upgrade_handoff(&self, data_root: &Path) -> bool {
        super::daemon_autostart::current_process_owns_daemon_upgrade_handoff(data_root)
    }

    fn acquire(
        &self,
        data_root: &Path,
        trigger: DaemonTrigger,
        loop_interval_seconds: Option<u64>,
        allow_active_upgrade: bool,
        allow_automatic_recovery: bool,
        persistent: bool,
    ) -> Result<Option<Self::Lease>> {
        let allow_active_upgrade = allow_active_upgrade
            || (allow_automatic_recovery
                && ctx_upgrade_engine::installation_interrupted_automatic_upgrade_is_recoverable(
                )?);
        super::daemon_autostart::InstallationDaemonLease::acquire(
            data_root,
            cli_trigger(trigger),
            loop_interval_seconds,
            allow_active_upgrade,
            persistent,
        )
        .map(|lease| lease.map(CliDaemonInstallationLease))
    }

    fn resume_completed(&self, data_root: &Path) -> Result<()> {
        super::daemon_autostart::resume_completed_installation_daemons(data_root)
    }

    fn acknowledge_restart_requests(&self, data_root: &Path) {
        super::daemon_autostart::acknowledge_daemon_restart_requests(data_root);
    }
}

pub(super) struct CliCoreGenerationPublishedPort;

impl CoreGenerationPublishedPort for CliCoreGenerationPublishedPort {
    fn notify(&self, data_root: &Path, publication: &CoreGenerationPublished) -> Result<()> {
        crate::composition::host().core_generation_published(data_root, publication)
    }
}

pub(crate) struct CliDaemonObservationPort;

impl DaemonObservationPort for CliDaemonObservationPort {
    fn provider_refresh_event(
        &self,
        job: &serde_json::Value,
        successor_pending: bool,
    ) -> Option<PublicEventV1> {
        provider_refresh::provider_refresh_event(job, successor_pending)
    }

    fn deliver(&self, data_root: &Path, events: &[PublicEventV1]) {
        if events.is_empty() {
            return;
        }
        crate::analytics::send_batch(data_root, events);
    }
}

pub fn deliver_daemon_events(data_root: &Path, events: &[PublicEventV1]) {
    OBSERVATION.deliver(data_root, events);
}

pub(super) struct CliDaemonArtifactFetcher;

impl ArtifactFetcher for CliDaemonArtifactFetcher {
    fn fetch_to_writer(
        &self,
        request: ArtifactFetchRequest<'_>,
        mut writer: &mut dyn std::io::Write,
    ) -> Result<u64> {
        crate::net::get_to_writer_limited(
            request.endpoint(),
            request.max_bytes(),
            request.timeout(),
            &mut writer,
        )
    }
}

fn cli_trigger(trigger: DaemonTrigger) -> DaemonTriggerCommandArg {
    match trigger {
        DaemonTrigger::Setup => DaemonTriggerCommandArg::Setup,
        DaemonTrigger::Import => DaemonTriggerCommandArg::Import,
        DaemonTrigger::Search => DaemonTriggerCommandArg::Search,
        DaemonTrigger::Semantic => DaemonTriggerCommandArg::Semantic,
    }
}
