use super::*;
use fs2::FileExt;

#[test]
fn released_230_blame_upgrade_reports_retryable_maintenance_and_recovers() {
    let temp = tempdir();
    let environment = IsolatedProcessEnvironment::new(temp.path());
    let data_root = temp.path().join("data");
    let commit = prepare_projection(&data_root, &environment);
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
        if during["attribution"]["maintenance"]["status"] == "running" {
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
    #[cfg(unix)]
    {
        // A held writer lock and an admitted job are insufficient if their
        // owner stops answering. Resume even if an assertion unwinds.
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
        let suspended = status();
        assert_eq!(suspended["daemon"]["running"], true);
        assert_ne!(suspended["attribution"]["status"], "rebuilding");
        assert!(suspended["attribution"]["maintenance"].is_null());
        let diagnostic: Value = serde_json::from_slice(&blame().stderr).unwrap();
        assert_eq!(diagnostic["error"], "needs_rebuild");
        assert_eq!(diagnostic["retryable"], false);
        assert_eq!(diagnostic["next_action"]["kind"], "import_all");
        drop(resume);
        let resumed = status();
        assert_eq!(
            resumed["attribution"]["status"], "rebuilding",
            "{resumed:#}"
        );
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

pub(super) fn prepare_projection(
    data_root: &Path,
    environment: &IsolatedProcessEnvironment,
) -> String {
    fs::create_dir(data_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(data_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/attribution-2.3.0");
    schema_upgrade::copy_fixture(&fixture.join("search"), &data_root.join("search"));
    let repo_root = prepare_fixture_repository(data_root.parent().unwrap());
    let sessions = environment.home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    write_rebound_fixture_source(
        &fixture.join("source.jsonl"),
        &sessions.join("rollout-019fb000-0000-7000-8000-000000000300.jsonl"),
        &repo_root,
    );
    fs::read_to_string(fixture.join("commit.txt")).unwrap()
}

/// Rebuilding attribution across the 2.3.0 -> current boundary re-evaluates
/// repository-neutral Core records against the local Git repository referenced
/// by `source.jsonl`. Bind that source to a hermetic per-test Git repository
/// with the same remote identity instead of depending on `/tmp/ctx-230-blame-fixture`.
fn prepare_fixture_repository(temp_root: &Path) -> PathBuf {
    let repo = temp_root.join("ctx-230-blame-fixture-repo");
    fs::create_dir(&repo).unwrap();
    let repo = fs::canonicalize(&repo).unwrap();
    let run_git = |args: &[&str]| {
        let output = StdCommand::new("git")
            .current_dir(&repo)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env(
                "SystemRoot",
                std::env::var_os("SystemRoot").unwrap_or_default(),
            )
            .env("HOME", temp_root)
            .env("XDG_CONFIG_HOME", temp_root.join("git-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", temp_root.join("absent-gitconfig"))
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=absent-hooks",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init", "-q", "--initial-branch=main"]);
    run_git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/example/ctx-upgrade-fixture.git",
    ]);
    repo
}

fn write_rebound_fixture_source(source: &Path, destination: &Path, repo_root: &Path) {
    const FIXTURE_REPO_PATH: &str = "/tmp/ctx-230-blame-fixture/repo";
    let repo_str = repo_root.to_str().unwrap();
    let mut rewritten = String::new();
    let mut updated_cwd = false;
    let mut updated_workdir = false;
    for line in fs::read_to_string(source).unwrap().lines() {
        let mut entry: Value = serde_json::from_str(line).unwrap();
        if entry["payload"]["cwd"].as_str() == Some(FIXTURE_REPO_PATH) {
            entry["payload"]["cwd"] = Value::String(repo_str.to_owned());
            updated_cwd = true;
        }
        if let Some(arguments) = entry["payload"]["arguments"].as_str() {
            let mut parsed: Value = serde_json::from_str(arguments).unwrap();
            if parsed["workdir"].as_str() == Some(FIXTURE_REPO_PATH) {
                parsed["workdir"] = Value::String(repo_str.to_owned());
                entry["payload"]["arguments"] = Value::String(parsed.to_string());
                updated_workdir = true;
            }
        }
        rewritten.push_str(&entry.to_string());
        rewritten.push('\n');
    }
    assert!(updated_cwd && updated_workdir);
    assert!(!rewritten.contains(FIXTURE_REPO_PATH));
    fs::write(destination, rewritten).unwrap();
}

#[cfg(unix)]
#[test]
fn failed_attribution_does_not_promise_rebuilding_with_a_live_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir();
    let environment = IsolatedProcessEnvironment::new(temp.path());
    let data_root = temp.path().join("data");
    let commit = prepare_projection(&data_root, &environment);
    fs::write(
        data_root.join("config.toml"),
        concat!(
            "[indexing]\nmode=\"automatic\"\n[analytics]\nenabled=false\n",
            "[upgrade]\nauto=\"off\"\n[search]\nsemantic=false\n",
        ),
    )
    .unwrap();
    let lock = data_root.join("search/attribution/attribution-materializer.lock");
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
    let command = |args: &[&str]| {
        isolated_local_command(&temp, &data_root, &environment)
            .args(args)
            .output()
            .unwrap()
    };
    let mut daemon = spawn_isolated_source_refresh_daemon(&temp, &data_root, &environment);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let output = command(&["status", "--format=json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        if status["attribution"]["maintenance"]["status"] == "failed" {
            assert_eq!(status["daemon"]["running"], true);
            assert_eq!(status["attribution"]["currentness"], "needs_rebuild");
            assert_ne!(status["attribution"]["status"], "rebuilding");
            let error = status["attribution"]["maintenance"]["error"]
                .as_str()
                .unwrap();
            assert!(error.contains("attribution-materializer.lock"), "{error}");
            assert_eq!(
                status["attribution"]["diagnostic"]["next_action"]["kind"],
                "import_all"
            );
            break;
        }
        assert!(Instant::now() < deadline, "{status:#}");
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = command(&["blame", "commit", commit.trim(), "--format=json"]);
    assert!(!output.status.success());
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["error"], "needs_rebuild");
    assert_eq!(diagnostic["retryable"], false);
    assert_eq!(
        diagnostic["next_action"]["argv"],
        json!(["ctx", "import", "--all"])
    );
    let human = command(&["status"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("Maintenance failed"));
    daemon.assert_running();
    drop(daemon);

    // Failure belongs to the old owner. Restart after repair must recover using
    // ordinary startup admission, with no manual import or stale failure state.
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    let mut daemon = spawn_isolated_source_refresh_daemon(&temp, &data_root, &environment);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let output = command(&["blame", "commit", commit.trim(), "--format=json"]);
        if output.status.success() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
