use super::*;

fn due_hint_install(temp: &tempfile::TempDir) -> PathBuf {
    let install = temp.path().join("ctx");
    fs::write(&install, b"test executable").unwrap();
    fs::write(
        crate::upgrade::install::install_marker_path(&install),
        b"managed marker candidate",
    )
    .unwrap();
    install
}

#[test]
fn foreground_due_hint_is_inert_without_a_regular_marker() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let install = temp.path().join("ctx");
    fs::write(&install, b"test executable")?;
    assert!(!automatic_upgrade_check_due_for(
        &install,
        Duration::from_secs(60)
    )?);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            temp.path().join("missing"),
            crate::upgrade::install::install_marker_path(&install),
        )?;
        assert!(!automatic_upgrade_check_due_for(
            &install,
            Duration::from_secs(60)
        )?);
    }
    Ok(())
}

#[test]
fn foreground_due_hint_uses_bounded_shared_cadence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let install = due_hint_install(&temp);
    let interval = Duration::from_secs(60);
    assert!(automatic_upgrade_check_due_for(&install, interval)?);

    fs::write(
        state_path(&install),
        serde_json::to_vec(&json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "next_check_unix_s": now_unix_s().saturating_add(60),
        }))?,
    )?;
    assert!(!automatic_upgrade_check_due_for(&install, interval)?);

    fs::write(state_path(&install), b"{malformed")?;
    assert!(automatic_upgrade_check_due_for(&install, interval)?);

    fs::write(
        state_path(&install),
        vec![b'x'; DUE_HINT_STATE_MAX_BYTES as usize + 1],
    )?;
    assert!(automatic_upgrade_check_due_for(&install, interval)?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn foreground_due_hint_does_not_follow_or_block_on_nonregular_state() -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

    let temp = tempfile::tempdir()?;
    let install = due_hint_install(&temp);
    let state = state_path(&install);
    std::os::unix::fs::symlink("/dev/zero", &state)?;
    assert!(automatic_upgrade_check_due_for(
        &install,
        Duration::from_secs(60)
    )?);

    fs::remove_file(&state)?;
    let state_c = CString::new(state.as_os_str().as_bytes())?;
    if unsafe { libc::mkfifo(state_c.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    assert!(automatic_upgrade_check_due_for(
        &install,
        Duration::from_secs(60)
    )?);
    Ok(())
}

#[test]
fn foreground_due_hint_suppresses_only_recent_in_progress_attempts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let install = due_hint_install(&temp);
    let interval = Duration::from_secs(60);

    fs::write(
        state_path(&install),
        serde_json::to_vec(&json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "checking",
            "last_attempt_at": utc_now(),
        }))?,
    )?;
    assert!(!automatic_upgrade_check_due_for(&install, interval)?);

    let stale = utc_now()
        - chrono::Duration::from_std(DUE_HINT_RECENT_ATTEMPT_GRACE)?
        - chrono::Duration::seconds(1);
    fs::write(
        state_path(&install),
        serde_json::to_vec(&json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "staged",
            "last_attempt_at": stale,
        }))?,
    )?;
    assert!(automatic_upgrade_check_due_for(&install, interval)?);

    let future = utc_now() + chrono::Duration::hours(24);
    fs::write(
        state_path(&install),
        serde_json::to_vec(&json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "checking",
            "last_attempt_at": future,
        }))?,
    )?;
    assert!(automatic_upgrade_check_due_for(&install, interval)?);
    Ok(())
}

#[test]
fn current_and_legacy_automatic_sources_share_cadence_and_backoff() {
    for source in ["automatic", "daemon"] {
        let mut success = UpgradeState::default();
        let attempt = success.begin(source);
        success.terminal(&attempt, "up_to_date", Duration::from_secs(60), 100);
        assert_eq!(success.next_check_unix_s, Some(160), "{source}");
        assert_eq!(success.consecutive_failures, 0, "{source}");

        let mut failure = UpgradeState::default();
        let attempt = failure.begin(source);
        failure.fail(&attempt, "network", 100);
        assert_eq!(failure.next_retry_unix_s, Some(160), "{source}");
        assert_eq!(failure.consecutive_failures, 1, "{source}");
    }
}

fn test_installation() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin)?;
    let install_path = bin.join("ctx");
    fs::write(&install_path, b"test ctx executable")?;
    Ok((temp, install_path))
}

#[test]
fn recovering_is_an_active_installation_phase() {
    assert!(is_active_upgrade_status("recovering"));
    assert!(!is_active_upgrade_status("error"));
}

#[test]
fn missing_state_contended_by_non_upgrade_checker_is_not_active() -> Result<()> {
    let (_temp, install_path) = test_installation()?;
    let _checker =
        InstallationLock::try_acquire(&install_path)?.ok_or_else(|| anyhow!("test lock held"))?;

    assert_eq!(
        observe_installation_upgrade(&install_path),
        InstallationUpgradeObservation::Missing
    );
    assert!(!installation_upgrade_is_active_for(&install_path)?);
    Ok(())
}

