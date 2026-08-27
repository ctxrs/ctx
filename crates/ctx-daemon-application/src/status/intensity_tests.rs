use std::process;

use ctx_daemon_runtime::{daemon_status_path, write_private_json_file, DaemonLock};
use serde_json::json;

use super::*;
use crate::{DaemonApplication, TestHost};

#[test]
fn released_status_without_intensity_is_quiet_and_full_configuration_drifts() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    write_private_json_file(
        &daemon_status_path(temp.path()),
        &json!({
            "status": "running",
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "config_reload": {
                "status": "applied",
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "full",
                    "semantic_enabled": true,
                },
            },
        }),
    )?;
    let host = TestHost;
    let application = DaemonApplication::new(&host);
    let mut config = DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
        semantic_indexing_intensity: SemanticIndexingIntensity::Quiet,
    };

    let preparation = application.prepare_daemon_status(temp.path(), true, Some(&config), true);
    let context = preparation.semantic_context();
    assert!(!context.config_reload.out_of_sync);
    assert_eq!(context.config_reload.status, "applied");
    assert_eq!(
        context.config_reload.applied_semantic_indexing_intensity,
        Some(SemanticIndexingIntensity::Quiet)
    );
    let daemon = preparation.finish().into_json();
    assert_eq!(
        daemon["config_reload"]["applied"]["semantic_indexing_intensity"],
        "quiet"
    );

    config.semantic_indexing_intensity = SemanticIndexingIntensity::Full;
    let preparation = application.prepare_daemon_status(temp.path(), true, Some(&config), true);
    let context = preparation.semantic_context();
    assert!(context.config_reload.out_of_sync);
    assert_eq!(context.config_reload.status, "pending");
    let daemon = preparation.finish().into_json();
    assert_eq!(daemon["config_reload"]["reason"], "config_changed");
    assert_eq!(
        daemon["config_reload"]["requested"]["semantic_indexing_intensity"],
        "full"
    );
    assert_eq!(
        daemon["config_reload"]["applied"]["semantic_indexing_intensity"],
        "quiet"
    );
    drop(lock);
    Ok(())
}
