use super::*;

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

#[test]
fn scheduler_state_path_is_installation_scoped() {
    let install = Path::new("/opt/ctx/bin/ctx");
    assert_eq!(
        state_path(install),
        Path::new("/opt/ctx/bin/.ctx.upgrade-state.json")
    );
    assert_eq!(
        installation_daemon_coordination_paths_for(install),
        (
            PathBuf::from("/opt/ctx/bin/.ctx.daemon-quiescence.lock"),
            PathBuf::from("/opt/ctx/bin/.ctx.daemon-quiescence-acks"),
        )
    );
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
fn successful_check_uses_normal_cadence() {
    let mut state = UpgradeState::default();
    let attempt = state.begin("daemon");
    state.terminal(&attempt, "up_to_date", Duration::from_secs(100), 1_000);
    assert!(!auto_check_due(&state, Duration::from_secs(100), 1_099));
    assert!(auto_check_due(&state, Duration::from_secs(100), 1_100));
}

#[test]
fn only_daemon_failures_advance_automatic_backoff() {
    let mut daemon = UpgradeState::default();
    let attempt = daemon.begin("daemon");
    daemon.fail(&attempt, "network", 1_000);
    assert_eq!(daemon.consecutive_failures, 1);
    assert_eq!(daemon.next_retry_unix_s, Some(1_060));

    let mut manual = UpgradeState::default();
    let attempt = manual.begin("manual_apply");
    manual.fail(&attempt, "network", 1_000);
    assert_eq!(manual.consecutive_failures, 0);
    assert_eq!(manual.next_retry_unix_s, None);
}

#[test]
fn manual_success_does_not_change_daemon_cadence_or_backoff() {
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
fn config_editor_keeps_one_canonical_upgrade_control() {
    let input = "[daemon]\nenabled = true\n\n[upgrade]\nauto = \"off\"\n";
    let output = set_toml_section_value(input, "upgrade", "auto", "\"apply\"");
    assert_eq!(output.matches("[upgrade]").count(), 1);
    assert_eq!(output.matches("auto = \"apply\"").count(), 1);
    assert!(!output.contains("auto = \"off\""));
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
    Ok(())
}
