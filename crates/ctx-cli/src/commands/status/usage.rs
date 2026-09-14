use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::{local_usage, output::print_json, ui::Ui, UsageStatusMode};
use ctx_app_config as config;
use ctx_cli_presentation::commands::{
    malformed_status_config_json, removed_cloud_config_json,
    render_malformed_status_config_failure, render_removed_cloud_config_failure,
    render_usage_action_human, render_usage_failure, usage_action_error_json, usage_action_json,
};

pub(crate) fn run_usage_action(
    mode: UsageStatusMode,
    data_root: &Path,
    storage: &local_usage::LocalUsageStorageAuthority,
    json_output: bool,
    quiet: bool,
    ui: &mut Ui,
) -> Result<()> {
    match mode {
        UsageStatusMode::Enable | UsageStatusMode::Disable => {
            let enabled = mode == UsageStatusMode::Enable;
            if config::set_local_usage_enabled(data_root, enabled).is_err() {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be changed",
                    ui,
                );
            }
            let Ok(control) = config::read_local_usage_control(data_root) else {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be confirmed",
                    ui,
                );
            };
            emit_usage_action(
                mode,
                json_output,
                quiet,
                json!({
                    "persisted_enabled": control.persisted_enabled,
                    "effective_enabled": control.effective_enabled,
                    "environment_override": control.environment_override.as_str(),
                }),
                ui,
            )
        }
        UsageStatusMode::Reset => {
            let store_state = match local_usage::reset_authorized(storage) {
                Ok(true) => "cleared",
                Ok(false) => "missing",
                Err(_) => {
                    return usage_action_failure(
                        mode,
                        json_output,
                        "usage_reset_failed",
                        "local usage could not be reset",
                        ui,
                    );
                }
            };
            emit_usage_action(
                mode,
                json_output,
                quiet,
                json!({"store_state": store_state}),
                ui,
            )
        }
    }
}

pub(crate) fn malformed_config_failure(json_output: bool, ui: &mut Ui) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&malformed_status_config_json())
                .expect("malformed-config status errors contain only static JSON")
        );
    } else {
        let document = render_malformed_status_config_failure(ui.stderr_context());
        ui.write_stderr(&document)?;
    }
    Err(crate::dispatch::rendered_cli_error())
}

pub(crate) fn removed_cloud_config_failure(json_output: bool, ui: &mut Ui) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&removed_cloud_config_json())
                .expect("removed-cloud status errors contain only static JSON")
        );
    } else {
        let document = render_removed_cloud_config_failure(ui.stderr_context());
        ui.write_stderr(&document)?;
    }
    Err(crate::dispatch::rendered_cli_error())
}

fn emit_usage_action(
    mode: UsageStatusMode,
    json_output: bool,
    quiet: bool,
    fields: Value,
    ui: &mut Ui,
) -> Result<()> {
    let mut action = fields.as_object().cloned().unwrap_or_default();
    action.insert("action".to_owned(), json!(mode.as_str()));
    action.insert("ok".to_owned(), json!(true));
    if json_output {
        print_json(usage_action_json(&action))?;
    } else if !quiet {
        let document = render_usage_action_human(ui.stdout_context(), mode, &action);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn usage_action_failure(
    mode: UsageStatusMode,
    json_output: bool,
    code: &'static str,
    message: &'static str,
    ui: &mut Ui,
) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&usage_action_error_json(mode, code, message))
                .expect("usage action errors contain only static JSON")
        );
    } else {
        let document = render_usage_failure(ui.stderr_context(), mode, code, message);
        ui.write_stderr(&document)?;
    }
    Err(crate::dispatch::rendered_cli_error())
}
