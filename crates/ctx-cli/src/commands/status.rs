use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use ctx_history_core::database_path;

use crate::analytics::{count_bucket, StatusTelemetry};
use crate::config::{self, CONFIG_FILE};
use crate::local_usage;
use crate::output::print_json;
use crate::provider_projection;
use crate::semantic::{
    daemon_report, semantic_worker_report_cached, semantic_worker_report_configured_json,
};
use crate::store_util::open_existing_store_read_only;
use crate::{StatusArgs, UsageStatusMode};

const LEXICAL_INDEX_BYTES_PER_SECOND: u64 = 16 * 1024 * 1024;

pub(super) fn upgrade_report(config: &config::AppConfig) -> serde_json::Value {
    crate::upgrade::upgrade_diagnostics(config).report
}

fn inventory_source_bytes(db_path: &Path) -> Result<u64> {
    let connection = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "open ctx status inventory database {} read-only",
            db_path.display()
        )
    })?;
    let (catalog_bytes, source_import_bytes) = connection.query_row(
        r#"
        SELECT
            COALESCE((
                SELECT SUM(file_size_bytes)
                FROM catalog_sessions
                WHERE is_stale = 0
            ), 0),
            COALESCE((
                SELECT SUM(file_size_bytes)
                FROM source_import_files
                WHERE is_stale = 0
                  AND json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
                  AND json_type(metadata_json, '$.inventory_control_v1') IS NULL
                  AND json_type(metadata_json, '$.inventory_generation_v1') IS NULL
                  AND json_type(metadata_json, '$.inventory_phase_v1') IS NULL
                  AND json_type(metadata_json, '$.inventory_discovery_complete_v1') IS NULL
                  AND json_type(metadata_json, '$.inventory_reconciliation_stage_v1') IS NULL
                  AND json_type(metadata_json, '$.inventory_stale_keyset_v1') IS NULL
            ), 0)
        "#,
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let catalog_bytes =
        u64::try_from(catalog_bytes).context("catalog source bytes must be nonnegative")?;
    let source_import_bytes =
        u64::try_from(source_import_bytes).context("source import bytes must be nonnegative")?;
    catalog_bytes
        .checked_add(source_import_bytes)
        .context("inventory source byte total exceeds u64")
}

