#[path = "../support/mod.rs"]
mod support;

use support::*;

fn success_stdout(command: &mut Command) -> String {
    let stdout = command.assert().success().get_output().stdout.clone();
    String::from_utf8(stdout).unwrap()
}

fn object_keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

fn unmarked_filesystem_root(path: &Path) -> &Path {
    let root = path
        .ancestors()
        .last()
        .expect("an absolute temporary path has a filesystem root");
    assert!(
        !root.join(".idea").exists(),
        "test root has an .idea marker"
    );
    assert!(!root.join(".git").exists(), "test root has a Git marker");
    root
}

fn codex_rollout(native_session_id: &str, marker: &str) -> Vec<u8> {
    [
        json!({
            "timestamp": "2026-08-18T01:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": native_session_id,
                "timestamp": "2026-08-18T01:00:00Z",
                "cwd": "/workspace/project",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-08-18T01:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "compressed-custom-root-message",
                "role": "user",
                "content": [{"type": "input_text", "text": marker}]
            }
        }),
    ]
    .into_iter()
    .flat_map(|record| format!("{}\n", serde_json::to_string(&record).unwrap()).into_bytes())
    .collect()
}

#[test]
fn sources_json_keeps_the_v1_top_level_and_source_fields() {
    let temp = tempdir();
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(codex_home.join("sessions")).unwrap();
    fs::write(codex_home.join("sessions/session.jsonl"), "{}\n").unwrap();
    let packet = json_output(ctx(&temp).env("CODEX_HOME", codex_home).args([
        "sources",
        "--provider",
        "codex",
        "--format=json",
    ]));

    assert_eq!(packet["schema_version"], 1);
    assert_eq!(
        object_keys(&packet),
        BTreeSet::from([
            "automatic_discovery",
            "hidden_missing_sources",
            "issues",
            "issues_truncated",
            "schema_version",
            "scope",
            "sources",
        ])
    );
    assert_eq!(packet["issues_truncated"], false);
    let source = packet["sources"].as_array().unwrap().first().unwrap();
    assert_eq!(
        object_keys(source),
        BTreeSet::from([
            "exists",
            "import_support",
            "importable",
            "native_import",
            "path",
            "provider",
            "selection",
            "source_format",
            "status",
            "status_reason",
            "unsupported_reason",
        ])
    );

    let mut no_path_command = ctx(&temp);
    no_path_command.current_dir(unmarked_filesystem_root(temp.path()));
    let no_path_report =
        json_output(no_path_command.args(["sources", "--provider", "firebender", "--format=json"]));
    assert_eq!(object_keys(&no_path_report), object_keys(&packet));
    assert!(no_path_report["sources"].as_array().unwrap().is_empty());
    assert_eq!(no_path_report["issues_truncated"], false);
    assert!(no_path_report["issues"].as_array().unwrap().is_empty());

    let marked_project = temp.path().join("marked-firebender-project");
    fs::create_dir_all(marked_project.join(".idea")).unwrap();
    let marked_report = json_output(ctx(&temp).current_dir(&marked_project).args([
        "sources",
        "--provider",
        "firebender",
        "--format=json",
    ]));
    let marked_sources = marked_report["sources"].as_array().unwrap();
    assert_eq!(marked_sources.len(), 1, "{marked_report:#}");
    let firebender = &marked_sources[0];
    assert_eq!(firebender["provider"], "firebender");
    assert_eq!(
        firebender["source_format"],
        "firebender_chat_history_sqlite"
    );
    assert_eq!(firebender["status"], "missing");
    assert_eq!(firebender["exists"], false);
    assert_eq!(firebender["native_import"], true);
    assert_eq!(firebender["importable"], false);
    assert_eq!(
        firebender["path"],
        marked_project
            .join(".idea/firebender/chat_history.db")
            .display()
            .to_string()
    );
}

