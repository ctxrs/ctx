use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::analytics::{count_bucket, StatusTelemetry};
use crate::config::{self, CONFIG_FILE};
use crate::local_usage;
use crate::output::print_json;
use crate::semantic::source_epoch_status_report;
use crate::{StatusArgs, UsageStatusMode};

pub(super) fn upgrade_report(config: &config::AppConfig) -> serde_json::Value {
    crate::upgrade::upgrade_diagnostics(config).report
}

pub(crate) fn run_status(
    args: StatusArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut StatusTelemetry,
) -> Result<()> {
    if args.usage.modifies_state() {
        return run_usage_action(args.usage, &data_root, args.format.is_json(), quiet);
    }
    let config_path = data_root.join(CONFIG_FILE);
    let Some(config) = load_status_config(&data_root) else {
        return malformed_config_failure(args.format.is_json());
    };
    let source = source_epoch_status_report(&data_root, &config)?;
    telemetry.initialized = Some(source.initialized);
    telemetry.indexed_items = source.indexed_items.map(count_bucket);
    telemetry.indexed_sessions = source.indexed_sessions.map(count_bucket);
    telemetry.indexed_events = source.indexed_events.map(count_bucket);
    telemetry.indexed_sources = source.indexed_sources.map(count_bucket);
    let mut pro = crate::pro::lifecycle_status_json(&data_root);
    if let Some(object) = pro.as_object_mut() {
        object.insert(
            "conversion_action".to_owned(),
            local_usage::pro_conversion_action(object.get("access_state").and_then(Value::as_str))
                .unwrap_or(Value::Null),
        );
    }
    let upgrade = upgrade_report(&config);
    let local_usage = local_usage::read_report(
        &data_root,
        config.local_usage.enabled,
        args.usage.detailed(),
    );

    if args.format.is_json() {
        let mut report = source.report;
        if let Some(object) = report.as_object_mut() {
            object.insert("upgrade".to_owned(), upgrade);
            object.insert("pro".to_owned(), pro);
            object.insert("local_usage".to_owned(), json!(local_usage));
            object.insert("local_usage_action".to_owned(), Value::Null);
            object.insert("read_only".to_owned(), json!(!args.usage.modifies_state()));
        }
        print_json(report)?;
    } else if !quiet {
        println!("data_root: {}", data_root.display());
        println!("config_path: {}", config_path.display());
        println!("initialized: {}", source.initialized);
        print_optional_count("indexed_items", source.indexed_items);
        print_optional_count("indexed_sessions", source.indexed_sessions);
        print_optional_count("indexed_events", source.indexed_events);
        print_optional_count("indexed_sources", source.indexed_sources);
        print_component_status("history_epoch", &source.report["history_epoch"]);
        print_component_status("lexical", &source.report["lexical"]);
        if let Some(generation) = source.report["lexical"]["generation_id"].as_str() {
            println!("lexical_generation: {generation}");
        }
        if let Some(policy) = source.report["lexical"]["policy"]["published_hash"].as_str() {
            println!("lexical_policy_hash: {policy}");
        }
        print_component_status("catalog", &source.report["catalog"]);
        print_component_status("resolver", &source.report["resolver"]);
        print_component_status("source_refresh", &source.report["refresh"]);
        print_component_status("semantic", &source.report["semantic"]);
        print_component_status("flat_f32", &source.report["semantic"]["flat_f32"]);
        print_component_status("relational", &source.report["relational"]);
        print_component_status("pro_projection", &source.report["pro_projection"]);
        print_component_status("legacy_history", &source.report["legacy_history"]);
        let daemon = &source.report["daemon"];
        print_component_status("daemon", daemon);
        println!("upgrade_auto: {}", config.auto_upgrade_mode().as_str());
        if let Some(reason) = daemon.get("reason").and_then(|value| value.as_str()) {
            println!("daemon_reason: {reason}");
        }
        if daemon
            .get("recoverable")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            println!("daemon_recoverable: true");
        }
        if pro["installed"].as_bool() == Some(true) {
            println!(
                "pro_status: {}",
                pro["state"].as_str().unwrap_or("unavailable")
            );
            if let Some(access_state) = pro["access_state"].as_str() {
                println!("pro_access_state: {access_state}");
            }
            for field in [
                "refresh_after_unix",
                "access_deadline_unix",
                "grace_deadline_unix",
            ] {
                if let Some(deadline) = pro[field].as_i64() {
                    println!("pro_{field}: {deadline}");
                }
            }
            if let Some(action) = pro["conversion_action"].as_object() {
                if action.get("kind").and_then(Value::as_str) == Some("pro_restore_access") {
                    println!(
                        "pro_restore_access: graph preserved ({})",
                        action
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or("ctx pro manage")
                    );
                } else {
                    println!(
                        "pro_conversion: {} ({})",
                        action
                            .get("price")
                            .and_then(Value::as_str)
                            .unwrap_or("$20/month"),
                        action
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or("ctx pro manage")
                    );
                }
            }
        }
        local_usage::render_human_summary(&local_usage, args.usage.detailed());
        println!("local_only: true");
        println!("read_only: {}", !args.usage.modifies_state());
    }
    Ok(())
}

fn print_optional_count(name: &str, value: Option<u64>) {
    match value {
        Some(value) => println!("{name}: {value}"),
        None => println!("{name}: unavailable"),
    }
}

