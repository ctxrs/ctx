use std::{path::Path, sync::Arc};

use anyhow::anyhow;
use ctx_history_core::utc_now;
use ctx_semantic_model::{
    semantic_query_service_supported, SemanticEmbeddingExecutorConfig,
    SemanticEmbeddingExecutorHandle, SemanticEmbeddingExecutorKind, SemanticModelConfig,
    SharedSemanticRuntime,
};
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonMode},
    DaemonConfigPort, DaemonRunArgs, DaemonRunProfile,
};

use super::{
    super::{
        daemon_wakeup::DaemonWakeup,
        paths_status::lower_semantic_worker_priority,
        query_service::{
            ctx_authenticated_request_handler_with_lifecycle,
            daemon_query_service_transport_supported, start_daemon_query_service,
            start_daemon_source_refresh_service, DaemonLifecycleState, DaemonQueryService,
        },
    },
    DaemonRuntime,
};

#[derive(Debug, Clone)]
pub(super) struct DaemonConfigReloadState {
    pub(super) status: &'static str,
    last_attempt_at_ms: i64,
    last_applied_at_ms: Option<i64>,
    requested_daemon_enabled: bool,
    requested_daemon_mode: DaemonMode,
    requested_semantic_enabled: bool,
    requested_semantic_executor: String,
    requested_semantic_contract_fingerprint: String,
    requested_semantic_builtin_throttling_configured: bool,
    requested_semantic_builtin_throttling_effective: Option<bool>,
    applied_daemon_enabled: Option<bool>,
    applied_daemon_mode: Option<DaemonMode>,
    applied_semantic_enabled: Option<bool>,
    applied_semantic_executor: Option<String>,
    applied_semantic_contract_fingerprint: Option<String>,
    applied_semantic_builtin_throttling_configured: Option<bool>,
    applied_semantic_builtin_throttling_effective: Option<bool>,
    pub(super) last_error: Option<String>,
}

impl DaemonConfigReloadState {
    pub(super) fn pending(config: &AppConfig) -> Self {
        Self {
            status: "pending",
            last_attempt_at_ms: utc_now().timestamp_millis(),
            last_applied_at_ms: None,
            requested_daemon_enabled: config.daemon.enabled,
            requested_daemon_mode: config.daemon.mode,
            requested_semantic_enabled: config.semantic_search_enabled(),
            requested_semantic_executor: semantic_executor_selector(config),
            requested_semantic_contract_fingerprint: config
                .semantic_executor
                .contract()
                .fingerprint()
                .to_owned(),
            requested_semantic_builtin_throttling_configured: config
                .semantic_builtin_throttling_configured,
            requested_semantic_builtin_throttling_effective: config
                .semantic_builtin_throttling_effective(),
            applied_daemon_enabled: None,
            applied_daemon_mode: None,
            applied_semantic_enabled: None,
            applied_semantic_executor: None,
            applied_semantic_contract_fingerprint: None,
            applied_semantic_builtin_throttling_configured: None,
            applied_semantic_builtin_throttling_effective: None,
            last_error: None,
        }
    }

    fn begin_attempt(&mut self, config: &AppConfig) {
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        self.requested_daemon_enabled = config.daemon.enabled;
        self.requested_daemon_mode = config.daemon.mode;
        self.requested_semantic_enabled = config.semantic_search_enabled();
        self.requested_semantic_executor = semantic_executor_selector(config);
        self.requested_semantic_contract_fingerprint =
            config.semantic_executor.contract().fingerprint().to_owned();
        self.requested_semantic_builtin_throttling_configured =
            config.semantic_builtin_throttling_configured;
        self.requested_semantic_builtin_throttling_effective =
            config.semantic_builtin_throttling_effective();
        self.last_error = None;
    }

    fn applied(&mut self) {
        self.status = "applied";
        self.last_applied_at_ms = Some(self.last_attempt_at_ms);
        self.applied_daemon_enabled = Some(self.requested_daemon_enabled);
        self.applied_daemon_mode = Some(self.requested_daemon_mode);
        self.applied_semantic_enabled = Some(self.requested_semantic_enabled);
        self.applied_semantic_executor = Some(self.requested_semantic_executor.clone());
        self.applied_semantic_contract_fingerprint =
            Some(self.requested_semantic_contract_fingerprint.clone());
        self.applied_semantic_builtin_throttling_configured =
            Some(self.requested_semantic_builtin_throttling_configured);
        self.applied_semantic_builtin_throttling_effective =
            self.requested_semantic_builtin_throttling_effective;
        self.last_error = None;
    }

