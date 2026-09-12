use std::{fs, process};

use ctx_daemon_runtime::{
    create_private_dir_all, daemon_lock_path, daemon_status_path, pid_lock_guard_path,
    pid_lock_payload, private_create_new_lock_file, write_private_json_file, DaemonLock,
};
use ctx_daemon_service::daemon_core_refresh_job_path;

use super::*;
use crate::{DaemonApplication, TestHost};

#[test]
fn stale_heartbeat_is_reported_without_revoking_live_ownership() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let _lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    for (heartbeat, pid, expected) in [
        (now - 62 * 60 * 60 * 1000, process::id(), Some(true)),
        (now, process::id(), Some(false)),
        (0, process::id(), None),
        (now + 60 * 60 * 1000, process::id(), None),
        (now - 62 * 60 * 60 * 1000, process::id() + 1, None),
    ] {
        write_private_json_file(
            &daemon_status_path(temp.path()),
            &json!({
                "status": "running", "pid": pid, "heartbeat_at_ms": heartbeat,
            }),
        )?;
        let daemon = report(temp.path(), true, None);
        assert_eq!(daemon["heartbeat_stale"].as_bool(), expected, "{daemon}");
        assert_eq!(daemon["status"], "running");
        assert_eq!(daemon["running"], true);
        assert_eq!(daemon["recoverable"], false);
    }
    Ok(())
}

fn write_lifecycle_status(
    data_root: &Path,
    status: &str,
    last_error: Option<&str>,
) -> anyhow::Result<()> {
    write_private_json_file(
        &daemon_status_path(data_root),
        &json!({
            "schema_version": 1,
            "status": status,
            "pid": 123,
            "started_at_ms": 123,
            "heartbeat_at_ms": 456,
            "finished_at_ms": 789,
            "start_mode": "auto",
            "trigger_command": "setup",
            "last_error": last_error,
        }),
    )
}

fn report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    config: Option<&DaemonConfigSnapshot>,
) -> Value {
    let host = TestHost;
    DaemonApplication::new(&host)
        .prepare_daemon_status(data_root, disabled_overrides_lifecycle, config, true)
        .finish()
        .into_json()
}

#[test]
fn orphaned_running_status_is_recoverable_without_claiming_a_live_pid() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    write_lifecycle_status(temp.path(), "running", None)?;

    let daemon = report(temp.path(), true, None);

    assert_eq!(daemon["status"], "stale_lock");
    assert_eq!(daemon["running"], false);
    assert_eq!(daemon["recoverable"], true);
    assert_eq!(daemon["reason"], "daemon_status_stale");
    assert!(daemon.get("live_pid").is_none());
    assert_eq!(daemon["pid"], 123);
    assert_eq!(daemon["started_at_ms"], 123);
    assert_eq!(daemon["heartbeat_at_ms"], 456);
    assert_eq!(daemon["finished_at_ms"], 789);
    assert_eq!(daemon["trigger_provenance"], "autostart");
    Ok(())
}

#[test]
fn terminal_status_survives_unreleased_advisory_metadata() -> anyhow::Result<()> {
    for (status, last_error) in [
        ("completed", None),
        ("failed", Some("history refresh rejected 1 record")),
    ] {
        let temp = tempfile::tempdir()?;
        write_lifecycle_status(temp.path(), status, last_error)?;
        let lock_path = daemon_lock_path(temp.path());
        create_private_dir_all(lock_path.parent().expect("daemon lock parent"))?;
        fs::write(
            &lock_path,
            serde_json::to_vec(&pid_lock_payload(json!({})))?,
        )?;
        drop(private_create_new_lock_file(&pid_lock_guard_path(
            &lock_path,
        ))?);

        let daemon = report(temp.path(), true, None);

        assert_eq!(daemon["status"], status);
        assert_eq!(daemon["running"], false);
        assert_eq!(daemon["recoverable"], false);
        assert!(daemon.get("reason").is_none(), "{daemon:#}");
        if let Some(last_error) = last_error {
            assert_eq!(daemon["last_error"], last_error);
        }
    }
    Ok(())
}