fn lexical_index_estimate_seconds(source_bytes: u64) -> u64 {
    if source_bytes == 0 {
        0
    } else {
        source_bytes.div_ceil(LEXICAL_INDEX_BYTES_PER_SECOND).max(1)
    }
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
    let db_path = database_path(data_root.clone());
    let initialized = db_path.exists();
    let config_path = data_root.join(CONFIG_FILE);
    let Some(config) = load_status_config(&data_root) else {
        return malformed_config_failure(args.format.is_json());
    };
    let (
        records,
        sessions,
        events,
        sources,
        catalog_counts,
        source_import_file_counts,
        inventory_source_bytes,
        semantic,
        daemon,
    ) = if initialized {
        let store = open_existing_store_read_only(&db_path, "ctx status")?;
        let counts = store.indexed_history_counts()?;
        let semantic_report = semantic_worker_report_cached(&data_root, Some(&store))?;
        let daemon = daemon_report(&data_root, &semantic_report);
        let catalog_counts = store.catalog_session_counts()?;
        let inventory_source_bytes = inventory_source_bytes(&db_path)?;
        (
            counts.items(),
            counts.sessions,
            counts.events,
            store.capture_source_count()?,
            catalog_counts,
            store.source_import_file_counts()?,
            Some(inventory_source_bytes),
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    } else {
        let semantic_report = semantic_worker_report_cached(&data_root, None)?;
        let daemon = daemon_report(&data_root, &semantic_report);
        (
            0,
            0,
            0,
            0,
            Default::default(),
            Default::default(),
            None,
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    };
    let inventory_units = catalog_counts
        .total
        .saturating_add(source_import_file_counts.total);
    let pending_inventory_units = catalog_counts
        .pending
        .saturating_add(source_import_file_counts.pending);
    let failed_inventory_units = catalog_counts
        .failed
        .saturating_add(source_import_file_counts.failed);
    let stale_inventory_units = catalog_counts
        .stale
        .saturating_add(source_import_file_counts.stale);
    telemetry.initialized = Some(initialized);
    telemetry.indexed_items = Some(count_bucket(records as u64));
    telemetry.indexed_sessions = Some(count_bucket(sessions as u64));
    telemetry.indexed_events = Some(count_bucket(events as u64));
    telemetry.indexed_sources = Some(count_bucket(sources as u64));
    telemetry.inventory_units = Some(count_bucket(inventory_units as u64));
    telemetry.pending_inventory_units = Some(count_bucket(pending_inventory_units as u64));
    telemetry.failed_inventory_units = Some(count_bucket(failed_inventory_units as u64));
    telemetry.stale_inventory_units = Some(count_bucket(stale_inventory_units as u64));
    let mut pro = crate::pro::lifecycle_status_json(&data_root);
    if let Some(object) = pro.as_object_mut() {
        object.insert(
            "conversion_action".to_owned(),
            local_usage::pro_conversion_action(object.get("access_state").and_then(Value::as_str))
                .unwrap_or(Value::Null),
        );
    }
    let upgrade = upgrade_report(&config);
    let provider_projection_state = provider_projection::observe(&data_root);
    let provider_projection = provider_projection::status_json(&data_root);
    let local_usage = local_usage::read_report(
        &data_root,
        config.local_usage.enabled,
        args.usage.detailed(),
    );

    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "initialized": initialized,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "indexed_items": records,
            "indexed_sessions": sessions,
            "indexed_events": events,
            "indexed_sources": sources,
            "inventory_units": inventory_units,
            "pending_inventory_units": pending_inventory_units,
            "failed_inventory_units": failed_inventory_units,
            "stale_inventory_units": stale_inventory_units,
            "cataloged_sessions": catalog_counts.total,
            "indexed_catalog_sessions": catalog_counts.indexed,
            "pending_catalog_sessions": catalog_counts.pending,
            "failed_catalog_sessions": catalog_counts.failed,
            "stale_catalog_sessions": catalog_counts.stale,
            "source_import_files": source_import_file_counts.total,
            "indexed_source_import_files": source_import_file_counts.indexed,
            "pending_source_import_files": source_import_file_counts.pending,
            "failed_source_import_files": source_import_file_counts.failed,
            "stale_source_import_files": source_import_file_counts.stale,
            "inventory_source_bytes": inventory_source_bytes,
            "lexical_index_estimate_seconds": inventory_source_bytes
                .map(lexical_index_estimate_seconds),
            "semantic": semantic,
            "daemon": daemon,
            "upgrade": upgrade,
            "provider_projection": provider_projection,
            "pro": pro,
            "local_usage": local_usage,
            "local_usage_action": Value::Null,
            "local_only": true,
            "read_only": !args.usage.modifies_state(),
        }))?;
    } else if !quiet {
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", db_path.display());
        println!("config_path: {}", config_path.display());
        println!("initialized: {initialized}");
        println!("indexed_items: {records}");
        println!("indexed_sources: {sources}");
        println!("inventory_units: {inventory_units}");
        println!("pending_inventory_units: {pending_inventory_units}");
        println!("failed_inventory_units: {failed_inventory_units}");
        println!("stale_inventory_units: {stale_inventory_units}");
        println!("cataloged_sessions: {}", catalog_counts.total);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("source_import_files: {}", source_import_file_counts.total);
        println!(
            "indexed_source_import_files: {}",
            source_import_file_counts.indexed
        );
        println!(
            "pending_source_import_files: {}",
            source_import_file_counts.pending
        );
        println!(
            "failed_source_import_files: {}",
            source_import_file_counts.failed
        );
        println!(
            "stale_source_import_files: {}",
            source_import_file_counts.stale
        );
        println!(
            "semantic_status: {}",
            semantic
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
        );
        println!(
            "semantic_embedded_items: {}",
            semantic
                .get("coverage")
                .and_then(|coverage| coverage.get("embedded_items"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        );
        println!(
            "daemon_enabled: {}",
            daemon
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(true)
        );
        println!(
            "daemon_status: {}",
            daemon
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
        );
        println!("upgrade_auto: {}", config.auto_upgrade_mode().as_str());
        println!(
            "provider_projection: {}",
            provider_projection_state.as_str()
        );
        if let Some(notice) = provider_projection::pending_notice(provider_projection_state) {
            println!("provider_projection_notice: {notice}");
        }
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