    fn load_failed(&mut self, error: anyhow::Error) {
        self.status = "failed";
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        // The requested file cannot be trusted, so no semantic runtime remains
        // applied even if the last successfully parsed configuration enabled
        // one. Core refresh can continue independently.
        self.applied_semantic_enabled = Some(false);
        self.applied_semantic_executor = None;
        self.applied_semantic_contract_fingerprint = None;
        self.applied_semantic_builtin_throttling_configured = None;
        self.applied_semantic_builtin_throttling_effective = None;
        self.last_error = Some(format!("{error:#}"));
    }

    fn activation_failed(&mut self, error: anyhow::Error) {
        self.status = "activation_failed";
        self.applied_daemon_enabled = Some(self.requested_daemon_enabled);
        self.applied_daemon_mode = Some(self.requested_daemon_mode);
        self.applied_semantic_enabled = Some(false);
        self.applied_semantic_executor = None;
        self.applied_semantic_contract_fingerprint = None;
        self.applied_semantic_builtin_throttling_configured = None;
        self.applied_semantic_builtin_throttling_effective = None;
        self.last_error = Some(format!("{error:#}"));
    }

    pub(super) fn blocks_daemon_startup(&self, source_refresh_active: bool) -> bool {
        self.status == "activation_failed" && !source_refresh_active
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "last_attempt_at_ms": self.last_attempt_at_ms,
            "last_applied_at_ms": self.last_applied_at_ms,
            "requested": {
                "daemon_enabled": self.requested_daemon_enabled,
                "daemon_mode": self.requested_daemon_mode.as_str(),
                "semantic_enabled": self.requested_semantic_enabled,
                "semantic_executor": self.requested_semantic_executor,
                "semantic_contract_fingerprint": self.requested_semantic_contract_fingerprint,
                "semantic_builtin_throttling_configured": self.requested_semantic_builtin_throttling_configured,
                "semantic_builtin_throttling_effective": self.requested_semantic_builtin_throttling_effective,
            },
            "applied": {
                "daemon_enabled": self.applied_daemon_enabled,
                "daemon_mode": self.applied_daemon_mode.map(DaemonMode::as_str),
                "semantic_enabled": self.applied_semantic_enabled,
                "semantic_executor": self.applied_semantic_executor,
                "semantic_contract_fingerprint": self.applied_semantic_contract_fingerprint,
                "semantic_builtin_throttling_configured": self.applied_semantic_builtin_throttling_configured,
                "semantic_builtin_throttling_effective": self.applied_semantic_builtin_throttling_effective,
            },
            "last_error": self.last_error,
        })
    }
}

fn semantic_executor_selector(config: &AppConfig) -> String {
    config
        .semantic_executor
        .http_endpoint()
        .unwrap_or("builtin")
        .to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonConfigReloadOutcome {
    Continue,
    StopDisabled,
}

pub(super) struct DaemonConfigReloadContext<'a> {
    pub(super) query_service: &'a mut Option<DaemonQueryService>,
    pub(super) refresh_service: &'a mut Option<DaemonQueryService>,
    pub(super) state: &'a mut DaemonConfigReloadState,
    pub(super) wakeup: &'a Arc<DaemonWakeup>,
    pub(super) lifecycle: &'a Arc<DaemonLifecycleState>,
    pub(super) config_port: &'static dyn DaemonConfigPort,
}

pub(super) fn reload_daemon_runtime_config(
    data_root: &Path,
    args: &DaemonRunArgs,
    runtime: &mut DaemonRuntime,
    context: DaemonConfigReloadContext<'_>,
) -> DaemonConfigReloadOutcome {
    reload_daemon_runtime_config_with_executor_builder(
        data_root,
        args,
        runtime,
        context,
        SemanticEmbeddingExecutorHandle::build_with_auth,
    )
}

