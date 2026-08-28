//! CLI composition adapter for neutral daemon-supervisor policy.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use ctx_client_observability::analytics::PublicEventV1;
use ctx_daemon_service::DaemonObservationPort as _;
use serde_json::Value;

pub(super) use ctx_daemon_application::{
    DaemonSupervisorStart, DaemonSupervisorUpgradeFence, DaemonSupervisorUpgradeResume,
};

pub(super) struct CliDaemonApplicationHost<'config, 'value> {
    run_config: Option<&'config crate::config::AppConfig<'value>>,
    reload_persisted_config: bool,
}

impl CliDaemonApplicationHost<'_, '_> {
    const fn new() -> Self {
        Self {
            run_config: None,
            reload_persisted_config: true,
        }
    }

    const fn for_daemon_run<'config, 'value>(
        config: &'config crate::config::AppConfig<'value>,
        reload_persisted_config: bool,
    ) -> CliDaemonApplicationHost<'config, 'value> {
        CliDaemonApplicationHost {
            run_config: Some(config),
            reload_persisted_config,
        }
    }
}

impl ctx_daemon_application::DaemonApplicationHost for CliDaemonApplicationHost<'_, '_> {
    fn hosted_uninstall_active(&self) -> Result<bool> {
        ctx_upgrade_engine::installation_hosted_uninstall_is_active()
    }

    fn hosted_uninstall_active_for_executable(&self, executable: &Path) -> Result<bool> {
        ctx_upgrade_engine::installation_hosted_uninstall_is_active_for_executable(executable)
    }

    fn managed_install_executable(&self) -> Result<Option<PathBuf>> {
        ctx_upgrade_engine::managed_install_executable()
    }

    fn installation_upgrade_active(&self) -> Result<bool> {
        ctx_upgrade_engine::installation_upgrade_is_active()
    }

    fn automatic_upgrade_recovery_allowed(&self, data_root: &Path) -> Result<bool> {
        let config = crate::config::AppConfig::load(data_root)?;
        Ok(config.daemon.enabled
            && config.daemon.mode == crate::config::DaemonMode::Full
            && config.auto_upgrade_enabled()
            && ctx_upgrade_engine::installation_interrupted_automatic_upgrade_is_recoverable()?)
    }

    fn daemon_config(
        &self,
        data_root: &Path,
    ) -> Result<ctx_daemon_application::DaemonConfigSnapshot> {
        if !self.reload_persisted_config {
            if let Some(config) = self.run_config {
                return Ok(daemon_config_snapshot(config));
            }
        }
        // Mutating control operations persist a new mode before asking the
        // application layer to apply it, so their snapshot must be reloaded.
        let config = crate::config::AppConfig::load(data_root)?;
        Ok(daemon_config_snapshot(&config))
    }

    fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool> {
        crate::config::persisted_daemon_enabled(data_root)
    }

    fn defer_restart_for_upgrade_handoff(
        &self,
        data_root: &Path,
        trigger: ctx_daemon_application::DaemonTrigger,
    ) -> Result<Option<ctx_daemon_runtime::DaemonHandoffRestartDeferral>> {
        super::daemon_autostart::defer_restart_for_upgrade_handoff(
            data_root,
            daemon_trigger_arg(trigger),
            &uuid::Uuid::now_v7().to_string(),
        )
    }