fn print_component_status(name: &str, component: &Value) {
    println!(
        "{name}_status: {}",
        component
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unavailable")
    );
    if let Some(reason) = component.get("reason").and_then(Value::as_str) {
        println!("{name}_reason: {reason}");
    }
}

fn load_status_config(data_root: &Path) -> Option<config::AppConfig> {
    // Dispatch already loaded this file, but a concurrent replacement can make
    // the status-specific reread fail. Discard that raw cause here so neither
    // its path nor its content can reach the generic CLI error renderer.
    config::AppConfig::load(data_root).ok()
}

pub(crate) fn run_usage_action(
    mode: UsageStatusMode,
    data_root: &std::path::Path,
    json_output: bool,
    quiet: bool,
) -> Result<()> {
    match mode {
        UsageStatusMode::Summary | UsageStatusMode::Detail => {
            unreachable!("reporting modes do not modify local usage")
        }
        UsageStatusMode::Enable => {
            if config::set_local_usage_enabled(data_root, true).is_err() {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be changed",
                );
            }
            let Ok(control) = config::read_local_usage_control(data_root) else {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be confirmed",
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
            )
        }
        UsageStatusMode::Disable => {
            if config::set_local_usage_enabled(data_root, false).is_err() {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be changed",
                );
            }
            let Ok(control) = config::read_local_usage_control(data_root) else {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be confirmed",
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
            )
        }
        UsageStatusMode::Reset => {
            let store_state = match local_usage::reset(data_root) {
                Ok(true) => "cleared",
                Ok(false) => "missing",
                Err(_) => {
                    return usage_action_failure(
                        mode,
                        json_output,
                        "usage_reset_failed",
                        "local usage could not be reset",
                    );
                }
            };
            emit_usage_action(
                mode,
                json_output,
                quiet,
                json!({"store_state": store_state}),
            )
        }
    }
}

pub(crate) fn malformed_config_failure(json_output: bool) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&malformed_config_json())
                .expect("malformed-config status errors contain only static JSON")
        );
    } else {
        eprintln!("local_usage_config_unavailable: local usage configuration could not be read");
    }
    Err(crate::dispatch::rendered_cli_error())
}

pub(crate) fn removed_cloud_config_failure(json_output: bool) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "error": {
                    "code": "removed_config_key",
                    "config_key": "cloud.mode",
                    "message": "cloud history configuration is no longer supported",
                },
                "local_only": true,
                "read_only": true,
            }))
            .expect("removed-cloud status errors contain only static JSON")
        );
    } else {
        eprintln!(
            "removed_config_key: cloud.mode is no longer supported; remove it from config.toml"
        );
    }
    Err(crate::dispatch::rendered_cli_error())
}

fn malformed_config_json() -> Value {
    json!({
        "schema_version": 1,
        "local_usage": local_usage::UsageReport::config_error(),
        "local_usage_action": Value::Null,
        "local_only": true,
        "read_only": true,
    })
}

fn emit_usage_action(
    mode: UsageStatusMode,
    json_output: bool,
    quiet: bool,
    fields: Value,
) -> Result<()> {
    let mut action = fields.as_object().cloned().unwrap_or_default();
    action.insert("action".to_owned(), json!(mode.as_str()));
    action.insert("ok".to_owned(), json!(true));
    if json_output {
        print_json(json!({
            "schema_version": 1,
            "local_usage_action": action,
            "local_only": true,
            "read_only": false,
        }))?;
    } else if !quiet {
        println!("local_usage_action: {}", mode.as_str());
        match mode {
            UsageStatusMode::Enable | UsageStatusMode::Disable => {
                println!(
                    "local_usage_persisted_enabled: {}",
                    action["persisted_enabled"].as_bool().unwrap_or(false)
                );
                println!(
                    "local_usage_effective_enabled: {}",
                    action["effective_enabled"].as_bool().unwrap_or(false)
                );
                println!(
                    "local_usage_environment_override: {}",
                    action["environment_override"].as_str().unwrap_or("invalid")
                );
            }
            UsageStatusMode::Reset => println!(
                "local_usage_store: {}",
                action["store_state"].as_str().unwrap_or("missing")
            ),
            UsageStatusMode::Summary | UsageStatusMode::Detail => unreachable!(),
        }
    }
    Ok(())
}

fn usage_action_failure(
    mode: UsageStatusMode,
    json_output: bool,
    code: &'static str,
    message: &'static str,
) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "local_usage_action": {
                    "action": mode.as_str(),
                    "ok": false,
                    "error": {
                        "code": code,
                        "message": message,
                    },
                },
                "local_only": true,
                "read_only": false,
            }))
            .expect("usage action errors contain only static JSON")
        );
    } else {
        eprintln!("{code}: {message}");
    }
    Err(crate::dispatch::rendered_cli_error())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn status_config_replacement_discards_raw_second_load_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(CONFIG_FILE);
        fs::write(&path, "[local_usage]\nenabled = true\n").unwrap();
        config::AppConfig::load(temp.path()).unwrap();

        let marker = "SECRET_REPLACEMENT_CONFIG_15d2";
        fs::write(
            &path,
            format!("malformed status replacement /private/{marker}/credential\n"),
        )
        .unwrap();

        assert!(load_status_config(temp.path()).is_none());
        let rendered = serde_json::to_string(&malformed_config_json()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&rendered).unwrap()["local_usage"]["error"]["code"],
            "local_usage_config_unavailable"
        );
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains("credential"));
        assert!(!rendered.contains(temp.path().to_string_lossy().as_ref()));
    }
}
