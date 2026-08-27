use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use ctx_cli_presentation::commands::{
    render_semantic_disabled, render_semantic_status, SemanticArgs, SemanticCommand,
};
use ctx_history_cli::HistoryConfigPort;
use serde_json::{json, Value};

use crate::{
    config,
    history_config::CliHistoryConfigAdapter,
    output::{compact_json, print_json},
    ui::Ui,
};

pub(crate) fn run_semantic(
    args: SemanticArgs,
    data_root: PathBuf,
    quiet: bool,
    config: &mut config::AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    match args.command {
        SemanticCommand::Status(args) => {
            let report = semantic_report(&data_root, config, "status", true)?;
            render_report(report, args.format.is_json(), quiet, ui)
        }
        SemanticCommand::Enable(args) => {
            if args.intensity.is_some() && !args.wait {
                bail!("semantic --intensity requires --wait");
            }
            if args.wait && !config.automatic_indexing_enabled() {
                bail!(
                    "semantic --wait requires automatic indexing; run `ctx index mode auto` or omit --wait and use an explicit semantic search with --refresh wait"
                );
            }
            let temporary_full = matches!(
                args.intensity,
                Some(ctx_cli_presentation::commands::semantic::SemanticEnableIntensityArg::Full)
            );
            let process_override_blocks_enable = config.semantic_search_source() == "environment"
                && !config.semantic_search_enabled();
            let mut intensity_lease = None;
            if temporary_full && !process_override_blocks_enable {
                // Bring up the authenticated control service while a first-time
                // opt-in is still disabled, then establish full authority before
                // semantic work can be scheduled.
                crate::semantic::autostart_daemon_and_wait(
                    &data_root,
                    config,
                    crate::DaemonTriggerCommandArg::Semantic,
                )?;
                intensity_lease =
                    Some(ctx_daemon_cli::SemanticIndexingIntensityLease::acquire_full(&data_root)?);
            }
            set_semantic_policy(&data_root, config, true)?;
            if config.automatic_indexing_enabled() {
                crate::semantic::autostart_daemon_and_wait(
                    &data_root,
                    config,
                    crate::DaemonTriggerCommandArg::Semantic,
                )?;
            }

            if args.wait {
                let mut telemetry = crate::analytics::IndexTelemetry::default();
                return super::index::run_semantic_index_wait(
                    args.format,
                    data_root,
                    quiet,
                    &mut telemetry,
                    intensity_lease.as_mut(),
                    ui,
                );
            }
            let report = semantic_report(&data_root, config, "enable", false)?;
            render_report(report, args.format.is_json(), quiet, ui)
        }
        SemanticCommand::Disable(args) => {
            set_semantic_policy(&data_root, config, false)?;
            let report = semantic_report(&data_root, config, "disable", false)?;
            if args.format.is_json() {
                print_json(report)
            } else if !quiet {
                ui.write_stdout(&render_semantic_disabled(ui.stdout_context(), &report))?;
                Ok(())
            } else {
                Ok(())
            }
        }
    }
}

pub(crate) fn persist_semantic_enabled(
    data_root: &Path,
    config: &mut config::AppConfig,
    enabled: bool,
) -> Result<()> {
    CliHistoryConfigAdapter::new(data_root, config).set_semantic_search_enabled(enabled)
}

pub(crate) fn set_semantic_policy(
    data_root: &Path,
    config: &mut config::AppConfig,
    enabled: bool,
) -> Result<()> {
    persist_semantic_enabled(data_root, config, enabled)?;
    *config = config::AppConfig::load(data_root)?;
    if config.semantic_search_enabled() != enabled {
        if enabled {
            bail!(
                "semantic search was enabled in config, but an active process override keeps it disabled; unset CTX_SEARCH_SEMANTIC or set it to true"
            );
        }
        bail!(
            "semantic search was disabled in config, but an active process override keeps it enabled; unset CTX_SEARCH_SEMANTIC or set it to false"
        );
    }
    Ok(())
}

