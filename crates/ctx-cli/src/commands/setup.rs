use std::path::PathBuf;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::analytics::{self, SetupMode, SetupTelemetry};
use crate::config::CONFIG_FILE;
use crate::output::print_json;
use crate::semantic::{
    autostart_daemon_and_wait, coordinate_source_backed_refresh,
    daemon_autostart_can_reuse_existing, daemon_autostart_suppression_reason,
    semantic_query_service_supported, source_epoch_status_report, DaemonHandoff,
    SourceBackedRefreshMode,
};
use crate::upgrade::data_migration;
use crate::{config, SetupArgs};

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    telemetry: &mut SetupTelemetry,
    _provider_refreshes: &mut crate::commands::import::ProviderRefreshCollector,
    quiet: bool,
    config: &mut config::AppConfig,
) -> Result<()> {
    let semantic_supported = semantic_query_service_supported();
    if args.semantic && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "`ctx setup --semantic` requires daemon maintenance. Enable [daemon] enabled = true and rerun without --no-daemon"
        );
    }
    if args.semantic {
        config::set_semantic_search_enabled(&data_root, true)?;
        config.search.semantic = Some(true);
    }
    let semantic_enabled = config.semantic_search_enabled();
    if semantic_enabled && semantic_supported && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, remove --no-daemon, or set [search] semantic = false"
        );
    }

    let epoch = data_migration::prepare(&data_root, &[])?;
    config::write_default_config(&data_root)?;

    let json_output = args.format.is_json();
    let machine_readable_output =
        json_output || args.progress == crate::progress::ProgressArg::Json;
    let suppression_reason = daemon_autostart_suppression_reason();
    let can_reuse_daemon =
        suppression_reason.is_none() && daemon_autostart_can_reuse_existing(&data_root);
    let daemon_autostart_requested = config.daemon.enabled
        && !args.no_daemon
        && suppression_reason.is_none()
        && (!machine_readable_output || can_reuse_daemon);
    let daemon_autostart_reason = if args.no_daemon {
        Some("explicit_opt_out")
    } else if !config.daemon.enabled {
        Some("daemon_disabled")
    } else if machine_readable_output && !can_reuse_daemon {
        Some("machine_readable_output")
    } else {
        suppression_reason
    };
    let daemon_handoff = if daemon_autostart_requested {
        Some(autostart_daemon_and_wait(
            &data_root,
            config,
            crate::DaemonTriggerCommandArg::Setup,
        )?)
    } else {
        None
    };

    let refresh_request = request_source_refresh(
        &data_root,
        config.daemon.enabled,
        args.no_daemon,
        args.wait,
        daemon_autostart_reason,
    );
    let source = source_epoch_status_report(&data_root, config)?;
    let lexical_status = source.report["lexical"]["status"]
        .as_str()
        .unwrap_or("unavailable");
    telemetry.mode = Some(if lexical_status == "ready" {
        SetupMode::Ready
    } else {
        SetupMode::Background
    });
    telemetry.providers_detected = source.indexed_sources.map(analytics::count_bucket);
    telemetry.has_indexed_content = source.indexed_items.map(|count| count > 0);

    let mode = match lexical_status {
        "ready" => "ready",
        "pending" => "pending",
        "stale" => "stale",
        _ => "unavailable",
    };
    let output = json!({
        "schema_version": 2,
        "data_root": data_root,
        "config_path": data_root.join(CONFIG_FILE),
        "mode": mode,
        "history_epoch": source.report["history_epoch"].clone(),
        "lexical": source.report["lexical"].clone(),
        "catalog": source.report["catalog"].clone(),
        "resolver": source.report["resolver"].clone(),
        "refresh": source.report["refresh"].clone(),
        "refresh_request": refresh_request,
        "semantic": source.report["semantic"].clone(),
        "relational": source.report["relational"].clone(),
        "pro_projection": source.report["pro_projection"].clone(),
        "prior_epoch": source.report["prior_epoch"].clone(),
        "daemon": source.report["daemon"].clone(),
        "daemon_autostart": daemon_autostart_json(
            daemon_autostart_requested,
            daemon_autostart_reason,
            daemon_handoff.as_ref(),
        ),
        "deprecated_catalog_only_ignored": args.catalog_only,
        "source_rebuild_required": epoch.daemon_rebuild_required(),
        "network_required": false,
        "repo_writes": false,
    });

    if json_output {
        print_json(output)?;
    } else if !quiet {
        print_setup_human(
            &data_root,
            mode,
            &source.report,
            &refresh_request,
            daemon_autostart_requested,
            daemon_autostart_reason,
            daemon_handoff.as_ref(),
        );
    }
    Ok(())
}

