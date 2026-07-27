use super::*;

#[test]
fn daemon_autostart_records_lifecycle_trigger_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let args = DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: Some(DaemonStartModeArg::Auto),
        trigger_command: Some(DaemonTriggerCommandArg::Setup),
        json: true,
    };

    write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;
    let status = read_daemon_status(temp.path()).expect("daemon status");
    assert_eq!(status["start_mode"], "auto");
    assert_eq!(status["trigger_command"], "setup");
    Ok(())
}

#[test]
fn daemon_report_marks_orphaned_running_status_recoverable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let args = DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: Some(DaemonStartModeArg::Manual),
        trigger_command: None,
        json: true,
    };
    write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;

    let daemon = daemon_report(
        temp.path(),
        &semantic_worker_report_best_effort(temp.path()),
    );

    assert_eq!(daemon["status"], "stale_lock");
    assert_eq!(daemon["running"], false);
    assert_eq!(daemon["recoverable"], true);
    assert_eq!(daemon["reason"], "daemon_status_stale");
    Ok(())
}