#[test]
fn unknown_disabled_lifecycle_is_disabled_while_job_override_remains_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let config = DaemonConfigSnapshot {
        enabled: false,
        mode: DaemonMode::Full,
        semantic_enabled: false,
        semantic_executor: "builtin".to_owned(),
        semantic_contract_fingerprint: "sha256:builtin-space".to_owned(),
        semantic_builtin_throttling_configured: true,
        semantic_builtin_throttling_effective: Some(true),
    };

    let disabled = report(temp.path(), true, Some(&config));
    let lifecycle_first = report(temp.path(), false, Some(&config));

    assert_eq!(disabled["status"], "disabled");
    assert_eq!(lifecycle_first["status"], "disabled");
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["jobs"]["core_refresh"]["status"], "disabled");
    assert_eq!(
        disabled["jobs"]["core_refresh"]["reason"],
        "daemon_disabled"
    );
    assert_eq!(lifecycle_first["jobs"]["core_refresh"]["status"], "unknown");
}

#[test]
fn current_config_reconciles_applied_snapshot_without_a_second_read() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "completed",
            "config_reload": {
                "status": "applied",
                "last_attempt_at_ms": 101,
                "last_applied_at_ms": 102,
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "full",
                    "semantic_enabled": false,
                    "semantic_executor": "builtin",
                    "semantic_contract_fingerprint": "sha256:builtin-space",
                    "semantic_builtin_throttling_configured": true,
                    "semantic_builtin_throttling_effective": true,
                },
            },
        }),
    )?;
    let config = DaemonConfigSnapshot {
        enabled: false,
        mode: DaemonMode::SourceRefreshOnly,
        semantic_enabled: true,
        semantic_executor: "https://embeddings.example.test/v1/".to_owned(),
        semantic_contract_fingerprint: "sha256:external-space".to_owned(),
        semantic_builtin_throttling_configured: true,
        semantic_builtin_throttling_effective: None,
    };
    let host = TestHost;
    let application = DaemonApplication::new(&host);

    let preparation = application.prepare_daemon_status(temp.path(), true, Some(&config), true);
    let context = preparation.semantic_context();

    assert_eq!(context.daemon_mode, DaemonMode::SourceRefreshOnly);
    assert!(!context.daemon_running);
    assert!(!context.config_reload.out_of_sync);
    assert_eq!(context.config_reload.status, "applied");
    assert_eq!(context.config_reload.requested_daemon_enabled, Some(false));
    assert_eq!(context.config_reload.requested_semantic_enabled, Some(true));
    assert_eq!(
        context.config_reload.requested_semantic_executor,
        Some("https://embeddings.example.test/v1/")
    );
    assert_eq!(
        context
            .config_reload
            .requested_semantic_contract_fingerprint,
        Some("sha256:external-space")
    );
    assert_eq!(context.config_reload.applied_daemon_enabled, Some(true));
    assert_eq!(context.config_reload.applied_semantic_enabled, Some(false));
    assert_eq!(
        context.config_reload.applied_semantic_executor,
        Some("builtin")
    );
    assert_eq!(
        context.config_reload.applied_semantic_contract_fingerprint,
        Some("sha256:builtin-space")
    );

    let daemon = preparation.finish().into_json();
    assert_eq!(daemon["enabled"], false);
    assert_eq!(daemon["mode"], "source-refresh-only");
    assert_eq!(daemon["config_reload"]["status"], "applied");
    assert_eq!(daemon["config_reload"]["out_of_sync"], false);
    assert_eq!(daemon["config_reload"]["last_attempt_at_ms"], 101);
    assert_eq!(daemon["config_reload"]["last_applied_at_ms"], 102);
    Ok(())
}

