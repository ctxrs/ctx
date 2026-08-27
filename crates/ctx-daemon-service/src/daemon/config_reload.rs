use std::{path::Path, sync::Arc};

use anyhow::anyhow;
use ctx_history_core::utc_now;
use ctx_semantic_model::{semantic_query_service_supported, SemanticIndexingIntensity};
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
    requested_semantic_indexing_intensity: SemanticIndexingIntensity,
    applied_daemon_enabled: Option<bool>,
    applied_daemon_mode: Option<DaemonMode>,
    applied_semantic_enabled: Option<bool>,
    applied_semantic_indexing_intensity: Option<SemanticIndexingIntensity>,
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
            requested_semantic_indexing_intensity: config.semantic_indexing_intensity(),
            applied_daemon_enabled: None,
            applied_daemon_mode: None,
            applied_semantic_enabled: None,
            applied_semantic_indexing_intensity: None,
            last_error: None,
        }
    }

    fn begin_attempt(&mut self, config: &AppConfig) {
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        self.requested_daemon_enabled = config.daemon.enabled;
        self.requested_daemon_mode = config.daemon.mode;
        self.requested_semantic_enabled = config.semantic_search_enabled();
        self.requested_semantic_indexing_intensity = config.semantic_indexing_intensity();
        self.last_error = None;
    }

    fn applied(&mut self) {
        self.status = "applied";
        self.last_applied_at_ms = Some(self.last_attempt_at_ms);
        self.applied_daemon_enabled = Some(self.requested_daemon_enabled);
        self.applied_daemon_mode = Some(self.requested_daemon_mode);
        self.applied_semantic_enabled = Some(self.requested_semantic_enabled);
        self.applied_semantic_indexing_intensity = Some(self.requested_semantic_indexing_intensity);
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
                "semantic_indexing_intensity": self.requested_semantic_indexing_intensity.as_str(),
            },
            "applied": {
                "daemon_enabled": self.applied_daemon_enabled,
                "daemon_mode": self.applied_daemon_mode.map(DaemonMode::as_str),
                "semantic_enabled": self.applied_semantic_enabled,
                "semantic_indexing_intensity": self
                    .applied_semantic_indexing_intensity
                    .map(SemanticIndexingIntensity::as_str),
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

pub(super) struct DaemonConfigReloadTargets<'a> {
    pub(super) query_service: &'a mut Option<DaemonQueryService>,
    pub(super) refresh_service: &'a mut Option<DaemonQueryService>,
    pub(super) state: &'a mut DaemonConfigReloadState,
}

pub(super) fn reload_daemon_runtime_config(
    data_root: &Path,
    args: &DaemonRunArgs,
    runtime: &mut DaemonRuntime,
    targets: DaemonConfigReloadTargets<'_>,
    wakeup: &Arc<DaemonWakeup>,
    lifecycle: &Arc<DaemonLifecycleState>,
    config_port: &'static dyn DaemonConfigPort,
) -> DaemonConfigReloadOutcome {
    let DaemonConfigReloadTargets {
        query_service,
        refresh_service,
        state: reload,
    } = targets;
    let mut config = match config_port.load(data_root) {
        Ok(config) => config,
        Err(error) => {
            reload.load_failed(error);
            return DaemonConfigReloadOutcome::Continue;
        }
    };
    if args.profile == DaemonRunProfile::FiniteCoreWorker {
        config.daemon.mode = DaemonMode::SourceRefreshOnly;
        config.semantic_enabled = false;
    }
    reload.begin_attempt(&config);
    runtime.config = config;
    runtime
        .semantic_intensity_leases
        .set_configured(runtime.config.semantic_indexing_intensity());

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
        let Some(source_refresh) = runtime.source_refresh_coordinator.as_ref().cloned() else {
            reload.activation_failed(anyhow!(
                "daemon source refresh engine was not recovered before IPC activation"
            ));
            return DaemonConfigReloadOutcome::Continue;
        };
        let handler = ctx_authenticated_request_handler_with_lifecycle(
            data_root,
            runtime.semantic_runtime.clone(),
            source_refresh,
            Arc::clone(wakeup),
            config_port,
            Arc::clone(lifecycle),
            Arc::clone(&runtime.semantic_intensity_leases),
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
            source_refresh,
            Arc::clone(wakeup),
            config_port,
            Arc::clone(lifecycle),
            Arc::clone(&runtime.semantic_intensity_leases),
        );
        match start_daemon_query_service(data_root, handler, Arc::clone(wakeup)) {
            Ok(service) => {
                *query_service = Some(service);
                // Full intensity changes only semantic inter-batch pacing. Keep
                // the daemon worker below the interactive query-service thread.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic_intensity::semantic_indexing_intensity_from_json;

    #[test]
    fn reload_json_reports_quiet_default_and_applied_full_intensity() {
        let quiet = AppConfig::default();
        let quiet_reload = DaemonConfigReloadState::pending(&quiet).to_json();
        assert_eq!(
            quiet_reload["requested"]["semantic_indexing_intensity"],
            "quiet"
        );
        assert!(quiet_reload["applied"]["semantic_indexing_intensity"].is_null());

        let mut full = quiet;
        full.semantic_indexing_intensity = SemanticIndexingIntensity::Full;
        let mut full_reload = DaemonConfigReloadState::pending(&full);
        full_reload.applied();
        let full_reload = full_reload.to_json();
        assert_eq!(
            full_reload["requested"]["semantic_indexing_intensity"],
            "full"
        );
        assert_eq!(
            full_reload["applied"]["semantic_indexing_intensity"],
            "full"
        );
    }

    #[test]
    fn missing_old_reload_intensity_compares_as_quiet() {
        let old_applied = json!({
            "daemon_enabled": true,
            "daemon_mode": "full",
            "semantic_enabled": true,
        });
        assert_eq!(
            semantic_indexing_intensity_from_json(old_applied.get("semantic_indexing_intensity")),
            SemanticIndexingIntensity::Quiet
        );
    }
}
