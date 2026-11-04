use super::*;

fn copy_fixture(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            // Git preserves bytes, not the published artifact's readonly seal.
            let mut permissions = fs::metadata(&target).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o600);
            }
            if matches!(
                target.extension().and_then(|ext| ext.to_str()),
                Some("fast" | "fieldnorm" | "idx" | "pos" | "store" | "term")
            ) {
                permissions.set_readonly(true);
            }
            fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

#[test]
fn schema_22_upgrade_starts_without_search_and_keeps_search_available() {
    let temp = tempdir();
    let environment = IsolatedProcessEnvironment::new(temp.path());
    let data_root = temp.path().join("data");
    let index = data_root.join("search/lexical");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    copy_fixture(&fixture.join("index"), &index);
    let sessions = environment.home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let source = sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl");
    fs::copy(fixture.join("source.jsonl"), &source).unwrap();
    // A second provider route must be included in the same fresh generation.
    let claude = environment.home.join(".claude/projects/example");
    fs::create_dir_all(&claude).unwrap();
    fs::write(claude.join("019fb000-0000-7000-8000-000000000200.jsonl"), concat!(
        r#"{"type":"user","uuid":"019fb000-0000-7000-8000-000000000201","sessionId":"019fb000-0000-7000-8000-000000000200","timestamp":"2026-08-17T00:00:00Z","cwd":"/example","message":{"role":"user","content":"secondproviderupgrademarker"}}"#,
        "\n",
    )).unwrap();
    fs::write(
        data_root.join("config.toml"),
        concat!(
            "[indexing]\nmode = \"automatic\"\n",
            "[analytics]\nenabled = false\n[upgrade]\nauto = \"off\"\n",
            "[search]\nsemantic = false\n[blame]\nenabled = true\n",
        ),
    )
    .unwrap();
    // Status is observational in both modes and identifies the actual readable
    // predecessor without initiating the upgrade.
    for (mode, state) in [("manual", "awaiting_upgrade"), ("automatic", "rebuilding")] {
        let config = data_root.join("config.toml");
        let text = fs::read_to_string(&config)
            .unwrap()
            .replace("mode = \"automatic\"", &format!("mode = \"{mode}\""))
            .replace("mode = \"manual\"", &format!("mode = \"{mode}\""));
        fs::write(config, text).unwrap();
        let output = isolated_local_command(&temp, &data_root, &environment)
            .args(["status", "--format=json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["lexical"]["status"], state, "{status:#}");
        assert_eq!(status["lexical"]["search_available"], true);
        assert_eq!(status["lexical"]["lexical_schema_version"], 22);
        assert_eq!(status["initialized"], true);
        assert!(!data_root.join("daemon/jobs/core-refresh.json").exists());
    }
    let pointer = index.join("active-generation.json");
    let before = fs::read(&pointer).unwrap();
    let mut daemon = spawn_isolated_source_refresh_daemon(&temp, &data_root, &environment);
    let deadline = Instant::now() + Duration::from_secs(30);
    let job = loop {
        daemon.assert_running();
        // No search or import admission: inspect the durable job only.
        let job = fs::read(data_root.join("daemon/jobs/core-refresh.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if let Some(job) = job
            .as_ref()
            .filter(|job| job["request_state"] == "published")
        {
            assert_ne!(fs::read(&pointer).unwrap(), before);
            break job.clone();
        }
        assert!(
            Instant::now() < deadline,
            "startup did not rebuild: {job:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(job["trigger"], "periodic");
    assert_eq!(job["refresh_scope"]["kind"], "all", "{job:#}");
    for marker in ["schema22upgrademarker", "secondproviderupgrademarker"] {
        let output = isolated_local_command(&temp, &data_root, &environment)
            .args(["search", marker, "--refresh", "off", "--format=json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let results: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            results["results"].as_array().unwrap().len(),
            1,
            "{results:#}; startup job: {job:#}"
        );
    }
    // Publication must wake attribution automatically, without import or blame.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        daemon.assert_running();
        let output = isolated_local_command(&temp, &data_root, &environment)
            .args(["status", "--format=json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        if status["attribution"]["currentness"] == "current" {
            assert_eq!(
                status["attribution"]["receipt"]["core_generation_id"],
                job["published_generation"]
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "attribution did not catch up: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(daemon);
}