#[test]
fn provider_filtered_human_sources_and_import_errors_are_actionable() {
    let no_disk = tempdir();
    let stdout = success_stdout(ctx(&no_disk).env("OPENCODE_DB", ":memory:").args([
        "sources",
        "--provider",
        "opencode",
    ]));
    assert!(stdout.contains("has no disk history selected"), "{stdout}");
    assert!(
        stdout.contains("ctx import --provider opencode --path <path>"),
        "{stdout}"
    );
    let stderr = failure_stderr(ctx(&no_disk).env("OPENCODE_DB", ":memory:").args([
        "import",
        "--provider",
        "opencode",
        "--format=json",
    ]));
    assert!(stderr.contains("has no disk history selected"), "{stderr}");
    assert!(
        stderr.contains("ctx import --provider opencode --path <path>"),
        "{stderr}"
    );

    let unreconstructible = tempdir();
    let stdout = success_stdout(
        ctx(&unreconstructible)
            .env("CLAUDE_CONFIG_DIR", "relative-provider-root")
            .args(["sources", "--provider", "claude"]),
    );
    assert!(
        stdout.contains("history location could not be selected safely"),
        "{stdout}"
    );
    assert!(
        stdout.contains("ctx import --provider claude --path <path>"),
        "{stdout}"
    );
    assert!(!stdout.contains("relative-provider-root"), "{stdout}");
    let stderr = failure_stderr(
        ctx(&unreconstructible)
            .env("CLAUDE_CONFIG_DIR", "relative-provider-root")
            .args(["import", "--provider", "claude", "--format=json"]),
    );
    assert!(
        stderr.contains("automatic history location cannot be safely reconstructed"),
        "{stderr}"
    );
    assert!(!stderr.contains("relative-provider-root"), "{stderr}");

    let unestablished = tempdir();
    let unmarked_cwd = unmarked_filesystem_root(unestablished.path());
    let stdout = success_stdout(ctx(&unestablished).current_dir(unmarked_cwd).args([
        "sources",
        "--provider",
        "firebender",
    ]));
    assert!(stdout.contains("No history sources found"), "{stdout}");
    assert!(stdout.contains("ctx sources --all"), "{stdout}");
    let stderr = failure_stderr(ctx(&unestablished).current_dir(unmarked_cwd).args([
        "import",
        "--provider",
        "firebender",
        "--format=json",
    ]));
    assert!(
        stderr.contains("no importable Firebender history source was discovered"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx import --provider firebender --path <path>"),
        "{stderr}"
    );

    let marked_project = unestablished.path().join("marked-firebender-project");
    fs::create_dir_all(marked_project.join(".idea")).unwrap();
    let stdout = success_stdout(ctx(&unestablished).current_dir(&marked_project).args([
        "sources",
        "--provider",
        "firebender",
    ]));
    assert!(stdout.contains("firebender"), "{stdout}");
    assert!(
        stdout.contains(".idea/firebender/chat_history.db"),
        "{stdout}"
    );
    assert!(stdout.contains("missing"), "{stdout}");
    let stderr = failure_stderr(ctx(&unestablished).current_dir(&marked_project).args([
        "import",
        "--provider",
        "firebender",
        "--format=json",
    ]));
    assert!(
        stderr.contains("no importable Firebender history source was discovered"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx import --provider firebender --path <path>"),
        "{stderr}"
    );
}

#[test]
fn current_kiro_discovery_is_human_only_and_provider_filtered_import_does_not_dispatch() {
    let temp = tempdir();
    let sessions = temp.path().join(".kiro/sessions");
    let cli = sessions.join("cli");
    fs::create_dir_all(&cli).unwrap();
    fs::write(cli.join("session-id.json"), b"{}").unwrap();
    fs::write(cli.join("session-id.jsonl"), b"{}\n").unwrap();

    let stdout = success_stdout(ctx(&temp).args(["sources", "--provider", "kiro-cli"]));
    let concise_sessions = Path::new("~").join(".kiro/sessions");
    assert!(
        stdout.contains(&concise_sessions.display().to_string()),
        "{stdout}"
    );
    assert!(!stdout.contains(temp.path().to_str().unwrap()), "{stdout}");
    assert!(stdout.contains("unsupported"), "{stdout}");
    assert!(stdout.contains("Kiro ACP/v3"), "{stdout}");
    assert!(
        stdout.contains("ctx import --provider kiro-cli --path <path>"),
        "{stdout}"
    );

    let json = json_output(ctx(&temp).args(["sources", "--provider", "kiro-cli", "--format=json"]));
    assert_eq!(json["schema_version"], 1);
    assert_eq!(
        object_keys(&json),
        BTreeSet::from([
            "automatic_discovery",
            "hidden_missing_sources",
            "issues",
            "issues_truncated",
            "schema_version",
            "scope",
            "sources",
        ])
    );
    assert_eq!(json["issues_truncated"], false);
    let source = json["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["status"] == "unsupported")
        .unwrap();
    assert_eq!(source["import_support"], "unsupported");
    assert_eq!(source["native_import"], false);
    assert_eq!(source["importable"], false);

    let stderr =
        failure_stderr(ctx(&temp).args(["import", "--provider", "kiro-cli", "--format=json"]));
    assert!(
        stderr.contains("detected unsupported history at"),
        "{stderr}"
    );
    assert!(stderr.contains(sessions.to_str().unwrap()), "{stderr}");
    assert!(
        stderr.contains("current ctx cannot import that path"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx import --provider kiro-cli --path <path>"),
        "{stderr}"
    );
}

#[test]
fn mux_archive_discovery_is_importable_and_dispatches() {
    let temp = tempdir();
    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let mux_root = temp.path().join("custom-mux");
    let sessions = mux_root.join("sessions/session-id");
    fs::create_dir_all(&sessions).unwrap();
    let marker = "mux-archive-custom-root-oracle";
    let archive = json!({
        "workspaceId": "session-id",
        "id": "archive-event-id",
        "role": "user",
        "parts": [{"type": "text", "text": marker}],
        "metadata": {"historySequence": 0}
    });
    fs::write(
        sessions.join("chat-archive.jsonl"),
        format!("{}\n", serde_json::to_string(&archive).unwrap()),
    )
    .unwrap();
    let _daemon = start_source_refresh_daemon_with_provider_env(
        &temp,
        &data_root(&temp),
        temp.path(),
        &state,
        "MUX_ROOT",
        &mux_root,
    );

    let stdout = success_stdout(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "sources",
        "--provider",
        "mux",
    ]));
    assert!(stdout.contains("available"), "{stdout}");
    assert!(!stdout.contains("unsupported"), "{stdout}");

    let sources = json_output(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "sources",
        "--provider",
        "mux",
        "--format=json",
    ]));
    let source = sources["sources"].as_array().unwrap().first().unwrap();
    assert_eq!(source["status"], "available");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let imported = json_output(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "import",
        "--provider",
        "mux",
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(imported["outcome"], "success", "{imported:#}");
    assert!(imported["totals"]["current_indexed_documents"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    let search = json_output(ctx(&temp).args([
        "search",
        marker,
        "--provider",
        "mux",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "mux", marker, 1, "message");
}

#[test]
fn explicit_manual_paths_import_and_current_kiro_stops_at_admission() {
    let manual = tempdir();
    let manual_state = manual.path().join("state");
    fs::create_dir_all(&manual_state).unwrap();
    let _daemon =
        start_source_refresh_daemon(&manual, &data_root(&manual), manual.path(), &manual_state);
    let query = "factory-manual-custom-root-oracle";
    let factory = write_native_factory_droid_fixture(&manual, query);
    let imported = json_output(ctx(&manual).args([
        "import",
        "--provider",
        "factory-ai-droid",
        "--path",
        &factory,
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    let search = json_output(ctx(&manual).args([
        "search",
        query,
        "--provider",
        "factory-ai-droid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "factory_ai_droid", query, 1, "message");

    let unsupported = tempdir();
    let unsupported_state = unsupported.path().join("state");
    fs::create_dir_all(&unsupported_state).unwrap();
    let _unsupported_daemon = start_source_refresh_daemon(
        &unsupported,
        &data_root(&unsupported),
        unsupported.path(),
        &unsupported_state,
    );
    let codex = unsupported.path().join("renamed-rollout.jsonl.zst");
    let codex_marker = "codex-compressed-custom-root-oracle";
    let compressed = zstd::stream::encode_all(
        std::io::Cursor::new(codex_rollout(
            "019fb000-0000-7000-8000-000000000070",
            codex_marker,
        )),
        1,
    )
    .unwrap();
    fs::write(&codex, compressed).unwrap();
    let imported_codex = json_output(ctx(&unsupported).args([
        "import",
        "--provider",
        "codex",
        "--path",
        codex.to_str().unwrap(),
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(imported_codex["outcome"], "success", "{imported_codex:#}");
    assert!(imported_codex["totals"]["current_indexed_documents"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    let kiro = unsupported.path().join("sessions");
    let kiro_cli = kiro.join("cli");
    fs::create_dir_all(&kiro_cli).unwrap();
    fs::write(kiro_cli.join("session-id.json"), b"{}").unwrap();
    fs::write(kiro_cli.join("session-id.jsonl"), b"{}\n").unwrap();
    let stderr = failure_stderr(ctx(&unsupported).args([
        "import",
        "--provider",
        "kiro-cli",
        "--path",
        kiro.to_str().unwrap(),
        "--no-daemon",
        "--progress",
        "none",
    ]));
    assert!(stderr.contains("is not importable"), "{stderr}");
    assert!(stderr.contains("Kiro ACP/v3"), "{stderr}");
}

#[test]
fn current_kiro_blocks_unqualified_all_provider_publication_without_dispatching_search() {
    let temp = tempdir();
    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let _daemon = start_source_refresh_daemon(&temp, &data_root(&temp), temp.path(), &state);
    let sessions = temp.path().join(".kiro/sessions/cli");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("session-id.json"), b"{}").unwrap();
    fs::write(sessions.join("session-id.jsonl"), b"{}\n").unwrap();

    let setup = failure_json_output(ctx(&temp).args([
        "setup",
        "--wait",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(setup["mode"], "unavailable", "{setup:#}");
    assert_eq!(
        setup["refresh_request"]["reason"], "refresh_failed",
        "{setup:#}"
    );
    assert!(
        setup["refresh_request"]["last_error"]
            .as_str()
            .unwrap()
            .contains("all_provider_terminal_coverage_unavailable"),
        "{setup:#}"
    );
    assert!(
        setup["import"].is_null() || setup["import"]["totals"]["imported_sources"] == 0,
        "{setup:#}"
    );
    assert!(
        setup["import"].is_null() || setup["import"]["totals"]["failed_sources"] == 0,
        "{setup:#}"
    );

    let baseline_status = json_output(ctx(&temp).args(["status", "--format=json"]));
    let discovery_status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        object_keys(&discovery_status),
        object_keys(&baseline_status)
    );
    assert_eq!(
        discovery_status["schema_version"],
        baseline_status["schema_version"]
    );
    for field in ["indexed_events", "indexed_sessions", "indexed_sources"] {
        assert_eq!(discovery_status[field], baseline_status[field], "{field}");
    }
    assert_eq!(
        discovery_status["daemon"]["status"],
        baseline_status["daemon"]["status"]
    );

    let import_all = failure_stderr(ctx(&temp).args(["import", "--all", "--progress", "none"]));
    assert!(
        import_all.contains("all_provider_terminal_coverage_unavailable"),
        "{import_all}"
    );
    assert!(import_all.contains("Kiro ACP/v3"), "{import_all}");

    ctx(&temp).args(["daemon", "disable"]).assert().success();
    let search = json_output(ctx(&temp).args([
        "search",
        "unsupported-kiro-should-not-dispatch",
        "--provider",
        "kiro-cli",
        "--refresh",
        "background",
        "--format=json",
    ]));
    assert_eq!(search["freshness"]["status"], "daemon_unavailable");
    assert_eq!(search["freshness"]["source_count"], 0);
    assert!(search["results"].as_array().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn import_all_survives_absent_kiro_and_relocated_codex_with_stable_replay() {
    use std::os::unix::fs::symlink;

    // Authored path-layout regression; native format support is covered by the
    // existing provider fixtures.
    let temp = tempdir();
    let local = temp.path().join("relocated-local");
    fs::create_dir(&local).unwrap();
    symlink(&local, temp.path().join(".local")).unwrap();
    let codex = temp.path().join(".codex");
    fs::create_dir_all(codex.join("sessions")).unwrap();
    let original = codex_rollout("relocated-session", "relocationoriginalmarker");
    fs::write(codex.join("sessions/session.jsonl"), &original).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    let _daemon = start_source_refresh_daemon(&temp, &data_root(&temp), temp.path(), &state);
    let import = || {
        json_output(ctx(&temp).args([
            "import",
            "--all",
            "--no-blame",
            "--no-daemon",
            "--progress",
            "none",
            "--format=json",
        ]))
    };
    let search = |query: &str| {
        json_output(ctx(&temp).args(["search", query, "--refresh", "off", "--format=json"]))
    };
    let initial = import();
    assert_eq!(initial["outcome"], "success", "{initial:#}");
    assert!(!local.join("share/kiro-cli/data.sqlite3").exists());
    let before = search("relocationoriginalmarker");
    assert_eq!(before["results"].as_array().unwrap().len(), 1);
    assert!(before["results"][0]["citations"]
        .as_array()
        .is_some_and(|citations| !citations.is_empty()));

    let relocated = temp.path().join("relocated-codex");
    fs::rename(&codex, &relocated).unwrap();
    symlink(&relocated, &codex).unwrap();
    let replay = import();
    assert_eq!(replay["outcome"], "success", "{replay:#}");
    assert_eq!(
        replay["totals"]["current_indexed_documents"],
        initial["totals"]["current_indexed_documents"]
    );
    let after = search("relocationoriginalmarker");
    assert_eq!(after["results"].as_array().unwrap().len(), 1);
    assert_eq!(
        before["results"][0]["citations"],
        after["results"][0]["citations"]
    );
    assert_eq!(
        fs::read(relocated.join("sessions/session.jsonl")).unwrap(),
        original
    );

    let appended = json!({
        "timestamp": "2026-08-18T01:00:02Z", "type": "response_item",
        "payload": {"type": "message", "id": "later-message", "role": "user",
            "content": [{"type": "input_text", "text": "relocationappendedmarker"}]}
    });
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(codex.join("sessions/session.jsonl"))
            .unwrap(),
        "{appended}"
    )
    .unwrap();
    let appended_import = import();
    assert_eq!(appended_import["outcome"], "success", "{appended_import:#}");
    assert_eq!(
        search("relocationappendedmarker")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search("relocationoriginalmarker")["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn unreadable_automatic_provider_retains_history_while_healthy_provider_refreshes() {
    unreadable_provider_retains_history(false);
}

#[cfg(unix)]
#[test]
fn saved_root_with_unreadable_ancestor_retains_history_and_recovers_while_peer_advances() {
    unreadable_provider_retains_history(true);
}

#[cfg(unix)]
fn unreadable_provider_retains_history(saved: bool) {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
    let blocked = temp.path().join(if saved {
        "saved-provider-parent"
    } else {
        ".claude"
    });
    let claude = if saved {
        blocked.join("claude")
    } else {
        blocked.clone()
    };
    let projects = claude.join("projects/project");
    fs::create_dir_all(&projects).unwrap();
    fs::write(
        projects.join("retained-session.jsonl"),
        format!(
            "{}\n",
            json!({
                "type": "user", "uuid": "retained-message", "sessionId": "retained-session",
                "timestamp": "2026-08-18T01:00:00Z", "cwd": "/workspace/project",
                "message": {"role": "user", "content": "unreadableretainedmarker"}
            })
        ),
    )
    .unwrap();
    let codex = temp.path().join(".codex/sessions");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("first.jsonl"),
        codex_rollout("healthy-first", "healthyoriginalmarker"),
    )
    .unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    let _daemon = start_source_refresh_daemon(&temp, &data_root(&temp), temp.path(), &state);
    if saved {
        ctx(&temp)
            .args([
                "sources",
                "add",
                "retained",
                "--provider",
                "claude",
                "--root",
            ])
            .arg(&claude)
            .assert()
            .success();
    }
    let config_path = data_root(&temp).join("config.toml");
    let config_before = fs::read(&config_path).unwrap();
    let import = || {
        ctx(&temp)
            .args([
                "import",
                "--all",
                "--no-blame",
                "--no-daemon",
                "--progress",
                "none",
                "--format=json",
            ])
            .output()
            .unwrap()
    };
    let initial = import();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let retained = json_output(ctx(&temp).args([
        "search",
        "unreadableretainedmarker",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        retained["results"].as_array().unwrap().len(),
        1,
        "{retained:#}"
    );
    assert!(!retained["results"][0]["citations"]
        .as_array()
        .unwrap()
        .is_empty());
    let permissions = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let blocked_read = fs::File::open(projects.join("retained-session.jsonl"));
    let blocked_resolution = fs::canonicalize(&claude);
    let unavailable_sources = saved.then(|| {
        ctx(&temp)
            .args(["sources", "--provider", "claude", "--all", "--format=json"])
            .output()
            .unwrap()
    });
    fs::write(
        codex.join("second.jsonl"),
        codex_rollout("healthy-second", "healthynewmarker"),
    )
    .unwrap();
    let partial = import();
    fs::set_permissions(&blocked, permissions).unwrap();
    assert_eq!(
        blocked_read.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    if let Some(listing) = unavailable_sources {
        // Resolving this saved root used to abort config loading before import
        // admission. The CLI must now load it and diagnose its unreadability.
        assert_eq!(
            blocked_resolution.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(
            listing.status.success(),
            "{}",
            String::from_utf8_lossy(&listing.stderr)
        );
        let sources: Value = serde_json::from_slice(&listing.stdout).unwrap();
        assert!(
            sources["sources"].as_array().unwrap().iter().any(|source| {
                source["path"] == claude.join("projects").display().to_string()
                    && source["status"] == "unknown"
                    && source["selection"]["root"] == "retained"
            }),
            "{sources:#}"
        );
        assert!(
            sources["issues"].as_array().unwrap().iter().any(|issue| {
                issue["path"] == claude.display().to_string()
                    && issue["code"] == "selector_unreconstructible"
                    && issue["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("access was denied"))
            }),
            "{sources:#}"
        );
    }
    let report: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(partial.status.success(), saved, "{report:#}");
    if saved {
        // Unavailable saved routes retain their prior ownership outside the
        // executable watch catalog. The discovery diagnostic above owns this
        // condition; import reports the healthy routes it actually attempted.
        assert_eq!(report["outcome"], "success", "{report:#}");
        assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
        assert_eq!(report["totals"]["current_source_count"], 3, "{report:#}");
        assert_eq!(report["totals"]["removed_source_count"], 0, "{report:#}");
        assert_eq!(
            report["totals"]["index_delta"]["searchable_events"], 1,
            "{report:#}"
        );
    } else {
        assert!(
            report["totals"]["failed_sources"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "{report:#}"
        );
    }
    for query in [
        "unreadableretainedmarker",
        "healthyoriginalmarker",
        "healthynewmarker",
    ] {
        let search =
            json_output(ctx(&temp).args(["search", query, "--refresh", "off", "--format=json"]));
        assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
        if query == "unreadableretainedmarker" {
            assert_eq!(
                search["results"][0]["citations"],
                retained["results"][0]["citations"]
            );
        }
    }

    let restored_message = json!({
        "type": "user", "uuid": "restored-message", "sessionId": "retained-session",
        "timestamp": "2026-08-18T01:00:02Z", "cwd": "/workspace/project",
        "message": {"role": "user", "content": "restoredprovidermarker"}
    });
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(projects.join("retained-session.jsonl"))
            .unwrap(),
        "{restored_message}"
    )
    .unwrap();
    let restored = import();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let report: Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
    for query in [
        "unreadableretainedmarker",
        "restoredprovidermarker",
        "healthynewmarker",
    ] {
        let search =
            json_output(ctx(&temp).args(["search", query, "--refresh", "off", "--format=json"]));
        assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
        if query == "unreadableretainedmarker" {
            assert_eq!(
                search["results"][0]["citations"],
                retained["results"][0]["citations"]
            );
        }
    }
    assert_eq!(fs::read(config_path).unwrap(), config_before);
}

#[cfg(unix)]
#[test]
fn relocated_default_claude_projects_import_replay_and_remain_healthy() {
    let temp = daemon_test_root();
    let query = "relocated Claude projects oracle";
    let projects = write_native_claude_fixture(&temp, query);
    let relocated = temp.path().join("relocated-projects");
    fs::rename(projects, &relocated).unwrap();
    let claude = temp.path().join(".claude");
    fs::create_dir(&claude).unwrap();
    std::os::unix::fs::symlink(&relocated, claude.join("projects")).unwrap();

    for _ in 0..2 {
        let report = json_output(ctx(&temp).args([
            "import",
            "--all",
            "--no-blame",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(
            report["totals"]["failed_sources"], 0,
            "{}",
            report["totals"]
        );
        wait_for_projection(&temp, &report);
        assert_eq!(provider_core_counts(&data_root(&temp), "claude"), (1, 2));
        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            "claude",
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, "claude", query, 1, "message");
    }
    json_output(ctx(&temp).args(["daemon", "disable", "--format=json"]));
    let doctor = json_output(ctx(&temp).args(["doctor", "--format=json"]));
    assert_eq!(doctor["ok"], true, "{}", doctor["findings"]);
}

#[cfg(unix)]
fn wait_for_projection(temp: &TempDir, report: &Value) {
    let generation = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|source| source["published_generation"].as_str())
        .unwrap();
    wait_for_test_lexical_projection(temp, generation);
}

#[cfg(unix)]
#[path = "custom_root_discovery/import_failures.rs"]
mod import_failures;
