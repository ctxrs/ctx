#[test]
fn corrupt_marker_keeps_persistent_daemon_automatic_scheduler_dormant() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_candidate(&temp, "ia_corrupt_daemon_marker");
    fs::write(install_marker_path(&binary), b"{not-json").unwrap();
    let daemon_root = data_root(&temp);
    let mut ready_at = None;

    let output = run_daemon_until(
        "corrupt-marker daemon observation",
        &managed_daemon(&temp, &release, &binary),
        || {
            if running_daemon_pid(&daemon_root, None).is_some() {
                let observed = ready_at.get_or_insert_with(Instant::now);
                return observed.elapsed() >= Duration::from_secs(2);
            }
            false
        },
    );

    assert!(output.status.success(), "{output:?}");
    assert!(
        !scheduler_state_path(&binary).exists(),
        "an invalid marker must not create an automatic scheduler attempt"
    );
}

#[test]
fn hash_mismatched_marker_keeps_persistent_daemon_automatic_scheduler_dormant() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_candidate(&temp, "ia_hash_mismatch_daemon_marker");
    let marker_path = install_marker_path(&binary);
    let mut marker: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    marker["sha256"] = Value::String("0".repeat(64));
    fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();
    let daemon_root = data_root(&temp);
    let mut ready_at = None;

    let output = run_daemon_until(
        "hash-mismatched-marker daemon observation",
        &managed_daemon(&temp, &release, &binary),
        || {
            if running_daemon_pid(&daemon_root, None).is_some() {
                let observed = ready_at.get_or_insert_with(Instant::now);
                return observed.elapsed() >= Duration::from_secs(2);
            }
            false
        },
    );

    assert!(output.status.success(), "{output:?}");
    assert!(
        !scheduler_state_path(&binary).exists(),
        "a hash-mismatched marker must not create an automatic scheduler attempt"
    );
}

#[test]
fn corrupt_marker_does_not_spawn_a_detached_automatic_worker() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_hook_candidate(&temp, "ia_corrupt_invocation_marker");
    fs::write(install_marker_path(&binary), b"{not-json").unwrap();
    configure_automatic_upgrades(&temp, "manual");
    let receipt = temp.path().join("automatic-worker.receipt");

    let output = managed_release_env(
        ctx_from_binary(&temp, &binary).args(["doctor", "--format=json"]),
        &release,
        &binary,
    )
    .env("CTX_AUTOMATIC_UPGRADE_WORKER_RECEIPT_FOR_TESTS", &receipt)
    .output()
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !receipt.exists(),
        "a malformed marker must not spawn an automatic worker"
    );
}
