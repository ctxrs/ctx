use super::*;
use fs2::FileExt;

#[test]
fn released_230_blame_upgrade_reports_retryable_maintenance_and_recovers() {
    let temp = tempdir();
    let environment = IsolatedProcessEnvironment::new(temp.path());
    let data_root = temp.path().join("data");
    fs::create_dir(&data_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/attribution-2.3.0");
    schema_upgrade::copy_fixture(&fixture.join("search"), &data_root.join("search"));
    let sessions = environment.home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::copy(
        fixture.join("source.jsonl"),
        sessions.join("rollout-019fb000-0000-7000-8000-000000000300.jsonl"),
    )
    .unwrap();
    let commit = fs::read_to_string(fixture.join("commit.txt")).unwrap();
    let config = |mode| {
        fs::write(data_root.join("config.toml"), format!(
        "[indexing]\nmode=\"{mode}\"\n[analytics]\nenabled=false\n[upgrade]\nauto=\"off\"\n[search]\nsemantic=false\n[blame]\nenabled=true\n"
    )).unwrap()
    };
    let command = |args: &[&str]| {
        isolated_local_command(&temp, &data_root, &environment)
            .args(args)
            .output()
            .unwrap()
    };
    let status = || {
        let output = command(&["status", "--format=json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let blame = || command(&["blame", "commit", commit.trim(), "--format=json"]);
    config("manual");
    let before = status();
    assert_eq!(
        before["attribution"]["receipt"]["materializer_revision"],
        "2026.10.01.2+audited-primary-producer",
        "{before:#}"
    );
    assert_eq!(before["attribution"]["currentness"], "needs_rebuild");
    assert_eq!(before["lexical"]["search_available"], true);
    let manual = blame();
    assert!(!manual.status.success());
    let manual: Value = serde_json::from_slice(&manual.stderr).unwrap();
    assert_eq!(manual["error"], "needs_rebuild");
    assert_eq!(manual["retryable"], false);
    assert_eq!(
        manual["message"],
        "Blame data must be rebuilt for this version."
    );
    assert_eq!(
        manual["next_action"]["argv"],
        json!(["ctx", "import", "--all"])
    );

    // Hold the real writer lock so the daemon's ordinary worker waits. This
    // exposes the scheduled window deterministically without changing old bytes.
    let lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(data_root.join("search/attribution/attribution-materializer.lock"))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    lock.lock_exclusive().unwrap();
    config("automatic");
    let mut daemon = spawn_isolated_source_refresh_daemon(&temp, &data_root, &environment);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let during = status();
        if during["daemon"]["running"] == true {
            assert_eq!(during["attribution"]["status"], "rebuilding", "{during:#}");
            assert_eq!(during["attribution"]["currentness"], "needs_rebuild");
            assert_eq!(during["attribution"]["availability"]["commit_blame"], false);
            let output = blame();
            assert!(!output.status.success());
            let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["error"], "rebuilding");
            assert_eq!(diagnostic["retryable"], true);
            assert_eq!(diagnostic["next_action"]["argv"], json!(["ctx", "status"]));
            assert!(diagnostic["progress"].is_object());
            assert_eq!(
                during["attribution"]["diagnostic"]["error"],
                diagnostic["error"]
            );
            let human = command(&["status"]);
            assert!(String::from_utf8_lossy(&human.stdout).contains("rebuilding after upgrade"));
            let human = command(&["blame", "commit", commit.trim()]);
            let text = String::from_utf8_lossy(&human.stderr);
            assert!(
                text.contains("rebuilding after upgrade") && text.contains("ctx status"),
                "{text}"
            );
            assert!(!text.contains("ctx import"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    FileExt::unlock(&lock).unwrap();
    drop(lock);
    loop {
        daemon.assert_running();
        let after = status();
        if after["attribution"]["currentness"] == "current"
            && after["lexical"]["lexical_schema_version"] == 23
        {
            assert_ne!(
                after["attribution"]["receipt"]["materializer_revision"],
                before["attribution"]["receipt"]["materializer_revision"]
            );
            assert_ne!(after["attribution"]["status"], "rebuilding");
            break;
        }
        assert!(Instant::now() < deadline, "{after:#}");
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = blame();
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    let after: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after["matches"].as_array().unwrap().len(), 1);
    assert_eq!(after["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(
        after["matches"][0]["value"]["subject"]["display"],
        commit.trim()
    );
    drop(daemon);
}
