#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{compact_json, config::AppConfig};

use super::health_search::{json_i64, json_string};

#[allow(unused_imports)]
pub(super) use ctx_daemon_runtime::*;

pub(super) fn daemon_core_refresh_job_path(data_root: &Path) -> PathBuf {
    ctx_daemon_service::daemon_core_refresh_job_path(data_root)
}

pub(super) fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    ctx_daemon_service::daemon_semantic_job_path(data_root)
}

fn application_config(config: &AppConfig<'_>) -> ctx_daemon_application::DaemonConfigSnapshot {
    ctx_daemon_application::DaemonConfigSnapshot {
        enabled: config.daemon.enabled,
        mode: super::daemon_supervisor::daemon_mode(config.daemon.mode),
        semantic_enabled: config.semantic_search_enabled(),
    }
}

#[cfg(test)]
pub(super) fn daemon_report(data_root: &Path) -> Value {
    daemon_report_with_disabled_status(data_root, true)
}

pub(super) fn daemon_report_with_disabled_status(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    super::daemon_supervisor::with_daemon_application(|application| {
        daemon_report_with_application(application, data_root, disabled_overrides_lifecycle)
    })
}

pub(super) fn daemon_report_with_config(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    current_config: &AppConfig<'_>,
) -> Value {
    super::daemon_supervisor::with_daemon_application(|application| {
        daemon_report_with_config_and_application(
            application,
            data_root,
            disabled_overrides_lifecycle,
            Some(current_config),
        )
    })
}

pub(crate) fn daemon_report_with_application(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let current_config = AppConfig::load(data_root).ok();
    daemon_report_with_config_and_application(
        application,
        data_root,
        disabled_overrides_lifecycle,
        current_config.as_ref(),
    )
}

fn daemon_report_with_config_and_application(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    current_config: Option<&AppConfig<'_>>,
) -> Value {
    let current_application_config = current_config.map(application_config);
    let preparation = application.prepare_daemon_status(
        data_root,
        disabled_overrides_lifecycle,
        current_application_config.as_ref(),
        crate::config::DAEMON_DEFAULT_ENABLED,
    );
    let semantic_job = daemon_semantic_job_report(
        data_root,
        disabled_overrides_lifecycle,
        preparation.semantic_context(),
        current_config,
    );
    let mut report = preparation.finish().into_json();
    if let Some(jobs) = report.get_mut("jobs").and_then(Value::as_object_mut) {
        jobs.insert("semantic_index".to_owned(), semantic_job);
    }
    report
}

fn daemon_semantic_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    context: ctx_daemon_application::DaemonSemanticStatusContext<'_>,
    current_config: Option<&AppConfig<'_>>,
) -> Value {
    let reload = context.config_reload;
    let daemon_enabled = reload
        .requested_daemon_enabled
        .or(reload.applied_daemon_enabled)
        .unwrap_or_else(|| {
            current_config
                .map(|config| config.daemon.enabled)
                .unwrap_or_else(|| AppConfig::default().daemon.enabled)
        });
    let semantic_enabled = reload
        .requested_semantic_enabled
        .or(reload.applied_semantic_enabled)
        .unwrap_or_else(|| current_config.is_some_and(AppConfig::semantic_search_enabled));
    let semantic_supported = super::semantic_query_service_supported();
    let mode_allows_semantic = !context.daemon_mode.runs_only_source_refresh();
    let enabled = daemon_enabled && semantic_enabled && semantic_supported && mode_allows_semantic;
    let activation_failed = reload.status == "activation_failed" && enabled;
    let reload_pending = context.daemon_running && reload.status == "pending" && reload.out_of_sync;
    let disabled = !enabled && disabled_overrides_lifecycle && !context.semantic_runtime_active;
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    let last_run_status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"));
    let last_run_reason = status_value
        .as_ref()
        .and_then(|value| json_string(value, "reason"));
    let status = if activation_failed {
        "failed"
    } else if reload_pending
        || (context.daemon_running && enabled && !context.semantic_runtime_active)
    {
        "pending"
    } else if disabled {
        "disabled"
    } else {
        last_run_status.as_deref().unwrap_or("unknown")
    };
    let reason = if activation_failed {
        Some("semantic_activation_failed".to_owned())
    } else if reload_pending {
        Some("daemon_config_reload_pending".to_owned())
    } else if context.daemon_running && enabled && !context.semantic_runtime_active {
        Some("semantic_runtime_inactive".to_owned())
    } else if disabled {
        Some(if context.daemon_mode.runs_only_source_refresh() {
            "daemon_mode_source_refresh_only".to_owned()
        } else if !semantic_enabled {
            "semantic_disabled".to_owned()
        } else if !semantic_supported {
            "unsupported_platform".to_owned()
        } else {
            "daemon_disabled".to_owned()
        })
    } else {
        last_run_reason.clone()
    };
    let embedding_runtime = (context.daemon_running && context.semantic_runtime_active)
        .then(|| crate::query_service::daemon_query_service_embedding_runtime(data_root))
        .flatten();
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "semantic_enabled": semantic_enabled,
        "daemon_configured": reload.applied_daemon_enabled,
        "semantic_configured": reload.applied_semantic_enabled,
        "runtime_active": context.semantic_runtime_active,
        "config_reload_status": reload.status,
        "configuration_pending": reload_pending,
        "reason": reason,
        "last_run_at_ms": status_value
            .as_ref()
            .and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": last_run_status,
        "last_run_reason": last_run_reason,
        "last_error": if activation_failed {
            reload.last_error.map(str::to_owned)
        } else {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "last_error"))
        },
        "retryable": status_value
            .as_ref()
            .and_then(|value| value.get("retryable").and_then(Value::as_bool)),
        "failure_class": status_value
            .as_ref()
            .and_then(|value| json_string(value, "failure_class")),
        "indexed_chunks": status_value
            .as_ref()
            .and_then(|value| value.get("indexed_chunks").and_then(Value::as_u64)),
        "model_key": status_value
            .as_ref()
            .and_then(|value| json_string(value, "model_key")),
        "embedding_runtime": embedding_runtime,
        "daemon_mode": context.daemon_mode.as_str(),
    }))
}

#[cfg(test)]
mod tests;