#[test]
fn active_current_state_remains_fenced_while_installation_is_locked() -> Result<()> {
    let (_temp, install_path) = test_installation()?;
    let _upgrade =
        InstallationLock::try_acquire(&install_path)?.ok_or_else(|| anyhow!("test lock held"))?;
    atomic_write_json(
        &state_path(&install_path),
        &json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "applying",
            "attempt_id": "ua_active_test",
        }),
    )?;

    assert!(installation_upgrade_is_active_for(&install_path)?);
    Ok(())
}

#[test]
fn untrusted_state_remains_fail_closed_while_installation_is_locked() -> Result<()> {
    let (_temp, install_path) = test_installation()?;
    let _upgrade =
        InstallationLock::try_acquire(&install_path)?.ok_or_else(|| anyhow!("test lock held"))?;
    fs::write(state_path(&install_path), b"{not valid upgrade state")?;

    assert_eq!(
        observe_installation_upgrade(&install_path),
        InstallationUpgradeObservation::Untrusted
    );
    assert!(installation_upgrade_is_active_for(&install_path)?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unmanaged_read_only_installation_is_never_active_without_a_lock() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let (temp, install_path) = test_installation()?;
    // No install marker: an unmanaged, third-party packaged installation.
    // Make the executable directory read-only to prove no installation lock
    // or coordination file is required to observe it.
    let bin = install_path.parent().unwrap();
    fs::set_permissions(bin, fs::Permissions::from_mode(0o555))?;

    assert!(!installation_upgrade_is_active_for(&install_path)?);

    fs::set_permissions(bin, fs::Permissions::from_mode(0o755))?;
    drop(temp);
    Ok(())
}

#[test]
fn unmanaged_installation_with_active_state_remains_fenced() -> Result<()> {
    let (_temp, install_path) = test_installation()?;
    // No install marker, but a leftover active scheduler record must still
    // fence: the unmanaged shortcut never bypasses active-state observation.
    atomic_write_json(
        &state_path(&install_path),
        &json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "applying",
            "attempt_id": "ua_unmanaged_active_test",
        }),
    )?;

    assert!(installation_upgrade_is_active_for(&install_path)?);
    Ok(())
}

#[test]
fn scheduler_state_path_remains_installation_scoped() {
    let install = Path::new("/opt/ctx/bin/ctx");
    assert_eq!(
        state_path(install),
        Path::new("/opt/ctx/bin/.ctx.upgrade-state.json")
    );
}

#[test]
fn daemon_coordination_is_user_scoped_by_canonical_executable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let user_state = temp.path().join("user-state");
    let first_bin = temp.path().join("first").join("bin");
    let second_bin = temp.path().join("second").join("bin");
    fs::create_dir_all(&first_bin)?;
    fs::create_dir_all(&second_bin)?;
    let first = first_bin.join("ctx");
    let second = second_bin.join("ctx");
    fs::write(&first, b"first ctx executable")?;
    fs::write(&second, b"second ctx executable")?;

    let first_paths = installation_daemon_coordination_paths_in(&user_state, &first)?;
    let aliased_first = first_bin.join("..").join("bin").join("ctx");
    assert_eq!(
        installation_daemon_coordination_paths_in(&user_state, &aliased_first)?,
        first_paths,
        "path aliases for one executable must share coordination"
    );
    assert_eq!(
        first_paths.0.file_name().and_then(|name| name.to_str()),
        Some(DAEMON_QUIESCENCE_LOCK_FILE)
    );
    assert_eq!(
        first_paths.1.file_name().and_then(|name| name.to_str()),
        Some(DAEMON_QUIESCENCE_ACK_DIR)
    );
    let expected_coordination_root = user_state.join(DAEMON_INSTALLATION_STATE_DIR);
    assert_eq!(
        first_paths.0.parent().and_then(Path::parent),
        Some(expected_coordination_root.as_path())
    );

    let second_paths = installation_daemon_coordination_paths_in(&user_state, &second)?;
    assert_ne!(
        first_paths.0.parent(),
        second_paths.0.parent(),
        "distinct executable paths must not share coordination"
    );
    assert_eq!(
        state_path(&first),
        first.with_file_name(".ctx.upgrade-state.json"),
        "daemon coordination must not move scheduler state"
    );
    Ok(())
}