#[test]
fn running_owner_marks_same_endpoint_changed_semantic_contract_pending() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "running",
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "semantic_runtime_active": true,
            "config_reload": {
                "status": "applied",
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "full",
                    "semantic_enabled": true,
                    "semantic_executor": "https://embeddings.example.test/v1/",
                    "semantic_contract_fingerprint": "sha256:external-space-a",
                    "semantic_builtin_throttling_configured": true,
                    "semantic_builtin_throttling_effective": null,
                },
            },
        }),
    )?;
    let config = DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
        semantic_executor: "https://embeddings.example.test/v1/".to_owned(),
        semantic_contract_fingerprint: "sha256:external-space-b".to_owned(),
        semantic_builtin_throttling_configured: true,
        semantic_builtin_throttling_effective: None,
    };
    let host = TestHost;
    let application = DaemonApplication::new(&host);

    let preparation = application.prepare_daemon_status(temp.path(), true, Some(&config), true);
    let context = preparation.semantic_context();
    assert!(context.daemon_running);
    assert!(context.semantic_runtime_active);
    assert!(context.config_reload.out_of_sync);
    assert_eq!(context.config_reload.status, "pending");
    assert_eq!(
        context.config_reload.requested_semantic_executor,
        Some("https://embeddings.example.test/v1/")
    );
    assert_eq!(
        context.config_reload.applied_semantic_executor,
        Some("https://embeddings.example.test/v1/")
    );
    assert_eq!(
        context
            .config_reload
            .requested_semantic_contract_fingerprint,
        Some("sha256:external-space-b")
    );
    assert_eq!(
        context.config_reload.applied_semantic_contract_fingerprint,
        Some("sha256:external-space-a")
    );

    let daemon = preparation.finish().into_json();
    assert_eq!(daemon["config_reload"]["status"], "pending");
    assert_eq!(daemon["config_reload"]["reason"], "config_changed");
    assert_eq!(daemon["config_reload"]["out_of_sync"], true);
    assert_eq!(
        daemon["config_reload"]["requested"]["semantic_executor"],
        "https://embeddings.example.test/v1/"
    );
    assert_eq!(
        daemon["config_reload"]["applied"]["semantic_executor"],
        "https://embeddings.example.test/v1/"
    );
    assert_eq!(
        daemon["config_reload"]["requested"]["semantic_contract_fingerprint"],
        "sha256:external-space-b"
    );
    assert_eq!(
        daemon["config_reload"]["applied"]["semantic_contract_fingerprint"],
        "sha256:external-space-a"
    );
    drop(lock);
    Ok(())
}

#[test]
fn running_owner_marks_changed_builtin_throttling_identity_pending() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "running",
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "semantic_runtime_active": true,
            "config_reload": {
                "status": "applied",
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "full",
                    "semantic_enabled": true,
                    "semantic_executor": "builtin",
                    "semantic_contract_fingerprint": "sha256:builtin-space",
                    "semantic_builtin_throttling_configured": true,
                    "semantic_builtin_throttling_effective": true,
                },
            },
        }),
    )?;
    let config = DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
        semantic_executor: "builtin".to_owned(),
        semantic_contract_fingerprint: "sha256:builtin-space".to_owned(),
        semantic_builtin_throttling_configured: false,
        semantic_builtin_throttling_effective: Some(false),
    };

    let daemon = report(temp.path(), true, Some(&config));

    assert_eq!(daemon["config_reload"]["status"], "pending");
    assert_eq!(daemon["config_reload"]["reason"], "config_changed");
    assert_eq!(
        daemon["config_reload"]["requested"]["semantic_builtin_throttling_configured"],
        false
    );
    assert_eq!(
        daemon["config_reload"]["requested"]["semantic_builtin_throttling_effective"],
        false
    );
    assert_eq!(
        daemon["config_reload"]["applied"]["semantic_builtin_throttling_configured"],
        true
    );
    assert_eq!(
        daemon["config_reload"]["applied"]["semantic_builtin_throttling_effective"],
        true
    );
    drop(lock);
    Ok(())
}

#[test]
fn persisted_applied_mode_is_used_when_current_config_is_unavailable() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "completed",
            "config_reload": {
                "status": "applied",
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
            },
        }),
    )?;

    let daemon = report(temp.path(), true, None);

    assert_eq!(daemon["mode"], "source-refresh-only");
    assert_eq!(daemon["config_reload"]["requested"], json!({}));
    Ok(())
}

