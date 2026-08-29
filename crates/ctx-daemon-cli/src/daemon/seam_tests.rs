use std::{fs, process};

use ctx_daemon_runtime::DaemonLock;
use ctx_daemon_service::{DaemonIpcService, DaemonQueryEndpoint};

use super::super::paths_status::daemon_report;
use super::*;

#[cfg(unix)]
#[test]
fn source_refresh_only_status_exposes_runtime_and_certified_refresh_identity() -> Result<()> {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nmode = \"source-refresh-only\"\n",
    )?;
    let semantic_contract_fingerprint = AppConfig::load(temp.path())?
        .semantic_model_contract()
        .fingerprint()
        .to_owned();
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    ctx_daemon_service::testing::write_daemon_lifecycle_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "running",
            "pid": process::id(),
            "started_at_ms": now,
            "heartbeat_at_ms": now,
            "start_mode": "auto",
            "trigger_command": "search",
            "semantic_runtime_active": false,
            "config_reload": {
                "status": "applied",
                "requested": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                    "semantic_executor": "builtin",
                    "semantic_contract_fingerprint": semantic_contract_fingerprint,
                },
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                    "semantic_executor": "builtin",
                    "semantic_contract_fingerprint": semantic_contract_fingerprint,
                },
            },
        }),
    )?;
    ctx_daemon_service::testing::write_daemon_service_endpoint(
        temp.path(),
        DaemonIpcService::SourceRefresh,
        &DaemonQueryEndpoint::Unix {
            path: temp.path().join("daemon/source-refresh.sock"),
            token: "must-not-appear-in-status-00000000".to_owned(),
        },
    )?;
    ctx_daemon_service::testing::write_core_refresh_status(
        temp.path(),
        &json!({
            "status": "completed",
            "daemon_mode": "source-refresh-only",
            "trigger": "search",
            "trigger_provenance": "autostart",
            "certified_source_count": 4,
            "certified_source_bytes": 8192,
            "timings_us": {
                "discovery": 5,
                "scan_stage": 7,
                "commit": 11,
            },
        }),
    )?;

    let report = daemon_report(temp.path());

    assert_eq!(report["mode"], "source-refresh-only");
    assert_eq!(report["live_pid"], process::id());
    assert_eq!(report["trigger_command"], "search");
    assert_eq!(report["trigger_provenance"], "autostart");
    assert_eq!(report["config_reload"]["out_of_sync"], false);
    assert_eq!(
        report["config_reload"]["applied"]["semantic_contract_fingerprint"],
        semantic_contract_fingerprint
    );
    assert_eq!(report["lock_identity"]["active"], true);
    assert!(report["lock_identity"]["owner_id"]
        .as_str()
        .is_some_and(|owner| !owner.is_empty()));
    assert_eq!(report["core_refresh_endpoint"]["available"], true);
    assert_eq!(report["core_refresh_endpoint"]["owner_pid"], process::id());
    assert!(!report.to_string().contains("must-not-appear-in-status"));
    assert_eq!(
        report["jobs"]["semantic_index"]["reason"],
        "daemon_mode_source_refresh_only"
    );
    assert_eq!(report["jobs"]["core_refresh"]["certified_source_count"], 4);
    assert_eq!(
        report["jobs"]["core_refresh"]["certified_source_bytes"],
        8192
    );
    for stage in ["discovery", "scan_stage", "commit"] {
        assert!(
            report["jobs"]["core_refresh"]["timings_us"][stage]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "{stage}"
        );
    }
    drop(lock);
    Ok(())
}
