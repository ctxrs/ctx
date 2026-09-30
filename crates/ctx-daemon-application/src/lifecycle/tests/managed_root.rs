use super::*;

#[test]
fn daemon_handoff_stall_without_authenticated_progress_is_bounded_to_five_seconds() {
    let pauses = DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS.saturating_sub(1);
    let maximum_wait = DAEMON_UPGRADE_POLL_INTERVAL
        .checked_mul(u32::try_from(pauses).expect("bounded test attempt count"))
        .expect("bounded handoff duration");
    assert_eq!(maximum_wait, Duration::from_secs(5));
    assert_eq!(DAEMON_SETUP_HANDOFF_STALL_TIMEOUT, maximum_wait);
    assert!(DAEMON_HEALTH_TIMEOUT < DAEMON_SETUP_HANDOFF_STALL_TIMEOUT);
}

#[test]
fn command_override_keeps_managed_authority_across_sanitized_launches() -> Result<()> {
    const STAGE: &str = "CTX_TEST_MANAGED_LAUNCH_STAGE";
    const TEST: &str =
        "lifecycle::tests::managed_root::command_override_keeps_managed_authority_across_sanitized_launches";
    if env::var(STAGE).as_deref() == Ok("child") {
        let expected = PathBuf::from(env::var_os("HOME").unwrap()).join("managed");
        assert_eq!(ctx_history_platform::managed_data_root()?, expected);
        assert_eq!(ctx_history_platform::default_data_root()?, expected);
        assert!(env::var_os(DAEMON_ENV_HOSTILE).is_none());
        return Ok(());
    }
    if env::var(STAGE).as_deref() == Ok("parent") {
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        let selected = home.join("managed");
        let storage = home.join("override");
        for finite in [false, true] {
            let launch = if finite {
                configured_finite_core_worker_command(
                    &env::current_exe()?,
                    &storage,
                    DaemonTrigger::Setup,
                )?
            } else {
                configured_daemon_autostart_command(
                    &env::current_exe()?,
                    &storage,
                    DaemonTrigger::Setup,
                    None,
                )?
            };
            let args = launch.args().collect::<Vec<_>>();
            assert_eq!(
                &args[..2],
                &[OsStr::new("--data-root"), storage.as_os_str()]
            );
            let mut environment = launch
                .environment()
                .map(|(name, value)| (name.to_os_string(), value.to_os_string()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                environment.get(OsStr::new("CTX_DATA_ROOT")),
                Some(&selected.as_os_str().to_os_string())
            );
            environment.insert(STAGE.into(), "child".into());
            // Exercise the real env_clear spawn boundary with a test process
            // in place of the daemon, retaining its normalized environment.
            let probe = NormalizedLaunch::new(
                env::current_exe()?,
                vec!["--exact".into(), TEST.into()],
                environment,
            );
            let mut child = if finite {
                ctx_daemon_runtime::spawn_attached(probe)?
            } else {
                spawn_detached_daemon_child(probe)?
            };
            assert!(child.wait()?.success());
        }
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    assert!(std::process::Command::new(env::current_exe()?)
        .args(["--exact", TEST])
        .env(STAGE, "parent")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("CTX_DATA_ROOT", temp.path().join("managed"))
        .env(DAEMON_ENV_HOSTILE, "must-be-cleared")
        .status()?
        .success());
    Ok(())
}