#[test]
fn activation_failure_context_is_typed_without_exposing_persisted_json() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "failed",
            "last_error": "outer lifecycle failure",
            "config_reload": {
                "status": "activation_failed",
                "last_error": "semantic runtime unavailable",
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "full",
                    "semantic_enabled": false,
                    "semantic_executor": "builtin",
                    "semantic_contract_fingerprint": "sha256:builtin-space",
                    "semantic_builtin_throttling_configured": true,
                    "semantic_builtin_throttling_effective": true,
                },
            },
        }),
    )?;
    let config = DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
        semantic_executor: "https://embeddings.example.test/v1/".to_owned(),
        semantic_contract_fingerprint: "sha256:external-space".to_owned(),
        semantic_builtin_throttling_configured: true,
        semantic_builtin_throttling_effective: None,
    };
    let host = TestHost;
    let application = DaemonApplication::new(&host);

    let preparation = application.prepare_daemon_status(temp.path(), true, Some(&config), true);
    let context = preparation.semantic_context();

    assert_eq!(context.config_reload.status, "activation_failed");
    assert_eq!(
        context.config_reload.last_error,
        Some("semantic runtime unavailable")
    );
    assert_eq!(context.config_reload.requested_semantic_enabled, Some(true));
    assert_eq!(context.config_reload.applied_semantic_enabled, Some(false));
    assert_eq!(
        context.config_reload.requested_semantic_executor,
        Some("https://embeddings.example.test/v1/")
    );
    assert_eq!(
        context.config_reload.applied_semantic_executor,
        Some("builtin")
    );
    assert_eq!(
        context
            .config_reload
            .requested_semantic_contract_fingerprint,
        Some("sha256:external-space")
    );
    assert_eq!(
        context.config_reload.applied_semantic_contract_fingerprint,
        Some("sha256:builtin-space")
    );
    let daemon = preparation.finish().into_json();
    assert_eq!(daemon["status"], "failed");
    assert_eq!(daemon["last_error"], "outer lifecycle failure");
    assert_eq!(
        daemon["config_reload"]["last_error"],
        "semantic runtime unavailable"
    );
    Ok(())
}

#[test]
fn manual_trigger_provenance_prefers_the_explicit_trigger_then_manual_fallback(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "completed",
            "start_mode": "manual",
            "trigger_command": "search",
        }),
    )?;

    let explicit = report(temp.path(), true, None);
    assert_eq!(explicit["start_mode"], "manual");
    assert_eq!(explicit["trigger_command"], "search");
    assert_eq!(explicit["trigger_provenance"], "search");

    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "completed",
            "start_mode": "manual",
        }),
    )?;
    let fallback = report(temp.path(), true, None);
    assert_eq!(fallback["trigger_provenance"], "manual");
    assert!(fallback.get("trigger_command").is_none());
    Ok(())
}

#[test]
fn core_job_snapshot_keeps_generic_progress_and_retry_fields() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    write_private_json_file(
        &daemon_core_refresh_job_path(temp.path()),
        &json!({
            "status": "running",
            "reason": "refreshing",
            "error_code": "retryable_source",
            "mode": "incremental",
            "owner": "daemon",
            "kind": "core_refresh",
            "request_id": "request-1",
            "request_state": "running",
            "last_run_at_ms": 99,
            "source_count": 7,
            "previous_generation": "g1",
            "published_generation": "g2",
            "generation_changed": true,
            "receipt": {"outcome": "completed"},
            "coalesced_requests": 3,
            "progress": {"records": 44},
            "daemon_mode": "full",
            "trigger": "search",
            "trigger_provenance": "autostart",
            "scanned_routes": ["codex"],
            "unsupported_routes": ["future"],
            "certified_source_count": 6,
            "certified_source_bytes": 8192,
            "timings_us": {"scan": 12},
            "structured_outcome": {
                "code": "source_refresh_failed",
                "class": "internal",
                "retryable": false,
            },
            "automatic_retry": {
                "state": "paused",
                "reason": "repeated_internal_failure",
                "confirmation_limit": 2,
                "routes": {
                    ("aa".repeat(32)): {
                        "state": "paused",
                        "matching_failures": 2,
                        "source_observation": "bb".repeat(32),
                        "failure_fingerprint": "cc".repeat(32),
                        "build_version": "0.0.0-test",
                    }
                },
                "resume_on": ["source_change", "ctx_upgrade", "manual_import"],
            },
            "retryable": true,
            "retry_after_ms": 500,
            "consecutive_failures": 2,
            "retry_not_before_at_ms": 1000,
            "last_error": "temporary",
        }),
    )?;

    let daemon = report(temp.path(), true, None);
    let job = &daemon["jobs"]["core_refresh"];

    assert_eq!(job["status"], "running");
    assert_eq!(job["owner"], "daemon");
    assert_eq!(job["request_id"], "request-1");
    assert_eq!(job["source_count"], 7);
    assert_eq!(job["receipt"]["outcome"], "completed");
    assert_eq!(job["progress"]["records"], 44);
    assert_eq!(job["scanned_routes"], json!(["codex"]));
    assert_eq!(job["certified_source_bytes"], 8192);
    assert_eq!(job["timings_us"]["scan"], 12);
    assert_eq!(job["structured_outcome"]["code"], "source_refresh_failed");
    assert_eq!(job["structured_outcome"]["class"], "internal");
    assert_eq!(job["structured_outcome"]["retryable"], false);
    assert_eq!(job["automatic_retry"]["state"], "paused");
    assert_eq!(job["automatic_retry"]["confirmation_limit"], 2);
    assert_eq!(
        job["automatic_retry"]["resume_on"],
        json!(["source_change", "ctx_upgrade", "manual_import"])
    );
    assert_eq!(job["retryable"], true);
    assert_eq!(job["retry_after_ms"], 500);
    assert_eq!(job["consecutive_failures"], 2);
    assert_eq!(job["retry_not_before_at_ms"], 1000);
    assert_eq!(job["last_error"], "temporary");
    Ok(())
}

