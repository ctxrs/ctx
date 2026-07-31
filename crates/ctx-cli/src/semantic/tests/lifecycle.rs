use super::*;

#[test]
fn daemon_autostart_records_lifecycle_trigger_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let args = DaemonRunArgs {
        foreground: false,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: Some(DaemonStartModeArg::Auto),
        trigger_command: Some(DaemonTriggerCommandArg::Setup),
        format: crate::output::JsonOutputFormat::Json,
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
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: Some(DaemonStartModeArg::Manual),
        trigger_command: None,
        format: crate::output::JsonOutputFormat::Json,
    };
    write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;

    let daemon = paths_status::daemon_report(temp.path());

    assert_eq!(daemon["status"], "stale_lock");
    assert_eq!(daemon["running"], false);
    assert_eq!(daemon["recoverable"], true);
    assert_eq!(daemon["reason"], "daemon_status_stale");
    Ok(())
}

#[test]
fn daemon_report_preserves_terminal_status_when_advisory_metadata_is_unreleased() -> Result<()> {
    for (status, last_error) in [
        ("completed", None),
        (
            "failed",
            Some("history refresh rejected 1 record".to_owned()),
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let args = DaemonRunArgs {
            foreground: false,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Auto),
            trigger_command: Some(DaemonTriggerCommandArg::Setup),
            format: crate::output::JsonOutputFormat::Json,
        };
        write_daemon_lifecycle_status(
            temp.path(),
            &args,
            status,
            123,
            Some(456),
            last_error.clone(),
        )?;
        let lock_path = daemon_lock_path(temp.path());
        create_private_dir_all(lock_path.parent().expect("daemon lock parent"))?;
        fs::write(
            &lock_path,
            serde_json::to_vec(&pid_lock_payload(json!({})))?,
        )?;
        drop(private_create_new_lock_file(&pid_lock_guard_path(
            &lock_path,
        ))?);

        let daemon = paths_status::daemon_report(temp.path());

        assert_eq!(daemon["status"], status);
        assert_eq!(daemon["running"], false);
        assert_eq!(daemon["recoverable"], false);
        assert!(daemon["reason"].is_null(), "{daemon:#}");
        if let Some(last_error) = last_error {
            assert_eq!(daemon["last_error"], last_error);
        }
    }
    Ok(())
}