    fn request_lifecycle_wakeup(
        &self,
        data_root: &Path,
        request: Value,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<Option<Value>> {
        ctx_daemon_service::daemon_source_refresh_request(
            data_root,
            request,
            timeout,
            response_limit,
        )
    }

    fn home_dir(&self) -> Option<PathBuf> {
        crate::identity::home_dir()
    }

    fn run_daemon_service(
        &self,
        data_root: &Path,
        request: ctx_daemon_application::DaemonHostRunRequest,
    ) -> Result<()> {
        if self.reload_persisted_config {
            let config = crate::config::AppConfig::load(data_root)?;
            return crate::composition::host().run_daemon_service(data_root, request, &config);
        }
        let config = self
            .run_config
            .ok_or_else(|| anyhow::anyhow!("daemon run host is missing its borrowed config"))?;
        crate::composition::host().run_daemon_service(data_root, request, config)
    }

    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()> {
        crate::config::set_daemon_enabled(data_root, enabled)
    }

    fn request_daemon_shutdown(
        &self,
        data_root: &Path,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<()> {
        ctx_daemon_service::daemon_source_refresh_request(
            data_root,
            crate::compact_json(serde_json::json!({
                "schema_version": 1,
                "op": "shutdown",
            })),
            timeout,
            response_limit,
        )
        .map(|_| ())
    }

    fn terminate_current_executable_daemon(&self, data_root: &Path) -> Result<()> {
        super::daemon_autostart::terminate_current_executable_daemon(data_root)
    }

    fn remove_released_daemon_service_artifacts(&self, data_root: &Path) -> Result<()> {
        super::daemon::control::remove_released_daemon_service_artifacts(data_root)
    }

    fn cancel_core_finalization_generation_lease(
        &self,
        _data_root: &Path,
        _reason: &str,
    ) -> Result<()> {
        // Retained only until the out-of-scope daemon-application lifecycle
        // trait drops its former projection cleanup callback. The public
        // daemon no longer owns or manipulates any such lease.
        Ok(())
    }

    fn observe_source_refresh_endpoint(
        &self,
        identity_path: &Path,
    ) -> ctx_daemon_application::DaemonEndpointObservation {
        let identity = std::fs::read_to_string(identity_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        ctx_daemon_application::DaemonEndpointObservation {
            available: identity.is_some(),
            transport: identity
                .as_ref()
                .and_then(|value| json_string(value, "transport")),
            owner_pid: identity.as_ref().and_then(|value| json_u32(value, "pid")),
            address: identity.as_ref().and_then(|value| {
                json_string(value, "path").or_else(|| json_string(value, "pipe_name"))
            }),
        }
    }

    fn deliver_daemon_events(&self, data_root: &Path, events: &[PublicEventV1]) {
        super::daemon_service_ports::OBSERVATION.deliver(data_root, events);
    }
}

fn daemon_config_snapshot(
    config: &crate::config::AppConfig<'_>,
) -> ctx_daemon_application::DaemonConfigSnapshot {
    ctx_daemon_application::DaemonConfigSnapshot {
        enabled: config.daemon.enabled,
        mode: daemon_mode(config.daemon.mode),
        semantic_enabled: config.semantic_search_enabled(),
        semantic_executor: config
            .semantic_embedding_executor()
            .http_endpoint()
            .unwrap_or("builtin")
            .to_owned(),
    }
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

pub(super) const fn daemon_trigger_arg(
    trigger: ctx_daemon_application::DaemonTrigger,
) -> crate::DaemonTriggerCommandArg {
    match trigger {
        ctx_daemon_application::DaemonTrigger::Setup => crate::DaemonTriggerCommandArg::Setup,
        ctx_daemon_application::DaemonTrigger::Import => crate::DaemonTriggerCommandArg::Import,
        ctx_daemon_application::DaemonTrigger::Search => crate::DaemonTriggerCommandArg::Search,
        ctx_daemon_application::DaemonTrigger::Semantic => crate::DaemonTriggerCommandArg::Semantic,
    }
}

pub(super) const fn daemon_trigger(
    trigger: crate::DaemonTriggerCommandArg,
) -> ctx_daemon_application::DaemonTrigger {
    match trigger {
        crate::DaemonTriggerCommandArg::Setup => ctx_daemon_application::DaemonTrigger::Setup,
        crate::DaemonTriggerCommandArg::Import => ctx_daemon_application::DaemonTrigger::Import,
        crate::DaemonTriggerCommandArg::Search => ctx_daemon_application::DaemonTrigger::Search,
        crate::DaemonTriggerCommandArg::Semantic => ctx_daemon_application::DaemonTrigger::Semantic,
    }
}

pub(super) const fn daemon_mode(
    mode: crate::config::DaemonMode,
) -> ctx_daemon_application::DaemonMode {
    match mode {
        crate::config::DaemonMode::Full => ctx_daemon_application::DaemonMode::Full,
        crate::config::DaemonMode::SourceRefreshOnly => {
            ctx_daemon_application::DaemonMode::SourceRefreshOnly
        }
    }
}

pub(super) fn with_daemon_application<T>(
    operation: impl FnOnce(&ctx_daemon_application::DaemonApplication<'_>) -> T,
) -> T {
    let host = CliDaemonApplicationHost::new();
    let application = ctx_daemon_application::DaemonApplication::new(&host);
    operation(&application)
}

pub(super) fn with_daemon_run_application<T>(
    config: &crate::config::AppConfig<'_>,
    reload_persisted_config: bool,
    operation: impl FnOnce(&ctx_daemon_application::DaemonApplication<'_>) -> T,
) -> T {
    let host = CliDaemonApplicationHost::for_daemon_run(config, reload_persisted_config);
    let application = ctx_daemon_application::DaemonApplication::new(&host);
    operation(&application)
}

pub(super) fn ensure_daemon_supervisor(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    data_root: &Path,
) -> Result<DaemonSupervisorStart> {
    application.ensure_daemon_supervisor(data_root)
}

pub(super) fn disable_daemon_supervisor(data_root: &Path) -> Result<()> {
    with_daemon_application(|application| application.disable_daemon_supervisor(data_root))
}

pub(super) fn resume_daemon_supervisor_after_upgrade(
    data_root: &Path,
    executable: &Path,
    loop_interval_seconds: Option<u64>,
    upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
) -> Result<DaemonSupervisorUpgradeResume> {
    with_daemon_application(|application| {
        application.resume_daemon_supervisor_after_upgrade(
            data_root,
            executable,
            loop_interval_seconds,
            upgrade_fence,
        )
    })
}