#[test]
fn absent_endpoint_and_wakeup_state_remain_generic_and_nonsecret() {
    let temp = tempfile::tempdir().unwrap();

    let daemon = report(temp.path(), true, None);

    assert_eq!(daemon["core_refresh_endpoint"]["available"], false);
    assert!(daemon["core_refresh_endpoint"].get("transport").is_none());
    assert!(daemon["core_refresh_endpoint"].get("owner_pid").is_none());
    assert!(daemon["core_refresh_endpoint"].get("address").is_none());
    assert_eq!(
        daemon["core_refresh_endpoint"]["identity_path"],
        json!(daemon_root_path(temp.path()).join("source-refresh-endpoint.json"))
    );
    assert!(daemon.get("wakeup").is_some());
    assert!(daemon.get("supervisor").is_some());
}

#[test]
fn source_refresh_only_snapshot_preserves_generic_lifecycle_identity() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    ctx_daemon_runtime::write_daemon_status(
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
                    "semantic_contract_fingerprint": "sha256:builtin-space",
                    "semantic_builtin_throttling_configured": true,
                    "semantic_builtin_throttling_effective": true,
                },
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                    "semantic_executor": "builtin",
                    "semantic_contract_fingerprint": "sha256:builtin-space",
                    "semantic_builtin_throttling_configured": true,
                    "semantic_builtin_throttling_effective": true,
                },
            },
        }),
    )?;
    ctx_daemon_runtime::write_daemon_job_status(
        &daemon_core_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "daemon_mode": "source-refresh-only",
            "trigger": "search",
            "trigger_provenance": "autostart",
            "certified_source_count": 4,
            "certified_source_bytes": 8192,
        }),
    )?;
    let host = TestHost;
    let application = DaemonApplication::new(&host);
    let config = DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::SourceRefreshOnly,
        semantic_enabled: false,
        semantic_executor: "builtin".to_owned(),
        semantic_contract_fingerprint: "sha256:builtin-space".to_owned(),
        semantic_builtin_throttling_configured: true,
        semantic_builtin_throttling_effective: Some(true),
    };

    let report = application
        .prepare_daemon_status(temp.path(), true, Some(&config), true)
        .finish()
        .into_json();

    assert_eq!(report["mode"], "source-refresh-only");
    assert_eq!(report["live_pid"], process::id());
    assert_eq!(report["trigger_command"], "search");
    assert_eq!(report["trigger_provenance"], "autostart");
    assert_eq!(report["lock_identity"]["active"], true);
    assert_eq!(report["jobs"]["core_refresh"]["certified_source_count"], 4);
    assert_eq!(report["config_reload"]["status"], "applied");
    assert_eq!(
        report["config_reload"]["requested"]["semantic_contract_fingerprint"],
        "sha256:builtin-space"
    );
    drop(lock);
    Ok(())
}
