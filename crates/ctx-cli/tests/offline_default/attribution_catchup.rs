use super::*;
use fs2::FileExt;

struct CatchupFixture {
    temp: TempDir,
    environment: IsolatedProcessEnvironment,
    data_root: PathBuf,
    commit: String,
}

impl CatchupFixture {
    fn new() -> Self {
        let temp = tempdir();
        let environment = IsolatedProcessEnvironment::new(temp.path());
        let data_root = temp.path().join("data");
        let commit = attribution_upgrade::prepare_projection(&data_root, &environment);
        let fixture = Self {
            temp,
            environment,
            data_root,
            commit,
        };
        fixture.configure("manual");
        fixture.import(&["import", "--all"]);
        assert_eq!(fixture.status()["attribution"]["currentness"], "current");
        // Publish newer Core through the ordinary import path, deliberately
        // leaving the completed attribution generation behind.
        let source = fixture
            .environment
            .home
            .join(".codex/sessions/rollout-019fb000-0000-7000-8000-000000000300.jsonl");
        writeln!(
            fs::OpenOptions::new().append(true).open(source).unwrap(),
            "{}",
            json!({
                "timestamp": "2026-11-01T12:00:02Z", "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "A later history event."}
                ]}
            })
        )
        .unwrap();
        fixture.import(&["import", "--all", "--no-blame"]);
        assert_eq!(fixture.status()["attribution"]["currentness"], "stale");
        fixture
    }

    fn configure(&self, mode: &str) {
        fs::write(self.data_root.join("config.toml"), format!(
            "[indexing]\nmode=\"{mode}\"\n[analytics]\nenabled=false\n[upgrade]\nauto=\"off\"\n[search]\nsemantic=false\n[blame]\nenabled=true\n"
        )).unwrap();
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        isolated_local_command(&self.temp, &self.data_root, &self.environment)
            .args(args)
            .output()
            .unwrap()
    }

    fn import(&self, args: &[&str]) {
        let mut daemon =
            spawn_isolated_daemon(&self.temp, &self.data_root, &self.environment, true);
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.status()["daemon"]["running"] != true {
            daemon.assert_running();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        let output = self.command(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn status(&self) -> Value {
        let output = self.command(&["status", "--format=json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn file_blame(&self) -> Value {
        let output = self.command(&["blame", "file", "unrecorded.txt", "--format=json"]);
        assert!(!output.status.success());
        serde_json::from_slice(&output.stderr).unwrap()
    }

    fn commit_blame(&self) -> Value {
        let output = self.command(&["blame", "commit", self.commit.trim(), "--format=json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn daemon(&self) -> OfflineSourceRefreshDaemon {
        spawn_isolated_source_refresh_daemon(&self.temp, &self.data_root, &self.environment)
    }

    fn wait_for_maintenance(
        &self,
        daemon: &mut OfflineSourceRefreshDaemon,
        expected: &str,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            daemon.assert_running();
            let status = self.status();
            if status["attribution"]["maintenance"]["status"] == expected {
                return status;
            }
            assert!(Instant::now() < deadline, "{status:#}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn assert_repair_advice(&self) {
        let status = self.status();
        assert_eq!(status["attribution"]["currentness"], "stale");
        assert_ne!(status["attribution"]["status"], "rebuilding");
        for diagnostic in [&status["attribution"]["diagnostic"], &self.file_blame()] {
            assert_eq!(diagnostic["error_code"], "stale_source");
            assert_eq!(diagnostic["reason"], "projection_stale");
            assert_eq!(diagnostic["retryable"], false);
            assert_eq!(diagnostic["next_action"]["kind"], "import_all");
            assert_eq!(
                diagnostic["next_action"]["argv"],
                json!(["ctx", "import", "--all"])
            );
        }
    }
}

#[test]
fn stale_file_blame_reports_admitted_catchup_and_preserves_committed_results() {
    let fixture = CatchupFixture::new();
    fixture.assert_repair_advice();
    let committed = fixture.commit_blame();
    assert_eq!(committed["freshness"]["state"], "stale_committed");
    assert_eq!(committed["matches"].as_array().unwrap().len(), 1);

    let lock = fs::OpenOptions::new()
        .write(true)
        .open(
            fixture
                .data_root
                .join("search/attribution/attribution-materializer.lock"),
        )
        .unwrap();
    lock.lock_exclusive().unwrap();
    fixture.configure("automatic");
    fixture.assert_repair_advice(); // No persistent owner yet.
    let mut daemon = fixture.daemon();
    let during = fixture.wait_for_maintenance(&mut daemon, "running");
    assert_eq!(during["attribution"]["currentness"], "stale");
    assert_eq!(during["attribution"]["status"], "rebuilding", "{during:#}");
    let negative = fixture.file_blame();
    for diagnostic in [&during["attribution"]["diagnostic"], &negative] {
        assert_eq!(diagnostic["error"], "rebuilding");
        assert_eq!(diagnostic["error_code"], "rebuilding");
        assert_eq!(diagnostic["reason"], "projection_stale");
        assert_eq!(diagnostic["retryable"], true);
        assert_eq!(diagnostic["next_action"]["kind"], "check_status");
        assert_eq!(diagnostic["next_action"]["argv"], json!(["ctx", "status"]));
        assert!(diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("catching up"));
    }
    assert_eq!(negative["freshness"]["state"], "stale_committed");
    assert_eq!(fixture.commit_blame(), committed);
    for args in [vec!["status"], vec!["blame", "file", "unrecorded.txt"]] {
        let output = fixture.command(&args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            text.contains("catching up") && text.contains("ctx status"),
            "{text}"
        );
        assert!(!text.contains("ctx import"), "{text}");
        assert!(!text.contains("after upgrade"), "{text}");
    }
    #[cfg(unix)]
    {
        struct Resume(u32);
        impl Drop for Resume {
            fn drop(&mut self) {
                let _ = StdCommand::new("kill")
                    .args(["-CONT", &self.0.to_string()])
                    .status();
            }
        }
        let pid = daemon.child.as_ref().unwrap().id();
        let resume = Resume(pid);
        assert!(StdCommand::new("kill")
            .args(["-STOP", &pid.to_string()])
            .status()
            .unwrap()
            .success());
        let suspended = fixture.status();
        assert_eq!(suspended["daemon"]["running"], true);
        assert!(suspended["attribution"]["maintenance"].is_null());
        fixture.assert_repair_advice();
        assert_eq!(fixture.commit_blame(), committed);
        drop(resume);
        let resumed = fixture.wait_for_maintenance(&mut daemon, "running");
        assert_eq!(resumed["attribution"]["status"], "rebuilding");
    }
    FileExt::unlock(&lock).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let after = fixture.status();
        if after["attribution"]["currentness"] == "current" {
            assert_ne!(after["attribution"]["status"], "rebuilding");
            break;
        }
        assert!(Instant::now() < deadline, "{after:#}");
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = fixture.commit_blame();
    assert_eq!(after["freshness"]["state"], "current");
    assert_eq!(after["evidence"].as_array().unwrap().len(), 1);
    for field in ["event_id", "session_id", "evidence_sha256"] {
        assert_eq!(
            after["evidence"][0]["citation"][field],
            committed["evidence"][0]["citation"][field]
        );
    }
    assert_eq!(fixture.file_blame()["next_action"]["kind"], "search_core");
}

#[cfg(unix)]
#[test]
fn failed_catchup_keeps_stale_repair_advice_with_a_live_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = CatchupFixture::new();
    let lock = fixture
        .data_root
        .join("search/attribution/attribution-materializer.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
    fixture.configure("automatic");
    let mut daemon = fixture.daemon();
    let failed = fixture.wait_for_maintenance(&mut daemon, "failed");
    assert_eq!(failed["daemon"]["running"], true);
    assert!(
        failed["attribution"]["maintenance"]["error"]
            .as_str()
            .unwrap()
            .contains("attribution-materializer.lock"),
        "{failed:#}"
    );
    fixture.assert_repair_advice();
    assert_eq!(
        fixture.commit_blame()["freshness"]["state"],
        "stale_committed"
    );
    assert!(
        String::from_utf8_lossy(&fixture.command(&["status"]).stdout)
            .contains("Maintenance failed")
    );
}