fn semantic_report(
    data_root: &Path,
    config: &config::AppConfig,
    operation: &str,
    read_only: bool,
) -> Result<Value> {
    let source = crate::semantic::source_epoch_status_report(data_root, config)?;
    let semantic = &source.report["semantic"];
    let daemon = &source.report["daemon"];
    let daemon_semantic = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("semantic_index"));
    let configured_intensity = config.semantic_indexing_intensity().as_str();
    let effective_intensity = daemon_semantic
        .and_then(|job| job.get("effective_indexing_intensity"))
        .and_then(Value::as_str)
        .or_else(|| {
            semantic
                .pointer("/indexing_intensity/effective")
                .and_then(Value::as_str)
        })
        .unwrap_or(configured_intensity);
    let (status, reason) = semantic_lifecycle_state(semantic, daemon, daemon_semantic, config);
    Ok(compact_json(json!({
        "schema_version": 1,
        "operation": operation,
        "enabled": semantic.get("enabled"),
        "status": status,
        "reason": reason,
        "config_source": config.semantic_search_source(),
        "indexing_intensity": {
            "configured": configured_intensity,
            "effective": effective_intensity,
            "config_source": config.semantic_indexing_intensity_source(),
        },
        "indexing": {
            "mode": config.indexing.mode.as_str(),
        },
        "projection": semantic.get("flat_f32"),
        "catch_up": semantic.get("catch_up"),
        "daemon": {
            "status": daemon.get("status"),
            "running": daemon.get("running"),
            "semantic_index": daemon_semantic,
        },
        "local_only": true,
        "read_only": read_only,
    })))
}

fn semantic_lifecycle_state(
    semantic: &Value,
    daemon: &Value,
    daemon_semantic: Option<&Value>,
    config: &config::AppConfig,
) -> (Value, Value) {
    let enabled = semantic
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let daemon_still_semantic = daemon_semantic.is_some_and(|job| {
        [
            "semantic_enabled",
            "runtime_active",
            "configuration_pending",
        ]
        .into_iter()
        .any(|field| job.get(field).and_then(Value::as_bool).unwrap_or(false))
    });
    if !enabled && daemon_still_semantic {
        return (json!("disabling"), json!("daemon_config_reload_pending"));
    }
    if enabled {
        let daemon_job_status = daemon_semantic
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str);
        if matches!(daemon_job_status, Some("failed" | "unavailable")) {
            let reason = daemon_semantic
                .and_then(|job| job.get("reason"))
                .cloned()
                .unwrap_or_else(|| json!("daemon_semantic_job_failed"));
            return (json!("failed"), reason);
        }
        let source_pending = semantic.get("status").and_then(Value::as_str) == Some("pending");
        let daemon_running = daemon
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if source_pending && config.automatic_indexing_enabled() && !daemon_running {
            return (json!("unavailable"), json!("daemon_not_running"));
        }
    }
    (
        semantic.get("status").cloned().unwrap_or(Value::Null),
        semantic.get("reason").cloned().unwrap_or(Value::Null),
    )
}

fn render_report(report: Value, json: bool, quiet: bool, ui: &mut Ui) -> Result<()> {
    if json {
        print_json(report)
    } else if !quiet {
        ui.write_stdout(&render_semantic_status(ui.stdout_context(), &report))?;
        Ok(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn persistence_helper_is_idempotent_and_preserves_other_config() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(config::CONFIG_FILE),
            "# user setting\n[analytics]\nenabled = false\n",
        )
        .unwrap();
        let mut config = config::AppConfig::load(temp.path()).unwrap();

        persist_semantic_enabled(temp.path(), &mut config, true).unwrap();
        let once = fs::read_to_string(temp.path().join(config::CONFIG_FILE)).unwrap();
        persist_semantic_enabled(temp.path(), &mut config, true).unwrap();

        assert_eq!(
            fs::read_to_string(temp.path().join(config::CONFIG_FILE)).unwrap(),
            once
        );
        assert!(once.contains("# user setting"), "{once}");
        assert!(once.contains("[semantic]\nenabled = true\n"), "{once}");
    }

    #[test]
    fn lifecycle_reports_pending_disable_until_the_daemon_quiesces() {
        let config = config::AppConfig::default();
        let semantic = json!({"enabled": false, "status": "disabled", "reason": "opt_out"});
        let daemon = json!({"running": true});
        let job = json!({
            "status": "ready",
            "semantic_enabled": true,
            "runtime_active": true,
            "configuration_pending": false,
        });

        let (status, reason) = semantic_lifecycle_state(&semantic, &daemon, Some(&job), &config);

        assert_eq!(status, "disabling");
        assert_eq!(reason, "daemon_config_reload_pending");
    }

    #[test]
    fn lifecycle_surfaces_daemon_semantic_failure_reason() {
        let mut config = config::AppConfig::default();
        config.semantic.enabled = Some(true);
        let semantic = json!({"enabled": true, "status": "pending"});
        let daemon = json!({"running": true});
        let job = json!({"status": "failed", "reason": "model_checksum_mismatch"});

        let (status, reason) = semantic_lifecycle_state(&semantic, &daemon, Some(&job), &config);

        assert_eq!(status, "failed");
        assert_eq!(reason, "model_checksum_mismatch");
    }
}