#[test]
fn status_reads_current_installation_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let install_path = temp.path().join("bin").join("ctx");
    atomic_write_json(
        &state_path(&install_path),
        &json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "up_to_date",
            "attempt_source": "daemon",
            "checked_at": "2026-07-10T12:00:00Z",
            "last_checked_unix_s": 1778499900_u64,
            "next_check_unix_s": 1778500000_u64,
        }),
    )?;

    let current = read_state_json_for_path(&install_path)
        .ok_or_else(|| anyhow!("current installation state was not read"))?;

    assert_eq!(current["schema_version"], STATE_SCHEMA_VERSION);
    assert_eq!(current["status"], "up_to_date");
    assert_eq!(current["attempt_source"], "daemon");
    assert_eq!(current["checked_at"], "2026-07-10T12:00:00Z");
    assert_eq!(current["last_checked_unix_s"], 1778499900_u64);
    assert_eq!(current["next_check_unix_s"], 1778500000_u64);
    Ok(())
}

#[test]
fn fresh_and_interrupted_state_are_due_without_a_persisted_lease() {
    let fresh = UpgradeState::default();
    assert!(auto_check_due(&fresh, Duration::from_secs(86_400), 10));

    let mut interrupted = UpgradeState {
        schema_version: STATE_SCHEMA_VERSION,
        status: "applying".to_owned(),
        attempt_id: Some("ua_interrupted".to_owned()),
        next_check_unix_s: Some(9),
        ..UpgradeState::default()
    };
    assert!(auto_check_due(
        &interrupted,
        Duration::from_secs(86_400),
        10
    ));

    interrupted.next_retry_unix_s = Some(11);
    assert!(!auto_check_due(
        &interrupted,
        Duration::from_secs(86_400),
        10
    ));
}

#[test]
fn successful_automatic_check_uses_normal_cadence() {
    let mut state = UpgradeState::default();
    let attempt = state.begin("automatic");
    state.terminal(&attempt, "up_to_date", Duration::from_secs(100), 1_000);
    assert!(!auto_check_due(&state, Duration::from_secs(100), 1_099));
    assert!(auto_check_due(&state, Duration::from_secs(100), 1_100));
}

#[test]
fn only_automatic_failures_advance_automatic_backoff() {
    let mut automatic = UpgradeState::default();
    let attempt = automatic.begin("automatic");
    automatic.fail(&attempt, "network", 1_000);
    assert_eq!(automatic.consecutive_failures, 1);
    assert_eq!(automatic.next_retry_unix_s, Some(1_060));

    let mut manual = UpgradeState::default();
    let attempt = manual.begin("manual_apply");
    manual.fail(&attempt, "network", 1_000);
    assert_eq!(manual.consecutive_failures, 0);
    assert_eq!(manual.next_retry_unix_s, None);
}

#[test]
fn manual_success_does_not_change_automatic_cadence_or_backoff() {
    let mut state = UpgradeState {
        schema_version: STATE_SCHEMA_VERSION,
        next_check_unix_s: Some(2_000),
        next_retry_unix_s: Some(1_500),
        consecutive_failures: 3,
        ..UpgradeState::default()
    };
    let attempt = state.begin("manual_apply");
    state.terminal(&attempt, "applied", Duration::from_secs(100), 1_000);

    assert_eq!(state.next_check_unix_s, Some(2_000));
    assert_eq!(state.next_retry_unix_s, Some(1_500));
    assert_eq!(state.consecutive_failures, 3);
}

#[test]
fn failure_backoff_is_exponential_and_bounded() {
    assert_eq!(failure_backoff(1), Duration::from_secs(60));
    assert_eq!(failure_backoff(2), Duration::from_secs(120));
    assert_eq!(failure_backoff(3), Duration::from_secs(240));
    assert_eq!(failure_backoff(100), MAX_FAILURE_BACKOFF);
}

#[test]
fn invalid_or_legacy_scheduler_state_starts_fresh() {
    let legacy = UpgradeState {
        schema_version: 2,
        status: "claimed".to_owned(),
        attempt_id: Some("old-root-authority".to_owned()),
        ..UpgradeState::default()
    };
    let normalized = legacy.valid_or_default();
    assert_eq!(normalized.schema_version, 0);
    assert!(normalized.attempt_id.is_none());
}

#[test]
fn attempt_ids_are_bounded_and_path_safe() {
    assert!(is_valid_upgrade_attempt_id(
        "ua_01890f3e-2c80-7000-8000-000000000001"
    ));
    assert!(!is_valid_upgrade_attempt_id(""));
    assert!(!is_valid_upgrade_attempt_id("../escape"));
    assert!(!is_valid_upgrade_attempt_id(&"a".repeat(129)));
}

#[test]
fn atomic_json_write_replaces_without_partial_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("state.json");
    atomic_write_json(&path, &json!({"status": "checking"}))?;
    atomic_write_json(&path, &json!({"status": "applied"}))?;
    assert_eq!(
        read_json_file(&path)
            .and_then(|value| value["status"].as_str().map(str::to_owned))
            .as_deref(),
        Some("applied")
    );
    assert_eq!(fs::read_dir(temp.path())?.count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}