fn reload_daemon_runtime_config_with_executor_builder<BuildExecutor>(
    data_root: &Path,
    args: &DaemonRunArgs,
    runtime: &mut DaemonRuntime,
    context: DaemonConfigReloadContext<'_>,
    build_executor: BuildExecutor,
) -> DaemonConfigReloadOutcome
where
    BuildExecutor: FnOnce(
        SemanticEmbeddingExecutorConfig,
        ctx_semantic_model::SemanticEmbeddingExecutorAuth,
        SharedSemanticRuntime,
        SemanticModelConfig,
    ) -> anyhow::Result<SemanticEmbeddingExecutorHandle>,
{
    let DaemonConfigReloadContext {
        query_service,
        refresh_service,
        state: reload,
        wakeup,
        lifecycle,
        config_port,
    } = context;
    let previous_activation_error = (reload.status == "activation_failed")
        .then(|| reload.last_error.clone())
        .flatten();
    let mut config = match config_port.load(data_root) {
        Ok(config) => config,
        Err(error) => {
            drop(query_service.take());
            runtime.semantic_executor = None;
            runtime.semantic_activation_retry.reset();
            runtime.semantic_retry = Default::default();
            runtime.semantic_blocked_job = None;
            runtime.sidecar_drain = Default::default();
            let _ = runtime.semantic_runtime.release_if_idle();
            reload.load_failed(error);
            return DaemonConfigReloadOutcome::Continue;
        }
    };
    if args.profile == DaemonRunProfile::FiniteCoreWorker {
        config.daemon.mode = DaemonMode::SourceRefreshOnly;
        config.semantic_enabled = false;
    }
    reload.begin_attempt(&config);

    if !config.daemon.enabled && !args.force {
        runtime.config = config;
        runtime.semantic_executor = None;
        runtime.semantic_activation_retry.reset();
        runtime.sidecar_drain = Default::default();
        drop(query_service.take());
        drop(refresh_service.take());
        let _ = runtime.semantic_runtime.release_if_idle();
        reload.applied();
        return DaemonConfigReloadOutcome::StopDisabled;
    }

    let service_supported = daemon_query_service_transport_supported();
    let semantic_runtime_requested = daemon_semantic_runtime_requested(&config, service_supported);
    let semantic_runtime_request_changed =
        daemon_semantic_runtime_requested(&runtime.config, service_supported)
            != semantic_runtime_requested;
    let executor_changed = runtime.config.semantic_executor != config.semantic_executor;
    let builtin_throttling_changed = runtime.config.semantic_builtin_throttling_configured
        != config.semantic_builtin_throttling_configured
        || runtime.config.semantic_builtin_throttling_effective()
            != config.semantic_builtin_throttling_effective();
    let semantic_activation_changed =
        runtime.config.semantic_search_enabled() != config.semantic_search_enabled();
    if executor_changed
        || builtin_throttling_changed
        || semantic_activation_changed
        || semantic_runtime_request_changed
    {
        drop(query_service.take());
        runtime.semantic_executor = None;
        runtime.semantic_activation_retry.reset();
        runtime.semantic_retry = Default::default();
        runtime.semantic_blocked_job = None;
        runtime.sidecar_drain = Default::default();
        let _ = runtime.semantic_runtime.release_if_idle();
    }
    runtime.config = config;
    if service_supported && refresh_service.is_none() {
        let Some(source_refresh) = runtime.source_refresh_coordinator.as_ref().cloned() else {
            reload.activation_failed(anyhow!(
                "daemon source refresh engine was not recovered before IPC activation"
            ));
            return DaemonConfigReloadOutcome::Continue;
        };
        let handler = ctx_authenticated_request_handler_with_lifecycle(
            data_root,
            runtime.semantic_runtime.clone(),
            None,
            source_refresh,
            Arc::clone(wakeup),
            config_port,
            Arc::clone(lifecycle),
        );
        let started = start_daemon_source_refresh_service(data_root, handler, Arc::clone(wakeup));
        match started {
            Ok(service) => *refresh_service = Some(service),
            Err(error) => {
                reload.activation_failed(error);
                return DaemonConfigReloadOutcome::Continue;
            }
        }
    }
    let core_refresh_pending = runtime
        .source_refresh_coordinator
        .as_ref()
        .is_some_and(|refresh| refresh.has_pending_request());
    if semantic_runtime_requested
        && runtime.semantic_executor.is_none()
        && runtime.semantic_activation_retry.consecutive_failures > 0
        && (!runtime.semantic_activation_retry.ready() || core_refresh_pending)
    {
        reload.activation_failed(anyhow!(previous_activation_error.unwrap_or_else(|| {
            "semantic executor activation is waiting for its retry deadline".to_owned()
        })));
        return DaemonConfigReloadOutcome::Continue;
    }
    let selected_executor = if semantic_runtime_requested {
        runtime.semantic_executor.clone().map_or_else(
            || {
                build_executor(
                    runtime.config.semantic_executor.clone(),
                    config_port.semantic_executor_auth()?,
                    runtime.semantic_runtime.clone(),
                    config_port.semantic_model_config(data_root),
                )
                .map(Arc::new)
                .map(Some)
            },
            |executor| Ok(Some(executor)),
        )
    } else {
        Ok(None)
    };
    let selected_executor = match selected_executor {
        Ok(executor) => executor,
        Err(error) => {
            semantic_activation_failed(runtime, reload, error);
            return DaemonConfigReloadOutcome::Continue;
        }
    };
    if let Some(executor) = selected_executor.as_ref() {
        let selected_contract = executor.executor().contract();
        let configured_contract = runtime.config.semantic_executor.contract();
        if selected_contract.fingerprint() != configured_contract.fingerprint() {
            drop(query_service.take());
            runtime.semantic_executor = None;
            runtime.semantic_retry = Default::default();
            runtime.semantic_blocked_job = None;
            runtime.sidecar_drain = Default::default();
            let _ = runtime.semantic_runtime.release_if_idle();
            semantic_activation_failed(
                runtime,
                reload,
                anyhow!("semantic executor contract does not match the selected configuration"),
            );
            return DaemonConfigReloadOutcome::Continue;
        }
        if let Err(error) = executor.verify_contract() {
            drop(query_service.take());
            runtime.semantic_executor = None;
            runtime.semantic_retry = Default::default();
            runtime.semantic_blocked_job = None;
            runtime.sidecar_drain = Default::default();
            let _ = runtime.semantic_runtime.release_if_idle();
            semantic_activation_failed(runtime, reload, error);
            return DaemonConfigReloadOutcome::Continue;
        }
    }
    if semantic_runtime_requested && query_service.is_none() {
        let Some(source_refresh) = runtime.source_refresh_coordinator.as_ref().cloned() else {
            reload.activation_failed(anyhow!(
                "daemon source refresh engine was not recovered before IPC activation"
            ));
            return DaemonConfigReloadOutcome::Continue;
        };
        let handler = ctx_authenticated_request_handler_with_lifecycle(
            data_root,
            runtime.semantic_runtime.clone(),
            selected_executor.clone(),
            source_refresh,
            Arc::clone(wakeup),
            config_port,
            Arc::clone(lifecycle),
        );
        match start_daemon_query_service(data_root, handler, Arc::clone(wakeup)) {
            Ok(service) => {
                *query_service = Some(service);
                // The query service thread keeps normal interactive priority.
                lower_semantic_worker_priority();
            }
            Err(error) => {
                runtime.semantic_executor = None;
                runtime.semantic_retry = Default::default();
                runtime.semantic_blocked_job = None;
                runtime.sidecar_drain = Default::default();
                let _ = runtime.semantic_runtime.release_if_idle();
                semantic_activation_failed(runtime, reload, error);
                return DaemonConfigReloadOutcome::Continue;
            }
        }
    } else if !semantic_runtime_requested && query_service.is_some() {
        drop(query_service.take());
        let _ = runtime.semantic_runtime.release_if_idle();
    }
    runtime.semantic_executor = selected_executor;
    runtime.semantic_activation_retry.reset();

    reload.applied();
    DaemonConfigReloadOutcome::Continue
}

fn semantic_activation_failed(
    runtime: &mut DaemonRuntime,
    reload: &mut DaemonConfigReloadState,
    error: anyhow::Error,
) {
    runtime.semantic_activation_retry.record_failure();
    reload.activation_failed(error);
}

pub(super) fn daemon_semantic_runtime_requested(
    config: &AppConfig,
    service_supported: bool,
) -> bool {
    service_supported
        && config.semantic_search_enabled()
        && (config.semantic_executor.kind() == SemanticEmbeddingExecutorKind::Http
            || semantic_query_service_supported())
        && !config.daemon.mode.runs_only_source_refresh()
}

pub(super) fn daemon_semantic_runtime_active(
    runtime: &DaemonRuntime,
    query_service: Option<&DaemonQueryService>,
) -> bool {
    query_service.is_some()
        && runtime.semantic_executor.is_some()
        && runtime.config.semantic_search_enabled()
        && (runtime.config.semantic_executor.kind() == SemanticEmbeddingExecutorKind::Http
            || semantic_query_service_supported())
}

#[cfg(test)]
#[path = "config_reload/tests.rs"]
mod tests;