fn request_source_refresh(
    data_root: &std::path::Path,
    daemon_enabled: bool,
    no_daemon: bool,
    wait: bool,
    daemon_unavailable_reason: Option<&str>,
) -> Value {
    if no_daemon || !daemon_enabled {
        return json!({
            "status": "unavailable",
            "reason": if no_daemon {
                "explicit_opt_out"
            } else {
                "daemon_disabled"
            },
            "mode": if wait { "wait" } else { "background" },
            "daemon_available": false,
        });
    }
    let mode = if wait {
        SourceBackedRefreshMode::Wait
    } else {
        SourceBackedRefreshMode::Background
    };
    match coordinate_source_backed_refresh(data_root, mode) {
        Ok(observation) => json!({
            "status": observation.status,
            "reason": Value::Null,
            "mode": if wait { "wait" } else { "background" },
            "request_id": observation.request_id,
            "daemon_available": observation.daemon_available,
            "source_count": observation.source_count,
            "published_generation": observation.pin.generation_id(),
        }),
        Err(error) => {
            let daemon_unavailable = error
                .downcast_ref::<crate::semantic::SourceBackedRefreshDaemonUnavailable>()
                .is_some();
            json!({
                "status": if daemon_unavailable {
                    "unavailable"
                } else if !wait {
                    "pending"
                } else {
                    "unavailable"
                },
                "reason": if daemon_unavailable {
                    daemon_unavailable_reason.unwrap_or("daemon_unavailable")
                } else if !wait {
                    "refresh_queued_without_published_generation"
                } else {
                    "refresh_failed"
                },
                "mode": if wait { "wait" } else { "background" },
                "daemon_available": !daemon_unavailable,
                "last_error": format!("{error:#}"),
            })
        }
    }
}

fn daemon_autostart_json(
    requested: bool,
    reason: Option<&str>,
    handoff: Option<&DaemonHandoff>,
) -> Value {
    match handoff {
        Some(handoff) => json!({
            "status": "verified",
            "reason": Value::Null,
            "requested": requested,
            "pid": handoff.pid,
            "status_command": "ctx daemon status",
        }),
        None => json!({
            "status": if requested { "unavailable" } else { "not_requested" },
            "reason": reason.unwrap_or("not_requested"),
            "requested": requested,
            "status_command": "ctx daemon status",
        }),
    }
}

fn print_setup_human(
    data_root: &std::path::Path,
    mode: &str,
    source: &Value,
    refresh_request: &Value,
    daemon_autostart_requested: bool,
    daemon_autostart_reason: Option<&str>,
    daemon_handoff: Option<&DaemonHandoff>,
) {
    println!("ctx source-backed history epoch: {mode}");
    if let Some(generation) = source["lexical"]["generation_id"].as_str() {
        println!("Lexical generation: {generation}");
    }
    if let Some(path) = source["lexical"]["path"].as_str() {
        println!("Lexical path: {path}");
    }
    if let Some(path) = source["semantic"]["flat_f32"]["path"].as_str() {
        println!("Semantic path: {path}");
    }
    println!(
        "Source refresh: {}",
        refresh_request["status"].as_str().unwrap_or("unavailable")
    );
    if source["prior_epoch"]["preserved"].as_bool() == Some(true) {
        println!(
            "Prior v0.25-or-earlier history epoch: preserved, non-authoritative, rollback/manual recovery only."
        );
    }
    match daemon_handoff {
        Some(handoff) => println!(
            "Daemon is running (PID {}); source refresh handoff is verified.",
            handoff.pid
        ),
        None if daemon_autostart_requested => {
            println!("Daemon handoff was not verified; run `ctx daemon status`.")
        }
        None if daemon_autostart_reason == Some("explicit_opt_out") => {
            println!("Daemon refresh was skipped because --no-daemon was used.")
        }
        None if daemon_autostart_reason == Some("daemon_disabled") => {
            println!("Daemon refresh is unavailable because daemon maintenance is disabled.")
        }
        None => {}
    }
    println!("Data: {}", data_root.display());
    println!();
    println!("Next:");
    println!("  ctx status");
    if mode == "ready" {
        println!("  ctx search \"test failure\"");
    } else {
        println!("  ctx daemon status");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn setup_source_has_no_legacy_store_runtime_dependency() {
        let source = include_str!("setup.rs");
        let runtime = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "ctx_history_store::Store",
            "Store::open",
            "run_import_internal",
            "inventory_available_sources",
            "ctx import --all",
        ] {
            assert!(
                !runtime.contains(forbidden),
                "setup retained forbidden legacy dependency {forbidden}"
            );
        }
    }
}
