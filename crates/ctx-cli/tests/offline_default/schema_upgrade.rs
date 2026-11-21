use super::*;

pub(super) fn copy_fixture(from: &Path, to: &Path) {
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

#[test]
fn schema_22_manual_import_all_incremental_and_explicit_upgrade_without_retired_route_error() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    for case in ["import_all_incremental", "import_explicit_codex"] {
        let temp = tempdir();
        let environment = IsolatedProcessEnvironment::new(temp.path());
        let data_root = temp.path().join("data");
        let index = data_root.join("search/lexical");
        copy_fixture(&fixture.join("index"), &index);
        let sessions = environment.home.join(".codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::copy(
            fixture.join("source.jsonl"),
            sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
        )
        .unwrap();
        let claude = environment.home.join(".claude/projects/example");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("019fb000-0000-7000-8000-000000000200.jsonl"),
            concat!(
                r#"{"type":"user","uuid":"019fb000-0000-7000-8000-000000000201","sessionId":"019fb000-0000-7000-8000-000000000200","timestamp":"2026-08-17T00:00:00Z","cwd":"/example","message":{"role":"user","content":"manualimportupgrademarker"}}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            data_root.join("config.toml"),
            concat!(
                "[indexing]\nmode = \"manual\"\n",
                "[analytics]\nenabled = false\n[upgrade]\nauto = \"off\"\n",
                "[search]\nsemantic = false\n[blame]\nenabled = false\n",
            ),
        )
        .unwrap();

        let pointer_before: Value =
            serde_json::from_slice(&fs::read(index.join("active-generation.json")).unwrap())
                .unwrap();
        let schema_22_generation_id = pointer_before["active"]["generation_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let mut command = isolated_local_command(&temp, &data_root, &environment);
        command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
        if case == "import_all_incremental" {
            command.args([
                "import",
                "--all",
                "--incremental",
                "--no-blame",
                "--format",
                "json",
                "--progress",
                "none",
            ]);
        } else {
            command.args([
                "import",
                "--provider",
                "codex",
                "--path",
                sessions.to_str().unwrap(),
                "--no-blame",
                "--format",
                "json",
                "--progress",
                "none",
            ]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "case={case} failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let pointer_after: Value =
            serde_json::from_slice(&fs::read(index.join("active-generation.json")).unwrap())
                .unwrap();
        assert_ne!(
            pointer_after["active"]["generation_id"], schema_22_generation_id,
            "case={case} did not publish a new generation"
        );
        assert_eq!(
            pointer_after["previous"]["generation_id"], schema_22_generation_id,
            "case={case} did not retain readable schema-22 predecessor"
        );

        let status_output = isolated_local_command(&temp, &data_root, &environment)
            .args(["status", "--format=json"])
            .output()
            .unwrap();
        assert!(status_output.status.success());
        let status: Value = serde_json::from_slice(&status_output.stdout).unwrap();
        assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
        assert_eq!(
            status["lexical"]["lexical_schema_version"], 23,
            "{status:#}"
        );

        let expected_markers: &[&str] = &["schema22upgrademarker", "manualimportupgrademarker"];
        for marker in expected_markers {
            let search_output = isolated_local_command(&temp, &data_root, &environment)
                .args(["search", marker, "--refresh", "off", "--format=json"])
                .output()
                .unwrap();
            assert!(search_output.status.success());
            let results: Value = serde_json::from_slice(&search_output.stdout).unwrap();
            assert_eq!(
                results["results"].as_array().unwrap().len(),
                1,
                "case={case} marker={marker} search results: {results:#}"
            );
        }
    }
}

fn write_rebuild_marker(index_root: &Path) {
    let pointer: Value =
        serde_json::from_slice(&fs::read(index_root.join("active-generation.json")).unwrap())
            .unwrap();
    let generation_id = pointer["active"]["generation_id"].as_str().unwrap();
    let directory = pointer["active"]["directory"].as_str().unwrap();
    let bytes =
        format!(r#"{{"version":1,"generation_id":"{generation_id}","directory":"{directory}"}}"#);
    let marker_path = index_root.join("active-generation-rebuild-required.json");
    fs::write(&marker_path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn write_claude_session(dir: &Path, session_id: &str, event_uuid: &str, marker: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{session_id}.jsonl")),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "user",
                "uuid": event_uuid,
                "sessionId": session_id,
                "timestamp": "2026-08-17T00:00:00Z",
                "cwd": "/example",
                "message": {"role": "user", "content": marker}
            })
        ),
    )
    .unwrap();
}

fn assert_search_marker_present(
    temp: &TempDir,
    data_root: &Path,
    environment: &IsolatedProcessEnvironment,
    marker: &str,
) {
    let search_output = isolated_local_command(temp, data_root, environment)
        .args(["search", marker, "--refresh", "off", "--format=json"])
        .output()
        .unwrap();
    assert!(
        search_output.status.success(),
        "search {marker} failed: {}",
        String::from_utf8_lossy(&search_output.stderr)
    );
    let results: Value = serde_json::from_slice(&search_output.stdout).unwrap();
    assert_eq!(
        results["results"].as_array().unwrap().len(),
        1,
        "marker={marker} search results: {results:#}"
    );
}

#[test]
fn schema_22_scoped_provider_import_preserves_other_providers_and_explicit_sources() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");

    // Part 1: Genuine schema-22 index upgraded via `ctx import --provider codex`
    // (no `--path`) must widen to a full rebuild and keep Claude as well as Codex.
    {
        let temp = tempdir();
        let environment = IsolatedProcessEnvironment::new(temp.path());
        let data_root = temp.path().join("data");
        let index = data_root.join("search/lexical");
        copy_fixture(&fixture.join("index"), &index);
        let sessions = environment.home.join(".codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::copy(
            fixture.join("source.jsonl"),
            sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
        )
        .unwrap();
        write_claude_session(
            &environment.home.join(".claude/projects/example"),
            "019fb000-0000-7000-8000-000000000210",
            "019fb000-0000-7000-8000-000000000211",
            "scopedproviderclaudesurvivesmarker",
        );
        fs::write(
            data_root.join("config.toml"),
            concat!(
                "[indexing]\nmode = \"manual\"\n",
                "[analytics]\nenabled = false\n[upgrade]\nauto = \"off\"\n",
                "[search]\nsemantic = false\n[blame]\nenabled = false\n",
            ),
        )
        .unwrap();

        let mut command = isolated_local_command(&temp, &data_root, &environment);
        command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
        let output = command
            .args([
                "import",
                "--provider",
                "codex",
                "--no-blame",
                "--format",
                "json",
                "--progress",
                "none",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "scoped codex upgrade failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let upgraded = ctx_history_index::VerifiedIndex::open_pinned(&index).unwrap();
        assert_eq!(upgraded.manifest().lexical_schema_version, 23);
        drop(upgraded);

        assert_search_marker_present(&temp, &data_root, &environment, "schema22upgrademarker");
        assert_search_marker_present(
            &temp,
            &data_root,
            &environment,
            "scopedproviderclaudesurvivesmarker",
        );
    }

    // Part 2: Multi-provider index containing Codex + Claude + an explicit
    // `--path` source survives both `ctx import --provider codex` and
    // `ctx import --all` when the active generation requires a full rebuild.
    {
        let temp = tempdir();
        let environment = IsolatedProcessEnvironment::new(temp.path());
        let data_root = temp.path().join("data");
        let index = data_root.join("search/lexical");
        let sessions = environment.home.join(".codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::copy(
            fixture.join("source.jsonl"),
            sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
        )
        .unwrap();
        write_claude_session(
            &environment.home.join(".claude/projects/example"),
            "019fb000-0000-7000-8000-000000000220",
            "019fb000-0000-7000-8000-000000000221",
            "multiproviderclaudemarker",
        );
        let explicit_claude_dir = temp.path().join("external-explicit-claude");
        write_claude_session(
            &explicit_claude_dir,
            "019fb000-0000-7000-8000-000000000230",
            "019fb000-0000-7000-8000-000000000231",
            "explicitpathclaudesurvivesmarker",
        );
        fs::create_dir_all(&data_root).unwrap();
        fs::write(
            data_root.join("config.toml"),
            concat!(
                "[indexing]\nmode = \"manual\"\n",
                "[analytics]\nenabled = false\n[upgrade]\nauto = \"off\"\n",
                "[search]\nsemantic = false\n[blame]\nenabled = false\n",
            ),
        )
        .unwrap();

        let run_import = |args: &[&str]| {
            let mut command = isolated_local_command(&temp, &data_root, &environment);
            command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
            let output = command.args(args).output().unwrap();
            assert!(
                output.status.success(),
                "import {args:?} failed:\nstdout={}\nstderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run_import(&[
            "import",
            "--all",
            "--no-blame",
            "--format",
            "json",
            "--progress",
            "none",
        ]);
        run_import(&[
            "import",
            "--provider",
            "claude",
            "--path",
            explicit_claude_dir.to_str().unwrap(),
            "--no-blame",
            "--format",
            "json",
            "--progress",
            "none",
        ]);
        for marker in [
            "schema22upgrademarker",
            "multiproviderclaudemarker",
            "explicitpathclaudesurvivesmarker",
        ] {
            assert_search_marker_present(&temp, &data_root, &environment, marker);
        }

        // Rebuild via scoped `ctx import --provider codex`: must keep Claude AND explicit path.
        write_rebuild_marker(&index);
        run_import(&[
            "import",
            "--provider",
            "codex",
            "--no-blame",
            "--format",
            "json",
            "--progress",
            "none",
        ]);
        for marker in [
            "schema22upgrademarker",
            "multiproviderclaudemarker",
            "explicitpathclaudesurvivesmarker",
        ] {
            assert_search_marker_present(&temp, &data_root, &environment, marker);
        }

        // Rebuild via `ctx import --all`: must also keep the manifest-stored explicit path.
        write_rebuild_marker(&index);
        run_import(&[
            "import",
            "--all",
            "--no-blame",
            "--format",
            "json",
            "--progress",
            "none",
        ]);
        for marker in [
            "schema22upgrademarker",
            "multiproviderclaudemarker",
            "explicitpathclaudesurvivesmarker",
        ] {
            assert_search_marker_present(&temp, &data_root, &environment, marker);
        }
    }
}

#[test]
fn rebuild_required_upgrade_preserves_released_configured_provider_root_identity() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    let temp = tempdir();
    let root = fs::canonicalize(temp.path()).unwrap();
    let environment = IsolatedProcessEnvironment::new(&root);
    let data_root = root.join("data");
    let index = data_root.join("search/lexical");
    let first_home = root.join("codex-released-first");
    let moved_home = root.join("codex-released-moved");
    fs::create_dir_all(first_home.join("sessions")).unwrap();
    fs::create_dir_all(first_home.join("archived_sessions")).unwrap();
    fs::write(first_home.join("history.jsonl"), b"").unwrap();
    fs::copy(
        fixture.join("source.jsonl"),
        first_home.join("sessions/rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();
    fs::create_dir_all(&data_root).unwrap();

    let write_config = |root_path: Option<&Path>| {
        let mut text = String::from(concat!(
            "[indexing]\nmode = \"manual\"\n",
            "[analytics]\nenabled = false\n[upgrade]\nauto = \"off\"\n",
            "[search]\nsemantic = false\n[blame]\nenabled = false\n",
        ));
        if let Some(path) = root_path {
            text.push_str(&format!(
                "\n[sources.roots.work]\nprovider = \"codex\"\npath = {:?}\n",
                path.to_str().unwrap()
            ));
        }
        fs::write(data_root.join("config.toml"), text).unwrap();
    };

    let run_import_all = |codex_home: &Path| {
        let mut command = isolated_local_command(&temp, &data_root, &environment);
        command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
        command.env("CODEX_HOME", codex_home);
        let output = command
            .args([
                "import",
                "--all",
                "--no-blame",
                "--format",
                "json",
                "--progress",
                "none",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "import --all failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    // 1. Index automatic CODEX_HOME, then register overlapping configured root
    //    so it acquires `Released` source identity.
    write_config(None);
    run_import_all(&first_home);
    write_config(Some(&first_home));
    run_import_all(&first_home);

    let before = ctx_history_index::VerifiedIndex::open_pinned(&index).unwrap();
    let before_root = before.manifest().provider_root("work").unwrap();
    assert_eq!(
        before_root.source_identity(),
        ctx_history_index::ProviderRootSourceIdentity::Released
    );
    let before_routes = before_root.routes().to_vec();
    let before_sources = before
        .manifest()
        .sources
        .iter()
        .map(|cert| cert.observation().source().clone())
        .collect::<Vec<_>>();
    assert_eq!(before_sources.len(), 2);
    drop(before);

    // 2. Relocate the configured root away from CODEX_HOME, mark active
    //    generation for full rebuild, and rebuild via `ctx import --all`.
    fs::rename(&first_home, &moved_home).unwrap();
    fs::create_dir(&first_home).unwrap();
    write_config(Some(&moved_home));
    write_rebuild_marker(&index);
    run_import_all(&first_home);

    let after = ctx_history_index::VerifiedIndex::open_pinned(&index).unwrap();
    let after_root = after.manifest().provider_root("work").unwrap();
    assert_eq!(
        after_root.source_identity(),
        ctx_history_index::ProviderRootSourceIdentity::Released,
        "rebuild downgraded relocated Released configured root to NamedV1"
    );
    assert_eq!(after_root.routes(), before_routes.as_slice());
    let after_sources = after
        .manifest()
        .sources
        .iter()
        .map(|cert| cert.observation().source().clone())
        .collect::<Vec<_>>();
    assert_eq!(after_sources, before_sources);
    drop(after);

    assert_search_marker_present(&temp, &data_root, &environment, "schema22upgrademarker");
}
