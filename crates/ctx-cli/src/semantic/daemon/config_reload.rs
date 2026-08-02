use std::{path::Path, sync::Arc};

use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonMode},
    DaemonRunArgs,
};

use super::{
    super::{
        daemon_wakeup::DaemonWakeup,
        paths_status::lower_semantic_worker_priority,
        query_service::{
            daemon_query_service_transport_supported, semantic_query_service_supported,
            start_daemon_query_service, start_daemon_source_refresh_service, DaemonQueryService,
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
    applied_daemon_enabled: Option<bool>,
    applied_daemon_mode: Option<DaemonMode>,
    applied_semantic_enabled: Option<bool>,
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

    pub(super) fn to_json(&self) -> Value {
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
pub(super) enum DaemonConfigReloadOutcome {
    Continue,
    StopDisabled,
}

pub(super) fn reload_daemon_runtime_config(
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
        match start_daemon_query_service(
            data_root,
            runtime.semantic_runtime.clone(),
            Arc::clone(wakeup),
        ) {
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

pub(super) fn daemon_semantic_runtime_requested(
    config: &AppConfig,
    service_supported: bool,
) -> bool {
    service_supported
        && config.semantic_search_enabled()
        && !config.daemon.mode.runs_only_source_refresh()
}

pub(super) fn daemon_semantic_runtime_active(
    runtime: &DaemonRuntime,
    query_service: Option<&DaemonQueryService>,
) -> bool {
    query_service.is_some()
        && runtime.config.semantic_search_enabled()
        && semantic_query_service_supported()
}
