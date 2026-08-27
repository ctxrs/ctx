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

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--progress", "none", "--format=json"]));
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
